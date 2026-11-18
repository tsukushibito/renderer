"""Calibrate and apply the Bistro B5 full-scene image gate."""

import argparse
from collections import Counter
import os
from pathlib import Path
import subprocess
from uuid import uuid4

import numpy as np
from PIL import Image

from bloom_acceptance import APIS, MODES, plane, reports
from compare_runtime import (
    PROFILES, _inside_directory, canonical_hash, compare_pixels, digest, load_capture, load_json,
)
from resource_acceptance import require, write_json


COLOR_PLANES = ("hdr", "bloom_hdr", "sdr", "bloom_bypass")
SHADOWS = tuple(f"shadow{cascade}" for cascade in range(4))
ROI = {
    "street-pavement": ("view/street", 384, 540, 1152, 540),
    "street-shadow": ("view/street", 192, 324, 1536, 648),
    "surfaces": ("view/material-detail", 192, 108, 1536, 864),
    "glass": ("view/glass-detail", 192, 108, 1536, 864),
    "label": ("view/label-detail", 192, 108, 1536, 864),
}
FORMATS = {"bloom_hdr": "hdr", "bloom_bypass": "sdr", **dict.fromkeys(SHADOWS, "depth")}


def sample_path(directory, args, binary, api, extra):
    root = directory / "report.json"
    command = [str(binary), "--api", api, "--manifest", str(args.manifest), "--ibl-manifest",
               str(args.ibl_manifest), "--width", "1920", "--height", "1080",
               "--frames", "4", "--warmup", "2", "--inflight", "2", "--output", str(directory),
               *extra]
    if args.msaa == 4:
        command += ["--msaa", "4"]
    if args.resume and root.is_file():
        require(load_json(directory / "command.json") == command, f"Changed command {directory}")
        return root, load_json(root)
    directory.mkdir(parents=True, exist_ok=False)
    write_json(directory / "command.json", command)
    with (directory / "process.log").open("wb") as log:
        subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=900)
    return root, load_json(root)


def validated_run(path, report, mode, api, samples):
    validation = report["validation"]
    identity = report["identity"]
    require(report["runtime"]["api"] == api and validation["mode"] == mode and validation["clean"],
            f"Wrong runtime or validation result {path}")
    require(all(validation[key] == 0 for key in ("warnings", "errors", "callback_failures")),
            f"Native diagnostics {path}")
    if api == "vulkan":
        strict = mode == "strict"
        require(validation["requested"] == ("Required" if strict else "Disabled")
                and validation["core_validation"] is (True if strict else None)
                and validation["synchronization_validation"] is (True if strict else None), path)
    else:
        require(validation["debug_layer_enabled"] is (mode == "strict")
                and validation["gpu_based_validation_enabled"] is (mode == "strict")
                and validation["corruption"] == 0
                and (mode != "strict" or validation["teardown_checked"]), path)
    require((identity["width"], identity["height"]) == (1920, 1080)
            and identity["shadows"] and identity["aa"] and identity.get("samples", 1) == samples,
            path)
    scene = report["scene"]
    require((scene["meshes"], scene["primitives"], scene["materials"], scene["alpha_zero_materials"])
            == (1296, 1591, 132, 2), f"Missing full-scene inputs {path}")
    require(report["memory"]["normal_allocation_free"], f"Normal frame memory {path}")


def read_plane(path, report, name):
    if name not in SHADOWS:
        return plane(path, report, name)
    values, meta = load_capture(path, {"captures": {"depth": report["shadow_captures"][name]}}, "depth")
    require((meta["width"], meta["height"]) == (2048, 2048), f"Incorrect shadow extent {path}")
    return values, meta


def equal_files(first, second):
    with first.open("rb") as a, second.open("rb") as b:
        while True:
            chunk = a.read(1024 * 1024)
            if chunk != b.read(1024 * 1024):
                return False
            if not chunk:
                return True


def share_capture(root, index, report, path, name, meta):
    """Share identical captured bytes while keeping each reported path readable."""
    group = "shadow_captures" if name in SHADOWS else "captures"
    capture = Path(report[group][name]["path"])
    if not capture.is_absolute():
        capture = path.parent / capture
    require(not capture.is_symlink() and _inside_directory(capture.resolve(), root),
            f"Unexpected capture outside cohort {capture}")
    key = meta["bytes"], meta["sha256"]
    first = index.setdefault(key, capture)
    if first == capture or first.samefile(capture):
        return
    require(equal_files(first, capture), f"Capture content differs despite matching hash {capture}")
    temporary = capture.with_name(f".capture-{uuid4().hex}.tmp")
    try:
        os.link(first, temporary)
        os.replace(temporary, capture)
    finally:
        temporary.unlink(missing_ok=True)


