"""Export evaluated Blender instances with shared meshes and PBR texture channels."""
import argparse
import json
import os
from pathlib import Path

os.environ.setdefault("OPENCV_IO_ENABLE_OPENEXR", "1")
import bpy
import cv2
import numpy as np

from mathutils import Matrix

# Blender Z-up to glTF Y-up. Unit scale and handedness are preserved.
BASIS = Matrix(((1, 0, 0, 0), (0, 0, 1, 0), (0, -1, 0, 0), (0, 0, 0, 1)))


def values(collection, name, width, dtype):
    data = np.empty(len(collection) * width, dtype=dtype)
    collection.foreach_get(name, data)
    return data.reshape((-1, width))


def texture_pixels(path, limit):
    img = cv2.imread(str(path), cv2.IMREAD_UNCHANGED)
    if img is None:
        raise ValueError(f"missing texture {path}")
    if max(img.shape[:2]) > limit:
        h, w = img.shape[:2]
        img = cv2.resize(img, (round(w * limit / max(h, w)), round(h * limit / max(h, w))),
                         interpolation=cv2.INTER_AREA)
    if np.issubdtype(img.dtype, np.floating):
        img = np.rint(img.clip(0, 1) * 255).astype(np.uint8)
    elif img.dtype == np.uint16:
        img = np.rint(img / 257).astype(np.uint8)
    return img


