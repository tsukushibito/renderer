"""Render source/reimport with identical settings for conversion QA, not Renderer acceptance."""
import argparse
import json
import math
from pathlib import Path
import sys

import bpy
from mathutils import Vector


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("views", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args(sys.argv[sys.argv.index("--") + 1:])
    if args.input.suffix == ".blend":
        bpy.ops.wm.open_mainfile(filepath=str(args.input.resolve()))
    else:
        bpy.ops.wm.read_factory_settings(use_empty=True)
        bpy.ops.import_scene.gltf(filepath=str(args.input.resolve()))
    scene = bpy.context.scene
    meshes = [o for o in scene.objects if o.type == "MESH"]
    if not args.views.exists():
        points = [o.matrix_world @ Vector(corner) for o in meshes for corner in o.bound_box]
        low = Vector(tuple(min(p[i] for p in points) for i in range(3)))
        high = Vector(tuple(max(p[i] for p in points) for i in range(3)))
        center = (low + high) * 0.5
        span = max(high.x - low.x, high.y - low.y)
        views = [{"name": "overview", "position": list(center + Vector((0.7 * span, -0.7 * span, 0.65 * span))),
                  "target": list(center), "lens": 45}]
        camera = scene.camera or next((o for o in scene.objects if o.type == "CAMERA"), None)
        if camera:
            pos = camera.matrix_world.translation
            direction = camera.matrix_world.to_quaternion() @ Vector((0, 0, -1))
            views.append({"name": "authored-camera", "position": list(pos), "target": list(pos + direction),
                          "lens": float(camera.data.lens)})
        detail = next((o for o in meshes if "vespa" in o.name.lower()), None)
        if detail:
            corners = [detail.matrix_world @ Vector(c) for c in detail.bound_box]
            point = sum(corners, Vector()) / 8
            extent = max((c - point).length for c in corners)
            views.append({"name": "material-detail", "position": list(point + Vector((2.5, -3.5, 2)) * max(extent, 0.5)),
                          "target": list(point), "lens": 50})
        args.views.write_text(json.dumps({"bounds": [list(low), list(high)], "views": views}, indent=2) + "\n", encoding="utf-8")
    config = json.loads(args.views.read_text(encoding="utf-8"))
    for obj in list(scene.objects):
        if obj.type in ("CAMERA", "LIGHT"):
            bpy.data.objects.remove(obj, do_unlink=True)
    world = bpy.data.worlds.new("Conversion QA fixed world")
    world.use_nodes = True
    world.node_tree.nodes["Background"].inputs["Color"].default_value = (0.15, 0.15, 0.15, 1)
    world.node_tree.nodes["Background"].inputs["Strength"].default_value = 1
    scene.world = world
    light = bpy.data.lights.new("Conversion QA sun", "SUN")
    light.energy = 3
    light.angle = 0.02
    sun = bpy.data.objects.new("Conversion QA sun", light)
    scene.collection.objects.link(sun)
    sun.rotation_euler = (math.radians(25), math.radians(-30), math.radians(-25))
    data = bpy.data.cameras.new("Conversion QA camera")
    camera = bpy.data.objects.new("Conversion QA camera", data)
    scene.collection.objects.link(camera)
    scene.camera = camera
    data.clip_start = 0.05
    data.clip_end = 5000
    scene.render.engine = "CYCLES"
    scene.cycles.device = "CPU"
    scene.cycles.samples = 8
    scene.cycles.use_denoising = False
    scene.cycles.use_adaptive_sampling = False
    scene.cycles.seed = 0
    scene.cycles.max_bounces = 4
    scene.render.resolution_x = 960
    scene.render.resolution_y = 540
    scene.render.resolution_percentage = 100
    scene.render.image_settings.file_format = "PNG"
    scene.view_settings.view_transform = "Standard"
    scene.view_settings.look = "None"
    scene.view_settings.exposure = 0
    scene.view_settings.gamma = 1
    args.output.mkdir(parents=True, exist_ok=True)
    for view in config["views"]:
        camera.location = view["position"]
        camera.rotation_euler = (Vector(view["target"]) - camera.location).to_track_quat("-Z", "Y").to_euler()
        data.lens = view["lens"]
        scene.render.filepath = str((args.output / f"{view['name']}.png").resolve())
        bpy.ops.render.render(write_still=True)
    (args.output / "settings.json").write_text(json.dumps({"blender": bpy.app.version_string,
        "engine": "Cycles CPU", "samples": 8, "seed": 0, "resolution": [960, 540],
        "view_transform": "Standard", "world": [0.15, 0.15, 0.15], "sun_energy": 3,
        "purpose": "Controlled conversion QA; not the selected Bistro runtime camera/light or GPU acceptance"}, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