def collect(path, report, root, index, planes, sample_count):
    samples = {}
    for name, capture_path in reports(path.parent, report).items():
        captured = load_json(capture_path)
        label = name.split("/", 1)[1]
        expected = captured["identity"].get("route_frame") if name.startswith("route/") else captured["identity"]["view"]
        require(str(expected) == label and captured["capture_only"]
                and captured["readback_outside_measurement"], f"Unexpected sample {capture_path}")
        require(captured["identity"].get("samples", 1) == sample_count,
                f"Sample count differs in {capture_path}")
        require(set(captured["captures"]) == set(planes)
                and set(captured["shadow_captures"]) == set(SHADOWS), f"Missing planes {capture_path}")
        require(captured["validation"] == report["validation"]
                and captured["runtime"] == report["runtime"], f"Sample owner mismatch {capture_path}")
        hashes = {}
        for key in planes + SHADOWS:
            values, meta = read_plane(capture_path, captured, key)
            if key in ("hdr", "bloom_hdr", "sdr", "bloom_bypass"):
                alpha = 255 if FORMATS.get(key, key) == "sdr" else 1.0
                require(np.all(values[:, :, 3] == alpha), f"Unexpected alpha {capture_path}/{key}")
            share_capture(root, index, captured, capture_path, key, meta)
            hashes[key] = meta["sha256"]
        samples[name] = (capture_path, captured["identity"], hashes)
    return samples


def roi_pixels(image, region):
    _, x, y, width, height = region
    return image[y:y + height, x:x + width]


def difference_image(path, first, second):
    delta = np.abs(first[:, :, :3].astype(np.int16) - second[:, :, :3].astype(np.int16))
    Image.fromarray(np.minimum(delta * 16, 255).astype(np.uint8)).save(path)


def compare_cross(first, second, differences, planes):
    require(first.keys() == second.keys(), "Mismatched Vulkan and D3D12 sample sets")
    counts = Counter()
    maximum = Counter()
    for name, (path_a, identity, _) in first.items():
        path_b, other_identity, _ = second[name]
        require(identity == other_identity, f"API identity differs for {name}")
        report_a, report_b = (load_json(path) for path in (path_a, path_b))
        for key in planes + SHADOWS:
            a, _ = read_plane(path_a, report_a, key)
            b, _ = read_plane(path_b, report_b, key)
            fmt = FORMATS.get(key, key)
            result = compare_pixels(a, b, fmt, profile="scene")
            require(result["passed"], f"API disagreement {name}/{key}: {result}")
            counts[key] += 1
            maximum[key] = max(maximum[key], result["exceeding_pixels"])
            if key == "sdr" and differences is not None:
                difference_image(differences / (name.replace("/", "-") + ".png"), a, b)
            if key in ("hdr", "sdr", "depth"):
                for region, bounds in ROI.items():
                    if bounds[0] != name:
                        continue
                    result = compare_pixels(roi_pixels(a, bounds), roi_pixels(b, bounds), fmt,
                                            profile="scene")
                    require(result["passed"], f"ROI disagreement {region}/{key}: {result}")
                    counts[f"roi/{region}/{key}"] += 1
                    maximum[f"roi/{region}/{key}"] = max(
                        maximum[f"roi/{region}/{key}"], result["exceeding_pixels"])
    return {"comparisons": dict(counts), "maximum_exceeding_pixels": dict(maximum)}


def probe_faults(args, root, street):
    path, _, _ = street
    report = load_json(path)
    output = {}
    normal, _ = read_plane(path, report, "sdr")
    common = ["--view", "street", "--bloom", "tent"]
    require(load_json(args.manifest)["materials"][116]["name"] == "Paris_StreetPivot",
            "Material sensitivity probe index no longer identifies the intended surface")
    for name, params, region in [
        ("missing-street-material", ["--omit-material", "116"], "street-pavement"),
        ("receiver-offset", ["--offset-shadow-receiver"], "street-shadow"),
    ]:
        wrong_path, wrong_report = sample_path(root / name, args, args.strict, "vulkan", common + params)
        validated_run(wrong_path, wrong_report, "strict", "vulkan", args.msaa)
        wrong, _ = read_plane(wrong_path, wrong_report, "sdr")
        result = compare_pixels(roi_pixels(normal, ROI[region]), roi_pixels(wrong, ROI[region]),
                                "sdr", profile="scene")
        require(not result["passed"], f"ROI did not detect {name}: {result}")
        output[name] = {"report_sha256": digest(wrong_path), "roi": region, "stats": result}

    # Model the encoded RGBA8 result being interpreted as linear and encoded again.
    double = normal.copy()
    values = normal[:, :, :3].astype(np.float64) / 255.0
    double[:, :, :3] = np.rint(np.where(values <= 0.0031308, 12.92 * values,
                                         1.055 * values ** (1.0 / 2.4) - 0.055) * 255).astype(np.uint8)
    result = compare_pixels(normal, double, "sdr", profile="scene")
    require(not result["passed"], "Image gate failed to detect double sRGB")
    Image.fromarray(double).save(root / "double-srgb.png")
    output["double-srgb"] = result

    for name in ("glass-detail", "label-detail"):
        paths = [sample_path(root / f"{name}-{label}", args, args.strict, "vulkan",
                             ["--view", name, "--alpha-only", "--capture-shadow-maps", *extra])
                 for label, extra in (("original", []), ("opaque", ["--alpha-override", "opaque"]))]
        for sample, report in paths:
            validated_run(sample, report, "strict", "vulkan", args.msaa)
        first, second = (item[1] for item in paths)
        if args.msaa == 1:
            require(first["depth_coverage_pixels"] == 0 and second["depth_coverage_pixels"] > 0,
                    f"Missing controlled alpha coverage {name}")
        else:
            require(all(item["depth_coverage_pixels"] is None and "depth" not in item["captures"]
                        for item in (first, second)), f"Unexpected multisample depth capture {name}")
        for key in SHADOWS:
            values, _ = read_plane(*paths[0], key)
            require(np.all(values == 1.0), f"Original alpha caused shadow coverage {name}/{key}")
        a, b = (read_plane(*item, "sdr")[0] for item in paths)
        require(np.all(a == a[0, 0]), f"Original alpha caused color coverage {name}")
        stats = compare_pixels(a, b, "sdr", profile="scene")
        require(not stats["passed"], f"Missing alpha sensitivity {name}: {stats}")
        output[name] = {"reports": [digest(path) for path, _ in paths], "stats": stats}
    return output


