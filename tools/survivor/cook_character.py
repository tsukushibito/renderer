"""Blender 4.5 LTS offline cook for the two Quaternius research subjects.

blender -b -t 1 --factory-startup --python-exit-code 1 -P tools/survivor/cook_character.py \
    -- <source-directory> assets/survivor/characters
"""

import json
import math
from pathlib import Path
import struct
import sys

import bpy
from mathutils import Matrix, Vector
import numpy as np


def geometry(rig, objects):
    # Armature-local -Y forward, Z up -> game +Z forward, Y up.
    to_game = Matrix.Rotation(-math.pi / 2, 4, "X")
    body = max(objects, key=lambda o: len(o.data.vertices))
    local = rig.matrix_world.inverted() @ body.matrix_world
    heights = [(local @ v.co).z for v in body.data.vertices]
    to_game = (Matrix.Scale(1.8 / (max(heights) - min(heights)), 4)
               @ to_game @ Matrix.Translation((0, 0, -min(heights))))
    joint_names = sorted({g.name for obj in objects for g in obj.vertex_groups
                          if g.name in rig.data.bones})
    joint_ids = {name: i for i, name in enumerate(joint_names)}
    indices = []
    vertices = {}
    for obj in objects:
        # Retain vertex groups through reduction to an overhead-view LOD.
        for modifier in list(obj.modifiers):
            obj.modifiers.remove(modifier)
        reduce = obj.modifiers.new("overhead LOD", "DECIMATE")
        reduce.ratio = 0.15
        reduce.use_collapse_triangulate = True
        evaluated = obj.evaluated_get(bpy.context.evaluated_depsgraph_get())
        mesh = evaluated.to_mesh()
        mesh.calc_loop_triangles()
        mesh.calc_tangents()
        atlas_offset, atlas_width = (0, 224) if obj == body else (224, 32)
        placement = to_game @ rig.matrix_world.inverted() @ obj.matrix_world
        normals = placement.to_3x3().inverted().transposed()
        for tri in mesh.loop_triangles:
            for loop_id in tri.loops:
                loop = mesh.loops[loop_id]
                vertex = mesh.vertices[loop.vertex_index]
                influences = [(g.weight, obj.vertex_groups[g.group].name)
                              for g in vertex.groups if g.weight > 0
                              and obj.vertex_groups[g.group].name in rig.data.bones]
                influences = sorted(influences, reverse=True)[:4]
                if not influences:
                    raise ValueError(f"{obj.name}: expected a weighted vertex")
                total = sum(w for w, _ in influences)
                joints, weights = [0]*4, [0.0]*4
                for i, (weight, name) in enumerate(influences):
                    joints[i] = joint_ids[name]
                    weights[i] = weight / total
                u, v = mesh.uv_layers.active.data[loop_id].uv
                uv = ((atlas_offset + 0.5 + min(1, max(0, u)) * (atlas_width - 1)) / 256,
                      1 - min(1, max(0, v)))
                record = struct.pack("<12f4H4f", *(placement @ vertex.co),
                                     *(normals @ loop.normal).normalized(),
                                     *(placement.to_3x3() @ loop.tangent).normalized(),
                                     -loop.bitangent_sign, *uv, *joints, *weights)
                indices.append(vertices.setdefault(record, len(vertices)))
        evaluated.to_mesh_clear()
    # Blender's evaluated modifier mesh can enumerate identical faces in
    # different orders. Canonicalize vertices spatially and preserve winding.
    ordered = sorted(vertices, key=lambda v: (struct.unpack_from("<3f", v), v))
    remap = {vertices[record]: i for i, record in enumerate(ordered)}
    triangles = []
    for i in range(0, len(indices), 3):
        a, b, c = (remap[j] for j in indices[i:i+3])
        triangles.append(min((a, b, c), (b, c, a), (c, a, b)))
    indices = [i for tri in sorted(triangles) for i in tri]
    return to_game, ordered, indices, joint_names


def palettes(target, source, joint_names, to_game, action):
    # Retarget by bone name. Keep target bone lengths while applying authored
    # rotations in armature space, plus the source's pelvis bob and sway.
    clip = next(a for a in bpy.data.actions if a.name.endswith("|" + action))
    source.animation_data.action = clip
    source.animation_data.action_slot = clip.slots[0]
    first, last = clip.frame_range
    scene = bpy.context.scene
    fps = scene.render.fps / scene.render.fps_base
    frames = round((last - first) * 60 / fps)
    inv_game = to_game.inverted()
    result = bytearray()
    for tick in range(frames):
        t = first + tick * fps / 60
        scene.frame_set(math.floor(t), subframe=t % 1)
        posed = {}
        for bone in target.data.bones:
            ref = source.pose.bones[bone.name]
            rest = bone.matrix_local
            src_rest = ref.bone.matrix_local
            rot_delta = ref.matrix.to_quaternion() @ src_rest.to_quaternion().inverted()
            orientation = rot_delta @ rest.to_quaternion()
            if bone.parent:
                point = posed[bone.parent.name] @ bone.parent.matrix_local.inverted() @ bone.head_local
            else:
                point = bone.head_local.copy()
            if bone.name == "pelvis":
                point += ref.head - ref.bone.head_local
            posed[bone.name] = Matrix.LocRotScale(point, orientation, Vector((1, 1, 1)))
        for name in joint_names:
            skin = to_game @ posed[name] @ target.data.bones[name].matrix_local.inverted() @ inv_game
            result.extend(struct.pack("<12f", *(x for row in list(skin)[:3] for x in row)))
    return frames, result


