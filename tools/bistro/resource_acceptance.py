"""Run Bistro residency/update acceptance serially, outside performance studies.

Requires separately built optimized strict and shipping executables with
memory-profiling. Stores complete reports, raw captures locally, binary/input
identities, and comparisons using the existing frozen image profiles.
"""
import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import subprocess
import time

from compare_runtime import compare_reports, digest, load_json


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2, ensure_ascii=False,
                                    allow_nan=False) + "\n", encoding="utf-8")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def positive(value):
    return type(value) is int and value > 0


def resource_captures(directory, report):
    """Use the existing finite-value/hash/containment checks for new captures."""
    checks = report["resource_checks"]
    views = checks["views"]
    items = {f"serial-{index}": captures for index, captures in enumerate(views["serial_captures"])}
    items.update({f"overlap-{index}": captures
                  for index, captures in enumerate(views["overlapping_captures"])})
    items["capacity-retry"] = views["capacity"]["retry_captures"]
    cycle = checks["replacement"]["cycles"][0]
    items["replacement-changed"] = cycle["changed_captures"]
    items["replacement-texture-only"] = cycle["texture_only_captures"]
    items["replacement-restored"] = cycle["restored_captures"]
    result = {}
    for name, captures in items.items():
        path = directory / f"resource-{name}.json"
        write_json(path, {"schema_version": 1, "runtime": report["runtime"],
                         "identity": {**report["identity"], "resource_capture": name},
                         "captures": captures})
        result[name] = (path, None)
    for probe in cycle["mip_probes"]:
        name = f"mip-{probe['level']}"
        path = directory / f"resource-{name}.json"
        write_json(path, {"schema_version": 1, "runtime": report["runtime"],
            "identity": {**report["identity"], "resource_capture": name,
                         "width": probe["width"], "height": probe["height"]},
            "captures": {"sdr": probe}})
        result[name] = (path, ["sdr"])
    return result