def cohort(args, modes, differences=None):
    seen = {}
    files = {}
    source_hashes = {}
    planes = (*COLOR_PLANES, "depth") if args.msaa == 1 else COLOR_PLANES
    for repeat in range(1, 4):
        for mode in modes:
            for api in APIS if repeat % 2 else reversed(APIS):
                binary = args.strict if mode == "strict" else args.shipping
                directory = args.output / f"{mode}-{api}-{repeat}"
                print(directory.name, flush=True)
                path, report = sample_path(directory, args, binary, api,
                                           ["--view", "route", "--bloom", "tent", "--capture-all-views",
                                            "--capture-route-samples", "--capture-shadow-maps",
                                            "--capture-bloom-bypass"])
                validated_run(path, report, mode, api, args.msaa)
                require(report["identity"]["bloom"] == "tent", f"Wrong Bloom identity {path}")
                samples = collect(path, report, args.output, files, planes, args.msaa)
                if api in seen:
                    before = {key: value[1:] for key, value in seen[api].items()}
                    after = {key: value[1:] for key, value in samples.items()}
                    require(before == after, f"Same-API repeat changed {path}")
                else:
                    seen[api] = samples
                source_hashes[directory.name] = {"report_sha256": digest(path),
                                                  "device": report["runtime"]}
    result = compare_cross(seen["vulkan"], seen["d3d12"], differences, planes)
    return seen, {"runs": source_hashes, "cross": result}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stage", choices=("pilot", "accept"))
    for argument in ("strict", "shipping", "manifest", "ibl-manifest", "output", "profile"):
        parser.add_argument(f"--{argument}", type=Path, required=True)
    parser.add_argument("--resume", action="store_true",
                        help="revalidate completed reports from the same cohort")
    parser.add_argument("--msaa", type=int, choices=(1, 4), default=1,
                        help="scene sample count for the MSAA comparison cohort")
    args = parser.parse_args()
    args.output = args.output.resolve()
    require(args.output.parent.exists(), "Create the parent evidence directory first")
    settings = {"scene": PROFILES["scene"], "repeat": PROFILES["repeat"], "regions": ROI,
                "source": {"strict_sha256": digest(args.strict), "shipping_sha256": digest(args.shipping),
                           "manifest_sha256": digest(args.manifest),
                           "ibl_manifest_sha256": digest(args.ibl_manifest)}}
    if args.msaa == 4:
        settings["samples"] = 4
    # Compare the serialized form, including the coordinate arrays.
    settings_hash = canonical_hash(settings)
    if args.stage == "accept":
        calibration = load_json(args.profile)
        require(calibration["status"] == "passed" and calibration["settings_sha256"] == settings_hash
                and canonical_hash(calibration["settings"]) == settings_hash, "Calibration identity differs")
    args.output.mkdir(exist_ok=args.resume)
    differences = None
    if args.stage == "accept":
        differences = args.output / "sdr-differences"
        differences.mkdir(exist_ok=args.resume)
    seen, result = cohort(args, ("strict",) if args.stage == "pilot" else MODES, differences)
    result.update(status="passed", settings=settings, settings_sha256=settings_hash)
    if args.stage == "pilot":
        result["faults"] = probe_faults(args, args.output, seen["vulkan"]["view/street"])
        require(not args.profile.exists(), "Choose an unused profile path")
        write_json(args.profile, result)
    else:
        result["calibration_sha256"] = digest(args.profile)
    write_json(args.output / "summary.json", result)


if __name__ == "__main__":
    main()
