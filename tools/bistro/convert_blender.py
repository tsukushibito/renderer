"""Pinned Bistro v5.2 FBX -> glTF conversion; run with Blender --background --python."""
import argparse
import hashlib
import importlib
import inspect
import json
from pathlib import Path
import sys
import textwrap

import bpy
import numpy as np

FBX_SHA256 = "471991629c7e35a88338c48f3565390ad19cc5d1d7c7e067fc2a60b4f3b5f195"


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def preserve_source_faces():
    # Blender's mesh cleanup deletes 2,894 authored duplicate faces. 980 have
    # opposite winding; UVs/normals differ too. Keep them after the raw FBX audit.
    if bpy.app.version != (4, 5, 16):
        raise ValueError("This import adaptation is audited only for Blender 4.5.16")
    module = importlib.import_module("io_scene_fbx.import_fbx")
    original = inspect.getsource(module.blen_read_geom)
    marker = "mesh.validate(clean_customdata=False)"
    if original.count(marker) != 1:
        raise ValueError("Unexpected FBX importer; re-audit duplicate-face handling")
    adapted = original.replace(marker, "mesh.update(calc_edges=True)  # Validate after separating authored duplicate faces")
    normal_marker = 'mesh.attributes["temp_custom_normals"].data.foreach_get("vector", clnors)'
    if adapted.count(normal_marker) != 1:
        raise ValueError("Unexpected FBX custom-normal import path")
    adapted = adapted.replace(normal_marker, normal_marker + '\n        mesh.attributes.new("_bistro_source_normals", "FLOAT_VECTOR", "CORNER").data.foreach_set("vector", clnors)')
    exec(compile(adapted, module.__file__, "exec"), module.__dict__)
    return {"file_sha256": digest(Path(module.__file__)),
            "function_sha256": hashlib.sha256(original.encode()).hexdigest(),
            "adapted_function_sha256": hashlib.sha256(adapted.encode()).hexdigest(),
            "change": "Defer importer mesh.validate until duplicate faces have independent vertex indices; then run normal Blender validation"}


def separate_duplicate_faces():
    """Preserve authored duplicate faces while satisfying Blender's index-set rule."""
    total = 0
    for obj in list(bpy.context.scene.objects):
        if obj.type != "MESH":
            continue
        mesh = obj.data
        raw_normals = np.empty(len(mesh.loops) * 3, dtype=np.float32)
        mesh.attributes["_bistro_source_normals"].data.foreach_get("vector", raw_normals)
        if not np.isfinite(raw_normals).all() or np.any(np.linalg.norm(raw_normals.reshape(-1, 3), axis=1) < 1e-8):
            raise ValueError(f"Invalid authored source normal: {obj.name}")
        sizes = np.empty(len(mesh.polygons), dtype=np.int32)
        mesh.polygons.foreach_get("loop_total", sizes)
        if not np.all(sizes == 3):
            raise ValueError(f"Unexpected nontriangle source: {obj.name}")
        indices = np.empty(len(mesh.loops), dtype=np.int32)
        mesh.loops.foreach_get("vertex_index", indices)
        faces = indices.reshape(-1, 3).copy()
        _, first = np.unique(np.sort(faces, axis=1), axis=0, return_index=True)
        duplicates = np.ones(len(faces), dtype=bool)
        duplicates[first] = False
        count = int(duplicates.sum())
        if count:
            vertices = np.empty(len(mesh.vertices) * 3, dtype=np.float32)
            mesh.vertices.foreach_get("co", vertices)
            vertices = vertices.reshape(-1, 3)
            extra = vertices[faces[duplicates]].reshape(-1, 3)
            faces[duplicates] = np.arange(len(vertices), len(vertices) + len(extra)).reshape(-1, 3)
            positions = np.concatenate((vertices, extra))
            materials = np.empty(len(mesh.polygons), dtype=np.int32)
            smooth = np.empty(len(mesh.polygons), dtype=bool)
            mesh.polygons.foreach_get("material_index", materials)
            mesh.polygons.foreach_get("use_smooth", smooth)
            uvs = []
            for layer in mesh.uv_layers:
                values = np.empty(len(layer.data) * 2, dtype=np.float32)
                layer.data.foreach_get("uv", values)
                uvs.append((layer.name, values))
            replacement = bpy.data.meshes.new(mesh.name + ".source_faces")
            replacement.vertices.add(len(positions))
            replacement.vertices.foreach_set("co", positions.ravel())
            replacement.loops.add(len(indices))
            replacement.loops.foreach_set("vertex_index", faces.ravel())
            replacement.polygons.add(len(faces))
            replacement.polygons.foreach_set("loop_start", np.arange(len(faces), dtype=np.int32) * 3)
            replacement.polygons.foreach_set("loop_total", sizes)
            replacement.polygons.foreach_set("material_index", materials)
            replacement.polygons.foreach_set("use_smooth", smooth)
            for mat in mesh.materials:
                replacement.materials.append(mat)
            for name, values in uvs:
                replacement.uv_layers.new(name=name).data.foreach_set("uv", values)
            replacement.update(calc_edges=True)
            replacement.attributes.new("_bistro_source_normals", "FLOAT_VECTOR", "CORNER").data.foreach_set("vector", raw_normals)
            obj.data = replacement
            bpy.data.meshes.remove(mesh)
            mesh = replacement
            total += count
        mesh.update(calc_edges=True)
        before = len(mesh.polygons)
        if mesh.validate(clean_customdata=False) or len(mesh.polygons) != before:
            raise ValueError(f"Source-preserving mesh still fails Blender validation: {obj.name}")
        mesh.normals_split_custom_set(raw_normals.reshape(-1, 3).tolist())
    if total != 2894:
        raise ValueError(f"Unexpected duplicate face count: {total}")
    return {"faces": total, "additional_vertices": total * 3,
            "policy": "Separate duplicate face vertex indices; preserve positions, corner normals, both UV sets, material and winding; Blender validation must pass unchanged"}