def image_bytes(path, width):
    image = bpy.data.images.load(str(path))
    image.scale(width, 256)
    values = np.empty(width * 256 * 4, dtype=np.float32)
    image.pixels.foreach_get(values)
    rgb = values.reshape((256, width, 4))[::-1, :, :3].clip(0, 1)
    srgb = np.where(rgb <= 0.0031308, 12.92 * rgb, 1.055 * rgb**(1/2.4) - 0.055)
    rgba = np.full((256, width, 4), 255, dtype=np.uint8)
    rgba[:, :, :3] = np.rint(srgb * 255).astype(np.uint8)
    return rgba


def cook(src, dest, variant, recipe):
    bpy.ops.wm.read_factory_settings(use_empty=True)
    bpy.ops.import_scene.fbx(filepath=str(src / recipe["model"]))
    rig = next(o for o in bpy.context.scene.objects if o.type == "ARMATURE")
    objects = [o for o in bpy.context.scene.objects if o.type == "MESH"]
    to_game, vertices, indices, names = geometry(rig, objects)
    bpy.ops.import_scene.fbx(filepath=str(src / recipe["animation"]))
    source = next(o for o in bpy.context.scene.objects if o.type == "ARMATURE" and o != rig)
    poses = bytearray()
    frames = 0
    clips = {}
    for name, settings in recipe["clips"].items():
        count, matrices = palettes(rig, source, names, to_game, settings["action"])
        if not count:
            raise ValueError(f"{variant}/{name}: expected a nonempty clip")
        clips[name] = {"first": frames, "frames": count, "hz": 60,
                       "looping": settings["looping"]}
        frames += count
        poses.extend(matrices)

    dest.mkdir(parents=True, exist_ok=True)
    with (dest / f"{variant}.skin").open("wb") as f:
        f.write(struct.pack("<8s4I", b"SURSKIN2", len(vertices), len(indices), len(names), frames))
        f.writelines(vertices)
        f.write(struct.pack(f"<{len(indices)}I", *indices))
        f.write(poses)
        # Skin matrices map mesh-space bind points to posed mesh-space points.
        # Preserve bind frames and nearest retained parents for local-space blends.
        joint_ids = {name: i for i, name in enumerate(names)}
        for name in names:
            bone = rig.data.bones[name]
            parent = bone.parent
            while parent and parent.name not in joint_ids:
                parent = parent.parent
            parent_id = joint_ids[parent.name] if parent else 0xffffffff
            bind = to_game @ bone.matrix_local
            f.write(struct.pack("<I12f", parent_id, *(x for row in list(bind)[:3] for x in row)))
    atlas = np.concatenate([image_bytes(src / recipe["albedo"], 224),
                            image_bytes(src / "T_Eye_Brown.png", 32)], axis=1)
    (dest / f"{variant}.rgba").write_bytes(atlas.tobytes())
    print(f"{variant}: {len(indices)//3} triangles, {len(names)} joints, {frames} sampled frames")

    return {"id": recipe["id"], "skin": f"{variant}.skin", "color": f"{variant}.rgba",
            "extent": [256, 256], "clips": clips, **({"tracks": recipe["tracks"]} if "tracks" in recipe else {})}


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--recipe", type=Path,
                        default=Path(__file__).with_name("character-recipes.json"))
    args = parser.parse_args(sys.argv[sys.argv.index("--") + 1:])
    recipes = json.loads(args.recipe.read_text(encoding="utf-8"))
    if not recipes:
        raise ValueError("no characters in recipe")
    catalog = {"version": 1, "models": {}}
    ids = set()
    for name, recipe in recipes.items():
        if not name or Path(name).name != name or name in (".", ".."):
            raise ValueError(f"invalid character name: {name}")
        if len(recipe["id"]) != 32 or any(c not in "0123456789abcdef" for c in recipe["id"]):
            raise ValueError(f"{name}: expected a 32 digit lowercase hex ID")
        if recipe["id"] in ids or not recipe["clips"]:
            raise ValueError(f"{name}: duplicate ID or missing clips")
        ids.add(recipe["id"])
        catalog["models"][name] = cook(args.source.resolve(), args.destination.resolve(), name, recipe)
    (args.destination / "catalog.json").write_text(json.dumps(catalog, indent=2) + "\n",
                                                   encoding="utf-8")
