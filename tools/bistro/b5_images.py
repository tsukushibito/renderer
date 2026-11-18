"""Calibrate and apply the Bistro B5 full-scene image gate."""

import argparse
from collections import Counter
import os
from pathlib import Path
import subprocess
from uuid import uuid4

import numpy as np
from PIL import Image

from b5_evidence import (
    PIPELINES, SAMPLES, attempt_directory, loaded_inputs, render_modes, report_record,
    validate_loaded_manifests, verified_report, write_checkpoint,
)
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


def source_identity(args):
    return {"strict_sha256": digest(args.strict), "shipping_sha256": digest(args.shipping),
            "manifest_sha256": digest(args.manifest), "ibl_manifest_sha256": digest(args.ibl_manifest)}


def begin_cohort(args, settings, calibration_hash):
    path = args.output / "progress.json"
    identity = {"schema_version": 1, "stage": args.stage, "settings": settings,
                "settings_sha256": canonical_hash(settings), "calibration_sha256": calibration_hash}
    if args.resume:
        require(path.is_file(), "No B5 image progress checkpoint; choose a new output directory")
        progress = load_json(path)
        actual = {key: progress.get(key) for key in identity}
        require(canonical_hash(actual) == canonical_hash(identity), "B5 image resume identity differs")
        return progress
    args.output.mkdir(exist_ok=False)
    progress = {**identity, "runs": {}}
    write_checkpoint(path, progress)
    return progress


def sample_path(args, progress, label, binary, api, extra):
    previous = progress["runs"].get(label)
    if previous is None:
        directory = attempt_directory(args.output, label)
        root = directory / "report.json"
    else:
        root = verified_report(args.output, previous["report"])
        directory = root.parent
        require(directory.parent == args.output and root.name == "report.json",
                f"Unexpected B5 run location: {root}")
    command = [str(binary), "--api", api, "--manifest", str(args.manifest), "--ibl-manifest",
               str(args.ibl_manifest), "--width", "1920", "--height", "1080",
               "--frames", "4", "--warmup", "2", "--inflight", "2", "--output", str(directory),
               "--msaa", str(args.msaa), "--pipeline", args.pipeline,
               "--submission", "direct", "--instances", "off", *extra]
    if previous is not None:
        require(previous["command"] == command
                and load_json(directory / "command.json") == command, f"Changed command {directory}")
        return root, load_json(root)
    require(source_identity(args) == progress["settings"]["source"],
            "B5 image executable or input identity changed")
    directory.mkdir(parents=True, exist_ok=False)
    write_json(directory / "command.json", command)
    with (directory / "process.log").open("wb") as log:
        subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=900)
    return root, load_json(root)


def record_run(args, progress, label, path, samples=None):
    record = {"report": report_record(args.output, path),
              "command": load_json(path.parent / "command.json"),
              "samples": samples if samples is not None else {}}
    if label in progress["runs"]:
        require(progress["runs"][label] == record, f"Stored evidence differs for {label}")
        return
    require(source_identity(args) == progress["settings"]["source"],
            "B5 image executable or input identity changed")
    progress["runs"][label] = record
    write_checkpoint(args.output / "progress.json", progress)


def validated_run(path, report, mode, api, settings):
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
            and identity["shadows"] and identity["aa"],
            path)
    require(render_modes(report, windowed=False) == settings["modes"],
            f"Bistro render modes changed: {path}")
    source = settings["source"]
    validate_loaded_manifests(report, source["manifest_sha256"], source["ibl_manifest_sha256"],
                              windowed=False)
    if settings["modes"]["pipeline"] in ("forward-plus", "forward-reference"):
        require(identity.get("local_lights") == {
            "profile": "bistro-street-grid-v1", "count": 64, "tile_size": 16,
        }, f"Incorrect local-light workload {path}")
    scene = report["scene"]
    require((scene["meshes"], scene["primitives"], scene["materials"], scene["alpha_zero_materials"])
            == (1296, 1591, 132, 2), f"Missing full-scene inputs {path}")
    require(report["memory"]["normal_allocation_free"], f"Normal frame memory {path}")


def read_plane(path, report, name):
    group = "shadow_captures" if name in SHADOWS else "captures"
    require("sha256" in report[group][name], f"Missing capture hash: {path}/{name}")
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


def collect(path, report, root, index, planes, previous):
    samples = {}
    capture_paths = reports(path.parent, report)
    records = {name: report_record(root, capture_path) for name, capture_path in capture_paths.items()}
    if previous is not None:
        require(records == previous["samples"], f"Stored capture reports differ for {path}")
    modes = render_modes(report, windowed=False)
    inputs = loaded_inputs(report, windowed=False)
    local_lights = report["identity"]["local_lights"]
    for name, capture_path in capture_paths.items():
        captured = load_json(capture_path)
        label = name.split("/", 1)[1]
        expected = captured["identity"].get("route_frame") if name.startswith("route/") else captured["identity"]["view"]
        require(str(expected) == label and captured["capture_only"]
                and captured["readback_outside_measurement"], f"Unexpected sample {capture_path}")
        require(render_modes(captured, windowed=False) == modes
                and loaded_inputs(captured, windowed=False) == inputs
                and captured["identity"]["local_lights"] == local_lights,
                f"Capture render modes or inputs differ in {capture_path}")
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
    return samples, records


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


