"""Render material-scoped Bistro alpha alternatives with all scene meshes retained.

Run in pinned Blender 4.5.16; this CPU diagnostic is not Renderer/GPU acceptance.
Views use Blender Z-up world coordinates and select one of the two audited materials.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import sys
from urllib.parse import unquote

import bpy
from mathutils import Vector


TARGETS = {"MASTER_Glass_Dirty_MASKED": 68, "Paris_LiquorBottle_01_Labels": 154}


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def inventory(scene):
    records = []
    for obj in sorted((o for o in scene.objects if o.type == "MESH"), key=lambda o: o.name):
        mesh = obj.data
        records.append({"name": obj.name, "vertices": len(mesh.vertices),
                        "triangles": sum(len(p.vertices) - 2 for p in mesh.polygons),
                        "materials": [s.material.name if s.material else None for s in obj.material_slots],
                        "matrix_world": [list(row) for row in obj.matrix_world]})
    return records


def opaque_copy(material):
    candidate = material.copy()
    candidate.name = material.name + ".alpha-opaque-diagnostic"
    nodes = [n for n in candidate.node_tree.nodes if n.type == "BSDF_PRINCIPLED"]
    if len(nodes) != 1:
        raise ValueError(f"Expected one Principled shader: {material.name}")
    alpha = nodes[0].inputs["Alpha"]
    for link in list(alpha.links):
        candidate.node_tree.links.remove(link)
    alpha.default_value = 1
    candidate.surface_render_method = "DITHERED"
    candidate.pass_index = 1
    return candidate


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("views", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--samples", type=int, default=16)
    parser.add_argument("--resolution", nargs=2, type=int, default=[960, 540])
    args = parser.parse_args(sys.argv[sys.argv.index("--") + 1:])
    if bpy.app.version != (4, 5, 16) or args.samples < 1 or min(args.resolution) < 1:
        raise ValueError("Require Blender 4.5.16 and positive render dimensions/samples")
    source = json.loads(args.input.read_text(encoding="utf-8"))
    views = json.loads(args.views.read_text(encoding="utf-8"))["views"]
    if not views or len({v["name"] for v in views}) != len(views):
        raise ValueError("Require uniquely named views")
    for view in views:
        if view["material"] not in TARGETS or Path(view["name"]).name != view["name"]:
            raise ValueError(f"Unsupported material or view name: {view}")
    bpy.ops.wm.read_factory_settings(use_empty=True)
    bpy.ops.import_scene.gltf(filepath=str(args.input.resolve()))
    scene = bpy.context.scene
    before = inventory(scene)
    original_indices = {m: m.pass_index for m in bpy.data.materials}
    for material in original_indices:
        material.pass_index = 0
    targets = {}
    for name, expected in TARGETS.items():
        material = bpy.data.materials.get(name)
        if material is None:
            raise ValueError(f"Missing audited material: {name}")
        slots = [(o, i) for o in scene.objects if o.type == "MESH"
                 for i, s in enumerate(o.material_slots) if s.material == material]
        triangles = sum(len(p.vertices) - 2 for obj, index in slots
                        for p in obj.data.polygons if p.material_index == index)
        if triangles != expected:
            raise ValueError(f"Unexpected affected triangle count for {name}: {triangles}")
        targets[name] = {"material": material, "slots": slots, "candidate": opaque_copy(material),
                         "triangles": triangles, "objects": sorted({o.name for o, _ in slots})}
    for obj in list(scene.objects):
        if obj.type in ("CAMERA", "LIGHT"):
            bpy.data.objects.remove(obj, do_unlink=True)
    world = bpy.data.worlds.new("Alpha QA fixed world")
    world.use_nodes = True
    world.node_tree.nodes["Background"].inputs["Color"].default_value = (0.15, 0.15, 0.15, 1)
    world.node_tree.nodes["Background"].inputs["Strength"].default_value = 1
    scene.world = world
    light = bpy.data.lights.new("Alpha QA sun", "SUN")
    light.energy = 3
    light.angle = 0.02
    sun = bpy.data.objects.new("Alpha QA sun", light)
    scene.collection.objects.link(sun)
    sun.rotation_euler = tuple(math.radians(x) for x in (25, -30, -25))
    data = bpy.data.cameras.new("Alpha QA camera")
    camera = bpy.data.objects.new("Alpha QA camera", data)
    scene.collection.objects.link(camera)
    scene.camera = camera
    data.clip_start, data.clip_end = 0.005, 5000
    scene.render.engine = "CYCLES"
    scene.cycles.device = "CPU"
    scene.cycles.samples = args.samples
    scene.cycles.use_denoising = False
    scene.cycles.use_adaptive_sampling = False
    scene.cycles.seed = 0
    scene.cycles.max_bounces = 4
    scene.render.resolution_x, scene.render.resolution_y = args.resolution
    scene.render.resolution_percentage = 100
    scene.render.image_settings.file_format = "PNG"
    scene.render.image_settings.color_mode = "RGB"
    scene.render.image_settings.color_depth = "8"
    scene.view_settings.view_transform = "Standard"
    scene.view_settings.look = "None"
    scene.view_settings.exposure = 0
    scene.view_settings.gamma = 1
    # IndexMA identifies the forced-opaque target's visible surface while other
    # geometry still occludes it. Unlike a pixel-color heuristic it cannot count
    # unrelated scene colors as coverage. No object/material is removed.
    scene.view_layers[0].use_pass_material_index = True
    scene.use_nodes = True
    tree = scene.node_tree
    tree.nodes.clear()
    layers = tree.nodes.new("CompositorNodeRLayers")
    mask = tree.nodes.new("CompositorNodeIDMask")
    mask.index = 1
    mask.use_antialiasing = True
    composite = tree.nodes.new("CompositorNodeComposite")
    tree.links.new(layers.outputs["IndexMA"], mask.inputs["ID value"])
    tree.links.new(mask.outputs["Alpha"], composite.inputs["Image"])
    args.output.mkdir(parents=True, exist_ok=True)
    renders = []
    for view in views:
        target = targets[view["material"]]
        camera.location = view["position"]
        camera.rotation_euler = (Vector(view["target"]) - camera.location).to_track_quat("-Z", "Y").to_euler()
        data.lens = view["lens"]
        record = dict(view, images={})
        for variant in ("source-alpha", "force-opaque", "coverage"):
            for obj, index in target["slots"]:
                obj.material_slots[index].material = target["material"] if variant == "source-alpha" else target["candidate"]
            scene.render.use_compositing = variant == "coverage"
            path = args.output / f"{view['name']}-{variant}.png"
            scene.render.filepath = str(path.resolve())
            bpy.ops.render.render(write_still=True)
            record["images"][variant] = {"file": path.name, "sha256": digest(path)}
        for obj, index in target["slots"]:
            obj.material_slots[index].material = target["material"]
        renders.append(record)
    for material, index in original_indices.items():
        material.pass_index = index
    after = inventory(scene)
    if before != after:
        raise ValueError("Mesh inventory changed during material-only diagnostics")
    uris = {unquote(item["uri"]) for group in ("buffers", "images")
            for item in source.get(group, []) if "uri" in item}
    files = {args.input.name: digest(args.input)}
    for uri in sorted(uris):
        if ":" in uri or not (args.input.parent / uri).resolve().is_relative_to(args.input.parent.resolve()):
            raise ValueError(f"Unsupported external resource: {uri}")
        files[uri] = digest(args.input.parent / uri)
    settings = {"blender": bpy.app.version_string, "blender_build": bpy.app.build_hash.decode(),
                "engine": "Cycles CPU", "samples": args.samples, "seed": 0,
                "denoising": False, "adaptive_sampling": False, "max_bounces": 4,
                "resolution": args.resolution, "view_transform": "Standard", "look": "None",
                "exposure": 0, "gamma": 1, "world_color": [0.15] * 3, "world_strength": 1,
                "sun_energy": 3, "sun_angle_radians": 0.02, "sun_rotation_degrees": [25, -30, -25],
                "camera_clip": [data.clip_start, data.clip_end]}
    report = {"schema_version": 1, "purpose": "Material-only CPU alpha diagnostic, not Renderer/GPU acceptance or proof of artist intent",
              "script_sha256": digest(Path(__file__)), "views_sha256": digest(args.views),
              "input_files_sha256": files, "settings": settings,
              "mesh_inventory_unchanged": before == after, "mesh_objects": len(before),
              "triangles": sum(o["triangles"] for o in before),
              "materials": {name: {key: value for key, value in target.items()
                                   if key in ("triangles", "objects")} for name, target in targets.items()},
              "source_materials": [m for m in source["materials"] if m["name"] in TARGETS],
              "coverage": "IndexMA=1 on the target's opaque copy, zero on all other materials; all scene meshes retained",
              "views": renders}
    (args.output / "renders.json").write_text(json.dumps(report, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    print(json.dumps({"views": len(renders), "mesh_objects": len(before), "triangles": report["triangles"]}))


if __name__ == "__main__":
    main()