def validate_report(report, *, api, mode, full_scene):
    require(report.get("schema_version") == 1, "Unexpected runner schema")
    require(report["runtime"]["api"] == api, "Wrong API")
    require(report["runtime"]["validation_mode"] == mode, "Wrong validation mode")
    validation = report["validation"]
    require(validation["clean"] is True and validation["mode"] == mode,
            "Native teardown validation failed or wrong mode")
    for field in ("warnings", "errors", "callback_failures"):
        require(validation[field] == 0, f"Native {field} is nonzero")
    if api == "vulkan":
        require(validation["requested"] == ("Required" if mode == "strict" else "Disabled"),
                "Wrong native Vulkan request")
        if mode == "strict":
            require(validation["core_validation"] is True
                    and validation["synchronization_validation"] is True,
                    "Core/SyncVal not observed")
    else:
        require(validation["debug_layer_enabled"] == (mode == "strict")
                and validation["gpu_based_validation_enabled"] == (mode == "strict"),
                "Wrong D3D12 effective validation")
        require(validation["corruption"] == 0, "D3D12 corruption")
        if mode == "strict":
            require(validation["teardown_checked"] is True, "Teardown not checked")
    if full_scene:
        scene = report["scene"]
        require((scene["meshes"], scene["primitives"], scene["materials"])
                == (1296, 1591, 132), "Full Bistro geometry/material counts differ")
        require(scene["resident_images_with_defaults"] >= 405,
                "Missing full Bistro images")
        require(scene["source"]["image_count"] == 405
                and scene["source"]["triangle_count"] == 2832120,
                "Full Bistro source images/triangles differ")
    require(report["memory"]["normal_allocation_free"] is True,
            "Normal frame allocation/native gate failed")
    checks = report["resource_checks"]
    require(checks["passed"] is True, "Resource acceptance failed")
    stages = checks["stages"]
    for name in ("prepared", "uploaded", "warmed", "after_frames", "after_checks"):
        snapshot = stages[name]
        require(snapshot["api"] == api, "Memory snapshot API differs")
        for field in ("allocation_count", "allocation_bytes", "block_count", "block_bytes"):
            require(positive(snapshot["native"][field]), f"Missing native {field}")
        # This target machine supports native budgets. Do not silently replace
        # them with heap size or an allocator fallback when collecting evidence.
        require(positive(snapshot["process_budget_bytes"]), "Native process budget unavailable")
        require(type(snapshot["process_usage_bytes"]) is int
                and snapshot["process_usage_bytes"] >= 0, "Native process usage unavailable")
    views = checks["views"]
    require(len(set(views["names"])) >= 2, "Two distinct camera views required")
    require(views["queued_before_completion_observation"] >= 2
            and views["serial_matches"] is True, "Concurrent view content mismatch")
    capacity = views["capacity"]
    require(capacity["retry_matches"] is True and capacity["resource"]
            and capacity["recovery"], "Typed capacity rejection/retry missing")
    require(positive(capacity["limit"])
            and views["queued_before_completion_observation"] == capacity["limit"]
            and (capacity["required"] is None or capacity["required"] > capacity["limit"]),
            "Capacity boundary was not exceeded")
    replacement = checks["replacement"]
    cycles = replacement["cycles"]
    require(len(cycles) >= 3, "At least three replacement cycles required")
    for cycle in cycles:
        for field in ("old_pending_matches", "stale_image_rejected", "stale_buffer_rejected",
                      "depth_unchanged", "restored_matches"):
            require(cycle[field] is True, f"Replacement check failed: {field}")
        require(all(positive(cycle["changed_pixels"][p]) for p in ("hdr", "sdr")),
                "Replacement has no observed rendered effect")
        for field in ("texture_changed_pixels", "material_changed_pixels"):
            require(all(positive(cycle[field][p]) for p in ("hdr", "sdr")),
                    "Texture/material PBR binding changes were not independently observed")
        require(cycle["pending_memory"]["logical"]["retained_bytes"]
                >= cycle["expected_retained_bytes"] > 0,
                "Old texture/material ownership was not retained before completion observation")
        probes = cycle["mip_probes"]
        require(len(probes) == replacement["mip_levels"]
                and all(p["passed"] is True for p in probes), "Incomplete mip probes")
    samples = checks["memory_samples"]
    require(len(samples) >= 3 and checks["plateau"]["passed"] is True,
            "Memory plateau evidence missing")
    for sample in samples[1:]:
        # Compare the actual recorded samples, not just a reported pass bit.
        for field in ("allocation_count", "allocation_bytes", "block_count", "block_bytes"):
            require(sample["native"][field] == samples[0]["native"][field],
                    "Native memory grew across replacement cycles")
        require(sample["logical"] == samples[0]["logical"],
                "Logical resource ownership changed across replacement cycles")
    return {"passed": True, "cycles": len(cycles), "mip_levels": replacement["mip_levels"],
            "stages": stages, "plateau": checks["plateau"]}


