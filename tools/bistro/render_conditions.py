"""Render pinned Bistro composition/lighting previews, not native GPU acceptance.

Run using Blender 4.5.16. Config positions are glTF right-handed Y-up metres.
Every image also has a scene-linear float32 OpenEXR companion. PNG output uses
the Khronos PBR Neutral operator followed by one explicit sRGB encoding.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import struct
import sys
from urllib.parse import unquote
import zlib

import numpy as np


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def gltf_to_blender(value):
    return (value[0], -value[2], value[1])


def neutral_tone_map(rgb):
    """Khronos PBR Neutral, input/output linear Rec.709, no exposure or OETF."""
    rgb = np.asarray(rgb, dtype=np.float64)
    if rgb.shape[-1] != 3 or not np.isfinite(rgb).all() or np.any(rgb < 0):
        raise ValueError("Tone mapping requires finite, nonnegative RGB")
    minimum = rgb.min(axis=-1, keepdims=True)
    offset = np.where(minimum < 0.08, minimum - 6.25 * minimum * minimum, 0.04)
    color = rgb - offset
    peak = color.max(axis=-1, keepdims=True)
    active = peak >= 0.76
    safe_peak = np.where(active, peak, 1.0)
    new_peak = 1.0 - 0.24 * 0.24 / (safe_peak + 0.24 - 0.76)
    compressed = color * new_peak / safe_peak
    desaturation = 1.0 - 1.0 / (0.15 * (safe_peak - new_peak) + 1.0)
    compressed = compressed * (1.0 - desaturation) + new_peak * desaturation
    return np.where(active, compressed, color)


def srgb_encode(linear):
    linear = np.clip(linear, 0.0, 1.0)
    return np.where(linear <= 0.0031308, 12.92 * linear,
                    1.055 * np.power(linear, 1.0 / 2.4) - 0.055)


def write_png(path, pixels):
    """Small RGB8 PNG writer: Blender's bundled Python does not require Pillow."""
    pixels = np.asarray(pixels, dtype=np.uint8)
    if pixels.ndim != 3 or pixels.shape[2] != 3:
        raise ValueError("Expected HxWx3 RGB8")
    height, width = pixels.shape[:2]
    def chunk(name, data):
        return struct.pack(">I", len(data)) + name + data + struct.pack(">I", zlib.crc32(name + data))
    payload = b"\x89PNG\r\n\x1a\n"
    payload += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
    payload += chunk(b"sRGB", b"\x00")
    payload += chunk(b"IDAT", zlib.compress(b"".join(b"\x00" + row.tobytes() for row in pixels), 9))
    payload += chunk(b"IEND", b"")
    path.write_bytes(payload)


def inventory(scene):
    records = []
    for obj in sorted((o for o in scene.objects if o.type == "MESH"), key=lambda o: o.name):
        records.append({"name": obj.name, "vertices": len(obj.data.vertices),
                        "triangles": sum(len(p.vertices) - 2 for p in obj.data.polygons),
                        "materials": [s.material.name if s.material else None for s in obj.material_slots],
                        "matrix_world": [list(row) for row in obj.matrix_world]})
    return records


def input_hashes(path):
    source = json.loads(path.read_text(encoding="utf-8"))
    paths = {path.name: digest(path)}
    for group in ("buffers", "images"):
        for item in source.get(group, []):
            uri = unquote(item["uri"])
            resource = (path.parent / uri).resolve()
            if ":" in uri or not resource.is_relative_to(path.parent.resolve()):
                raise ValueError("External resource escapes the asset directory")
            paths[uri] = digest(resource)
    return dict(sorted(paths.items()))


def project_camera_points(points, vertical_fov_degrees, aspect, near, far):
    """Project camera-local points (-Z forward) and classify frustum membership."""
    points = np.asarray(points, dtype=np.float64)
    depth = -points[:, 2]
    positive = depth > 0
    denominator = np.where(positive, depth, 1.0) * math.tan(math.radians(vertical_fov_degrees) / 2)
    ndc = points[:, :2] / denominator[:, None]
    ndc[:, 0] /= aspect
    outside = (~positive) | (depth < near) | (depth > far) | (np.abs(ndc) > 1).any(axis=1)
    return ndc, depth, outside


