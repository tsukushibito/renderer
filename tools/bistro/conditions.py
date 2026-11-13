"""Offline Bistro conditions contract and deterministic camera-path sampling.

Validate once at setup, then sample the validated data. This is a preparation
tool, not a per-frame Renderer asset-validation path. Coordinates are glTF's
right-handed Y-up metres; cameras look toward target with world-up +Y.
"""
import argparse
from bisect import bisect_right
import hashlib
import json
import math
from pathlib import Path, PurePosixPath
import re


PBR_NEUTRAL_COMMIT = "180b1a7bddec33f73fe41712a2963cc3ad8e5547"


def _number(value, label):
    if type(value) not in (int, float) or not math.isfinite(value):
        raise ValueError(f"{label} must be a finite number")
    return value


def _vector(value, label):
    if not isinstance(value, list) or len(value) != 3:
        raise ValueError(f"{label} must contain three numbers")
    return tuple(_number(component, label) for component in value)


def _finite_tree(value):
    if isinstance(value, float) and not math.isfinite(value):
        raise ValueError("Conditions contain a non-finite number")
    if isinstance(value, dict):
        for item in value.values():
            _finite_tree(item)
    elif isinstance(value, list):
        for item in value:
            _finite_tree(item)


def _safe_path(name):
    if not isinstance(name, str) or not name:
        raise ValueError("Input path must be a nonempty repository-relative path")
    path = PurePosixPath(name)
    if path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name:
        raise ValueError(f"Input path escapes repository: {name}")
    return name


def _digest(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value):
        raise ValueError("Input SHA-256 must be 64 lowercase hexadecimal characters")


def _direction(view):
    return tuple(b - a for a, b in zip(view["position"], view["target"]))


def _check_segment(first, second):
    """Reject an up-axis or zero look direction anywhere in linear interpolation."""
    a, b = _direction(first), _direction(second)
    delta = tuple(y - x for x, y in zip(a, b))
    horizontal_squared = delta[0] ** 2 + delta[2] ** 2
    fraction = (max(0, min(1, -(a[0] * delta[0] + a[2] * delta[2]) / horizontal_squared))
                if horizontal_squared else 0)
    closest = tuple(x + fraction * d for x, d in zip(a, delta))
    scale = max(math.hypot(*a), math.hypot(*b))
    if math.hypot(closest[0], closest[2]) <= 1e-8 * max(scale, 1):
        raise ValueError("Camera direction becomes degenerate or parallel to world-up")