def probe_faults(args, progress, street):
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
        wrong_path, wrong_report = sample_path(args, progress, name, args.strict, "vulkan", common + params)
        validated_run(wrong_path, wrong_report, "strict", "vulkan", progress["settings"])
        wrong, _ = read_plane(wrong_path, wrong_report, "sdr")
        result = compare_pixels(roi_pixels(normal, ROI[region]), roi_pixels(wrong, ROI[region]),
                                "sdr", profile="scene")
        require(not result["passed"], f"ROI did not detect {name}: {result}")
        record_run(args, progress, name, wrong_path)
        output[name] = {"report_sha256": digest(wrong_path), "roi": region, "stats": result}

    # Model the encoded RGBA8 result being interpreted as linear and encoded again.
    double = normal.copy()
    values = normal[:, :, :3].astype(np.float64) / 255.0
    double[:, :, :3] = np.rint(np.where(values <= 0.0031308, 12.92 * values,
                                         1.055 * values ** (1.0 / 2.4) - 0.055) * 255).astype(np.uint8)
    result = compare_pixels(normal, double, "sdr", profile="scene")
    require(not result["passed"], "Image gate failed to detect double sRGB")
    Image.fromarray(double).save(args.output / "double-srgb.png")
    output["double-srgb"] = result

    for name in ("glass-detail", "label-detail"):
        labels = (f"{name}-original", f"{name}-opaque")
        paths = [sample_path(args, progress, label, args.strict, "vulkan",
                             ["--view", name, "--alpha-only", "--capture-shadow-maps", *extra])
                 for label, extra in zip(labels, ([], ["--alpha-override", "opaque"]))]
        for sample, report in paths:
            validated_run(sample, report, "strict", "vulkan", progress["settings"])
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
        for label, (path, _) in zip(labels, paths):
            record_run(args, progress, label, path)
        output[name] = {"reports": [digest(path) for path, _ in paths], "stats": stats}
    return output


def cohort(args, progress, modes, differences=None):
    seen = {}
    files = {}
    records = {}
    planes = (*COLOR_PLANES, "depth") if args.msaa == 1 else COLOR_PLANES
    for repeat in range(1, 4):
        for mode in modes:
            for api in APIS if repeat % 2 else reversed(APIS):
                binary = args.strict if mode == "strict" else args.shipping
                label = f"{mode}-{api}-{repeat}"
                print(label, flush=True)
                path, report = sample_path(args, progress, label, binary, api,
                                           ["--view", "route", "--bloom", "tent", "--capture-all-views",
                                            "--capture-route-samples", "--capture-shadow-maps",
                                            "--capture-bloom-bypass"])
                validated_run(path, report, mode, api, progress["settings"])
                require(report["identity"]["bloom"] == "tent", f"Wrong Bloom identity {path}")
                samples, sample_records = collect(
                    path, report, args.output, files, planes, progress["runs"].get(label),
                )
                if api in seen:
                    before = {key: value[1:] for key, value in seen[api].items()}
                    after = {key: value[1:] for key, value in samples.items()}
                    require(before == after, f"Same-API repeat changed {path}")
                else:
                    seen[api] = samples
                record_run(args, progress, label, path, sample_records)
                record = progress["runs"][label]
                records[label] = {"report": record["report"], "samples": record["samples"],
                                  "device": report["runtime"]}
    result = compare_cross(seen["vulkan"], seen["d3d12"], differences, planes)
    return seen, {"runs": records, "cross": result}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stage", choices=("pilot", "accept"))
    for argument in ("strict", "shipping", "manifest", "ibl-manifest", "output", "profile"):
        parser.add_argument(f"--{argument}", type=Path, required=True)
    parser.add_argument("--resume", action="store_true",
                        help="revalidate completed reports from the same cohort")
    parser.add_argument("--msaa", type=int, choices=SAMPLES, default=1,
                        help="scene sample count for the MSAA comparison cohort")
    parser.add_argument("--pipeline", choices=PIPELINES, default="forward")
    args = parser.parse_args()
    if args.pipeline == "deferred" and args.msaa != 1:
        parser.error("the deferred pipeline requires --msaa 1")
    args.output = args.output.resolve()
    require(args.output.parent.exists(), "Create the parent evidence directory first")
    settings = {"scene": PROFILES["scene"], "repeat": PROFILES["repeat"], "regions": ROI,
                "source": source_identity(args),
                "modes": {"samples": args.msaa, "pipeline": args.pipeline,
                          "submission": "direct", "instances": "off"}}
    # Compare the serialized form, including the coordinate arrays.
    settings_hash = canonical_hash(settings)
    calibration_hash = None
    if args.stage == "accept":
        calibration_hash = digest(args.profile)
        calibration = load_json(args.profile)
        require(calibration["stage"] == "pilot" and calibration["status"] == "passed"
                and calibration["settings_sha256"] == settings_hash
                and canonical_hash(calibration["settings"]) == settings_hash, "Calibration identity differs")
    elif not args.resume:
        require(not args.profile.exists(), "Choose an unused profile path")
    progress = begin_cohort(args, settings, calibration_hash)
    differences = None
    if args.stage == "accept":
        differences = args.output / "sdr-differences"
        differences.mkdir(exist_ok=args.resume)
    seen, result = cohort(args, progress, ("strict",) if args.stage == "pilot" else MODES, differences)
    result.update(schema_version=1, stage=args.stage, status="passed",
                  settings=settings, settings_sha256=settings_hash)
    if args.stage == "pilot":
        result["faults"] = probe_faults(args, progress, seen["vulkan"]["view/street"])
        require(source_identity(args) == settings["source"], "B5 image executable or input identity changed")
        if args.profile.exists():
            require(args.resume and canonical_hash(load_json(args.profile)) == canonical_hash(result),
                    "Choose an unused profile path")
        else:
            write_checkpoint(args.profile, result)
    else:
        require(source_identity(args) == settings["source"] and digest(args.profile) == calibration_hash,
                "B5 image executable, input or calibration identity changed")
        result["calibration_sha256"] = calibration_hash
    write_checkpoint(args.output / "summary.json", result)


if __name__ == "__main__":
    main()