def projection_bounds(scene, camera, fov, resolution):
    """Record all imported vertices and conservative local-box corner bounds.

    Frustum membership proves framing only. It does not prove a vertex is
    visible through other geometry, or replace the retained mesh inventory.
    """
    mesh_objects = [o for o in scene.objects if o.type == "MESH"]
    aspect = resolution[0] / resolution[1]
    view = camera.matrix_world.inverted()
    result = {}
    for kind in ("vertices", "bounding_box_corners"):
        low, high = np.array([math.inf, math.inf]), np.array([-math.inf, -math.inf])
        depth_low, depth_high = math.inf, -math.inf
        count = outside_count = behind = partly = wholly = 0
        for obj in mesh_objects:
            if kind == "vertices":
                coordinates = np.empty(len(obj.data.vertices) * 3, dtype=np.float32)
                obj.data.vertices.foreach_get("co", coordinates)
                coordinates = coordinates.reshape(-1, 3)
            else:
                coordinates = np.asarray(obj.bound_box, dtype=np.float64)
            if not len(coordinates):
                continue
            matrix = np.asarray(view @ obj.matrix_world, dtype=np.float64)
            points = coordinates @ matrix[:3, :3].T + matrix[:3, 3]
            ndc, depth, outside = project_camera_points(points, fov, aspect, camera.data.clip_start, camera.data.clip_end)
            positive = depth > 0
            if positive.any():
                low = np.minimum(low, ndc[positive].min(axis=0))
                high = np.maximum(high, ndc[positive].max(axis=0))
            depth_low, depth_high = min(depth_low, float(depth.min())), max(depth_high, float(depth.max()))
            count += len(depth)
            outside_count += int(outside.sum())
            behind += int((~positive).sum())
            partly += int(outside.any())
            wholly += int(outside.all())
        result[kind] = {"instances": count, "outside_frustum": outside_count,
                        "behind_camera": behind, "mesh_instances_with_any_outside": partly,
                        "mesh_instances_with_all_points_outside": wholly,
                        "positive_depth_ndc_min": low.tolist() if np.isfinite(low).all() else None,
                        "positive_depth_ndc_max": high.tolist() if np.isfinite(high).all() else None,
                        "depth_range_m": [depth_low, depth_high]}
    result["limitation"] = "All imported mesh vertices and eight conservative local bounding-box corners per instance; frustum framing only, not occlusion visibility or material coverage. NDC [-1,1] with image-up +Y."
    return result