def validate_conditions(config):
    """Validate the small versioned B0 contract; return the original mapping."""
    _finite_tree(config)
    if type(config["schema_version"]) is not int or config["schema_version"] != 1:
        raise ValueError("Unsupported conditions schema_version")
    resolution = config["resolution"]
    if (not isinstance(resolution, list) or len(resolution) != 2
            or any(type(value) is not int or value <= 0 for value in resolution)):
        raise ValueError("resolution must contain two positive integers")
    near = _number(config["camera"]["near_m"], "camera.near_m")
    far = _number(config["camera"]["far_m"], "camera.far_m")
    if not 0 < near < far:
        raise ValueError("Require 0 < near_m < far_m")
    for option in ("jitter", "auto_exposure", "depth_of_field", "motion_blur"):
        if config["camera"].get(option, False) is not False:
            raise ValueError(f"Fixed comparison camera requires {option}=false")
    views = {}
    for view in config["views"]:
        name = view["name"]
        if not isinstance(name, str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]*", name) or name in views:
            raise ValueError("View names must be unique lowercase filename-safe names")
        _vector(view["position"], f"{name}.position")
        _vector(view["target"], f"{name}.target")
        if not 0 < _number(view["vertical_fov_degrees"], f"{name}.FOV") < 180:
            raise ValueError("Require 0 < vertical_fov_degrees < 180")
        _check_segment(view, view)
        views[name] = view
    if not views:
        raise ValueError("Require at least one camera view")
    route = config["route"]
    fps = route["fps"]
    duration = _number(route["duration_seconds"], "route.duration_seconds")
    if type(fps) is not int or fps <= 0 or duration <= 0:
        raise ValueError("Require positive integer route fps and positive duration")
    count = duration * fps
    if (not math.isfinite(count) or round(count) < 1
            or not math.isclose(count, round(count), rel_tol=0, abs_tol=1e-9)):
        raise ValueError("Route duration must contain a whole number of frames")
    captures = route.get("capture_frames", [])
    if (not isinstance(captures, list) or len(set(captures)) != len(captures)
            or any(type(index) is not int or not 0 <= index < round(count) for index in captures)):
        raise ValueError("Route capture_frames must be distinct valid frame indices")
    keys = route["keyframes"]
    if not isinstance(keys, list) or len(keys) < 2:
        raise ValueError("Require at least two route keyframes")
    previous = -1
    for key in keys:
        seconds = _number(key["seconds"], "keyframe.seconds")
        if not 0 <= seconds <= duration or seconds <= previous or key["view"] not in views:
            raise ValueError("Route keyframes must increase strictly and reference existing views")
        previous = seconds
    if keys[0]["seconds"] != 0 or keys[-1]["seconds"] != duration:
        raise ValueError("Route keyframes must span exactly 0 through duration_seconds")
    first, last = views[keys[0]["view"]], views[keys[-1]["view"]]
    fields = ("position", "target", "vertical_fov_degrees")
    if any(first[field] != last[field] for field in fields):
        raise ValueError("Route must close with the same pose and FOV")
    for first_key, second_key in zip(keys, keys[1:]):
        _check_segment(views[first_key["view"]], views[second_key["view"]])
    sun = config["lighting"]["sun"]
    direction = _vector(sun["direction_to_light"], "sun.direction_to_light")
    if not math.isclose(math.hypot(*direction), 1, rel_tol=0, abs_tol=1e-6):
        raise ValueError("Sun direction_to_light must be a unit vector")
    rgb = _vector(sun["irradiance_rgb"], "sun.irradiance_rgb")
    if min(rgb) < 0 or max(rgb) <= 0:
        raise ValueError("Sun irradiance_rgb must be nonnegative and nonzero")
    if not 0 < _number(sun["angular_diameter_degrees"], "sun.angular_diameter_degrees") < 180:
        raise ValueError("Sun angular diameter must be between 0 and 180 degrees")
    ibl = config["lighting"]["ibl"]
    _safe_path(ibl["path"])
    _digest(ibl["sha256"])
    _number(ibl["yaw_degrees"], "ibl.yaw_degrees")
    if _number(ibl["strength"], "ibl.strength") <= 0:
        raise ValueError("IBL strength must be positive")
    if ibl["preprocess"]["mode"] != "luminance-cap" or ibl["preprocess"]["cap"] != 4:
        raise ValueError("Require luminance-cap preprocessing at linear Rec.709 luminance 4")
    if ibl["preprocess"].get("luminance_coefficients", [0.2126, 0.7152, 0.0722]) != [0.2126, 0.7152, 0.0722]:
        raise ValueError("Require linear Rec.709 luminance coefficients")
    _number(config["output"]["exposure_ev"], "output.exposure_ev")
    output = config["output"]
    if output["tone_mapper"] != "Khronos PBR Neutral" or output["srgb"] is not True:
        raise ValueError("Require Khronos PBR Neutral and one sRGB output transfer")
    if output.get("tone_mapper_commit", PBR_NEUTRAL_COMMIT) != PBR_NEUTRAL_COMMIT:
        raise ValueError("Unsupported pinned PBR Neutral reference commit")
    if "background_linear_rgb" in output:
        if min(_vector(output["background_linear_rgb"], "background_linear_rgb")) < 0:
            raise ValueError("Background linear RGB must be nonnegative")
    paths = set()
    for entry in config.get("inputs", []):
        name = _safe_path(entry["path"])
        if name in paths:
            raise ValueError(f"Duplicate pinned input: {name}")
        paths.add(name)
        _digest(entry["sha256"])
        if type(entry["size"]) is not int or entry["size"] < 0:
            raise ValueError("Input size must be a nonnegative integer")
    return config


