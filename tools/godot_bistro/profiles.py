"""Pinned rendering contracts for the B2 baseline and B3 quality reference."""
import math
from pathlib import Path


PROFILES = ("b2", "b3")


def split_offsets(near, distance=120.0, weight=.6):
    """Godot offsets are absolute fractions of [near, shadow_max], not intervals."""
    return [(weight * near * (distance / near) ** (index / 4)
             + (1 - weight) * (near + (distance - near) * index / 4) - near)
            / (distance - near) for index in (1, 2, 3)]


def settings(profile, conditions):
    if profile not in PROFILES:
        raise ValueError("Unknown quality profile")
    result = {
        "profile": profile, "background_mode": 1,
        "background_linear_rgb": conditions["output"]["background_linear_rgb"],
        "background_energy": 1.0, "ambient_source": 2, "ambient_energy": .03,
        "ambient_sky_contribution": 0.0, "reflection_source": 0,
        "sky_rotation": [0., 0., 0.], "sun_energy": 1.0,
        "sun_color": [1., 1., 1.],
        "sun_direction": conditions["lighting"]["sun"]["direction_to_light"],
        "shadow_enabled": False, "tonemap_mode": 0, "exposure": 1.0,
        "screen_space_aa": 0, "msaa_3d": 0, "taa": False,
        "scaling_3d_scale": 1.0, "mesh_lod_threshold": 0.0,
        "occlusion_culling": False, "ssao": False, "ssil": False,
        "ssr": False, "sdfgi": False, "glow": False, "fog": False,
        "volumetric_fog": False, "adjustments": False, "debanding": False,
        "physical_light_units": False, "mesh_shadow_casting": 0,
    }
    if profile == "b3":
        result.update({
            "ambient_source": 3, "ambient_energy": 1.0,
            "ambient_sky_contribution": 1.0, "reflection_source": 2,
            "ibl_energy": 3.0, "sky_process_mode": 1, "sky_radiance_size": 4,
            "shadow_enabled": True, "shadow_mode": 2,
            "shadow_distance": 120.0,
            "shadow_splits": split_offsets(conditions["camera"]["near_m"]),
            "shadow_blend_splits": True, "shadow_fade_start": .9,
            "shadow_bias": .1, "shadow_normal_bias": 2.0,
            "shadow_blur": 1.0, "shadow_pancake_size": 20.0,
            "sun_angular_distance": 0.0,
            "shadow_atlas_size": 4096, "shadow_16_bits": False,
            "shadow_filter_quality": 2,
            "tonemap_mode": 4, "exposure": 2.0,
            "agx_white": 16.29, "agx_contrast": 1.25,
            "screen_space_aa": 1, "mesh_shadow_casting": 1,
        })
    return result


def project_settings(profile):
    if profile == "b2":
        return ""
    if profile != "b3":
        raise ValueError("Unknown quality profile")
    return ("lights_and_shadows/directional_shadow/size=4096\n"
            "lights_and_shadows/directional_shadow/16_bits=false\n"
            "lights_and_shadows/directional_shadow/soft_shadow_filter_quality=2\n"
            "lights_and_shadows/use_physical_light_units=false\n")


def compare_settings(actual, expected, path="quality"):
    """Allow native float32 roundoff, but reject omitted or additional settings."""
    if isinstance(expected, dict):
        if not isinstance(actual, dict) or actual.keys() != expected.keys():
            raise ValueError(f"{path} fields differ from the pinned profile")
        for key, value in expected.items():
            compare_settings(actual[key], value, path + "." + key)
    elif isinstance(expected, list):
        if not isinstance(actual, list) or len(actual) != len(expected):
            raise ValueError(f"{path} dimensions differ")
        for index, value in enumerate(expected):
            compare_settings(actual[index], value, f"{path}[{index}]")
    elif type(expected) in (int, float):
        if (type(actual) not in (int, float) or not math.isfinite(actual)
                or not math.isclose(actual, expected, rel_tol=1e-6, abs_tol=1e-6)):
            raise ValueError(f"{path} differs from the pinned profile")
    elif type(actual) is not type(expected) or actual != expected:
        raise ValueError(f"{path} differs from the pinned profile")


def verify_ibl_manifest(path, conditions, read, digest):
    """Accept only the shared cap-4 cook's unscaled, orientation-corrected panorama."""
    path = Path(path).resolve()
    manifest = read(path)
    try:
        source = manifest["source"]
        light = manifest["lighting"]
        panorama = manifest["godot_panorama"]
        if (manifest["schema_version"] != 1
                or source["sha256"] != conditions["lighting"]["ibl"]["sha256"]
                or [source["width"], source["height"]] != [4096, 2048]
                or light["strength"] != 3 or light["yaw_degrees"] != 0
                or manifest["recipe"]["cap"] != 4
                or manifest["max_prefilter_lod"] != 4
                or manifest["recipe"]["max_prefilter_lod"] != 4
                or [panorama["width"], panorama["height"]] != [4096, 2048]
                or panorama["format"] != "radiance-rgbe"
                or panorama["rotation_columns"] != 1024
                or panorama["mapping"] != "new[x]=source[(x+W/4)%W]"
                or panorama["strength_applied"] is not False
                or panorama["yaw_applied"] is not False):
            raise ValueError("IBL cook does not match the B3 reference contract")
        relative = Path(panorama["path"])
        image = (path.parent / relative).resolve()
        if relative.is_absolute() or not image.is_relative_to(path.parent):
            raise ValueError("IBL panorama escapes the cook directory")
        if image.stat().st_size != panorama["byte_length"] or digest(image) != panorama["sha256"]:
            raise ValueError("IBL panorama bytes differ from the cook manifest")
    except (KeyError, TypeError) as error:
        raise ValueError("Incomplete B3 IBL cook manifest") from error
    return image, {"manifest_sha256": digest(path), "source": source,
                   "lighting": light, "recipe": manifest["recipe"],
                   "panorama": panorama}


def godot_panorama_u(direction):
    """Pinned Godot 4.7.2 SKY_COORDS convention, before the quarter-turn copy."""
    x, _, z = direction
    return (math.atan2(x, -z) / (2 * math.pi)) % 1


def renderer_panorama_u(direction):
    x, _, z = direction
    return (.5 + math.atan2(z, x) / (2 * math.pi)) % 1


def orientation_fixture():
    """Six cardinal directions establish orientation independently of scene images.

    This is a CPU mapping fixture, not a claim of native GPU acceptance. U is
    undefined at the poles, where the fixture deliberately checks only V.
    """
    rows = []
    for name, direction in (("+X", [1, 0, 0]), ("-X", [-1, 0, 0]),
                            ("+Z", [0, 0, 1]), ("-Z", [0, 0, -1]),
                            ("+Y", [0, 1, 0]), ("-Y", [0, -1, 0])):
        vertical = math.acos(direction[1]) / math.pi
        row = {"name": name, "direction": direction, "v": vertical}
        if direction[1] == 0:
            godot_u = godot_panorama_u(direction)
            source_u = renderer_panorama_u(direction)
            if not math.isclose((godot_u + .25) % 1, source_u, abs_tol=1e-12):
                raise ValueError("Quarter-turn panorama orientation regression")
            row.update(godot_u=godot_u, source_u=source_u)
        rows.append(row)
    return {"kind": "cpu-cardinal-direction-mapping", "native_gpu_verified": False,
            "mapping": "new[x]=source[(x+W/4)%W]", "directions": rows,
            "source": "Godot 4.7.2-stable sky.glsl SKY_COORDS and PanoramaSkyMaterial"}