def configure_environment(bpy, scene, config, asset_root):
    spec = config["lighting"]["ibl"]
    source = asset_root / spec["path"]
    if digest(source) != spec["sha256"]:
        raise ValueError("IBL source SHA-256 differs")
    original = bpy.data.images.load(str(source.resolve()), check_existing=False)
    pixels = np.empty(len(original.pixels), dtype=np.float32)
    original.pixels.foreach_get(pixels)
    pixels = pixels.reshape(original.size[1], original.size[0], 4)
    if not np.isfinite(pixels).all() or np.any(pixels[:, :, :3] < 0):
        raise ValueError("Environment contains negative/nonfinite radiance")
    luminance = pixels[:, :, :3] @ np.array([0.2126, 0.7152, 0.0722], dtype=np.float32)
    cap = spec["preprocess"]["cap"]
    capped = pixels.copy()
    capped[:, :, :3] *= np.minimum(1.0, cap / np.maximum(luminance, 1e-20))[:, :, None]
    image = bpy.data.images.new("Bistro bright-source-suppressed environment", width=original.size[0],
                                height=original.size[1], alpha=True, float_buffer=True)
    image.colorspace_settings.name = "Linear Rec.709"
    image.pixels.foreach_set(capped.ravel())
    image.update()
    world = bpy.data.worlds.new("Bistro fixed environment")
    world.use_nodes = True
    tree = world.node_tree
    tree.nodes.clear()
    texture = tree.nodes.new("ShaderNodeTexEnvironment")
    texture.image = image
    texture.interpolation = "Linear"
    mapping = tree.nodes.new("ShaderNodeMapping")
    # Inverse source rotation: B * R_y(-yaw) * inverse(B) = R_z(-yaw),
    # with B(x,y,z)=(x,-z,y). Native Cycles environment mapping then agrees
    # with u=fract(.5+atan2(z,x)/(2*pi)), top-origin v=acos(y)/pi.
    mapping.inputs["Rotation"].default_value[2] = -math.radians(spec["yaw_degrees"])
    coordinates = tree.nodes.new("ShaderNodeTexCoord")
    tree.links.new(coordinates.outputs["Generated"], mapping.inputs["Vector"])
    tree.links.new(mapping.outputs["Vector"], texture.inputs["Vector"])
    background = tree.nodes.new("ShaderNodeBackground")
    background.inputs["Strength"].default_value = spec["strength"]
    tree.links.new(texture.outputs["Color"], background.inputs["Color"])
    output = tree.nodes.new("ShaderNodeOutputWorld")
    camera_background = tree.nodes.new("ShaderNodeBackground")
    camera_background.inputs["Color"].default_value = (*config["output"].get("background_linear_rgb", [0.18] * 3), 1)
    camera_background.inputs["Strength"].default_value = 1
    path = tree.nodes.new("ShaderNodeLightPath")
    mix = tree.nodes.new("ShaderNodeMixShader")
    tree.links.new(path.outputs["Is Camera Ray"], mix.inputs[0])
    tree.links.new(background.outputs["Background"], mix.inputs[1])
    tree.links.new(camera_background.outputs["Background"], mix.inputs[2])
    tree.links.new(mix.outputs[0], output.inputs["Surface"])
    scene.world = world
    weights = np.sin(math.pi * (np.arange(original.size[1]) + 0.5) / original.size[1])[:, None]
    weighted_total = float((luminance * weights).sum())
    capped_luminance = capped[:, :, :3] @ np.array([0.2126, 0.7152, 0.0722])
    peak_row, peak_column = np.unravel_index(luminance.argmax(), luminance.shape)
    return texture, background, original, image, {
        "path": spec["path"], "sha256": digest(source), "resolution": list(original.size),
        "original_luminance_max": float(luminance.max()),
        "capped_luminance_max": float(capped_luminance.max()),
        "capped_texels": int((luminance > cap).sum()),
        "texels_luminance_above_100": int((luminance > 100).sum()),
        "solid_angle_weighted_luminance_above_100_fraction": float((luminance * weights * (luminance > 100)).sum() / weighted_total),
        "solid_angle_weighted_luminance_mean_original": weighted_total / float(weights.sum() * original.size[0]),
        "solid_angle_weighted_luminance_mean_capped": float((capped_luminance * weights).sum() / (weights.sum() * original.size[0])),
        "peak_texel_top_origin": [int(peak_column), int(original.size[1] - 1 - peak_row)],
        "cap": cap, "preprocessed_float32_rgba_sha256": hashlib.sha256(capped.tobytes()).hexdigest(),
        "pixel_order": "Blender bottom-to-top image rows; preprocessing is per texel, independent of row orientation"}


def route_clearance(bpy, scene, config):
    """Audit eye segments and all logical-frame near-plane corner rays.

    This is a composition audit, not a navigation/collision implementation. The
    two proven alpha-zero materials do not count as solid obstacles here.
    """
    from mathutils import Vector
    graph = bpy.context.evaluated_depsgraph_get()
    transparent = {"MASTER_Glass_Dirty_MASKED", "Paris_LiquorBottle_01_Labels"}
    def cast(origin, direction, distance):
        remaining = distance
        for _ in range(32):
            hit, point, _normal, index, obj, _matrix = scene.ray_cast(graph, origin, direction, distance=remaining)
            if not hit:
                return None
            travelled = (point - origin).length
            material = None
            if 0 <= index < len(obj.data.polygons):
                slot = obj.data.polygons[index].material_index
                if slot < len(obj.material_slots):
                    material = obj.material_slots[slot].material
            if material is None or material.name not in transparent:
                return {"object": obj.name, "material": material.name if material else None, "distance_m": travelled}
            origin = point + direction * 1e-5
            remaining -= travelled + 1e-5
            if remaining <= 0:
                return None
        raise ValueError("More than 32 transparent surfaces in clearance ray")
    route = config["route"]
    named = {v["name"]: v for v in config["views"]}
    keys = route["keyframes"]
    blocked_segments = []
    for left, right in zip(keys, keys[1:]):
        origin = Vector(gltf_to_blender(named[left["view"]]["position"]))
        delta = Vector(gltf_to_blender(named[right["view"]]["position"])) - origin
        if delta.length > 1e-10:
            hit = cast(origin, delta.normalized(), delta.length)
            if hit:
                blocked_segments.append({"from": left["view"], "to": right["view"], **hit})
    intrusions = []
    frames = int(round(route["duration_seconds"] * route["fps"]))
    near = config["camera"]["near_m"]
    aspect = config["resolution"][0] / config["resolution"][1]
    for frame in range(frames):
        seconds = frame / route["fps"]
        left, right = next((a, b) for a, b in zip(keys, keys[1:]) if a["seconds"] <= seconds <= b["seconds"])
        factor = (seconds - left["seconds"]) / (right["seconds"] - left["seconds"])
        a, b = named[left["view"]], named[right["view"]]
        position = [x + factor * (y - x) for x, y in zip(a["position"], b["position"])]
        target = [x + factor * (y - x) for x, y in zip(a["target"], b["target"])]
        fov = a["vertical_fov_degrees"] + factor * (b["vertical_fov_degrees"] - a["vertical_fov_degrees"])
        origin = Vector(gltf_to_blender(position))
        forward = (Vector(gltf_to_blender(target)) - origin).normalized()
        right_vector = forward.cross(Vector((0, 0, 1))).normalized()
        up = right_vector.cross(forward).normalized()
        half_height = near * math.tan(math.radians(fov) / 2)
        offsets = [(0, 0), (-1, -1), (-1, 1), (1, -1), (1, 1)]
        for corner, (x, y) in enumerate(offsets):
            ray = forward * near + right_vector * (x * half_height * aspect) + up * (y * half_height)
            hit = cast(origin, ray.normalized(), ray.length)
            if hit:
                intrusions.append({"frame": frame, "near_plane_ray": corner, **hit})
    return {"logical_frames": frames, "near_plane_rays": frames * 5,
            "eye_path_segments": len(keys) - 1, "blocked_eye_path_segments": blocked_segments,
            "near_plane_intrusions": intrusions,
            "limitation": "Point eye path and five near-plane rays per logical frame, not swept volume, continuous frustum clearance or walkability; alpha-zero audited materials skipped."}