class Export:
    def __init__(self, directory):
        self.directory = directory
        self.gltf = dict(asset={"version": "2.0"}, scene=0, scenes=[{"nodes": []}], nodes=[],
                         materials=[], images=[], textures=[], meshes=[], buffers=[],
                         bufferViews=[], accessors=[])
        self.materials = {}
        self.meshes = {}
        self.binary = (directory / "geometry.bin").open("wb")

    def texture(self, name, pixels):
        name = name + ".png"
        if not cv2.imwrite(str(self.directory / name), pixels):
            raise OSError(name)
        index = len(self.gltf["images"])
        self.gltf["images"].append({"uri": name})
        self.gltf["textures"].append({"source": index})
        return {"index": index}

    def material(self, mat):
        if mat.name in self.materials:
            return self.materials[mat.name]
        paths = [Path(bpy.path.abspath(node.image.filepath)) for node in mat.node_tree.nodes
                 if node.type == "TEX_IMAGE" and node.image]
        def find(*tokens):
            return next((p for p in paths if any(t in p.stem.lower() for t in tokens)), None)
        color = find("_diff", "_albedo")
        if color is None:
            raise ValueError(f"material has no base color: {mat.name}")
        limit = 2048 if mat.name.startswith(("landscape", "cliff", "namaqualand_boulder")) else 1024
        rgba = texture_pixels(color, limit)
        alpha = find("_alpha")
        if mat.name == "fine_leaf_01":
            alpha = find("_mask")
        if alpha:
            a = texture_pixels(alpha, limit)
            if a.ndim == 3:
                a = a[:, :, 0]
            rgba = np.dstack((rgba[:, :, :3], cv2.resize(a, (rgba.shape[1], rgba.shape[0]))))
        masked = rgba.shape[-1] == 4 and rgba[:, :, 3].min() < 128
        pbr = {"metallicFactor": 0, "roughnessFactor": 0.85,
               "baseColorTexture": self.texture(mat.name + "-color", rgba)}
        record = {"name": mat.name, "pbrMetallicRoughness": pbr,
                  "doubleSided": bool(masked), "alphaMode": "MASK" if masked else "OPAQUE"}
        normal = find("_nor_gl", "_norm_gl", "_gl_norm", "_normal")
        if normal:
            record["normalTexture"] = self.texture(mat.name + "-normal",
                                                   texture_pixels(normal, limit)[:, :, :3])
        roughness = find("_rough")
        if roughness:
            r = texture_pixels(roughness, limit)
            if r.ndim == 3:
                r = r[:, :, 0]
            packed = np.zeros((*r.shape, 3), np.uint8)
            packed[:, :, 1] = r
            packed[:, :, 2] = 255
            pbr["metallicRoughnessTexture"] = self.texture(mat.name + "-orm", packed)
            pbr["roughnessFactor"] = 1
        index = len(self.gltf["materials"])
        self.gltf["materials"].append(record)
        self.materials[mat.name] = index
        return index

    def accessor(self, arr, kind, *, position=False):
        arr = np.ascontiguousarray(arr)
        offset = self.binary.tell()
        self.binary.write(arr.tobytes())
        view = len(self.gltf["bufferViews"])
        self.gltf["bufferViews"].append(dict(buffer=0, byteOffset=offset, byteLength=arr.nbytes))
        record = dict(bufferView=view, componentType=5125 if arr.dtype == np.uint32 else 5126,
                      count=len(arr), type=kind)
        if position:
            record.update(min=arr.min(axis=0).tolist(), max=arr.max(axis=0).tolist())
        self.gltf["accessors"].append(record)
        return len(self.gltf["accessors"]) - 1

    def mesh(self, mesh):
        key = mesh.as_pointer()
        if key in self.meshes:
            return self.meshes[key]
        mesh.calc_loop_triangles()
        pos = values(mesh.vertices, "co", 3, np.float32)
        indices = values(mesh.loops, "vertex_index", 1, np.int32).ravel()
        normals = values(mesh.corner_normals, "vector", 3, np.float32)
        uv = values(mesh.uv_layers.active.data, "uv", 2, np.float32)
        if mesh.materials[0].name in ("landscape_main", "landscape_background"):
            uv *= 7
        elif mesh.materials[0].name == "damp_sand":
            uv *= 1.5
        uv[:, 1] = 1 - uv[:, 1]
        # Weld loops only when their source vertex, corner normal and UV all agree.
        key_data = np.column_stack((indices.view(np.float32), normals, uv))
        _, first, inverse = np.unique(key_data.view("V24").ravel(), return_index=True, return_inverse=True)
        p = pos[indices[first]][:, [0, 2, 1]].copy()
        n = normals[first][:, [0, 2, 1]].copy()
        p[:, 2] *= -1
        n[:, 2] *= -1
        attrs = dict(POSITION=self.accessor(p, "VEC3", position=True),
                     NORMAL=self.accessor(n, "VEC3"), TEXCOORD_0=self.accessor(uv[first], "VEC2"))
        tris = values(mesh.loop_triangles, "loops", 3, np.int32)
        slots = values(mesh.loop_triangles, "material_index", 1, np.int32).ravel()
        primitives = []
        for slot in np.unique(slots):
            idx = inverse[tris[slots == slot]].astype(np.uint32).ravel()
            primitives.append(dict(attributes=attrs, indices=self.accessor(idx, "SCALAR"),
                                   material=self.material(mesh.materials[slot])))
        index = len(self.gltf["meshes"])
        self.gltf["meshes"].append({"name": mesh.name, "primitives": primitives})
        self.meshes[key] = index
        return index

    def finish(self):
        self.gltf["buffers"] = [{"uri": "geometry.bin", "byteLength": self.binary.tell()}]
        self.binary.close()
        self.gltf["scenes"][0]["nodes"] = list(range(len(self.gltf["nodes"])))
        (self.directory / "section.gltf").write_text(json.dumps(self.gltf, separators=(",", ":")))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("blend", type=Path)
    parser.add_argument("out", type=Path)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    bpy.ops.wm.open_mainfile(filepath=str(args.blend.resolve()), load_ui=False, use_scripts=False)
    graph = bpy.context.evaluated_depsgraph_get()
    scene_objects = set(bpy.data.collections["scene_main"].all_objects)
    export = Export(args.out)
    for inst in graph.object_instances:
        obj = inst.object
        if obj.type != "MESH" or not obj.data.polygons or obj.hide_render:
            continue
        if (inst.parent.original if inst.is_instance else obj.original) not in scene_objects:
            continue
        x, y, z = inst.matrix_world.translation
        scatter = inst.is_instance and inst.parent.name in ("ground_plane_scatter", "ground_plane_foliage", "background_hill")
        if scatter:
            # Preserve every authored placement in the walkable patch. Extend its
            # silhouette with a repeatable, tapered apron of the original instances.
            distance = np.hypot(max(-5 - x, 0, x - 6), max(-15 - y, 0, y))
            if distance > 0:
                density = max(0., 1. - distance / 14.) ** 3
                sample = (int(inst.random_id) & 0xffffffff) / 2**32
                if sample >= density:
                    continue
        if obj.name.startswith("billboards") or not obj.data.materials:
            continue
        index = export.mesh(obj.data)
        matrix = BASIS @ inst.matrix_world @ BASIS.transposed()
        export.gltf["nodes"].append({"name": obj.original.name, "mesh": index,
                                    "matrix": list(np.array(matrix).T.ravel())})
    export.finish()
    print(f"wrote {len(export.meshes)} shared meshes and {len(export.gltf['nodes'])} placements")


if __name__ == "__main__":
    main()