def rebuild_materials(audit, texture_report, texture_dir):
    prepared = {item["source"]: item for item in texture_report["textures"]}
    recipes = []
    settings = bpy.data.node_groups.new("glTF Material Output", "ShaderNodeTree")
    settings.interface.new_socket(name="Occlusion", in_out="INPUT", socket_type="NodeSocketFloat")
    settings.nodes.new("NodeGroupOutput")
    settings.nodes.new("NodeGroupInput")
    for source in sorted(audit["materials"], key=lambda item: item["name"]):
        material = bpy.data.materials.get(source["name"])
        if material is None:
            raise ValueError(f"Missing imported material: {source['name']}")
        material.use_nodes = True
        material.use_backface_culling = False  # All source mesh Models: CullingOff.
        tree = material.node_tree
        tree.nodes.clear()
        bsdf = tree.nodes.new("ShaderNodeBsdfPrincipled")
        out = tree.nodes.new("ShaderNodeOutputMaterial")
        tree.links.new(bsdf.outputs["BSDF"], out.inputs["Surface"])
        bsdf.inputs["Base Color"].default_value = (1, 1, 1, 1)
        bsdf.inputs["Metallic"].default_value = 0
        bsdf.inputs["Roughness"].default_value = 0.5
        bsdf.inputs["Specular IOR Level"].default_value = 0.5
        recipe = {"name": source["name"], "source_id": source["id"], "textures": source["textures"],
                  "double_sided": True, "alpha_mode": "OPAQUE", "uv_set": "TextureUV"}
        uv_map = tree.nodes.new("ShaderNodeUVMap")
        uv_map.uv_map = "TextureUV"
        for channel, relative in source["textures"].items():
            record = prepared[relative]
            node = tree.nodes.new("ShaderNodeTexImage")
            node.image = bpy.data.images.load(str(texture_dir / record["output"]), check_existing=True)
            node.image.colorspace_settings.name = "sRGB" if record["colorspace"] == "sRGB" else "Non-Color"
            node.interpolation = "Linear"
            node.extension = "REPEAT"
            tree.links.new(uv_map.outputs["UV"], node.inputs["Vector"])
            if channel == "DiffuseColor":
                tree.links.new(node.outputs["Color"], bsdf.inputs["Base Color"])
                if record["alpha"]["min"] < 255:
                    if "Foliage" in source["name"] or "MASKED" in source["name"]:
                        clip = tree.nodes.new("ShaderNodeMath")
                        clip.operation = "LESS_THAN"
                        clip.inputs[1].default_value = 0.5
                        tree.links.new(node.outputs["Alpha"], clip.inputs[0])
                        keep = tree.nodes.new("ShaderNodeMath")
                        keep.operation = "SUBTRACT"
                        keep.inputs[0].default_value = 1
                        tree.links.new(clip.outputs[0], keep.inputs[1])
                        tree.links.new(keep.outputs[0], bsdf.inputs["Alpha"])
                        recipe.update(alpha_mode="MASK", alpha_cutoff=0.5,
                                      alpha_reason="Bistro cutout material name plus nonopaque BaseColor alpha; explicit conversion policy")
                    else:
                        tree.links.new(node.outputs["Alpha"], bsdf.inputs["Alpha"])
                        recipe.update(alpha_mode="BLEND", alpha_reason="Fractional/transparent BaseColor alpha; retained without opaque substitution")
            elif channel == "NormalMap":
                normal = tree.nodes.new("ShaderNodeNormalMap")
                normal.uv_map = "TextureUV"
                tree.links.new(node.outputs["Color"], normal.inputs["Color"])
                tree.links.new(normal.outputs["Normal"], bsdf.inputs["Normal"])
            elif channel == "SpecularColor":
                split = tree.nodes.new("ShaderNodeSeparateColor")
                tree.links.new(node.outputs["Color"], split.inputs["Color"])
                tree.links.new(split.outputs["Green"], bsdf.inputs["Roughness"])
                tree.links.new(split.outputs["Blue"], bsdf.inputs["Metallic"])
                group = tree.nodes.new("ShaderNodeGroup")
                group.node_tree = settings
                tree.links.new(split.outputs["Red"], group.inputs["Occlusion"])
            elif channel == "EmissiveColor":
                tree.links.new(node.outputs["Color"], bsdf.inputs["Emission Color"])
                factor = source["properties"].get("EmissiveColor", {"value": [1, 1, 1]})["value"]
                if len(set(factor)) != 1:
                    raise ValueError(f"Unaudited nonuniform emissive factor: {source['name']}")
                bsdf.inputs["Emission Strength"].default_value = factor[0]
                recipe["emissive_factor"] = factor
            else:
                raise ValueError(f"Unaudited FBX texture channel: {channel}")
        if "SpecularColor" not in source["textures"]:
            recipe["orm_fallback"] = {"metallic": 0, "roughness": 0.5,
                                      "reason": "No ORM texture in source; explicit nonmetal label material baseline, not inferred authored PBR values"}
        material.surface_render_method = "BLENDED" if recipe["alpha_mode"] == "BLEND" else "DITHERED"
        recipes.append(recipe)
    return recipes