def verify_input_files(config, repository_root):
    """Hash only named pinned files at offline setup; LFS full coverage is separate."""
    root = Path(repository_root).resolve()
    entries = {entry["path"]: entry for entry in config.get("inputs", [])}
    ibl = config["lighting"]["ibl"]
    if ibl["path"] in entries and entries[ibl["path"]]["sha256"] != ibl["sha256"]:
        raise ValueError("IBL SHA-256 disagrees with pinned inputs")
    entries.setdefault(ibl["path"], {"path": ibl["path"], "sha256": ibl["sha256"]})
    for name, entry in entries.items():
        path = (root / name).resolve()
        if not path.is_relative_to(root):
            raise ValueError(f"Input escapes repository: {name}")
        if "size" in entry and path.stat().st_size != entry["size"]:
            raise ValueError(f"Input size differs: {name}")
        with path.open("rb") as stream:
            if hashlib.file_digest(stream, "sha256").hexdigest() != entry["sha256"]:
                raise ValueError(f"Input SHA-256 differs: {name}")
    return len(entries)


def load_conditions(path, *, verify_inputs=False, repository_root=None):
    path = Path(path)
    config = validate_conditions(json.loads(path.read_text(encoding="utf-8")))
    if verify_inputs:
        verify_input_files(config, repository_root or path.resolve().parents[2])
    return config


def sample_route(config, seconds):
    """Sample an already validated route, including its duplicate closing pose."""
    _number(seconds, "sample time")
    route = config["route"]
    if not 0 <= seconds <= route["duration_seconds"]:
        raise ValueError("Sample time lies outside the camera route")
    keys = route["keyframes"]
    index = min(len(keys) - 2, bisect_right([key["seconds"] for key in keys], seconds) - 1)
    left, right = keys[index:index + 2]
    views = {view["name"]: view for view in config["views"]}
    first, second = views[left["view"]], views[right["view"]]
    fraction = (seconds - left["seconds"]) / (right["seconds"] - left["seconds"])
    sample = {"time_seconds": seconds, **config["camera"]}
    for name in ("position", "target"):
        sample[name] = [a + (b - a) * fraction for a, b in zip(first[name], second[name])]
    sample["vertical_fov_degrees"] = (first["vertical_fov_degrees"]
        + (second["vertical_fov_degrees"] - first["vertical_fov_degrees"]) * fraction)
    return sample


def route_frames(config):
    """Yield i/fps for 0 <= i < duration*fps; do not repeat the closing frame."""
    route = config["route"]
    for index in range(round(route["duration_seconds"] * route["fps"])):
        yield {"frame": index, **sample_route(config, index / route["fps"])}


def gltf_to_blender(vector):
    """Basis conversion for a direction or position: (x, y, z) -> (x, -z, y)."""
    x, y, z = vector
    return [x, -z, y]


def environment_uv(direction, yaw_degrees=0):
    """Inverse Y-yaw then lat-long map, U-repeat and V=0 at the source top row."""
    x, y, z = _vector(direction, "environment direction")
    angle = math.radians(_number(yaw_degrees, "environment yaw"))
    length = math.hypot(x, y, z)
    if length == 0:
        raise ValueError("Environment direction cannot be zero")
    source_x = math.cos(angle) * x - math.sin(angle) * z
    source_z = math.sin(angle) * x + math.cos(angle) * z
    return [(0.5 + math.atan2(source_z, source_x) / (2 * math.pi)) % 1,
            math.acos(max(-1, min(1, y / length))) / math.pi]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("conditions", type=Path)
    parser.add_argument("--verify-inputs", action="store_true")
    parser.add_argument("--root", type=Path)
    parser.add_argument("--frames-output", type=Path, help="Write the deterministic route-frame schedule")
    args = parser.parse_args()
    try:
        config = load_conditions(args.conditions, verify_inputs=args.verify_inputs, repository_root=args.root)
        frames = list(route_frames(config))
        if args.frames_output:
            args.frames_output.parent.mkdir(parents=True, exist_ok=True)
            args.frames_output.write_text(json.dumps(frames, indent=2, allow_nan=False) + "\n", encoding="utf-8", newline="\n")
        print(json.dumps({"passed": True, "views": len(config["views"]), "route_frames": len(frames),
                          "duration_seconds": config["route"]["duration_seconds"],
                          "inputs_verified": args.verify_inputs}))
    except (ValueError, OSError, KeyError, TypeError, OverflowError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