def main():
    import bpy
    from mathutils import Vector
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("config", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--asset-root", type=Path, required=True)
    parser.add_argument("--views", nargs="*")
    parser.add_argument("--samples", type=int)
    parser.add_argument("--resolution", nargs=2, type=int)
    parser.add_argument("--no-alternatives", action="store_true")
    args = parser.parse_args(sys.argv[sys.argv.index("--") + 1:])
    config_bytes = args.config.read_bytes()
    config = json.loads(config_bytes)
    config_sha256 = hashlib.sha256(config_bytes).hexdigest()
    script_sha256 = digest(Path(__file__))
    preview = config["preview"]
    samples = args.samples if args.samples is not None else preview["samples"]
    resolution = args.resolution if args.resolution is not None else preview["resolution"]
    if bpy.app.version != (4, 5, 16) or samples < 1 or min(resolution) < 1:
        raise ValueError("Require pinned Blender 4.5.16 and positive samples/dimensions")
    file_hashes = input_hashes(args.input)
    bpy.ops.wm.read_factory_settings(use_empty=True)
    bpy.ops.import_scene.gltf(filepath=str(args.input.resolve()))
    scene = bpy.context.scene
    before = inventory(scene)
    if len(before) != 1296 or sum(r["triangles"] for r in before) != 2832120:
        raise ValueError("Expected the complete audited Bistro exterior")
    for obj in list(scene.objects):
        if obj.type in ("CAMERA", "LIGHT"):
            bpy.data.objects.remove(obj, do_unlink=True)
    texture, background, original, capped, ibl_evidence = configure_environment(bpy, scene, config, args.asset_root)
    sun_spec = config["lighting"]["sun"]
    sun_data = bpy.data.lights.new("Bistro fixed directional sun", "SUN")
    intensity = max(sun_spec["irradiance_rgb"])
    sun_data.energy = intensity
    sun_data.color = tuple(x / intensity for x in sun_spec["irradiance_rgb"])
    sun_data.angle = math.radians(sun_spec["angular_diameter_degrees"])
    sun = bpy.data.objects.new("Bistro fixed directional sun", sun_data)
    scene.collection.objects.link(sun)
    to_light = Vector(gltf_to_blender(sun_spec["direction_to_light"])).normalized()
    sun.rotation_euler = (-to_light).to_track_quat("-Z", "Y").to_euler()
    data = bpy.data.cameras.new("Bistro fixed view")
    camera = bpy.data.objects.new("Bistro fixed view", data)
    scene.collection.objects.link(camera)
    scene.camera = camera
    data.clip_start, data.clip_end = config["camera"]["near_m"], config["camera"]["far_m"]
    data.sensor_fit, data.sensor_height = "VERTICAL", 24.0
    scene.render.engine = "CYCLES"
    scene.cycles.device = "CPU"
    scene.cycles.samples = samples
    scene.cycles.seed = preview["seed"]
    scene.cycles.use_denoising = False
    scene.cycles.use_adaptive_sampling = False
    scene.cycles.max_bounces = preview["max_bounces"]
    scene.cycles.diffuse_bounces = 1
    scene.cycles.glossy_bounces = 1
    scene.cycles.transmission_bounces = 1
    scene.cycles.transparent_max_bounces = 16
    scene.render.resolution_x, scene.render.resolution_y = resolution
    scene.render.resolution_percentage = 100
    scene.render.use_persistent_data = True
    scene.render.compositor_device = "CPU"
    scene.render.use_motion_blur = False
    data.dof.use_dof = False
    scene.render.image_settings.file_format = "OPEN_EXR"
    scene.render.image_settings.color_depth = "32"
    scene.render.image_settings.color_mode = "RGBA"
    scene.render.image_settings.exr_codec = "ZIP"
    scene.render.film_transparent = False
    scene.render.use_compositing = False
    scene.view_settings.view_transform = "Raw"
    scene.view_settings.look = "None"
    scene.view_settings.exposure = 0.0
    scene.view_settings.gamma = 1.0
    args.output.mkdir(parents=True, exist_ok=True)
    denoising = preview.get("denoising", False)
    noisy_output = None
    if denoising:
        # Composition aid only: retain the original noisy scene-linear image,
        # and use CPU OIDN with Cycles' normal/albedo guides for the shared PNG.
        scene.view_layers[0].cycles.denoising_store_passes = True
        scene.use_nodes = True
        scene.render.use_compositing = True
        tree = scene.node_tree
        tree.nodes.clear()
        layers = tree.nodes.new("CompositorNodeRLayers")
        denoise = tree.nodes.new("CompositorNodeDenoise")
        denoise.prefilter = "ACCURATE"
        denoise.use_hdr = True
        tree.links.new(layers.outputs["Image"], denoise.inputs["Image"])
        tree.links.new(layers.outputs["Denoising Normal"], denoise.inputs["Normal"])
        tree.links.new(layers.outputs["Denoising Albedo"], denoise.inputs["Albedo"])
        composite = tree.nodes.new("CompositorNodeComposite")
        tree.links.new(denoise.outputs["Image"], composite.inputs["Image"])
        noisy_output = tree.nodes.new("CompositorNodeOutputFile")
        noisy_output.base_path = str(args.output.resolve())
        noisy_output.format.file_format = "OPEN_EXR"
        noisy_output.format.color_depth = "32"
        noisy_output.format.color_mode = "RGBA"
        noisy_output.format.exr_codec = "ZIP"
        tree.links.new(layers.outputs["Image"], noisy_output.inputs[0])
    views = [v for v in config["views"] if not args.views or v["name"] in args.views]
    variants = [{"name": "selected", "ibl_strength": config["lighting"]["ibl"]["strength"],
                 "sun_multiplier": 1, "exposure_ev": config["output"]["exposure_ev"], "uncapped_ibl": False}]
    alternatives = [] if args.no_alternatives else config.get("alternatives", [])
    records = []
    for view in views:
        camera.location = gltf_to_blender(view["position"])
        camera.rotation_euler = (Vector(gltf_to_blender(view["target"])) - camera.location).to_track_quat("-Z", "Y").to_euler()
        data.lens = 12.0 / math.tan(math.radians(view["vertical_fov_degrees"]) / 2.0)
        current = variants + [a for a in alternatives if not a.get("views") or view["name"] in a["views"]]
        bpy.context.view_layer.update()
        record = {"view": view, "blender_camera": {"position": list(camera.location),
                    "matrix_world": [list(row) for row in camera.matrix_world],
                    "lens_mm": data.lens, "sensor_height_mm": data.sensor_height,
                    "sensor_fit": data.sensor_fit, "angle_y_degrees": math.degrees(data.angle_y)}, "variants": []}
        record["projection"] = projection_bounds(scene, camera, view["vertical_fov_degrees"], resolution)
        for variant in current:
            name = variant["name"]
            if Path(name).name != name or Path(view["name"]).name != view["name"]:
                raise ValueError("Image names must be plain file names")
            sun_data.energy = intensity * variant["sun_multiplier"]
            variant_direction = variant.get("sun_direction_to_light", sun_spec["direction_to_light"])
            to_light = Vector(gltf_to_blender(variant_direction)).normalized()
            sun.rotation_euler = (-to_light).to_track_quat("-Z", "Y").to_euler()
            background.inputs["Strength"].default_value = variant["ibl_strength"]
            uncapped = variant.get("uncapped_ibl", variant.get("ibl_cap", ibl_evidence["cap"]) is None)
            if variant.get("ibl_cap", ibl_evidence["cap"]) not in (None, ibl_evidence["cap"]):
                raise ValueError("Alternative requires the selected cap or null for uncapped")
            texture.image = original if uncapped else capped
            exr = args.output / f"{view['name']}-{name}.exr"
            png = exr.with_suffix(".png")
            scene.render.filepath = str(exr.resolve())
            if noisy_output:
                noisy_output.file_slots[0].path = exr.stem + "-noisy-"
            bpy.ops.render.render(write_still=True)
            noisy_artifact = {}
            if noisy_output:
                noisy_actual = args.output / (exr.stem + "-noisy-0001.exr")
                noisy = args.output / (exr.stem + "-noisy.exr")
                noisy_actual.replace(noisy)
                noisy_artifact = {"noisy_exr": {"file": noisy.name, "sha256": digest(noisy)}}
            image = bpy.data.images.load(str(exr.resolve()), check_existing=False)
            raw = np.empty(len(image.pixels), dtype=np.float32)
            image.pixels.foreach_get(raw)
            raw = raw.reshape(resolution[1], resolution[0], 4)[::-1, :, :3]
            if not np.isfinite(raw).all():
                raise ValueError("Nonfinite scene-linear output")
            negative_channels = int((raw < 0).sum())
            exposed = np.maximum(raw, 0).astype(np.float64) * 2.0 ** variant["exposure_ev"]
            sdr = np.rint(srgb_encode(neutral_tone_map(exposed)) * 255).astype(np.uint8)
            write_png(png, sdr)
            record["variants"].append({**variant, **noisy_artifact, "exr": {"file": exr.name, "sha256": digest(exr)},
                                        "png": {"file": png.name, "sha256": digest(png)},
                                        "linear_max": float(raw.max()), "negative_channels_clamped": negative_channels,
                                        "rgb8_mean": sdr.mean(axis=(0, 1)).tolist(),
                                        "rgb8_black_pixels": int((sdr.max(axis=2) <= 2).sum()),
                                        "rgb8_black_fraction": float((sdr.max(axis=2) <= 2).mean()),
                                        "rgb8_white_pixels": int((sdr.min(axis=2) >= 253).sum()),
                                        "rgb8_white_fraction": float((sdr.min(axis=2) >= 253).mean())})
            bpy.data.images.remove(image)
        records.append(record)
    after = inventory(scene)
    if after != before:
        raise ValueError("Geometry/material inventory changed during preview")
    inventory_bytes = json.dumps(before, sort_keys=True, separators=(",", ":")).encode()
    report = {"schema_version": 1, "purpose": "B0 composition, light and exposure evidence only; not native GPU acceptance, exact raster reference or performance evidence",
              "blender": bpy.app.version_string, "blender_build": bpy.app.build_hash.decode(),
              "script_sha256": script_sha256, "config_sha256": config_sha256,
              "input_files_sha256": file_hashes, "ibl": ibl_evidence,
              "settings": {"engine": "Cycles CPU", "samples": samples, "seed": preview["seed"],
                           "resolution": resolution, "denoising": denoising,
                           "denoiser": "CPU compositor OpenImageDenoise, accurate prefilter, HDR, Cycles normal/albedo guides; original noisy EXR also retained" if denoising else None,
                           "adaptive_sampling": False,
                           "max_bounces": preview["max_bounces"], "diffuse_bounces": 1, "glossy_bounces": 1,
                           "transmission_bounces": 1, "transparent_max_bounces": 16,
                           "persistent_data": True, "motion_blur": False, "depth_of_field": False,
                           "compositor_device": "CPU",
                           "tone_mapping": "Khronos PBR Neutral applied to nonnegative scene-linear Rec.709 * 2^EV; one sRGB OETF",
                           "exr": "Scene-linear Rec.709 RGBA float32 ZIP, before exposure/tone mapping", "light": config["lighting"]},
              "inventory_unchanged": True, "mesh_objects": len(before), "materials": len({m for o in before for m in o["materials"] if m}),
              "triangles": sum(o["triangles"] for o in before), "inventory_sha256": hashlib.sha256(inventory_bytes).hexdigest(),
              "route_clearance": route_clearance(bpy, scene, config), "views": records}
    (args.output / "renders.json").write_text(json.dumps(report, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    print(json.dumps({"views": len(records), "images": sum(len(r["variants"]) for r in records), "mesh_objects": len(before)}))


if __name__ == "__main__":
    main()