def save_baseline(output):
    geometry_dir = output / "geometry"
    geometry_dir.mkdir(exist_ok=True)
    objects = []
    for index, obj in enumerate(sorted((o for o in bpy.context.scene.objects if o.type == "MESH"), key=lambda o: o.name)):
        mesh = obj.data
        mesh.calc_loop_triangles()
        mesh.calc_tangents(uvmap="TextureUV")
        positions = np.empty(len(mesh.vertices) * 3, dtype=np.float32)
        mesh.vertices.foreach_get("co", positions)
        triangles = np.empty(len(mesh.loop_triangles) * 3, dtype=np.int32)
        loops = np.empty_like(triangles)
        material_indices = np.empty(len(mesh.loop_triangles), dtype=np.int32)
        mesh.loop_triangles.foreach_get("vertices", triangles)
        mesh.loop_triangles.foreach_get("loops", loops)
        mesh.loop_triangles.foreach_get("material_index", material_indices)
        normals = np.empty(len(mesh.corner_normals) * 3, dtype=np.float32)
        mesh.corner_normals.foreach_get("vector", normals)
        raw_normals = np.empty_like(normals)
        mesh.attributes["_bistro_source_normals"].data.foreach_get("vector", raw_normals)
        authored = raw_normals.reshape(-1, 3).copy()
        authored /= np.linalg.norm(authored, axis=1, keepdims=True)
        arrays = {"positions": positions.reshape(-1, 3), "triangles": triangles.reshape(-1, 3),
                  "triangle_loops": loops.reshape(-1, 3), "material_indices": material_indices,
                  "loop_normals": authored, "source_loop_normals": raw_normals.reshape(-1, 3),
                  "blender_loop_normals": normals.reshape(-1, 3)}
        if not np.isfinite(positions).all() or not np.isfinite(normals).all():
            raise ValueError(f"Nonfinite imported vertex/normal: {obj.name}")
        if triangles.min() < 0 or triangles.max() >= len(mesh.vertices):
            raise ValueError(f"Imported index out of range: {obj.name}")
        for uv_index, layer in enumerate(mesh.uv_layers):
            uv = np.empty(len(layer.data) * 2, dtype=np.float32)
            layer.data.foreach_get("uv", uv)
            arrays["loop_uv" if uv_index == 0 else f"loop_uv_{uv_index}"] = uv.reshape(-1, 2)
            if not np.isfinite(uv).all():
                raise ValueError(f"Nonfinite imported UV: {obj.name}")
        relative = f"geometry/{index:04d}.npz"
        np.savez(output / relative, **arrays)
        objects.append({"name": obj.name, "matrix_world": [list(row) for row in obj.matrix_world],
                        "materials": [slot.material.name if slot.material else None for slot in obj.material_slots],
                        "uv_layers": list(mesh.uv_layers.keys()), "geometry": relative,
                        "triangles": len(mesh.loop_triangles), "vertices": len(mesh.vertices)})
    return {"schema_version": 1, "coordinates": "Blender Z-up world, FBX import scale already in matrix_world; glTF Y-up=(x,z,-y)",
            "objects": objects, "triangles": sum(o["triangles"] for o in objects)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("audit", type=Path)
    parser.add_argument("texture_report", type=Path)
    parser.add_argument("texture_directory", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args(sys.argv[sys.argv.index("--") + 1:])
    args.output.mkdir(parents=True, exist_ok=True)
    fbx = args.source.resolve() / "BistroExterior.fbx"
    if digest(fbx) != FBX_SHA256:
        raise ValueError("Unsupported FBX; audit before adapting the importer")
    audit = json.loads(args.audit.read_text(encoding="utf-8"))
    if audit["source"]["sha256"] != FBX_SHA256 or audit["summary"]["source_geometry_triangles"] != 2832120 or audit["summary"]["missing_texture_references"]:
        raise ValueError("Unexpected or incomplete source audit")
    bpy.ops.wm.read_factory_settings(use_empty=True)
    importer_change = preserve_source_faces()
    bpy.ops.import_scene.fbx(filepath=str(fbx), use_custom_normals=True, use_image_search=False, use_anim=False)
    duplicate_preservation = separate_duplicate_faces()
    bpy.context.view_layer.update()
    recipes = rebuild_materials(audit, json.loads(args.texture_report.read_text(encoding="utf-8")), args.texture_directory.resolve())
    baseline = save_baseline(args.output)
    if baseline["triangles"] != 2832120 or len(baseline["objects"]) != 1296:
        raise ValueError(f"Source topology not preserved: {baseline['triangles']} triangles")
    baseline.update(materials=recipes, importer_change=importer_change, blender=bpy.app.version_string,
                    duplicate_preservation=duplicate_preservation,
                    blender_build=bpy.app.build_hash.decode(), source_sha256=FBX_SHA256,
                    source_audit_sha256=digest(args.audit), texture_report_sha256=digest(args.texture_report),
                    script_sha256=digest(Path(__file__)), snapshot="Static source Model transforms, use_anim=False")
    (args.output / "baseline.json").write_text(json.dumps(baseline, indent=2) + "\n", encoding="utf-8")
    bpy.ops.wm.save_as_mainfile(filepath=str((args.output / "source.blend").resolve()))
    settings = dict(filepath=str((args.output / "BistroExterior.gltf").resolve()), export_format="GLTF_SEPARATE",
                    export_texture_dir="textures", export_image_format="AUTO", export_keep_originals=False,
                    export_texcoords=True, export_normals=True, export_tangents=False,
                    export_materials="EXPORT", export_all_vertex_colors=True, export_cameras=True,
                    export_lights=True, use_selection=False, use_visible=False, use_renderable=False,
                    export_animations=False, export_yup=True, export_apply=False,
                    export_draco_mesh_compression_enable=False, export_use_gltfpack=False)
    (args.output / "export-settings.json").write_text(json.dumps(settings, indent=2) + "\n", encoding="utf-8")
    extractor = importlib.import_module("io_scene_gltf2.blender.exp.primitive_extract")
    baseline["export_normal_rounding_digits"] = {"original": extractor.ROUNDING_DIGIT, "selected": 7}
    extractor.ROUNDING_DIGIT = 7
    original = textwrap.dedent(inspect.getsource(extractor.PrimitiveCreator._PrimitiveCreator__get_normals))
    marker = "self.blender_mesh.corner_normals.foreach_get('vector', self.normals)"
    if original.count(marker) != 1:
        raise ValueError("Unexpected glTF normal extraction; re-audit authored normal preservation")
    adapted = original.replace(marker, 'self.blender_mesh.attributes["_bistro_source_normals"].data.foreach_get("vector", self.normals)')
    adapted = adapted.replace("def __get_normals(self):", "def bistro_get_normals(self):")
    exec(compile(adapted, extractor.__file__, "exec"), extractor.__dict__)
    extractor.PrimitiveCreator._PrimitiveCreator__get_normals = extractor.bistro_get_normals
    baseline["normal_export"] = {"policy": "Export normalized authored CORNER normals directly, bypassing Blender packed-normal precision loss",
        "extractor_file_sha256": digest(Path(extractor.__file__)),
        "original_function_sha256": hashlib.sha256(original.encode()).hexdigest(),
        "adapted_function_sha256": hashlib.sha256(adapted.encode()).hexdigest()}
    baseline["tangent_policy"] = "No authored tangent layer. Omit TANGENT in glTF; consumer generates MikkTSpace. A diagnostic export generated zero vectors, so robust cook generation remains a B2 requirement."
    (args.output / "baseline.json").write_text(json.dumps(baseline, indent=2) + "\n", encoding="utf-8")
    bpy.ops.export_scene.gltf(**settings)
    print(json.dumps({"objects": len(baseline["objects"]), "triangles": baseline["triangles"],
                      "materials": len(recipes), "glTF_sha256": digest(args.output / "BistroExterior.gltf")}, indent=2))


if __name__ == "__main__":
    main()
