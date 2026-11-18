"""Verify the real Window smoke reports against the offscreen color output."""

import argparse
from pathlib import Path

import numpy as np

from bloom_acceptance import APIS, plane
from compare_runtime import digest, load_json
from resource_acceptance import require, write_json


def encoded(linear):
    return 255 * np.where(linear <= 0.0031308, 12.92 * linear,
                          1.055 * linear ** (1.0 / 2.4) - 0.055)


def compare_linear(path, expected, sha256):
    require(digest(path) == sha256, f"Capture hash differs: {path}")
    values = np.fromfile(path, dtype=np.uint8).reshape(expected.shape)
    require(np.all(values[:, :, 3] == 255), f"Invalid linear output alpha {path}")
    rgb = values[:, :, :3].astype(np.float64)
    # An 8-bit linear texel represents a half-code interval in linear space.
    # The offscreen shader's directly encoded code must fit that interval,
    # allowing one final sRGB code for finite precision at the boundaries.
    lower = np.rint(encoded(np.maximum(rgb - 0.5, 0.0) / 255.0)) - 1
    upper = np.rint(encoded(np.minimum(rgb + 0.5, 255.0) / 255.0)) + 1
    sdr = expected[:, :, :3]
    outside = np.any((sdr < lower) | (sdr > upper), axis=2)
    require(not np.any(outside), f"Linear source disagrees with sRGB reference: {np.count_nonzero(outside)} pixels")
    actual = np.rint(encoded(rgb / 255.0))
    return {"linear_capture_sha256": sha256, "compared_pixels": outside.size,
            "maximum_srgb_code_error_after_linear_quantization": int(np.max(np.abs(actual - sdr)))}


def verify(directory, references, route_directory=None):
    reports = {}
    for mode in ("strict", "shipping"):
        for api in APIS:
            tag = f"{mode}-{api}"
            path = directory / tag / "report.json"
            report = load_json(path)
            validation = report["validation"]
            require(report["runtime"]["api"] == api and report["presentation"]
                    and report["bloom"] == "tent", f"Wrong acceptance path {tag}")
            require(validation["clean"] and validation["mode"] == ("strict" if mode == "strict" else "disabled")
                    and all(validation[key] == 0 for key in ("warnings", "errors", "callback_failures")),
                    f"Native validation failed {tag}")
            require(report["accepted"] == 24 and report["pending_on_exit"] > 0
                    and (report["initial_width"], report["initial_height"]) == (1920, 1080),
                    f"Incomplete resize/pending acceptance {tag}")
            smoke = report["smoke"]
            require(all(smoke[name] is True for name in
                        ("passed", "resized", "minimized", "restored", "rendered_camera_replay")),
                    f"Window smoke check failed {tag}")
            reference = references / f"strict-{api}-1" / "views" / "street" / "report.json"
            reference_report = load_json(reference)
            samples = reference_report["identity"].get("samples", 1)
            pipeline = reference_report["identity"].get("pipeline", "forward")
            local_lights = reference_report["identity"].get("local_lights")
            require(report.get("samples", 1) == samples, f"Incorrect MSAA mode {tag}")
            require(report.get("pipeline", "forward") == pipeline, f"Incorrect pipeline {tag}")
            require(report.get("local_lights") == local_lights, f"Incorrect local lights {tag}")
            expected, meta = plane(reference, reference_report, "sdr")
            comparison = compare_linear(path.parent / "linear-first.bin", expected, smoke["linear_first_sha256"])
            reports[tag] = {"report_sha256": digest(path), "reference_sdr_sha256": meta["sha256"],
                            "comparison": comparison, "runtime": report["runtime"],
                            "pending_on_exit": report["pending_on_exit"]}
            if route_directory is not None:
                route_path = route_directory / tag / "report.json"
                route = load_json(route_path)
                require(route["runtime"] == report["runtime"] and route["validation"] == validation,
                        f"Route changed adapter or validation {tag}")
                require(route["presentation"] and route["bloom"] == "tent"
                        and route.get("samples", 1) == samples
                        and route.get("pipeline", "forward") == pipeline
                        and route.get("local_lights") == local_lights
                        and not route["smoke"] and route["warmup"] == 24
                        and route["accepted"] == 1824 and route["pending_on_exit"] > 0
                        and (route["initial_width"], route["initial_height"]) == (1920, 1080)
                        and len(route["timing"]["frame_intervals_ns"]) == 1800,
                        f"Incomplete Window route {tag}")
                require(route["steady_memory"]["allocation_free"] is True
                        and not route["steady_memory"]["overflowed"], f"Route memory gate failed {tag}")
                reports[tag]["route"] = {"report_sha256": digest(route_path),
                                         "steady_memory": route["steady_memory"],
                                         "pending_on_exit": route["pending_on_exit"]}
    return {"passed": True, "runs": reports}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("image_cohort", type=Path)
    parser.add_argument("--route-directory", type=Path)
    parser.add_argument("--output", type=Path, help="write the combined summary to this file")
    args = parser.parse_args()
    write_json(args.output or args.directory / "summary.json",
               verify(args.directory, args.image_cohort, args.route_directory))