def run(args):
    require(args.repeats >= 2, "Use at least two independent processes per API/configuration")
    output = args.output.resolve()
    require(not output.exists(), "Choose a new output directory")
    binaries = {"strict": args.strict_binary.resolve(), "disabled": args.shipping_binary.resolve()}
    identities = {mode: {"path": str(path), "sha256": digest(path)}
                  for mode, path in binaries.items()}
    output.mkdir(parents=True)
    (output / "logs").mkdir()
    summary = {"schema_version": 1, "status": "running",
               "started_utc": datetime.now(timezone.utc).isoformat(), "binaries": identities,
               "source_commit": args.source_commit, "source_manifest_sha256":
                   digest(args.manifest) if args.manifest else None,
               "tool_sha256": digest(__file__), "runs": [], "comparisons": [],
               "scope": "Serial acceptance with memory instrumentation; not normal-release performance, guaranteed VRAM residency, native OOM or device-loss injection."}
    write_json(output / "summary.json", summary)
    first = {}
    try:
        for repeat in range(args.repeats):
            # Reverse API order between repetitions; no GPU/build overlap.
            for mode in binaries:
                for api in (("vulkan", "d3d12") if repeat % 2 == 0 else ("d3d12", "vulkan")):
                    name = f"{mode}-{api}-{repeat + 1}"
                    directory = output / name
                    command = [str(binaries[mode]), "--api", api, "--resource-checks",
                               "--frames", "24", "--warmup", "8", "--inflight", "2",
                               "--view", "street", "--output", str(directory)]
                    if args.manifest:
                        command += ["--manifest", str(args.manifest.resolve()),
                                    "--width", "1920", "--height", "1080", "--capture-all-views"]
                    else:
                        command += ["--fixture"]
                    require(digest(binaries[mode]) == identities[mode]["sha256"],
                            "Archived binary changed during acceptance")
                    record = {"name": name, "api": api, "mode": mode, "command": command}
                    summary["runs"].append(record)
                    start = time.perf_counter()
                    print(f"START {name}", flush=True)
                    with (output / "logs" / f"{name}.stdout.txt").open("wb") as stdout, \
                            (output / "logs" / f"{name}.stderr.txt").open("wb") as stderr:
                        completed = subprocess.run(command, stdout=stdout, stderr=stderr,
                                                   timeout=args.timeout, check=False)
                    record["controller_seconds"] = time.perf_counter() - start
                    record["exit_code"] = completed.returncode
                    require(completed.returncode == 0, f"{name} failed; inspect retained logs")
                    path = directory / "report.json"
                    report = load_json(path)
                    record["acceptance"] = validate_report(report, api=api, mode=mode,
                                                           full_scene=args.manifest is not None)
                    record["report_sha256"] = digest(path)
                    record["report"] = report
                    resource_reports = resource_captures(directory, report)
                    if args.manifest:
                        require(report["identity"]["cook_manifest_sha256"]
                                == summary["source_manifest_sha256"], "Loaded cook identity changed")
                        require(len(report["views"]) == 7, "Seven fixed views missing")
                    for key, other in (((mode, api), first.get((mode, api))),
                                       ((mode, "cross"), first.get((mode, "cross")))):
                        if other is not None and (key[1] != "cross" or other[0] != api):
                            comparison = compare_reports(other[1], path,
                                profile="repeat" if key[1] != "cross" else ("scene" if args.manifest else "fixture"))
                            require(comparison["passed"], "Frozen image comparison failed")
                            summary["comparisons"].append(comparison)
                            previous_resources = resource_captures(other[1].parent, load_json(other[1]))
                            for capture_name, (capture_path, planes) in resource_reports.items():
                                comparison = compare_reports(previous_resources[capture_name][0], capture_path,
                                    planes=planes,
                                    profile="repeat" if key[1] != "cross" else ("scene" if args.manifest else "fixture"))
                                require(comparison["passed"], "Resource image comparison failed")
                                summary["comparisons"].append(comparison)
                            if args.manifest:
                                older = load_json(other[1])
                                older_views = {v["view"]: v["report"] for v in older["views"]}
                                for view in report["views"]:
                                    comparison = compare_reports(older_views[view["view"]], view["report"],
                                        profile="repeat" if key[1] != "cross" else "scene")
                                    require(comparison["passed"], "Frozen fixed-view comparison failed")
                                    summary["comparisons"].append(comparison)
                    first.setdefault((mode, api), (api, path))
                    first.setdefault((mode, "cross"), (api, path))
                    write_json(output / "summary.json", summary)
                    print(f"PASS {name}: {record['controller_seconds']:.1f}s", flush=True)
        summary["status"] = "passed"
    except Exception as error:
        summary["status"] = "failed"
        summary["error"] = str(error)
        raise
    finally:
        write_json(output / "summary.json", summary)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--strict-binary", type=Path, required=True)
    parser.add_argument("--shipping-binary", type=Path, required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repeats", type=int, default=2)
    parser.add_argument("--timeout", type=int, default=900)
    run(parser.parse_args())


if __name__ == "__main__":
    main()
