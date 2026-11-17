"""Serial B3 image/resource acceptance, separate from normal performance runs.

Frozen comparator profiles are reused unchanged. This driver adds shadow and
AA-off captures to the existing residency/update evidence. Both executables must
be optimized and include memory-profiling; only the strict executable enables
Renderer/native development validation. This is not a frame-rate benchmark.
"""
import argparse
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path
import re
import subprocess
import time

import numpy as np

from compare_runtime import (FORMATS, _inside_directory, _resolve_capture_path,
                             compare_reports, digest, load_capture, load_json)
from resource_acceptance import (require, resource_captures,
                                 validate_report, write_json)


ROOT = Path(__file__).resolve().parents[2]
SHADOW_EXTENT = 2048
SHADOW_NAMES = {f"shadow{i}" for i in range(4)}
EVENT_ORDER = ("Rust Allocate/AllocateZeroed/Reallocate/Deallocate; VMA Allocate/Reallocate/Deallocate; "
               "native ObjectCreate/ObjectDestroy/ResourceCreate/ResourceDestroy/MemoryAllocate/MemoryFree/DescriptorAllocate/DescriptorFree/Reset")


def schedule(repeats):
    require(type(repeats) is int and repeats >= 3, "B3 requires at least three independent processes per condition")
    base = [("strict", "vulkan"), ("disabled", "d3d12"),
            ("strict", "d3d12"), ("disabled", "vulkan")]
    result = []
    for repeat in range(repeats):
        order = list(reversed(base)) if repeat % 2 else base[:]
        offset = repeat % len(order)
        order = order[offset:] + order[:offset]
        result.extend((mode, api, repeat + 1) for mode, api in order)
    return result


def command(binary, directory, *, api, manifest=None, ibl_manifest=None):
    result = [str(binary), "--api", api, "--resource-checks", "--capture-shadow-maps",
              "--capture-all-views", "--frames", "24", "--warmup", "8", "--inflight", "2",
              "--view", "route", "--output", str(directory)]
    if manifest:
        require(ibl_manifest is not None, "Full B3 acceptance needs --ibl-manifest")
        result += ["--manifest", str(manifest.resolve()), "--ibl-manifest", str(ibl_manifest.resolve()),
                   "--width", "1920", "--height", "1080"]
    else:
        result += ["--fixture"]
    return result


def _counter(value):
    return type(value) is int and value >= 0


def validate_memory(memory):
    """Independently check raw counters, including setup/shutdown controls."""
    for stage in ("normal", "setup", "shutdown"):
        sample = memory[stage]
        require(sample["overflowed"] is False and sample["event_order"] == EVENT_ORDER,
                f"Invalid {stage} memory observation")
        for category, count in (("rust", 4), ("vma_host", 3)):
            values = sample[category]
            require(isinstance(values, list) and len(values) == count,
                    f"Wrong {stage} {category} counter shape")
            require(all(isinstance(item, dict) and set(item) == {"calls", "bytes"}
                        and all(_counter(item[field]) for field in ("calls", "bytes"))
                        for item in values), f"Invalid {stage} {category} counters")
        require(isinstance(sample["native"], list) and len(sample["native"]) == 9
                and all(_counter(value) for value in sample["native"]),
                f"Wrong {stage} native counter shape")
    normal = memory["normal"]
    require(memory["normal_allocation_free"] is True and normal["allocation_free"] is True,
            "Normal memory gate is false")
    require(all(item[field] == 0 for category in ("rust", "vma_host")
                for item in normal[category] for field in ("calls", "bytes"))
            and all(value == 0 for value in normal["native"][:8]),
            "Normal frame raw allocation/native counters are nonzero")
    require(any(item["calls"] > 0 for item in memory["setup"]["rust"][:3])
            and memory["shutdown"]["rust"][3]["calls"] > 0,
            "Rust setup/shutdown positive controls missing")
    require(all(memory["setup"]["native"][index] > 0 for index in (0, 2, 4, 6))
            and all(memory["shutdown"]["native"][index] > 0 for index in (1, 3, 5, 7)),
            "Native setup/shutdown positive controls missing")
    return {"passed": True, "normal_native_reset_calls": normal["native"][8]}


def _fixed_f32(value, expected):
    # Rust report values may be serialized as their decimal literal or as the
    # exact f64 expansion of a prepared f32 field. Accept those two identities,
    # rather than relaxing the quality setting by an arbitrary tolerance.
    return type(value) in (int, float) and value in (expected, np.float32(expected).item())


def validate_identity(report, expected, *, full_scene):
    identity = report["identity"]
    for key in ("conditions_sha256", "shaders_sha256"):
        require(identity[key] == expected[key], f"Loaded {key} differs from archived source")
    require(identity["aa"] is True and identity["shadows"] is True
            and identity["alpha_only"] is False and identity["alpha_override"] is False,
            "B3 acceptance requires AA/shadows and original full material inputs")
    require(identity["inflight"] == 2, "Wrong in-flight capacity")
    require([identity["width"], identity["height"]] == ([1920, 1080] if full_scene else [128, 96]),
            "Wrong B3 acceptance extent")
    shadow = identity["shadow_profile"]
    require(shadow["cascades"] == 4 and shadow["resolution"] == SHADOW_EXTENT
            and shadow["distance_m"] == 120 and _fixed_f32(shadow["split_lambda"], 0.6)
            and _fixed_f32(shadow["blend_fraction"], 0.1),
            "Shadow quality differs from the fixed B3 starting profile")
    aa = identity["aa_profile"]
    require(_fixed_f32(aa["edge_threshold_min"], 0.0312)
            and _fixed_f32(aa["edge_threshold_max"], 0.125)
            and aa["iterations"] == 12 and _fixed_f32(aa["subpixel_quality"], 0.75)
            and aa["position"] == "after Neutral, before sRGB",
            "AA quality differs from the fixed B3 profile")
    if full_scene:
        policy = report["scene"]["source"].get("applied_material_policy", {})
        require(policy.get("ambient_occlusion_enabled") is False
                and policy.get("source_pixels_and_references_preserved") is True,
                "Full Bistro must apply the fixed default-disabled AO lighting profile")
        require(identity["cook_manifest_sha256"] == expected["cook_manifest_sha256"],
                "Loaded geometry cook identity changed")
        require(identity["environment"]["manifest_sha256"] == expected["ibl_manifest_sha256"],
                "Loaded IBL identity changed")
    else:
        require(identity["environment"]["kind"] == "fixture-constant-diffuse",
                "Unexpected fixture IBL")
    measurement = report["measurement"]
    require((measurement["frames"], measurement["warmup_frames"], measurement["inflight"])
            == (24, 8, 2), "Wrong short acceptance ROI")
    require(measurement["native_profiling_enabled"] is False
            and measurement["presentation"] is False
            and measurement["readback_outside_measurement"] is True
            and report["readback_outside_measurement"] is True,
            "Acceptance ROI has profiling, presentation or readback scope drift")
    cycles = report["resource_checks"]["replacement"]["cycles"]
    require(len(cycles) == 4, "Require all four replacement cycles")
    capacity = report["resource_checks"]["views"]["capacity"]
    require(capacity["resource"] == "ExecutionSlots" and capacity["recovery"] == "ReleaseOwners"
            and capacity["limit"] == 2, "Expected typed execution-slot exhaustion/recovery")


def _write_capture_report(path, source, identity, captures):
    write_json(path, {"schema_version": 1, "runtime": source["runtime"],
                      "identity": identity, "captures": captures})
    return path


def diagnostics(path, report):
    """Keep synthetic reports beside their owner, preserving file containment."""
    identity = report["identity"]
    require(set(report["captures"]) == {"hdr", "sdr", "depth"}, "Primary capture planes incomplete")
    shadows = report["shadow_captures"]
    require(isinstance(shadows, dict) and set(shadows) == SHADOW_NAMES,
            "Four distinct shadow captures required")
    require(len({_report_path(path.parent, meta["path"], "Shadow capture")
                 for meta in shadows.values()}) == 4,
            "Shadow captures reuse one artifact path")
    result = {"main": (path, None)}
    for name in sorted(shadows):
        meta = shadows[name]
        require(meta["format"] == "r32f-le" and meta["width"] == SHADOW_EXTENT
                and meta["height"] == SHADOW_EXTENT,
                "Wrong shadow capture format/extent")
        shadow_identity = {**identity, "capture_variant": name,
                           "width": SHADOW_EXTENT, "height": SHADOW_EXTENT}
        synthetic = _write_capture_report(path.parent / f"acceptance-{name}.json", report,
                                          shadow_identity, {"depth": meta})
        result[name] = (synthetic, ["depth"])
    aa = report["aa_off"]
    require(isinstance(aa, dict) and set(aa["captures"]) == {"hdr", "sdr", "depth"}
            and aa["shadow_captures"] == {} and aa["readback_outside_measurement"] is True,
            "AA-off diagnostic captures are incomplete")
    aa_identity = {**identity, "aa": False, "capture_variant": "aa-off"}
    synthetic = _write_capture_report(path.parent / "acceptance-aa-off.json", report,
                                      aa_identity, aa["captures"])
    result["aa-off"] = (synthetic, None)
    return result


def _report_path(directory, name, label="Fixed-view report"):
    candidate = Path(name)
    path = _resolve_capture_path(candidate if candidate.is_absolute() else directory / candidate)
    require(_inside_directory(path, directory), f"{label} escapes its run directory")
    return path


def collect(path, report, expected_views, *, aa_checks=None):
    artifacts = diagnostics(path, report)
    owners = [(path, report)]
    views = report["views"]
    require(isinstance(views, list) and len(views) == len(expected_views)
            and {view["view"] for view in views} == set(expected_views),
            "Fixed-view names missing, duplicated or unexpected")
    paths = set()
    for view in views:
        view_path = _report_path(path.parent, view["report"])
        require(view_path not in paths, "Duplicate fixed-view report path")
        paths.add(view_path)
        value = load_json(view_path)
        require(value["identity"] == {**report["identity"], "view": view["view"]}
                and value["runtime"] == report["runtime"]
                and value["capture_only"] is True
                and value["readback_outside_measurement"] is True,
                "Fixed-view rendering identity or capture scope differs")
        owners.append((view_path, value))
        artifacts.update({f"view-{view['view']}-{key}": item
                          for key, item in diagnostics(view_path, value).items()})
    artifacts.update({f"resource-{key}": item
                      for key, item in resource_captures(path.parent, report).items()})
    # Validate first-run data as well; every later comparison performs these same
    # finite/hash/byte-size/containment checks again on both captured artifacts.
    for capture_path, planes in artifacts.values():
        value = load_json(capture_path)
        for plane in (planes or ("hdr", "sdr", "depth")):
            meta = value["captures"][plane]
            require(re.fullmatch(r"[0-9a-f]{64}", meta.get("sha256", "")) is not None,
                    "Capture SHA-256 is required")
            _, dtype, channels = FORMATS[plane]
            require(type(meta.get("byte_length")) is int and meta["byte_length"] ==
                    meta["width"] * meta["height"] * channels * dtype.itemsize,
                    "Capture byte-length metadata differs from dimensions/format")
            pixels, _ = load_capture(capture_path, value, plane)
            require(pixels.shape[:2] == (value["identity"]["height"], value["identity"]["width"]),
                    "Capture extent differs from its rendering identity")
    changes = []
    for owner_path, value in owners:
        # HDR and depth are independent of output AA. Their required hashes were
        # verified against actual bytes above, so equality proves bit identity.
        for plane in ("hdr", "depth"):
            require(value["captures"][plane]["sha256"] == value["aa_off"]["captures"][plane]["sha256"],
                    f"AA changes the {plane} input rather than only SDR output")
        normal, _ = load_capture(owner_path, value, "sdr")
        diagnostic, _ = load_capture(owner_path, {**value, "captures": value["aa_off"]["captures"]}, "sdr")
        changes.append({"view": value["identity"]["view"], "hdr_depth_bit_exact": True,
                        "sdr_changed_pixels": int(np.count_nonzero(np.any(normal != diagnostic, axis=2)))})
    require(any(item["sdr_changed_pixels"] > 0 for item in changes),
            "AA has no observed SDR effect in any captured view")
    if aa_checks is not None:
        aa_checks.extend(changes)
    return artifacts


def compare_sets(first, second, profile, kind, *, destination=None):
    require(set(first) == set(second), "Capture set changed between runs")
    comparisons = []
    for name, (path, planes) in second.items():
        require(first[name][1] == planes, "Capture planes changed between runs")
        compared = compare_reports(first[name][0], path, profile=profile, planes=planes)
        compared.update(capture=name, relation=kind)
        comparisons.append(compared)
        if destination is not None:
            destination.append(compared)
        require(compared["passed"], f"Frozen {kind} comparison failed: {name}")
    return comparisons


def run(args):
    plan = schedule(args.repeats)
    require(bool(args.manifest) == bool(args.ibl_manifest),
            "Use both geometry/IBL manifests for full Bistro, or neither for fixture")
    require(re.fullmatch(r"[0-9a-f]{40}", args.source_commit) is not None,
            "Use a full source commit hash")
    output = args.output.resolve()
    require(not output.exists(), "Choose a new output directory")
    binaries = {"strict": args.strict_binary.resolve(), "disabled": args.shipping_binary.resolve()}
    source_paths = {"conditions": ROOT / "tools/bistro/conditions.json",
                    "shaders": ROOT / "crates/renderer/examples/support/bistro/shaders/manifest.json",
                    "driver": Path(__file__).resolve(),
                    "resource_driver": ROOT / "tools/bistro/resource_acceptance.py",
                    "comparator": ROOT / "tools/bistro/compare_runtime.py"}
    if args.manifest:
        source_paths.update(cook=args.manifest.resolve(), ibl=args.ibl_manifest.resolve())
    sources = {name: {"path": str(path), "sha256": digest(path)} for name, path in source_paths.items()}
    expected = {f"{name}_sha256": item["sha256"] for name, item in sources.items()}
    expected.update(conditions_sha256=sources["conditions"]["sha256"],
                    shaders_sha256=sources["shaders"]["sha256"],
                    cook_manifest_sha256=sources.get("cook", {}).get("sha256"),
                    ibl_manifest_sha256=sources.get("ibl", {}).get("sha256"))
    binary_identity = {mode: {"path": str(path), "sha256": digest(path)}
                       for mode, path in binaries.items()}
    expected_views = [view["name"] for view in load_json(source_paths["conditions"])["views"]] if args.manifest else []
    require(not args.manifest or len(expected_views) == len(set(expected_views)) == 7,
            "Full B3 acceptance requires exactly seven named source views")
    output.mkdir(parents=True)
    (output / "logs").mkdir()
    summary = {"schema_version": 1, "status": "running",
               "started_utc": datetime.now(timezone.utc).isoformat(),
               "source_commit": args.source_commit, "sources": sources, "binaries": binary_identity,
               "schedule": [{"mode": mode, "api": api, "repeat": repeat} for mode, api, repeat in plan],
               "runs": [], "comparisons": [],
               "scope": "B3 short image/resource acceptance with memory instrumentation and changing route camera/CSM inputs; not normal-release performance, display FPS, physical residency, OOM or device-loss injection. Existing numerical profiles are unchanged; independent B3 fixture/error-sensitivity calibration precedes formal acceptance."}
    write_json(output / "summary.json", summary)
    conditions, modes, apis = {}, {}, {}
    try:
        for mode, api, repeat in plan:
            for item in [*sources.values(), *binary_identity.values()]:
                require(digest(item["path"]) == item["sha256"], "Source/input/binary changed during acceptance")
            name = f"{mode}-{api}-{repeat}"
            directory = output / name
            invocation = command(binaries[mode], directory, api=api,
                                 manifest=args.manifest, ibl_manifest=args.ibl_manifest)
            record = {"name": name, "mode": mode, "api": api, "repeat": repeat, "command": invocation}
            summary["runs"].append(record)
            print(f"START {name}", flush=True)
            started = time.perf_counter()
            with (output / "logs" / f"{name}.stdout.txt").open("wb") as stdout, \
                    (output / "logs" / f"{name}.stderr.txt").open("wb") as stderr:
                completed = subprocess.run(invocation, stdout=stdout, stderr=stderr,
                                           timeout=args.timeout, check=False)
            record.update(controller_seconds=time.perf_counter() - started, exit_code=completed.returncode)
            for item in [*sources.values(), *binary_identity.values()]:
                require(digest(item["path"]) == item["sha256"], "Source/input/binary changed during process execution")
            require(completed.returncode == 0, f"{name} failed; inspect retained logs")
            path = directory / "report.json"
            report = load_json(path)
            require(isinstance(report.get("memory"), dict), "B3 executables require memory-profiling")
            require(isinstance(report.get("resource_checks"), dict), "Resource checks were not executed")
            record["acceptance"] = validate_report(report, api=api, mode=mode, full_scene=bool(args.manifest))
            validate_identity(report, expected, full_scene=bool(args.manifest))
            record["raw_memory"] = validate_memory(report["memory"])
            record["aa_checks"] = []
            artifacts = collect(path, report, expected_views, aa_checks=record["aa_checks"])
            record.update(report_sha256=digest(path), report=report,
                          capture_reports={key: {"path": str(value[0]), "sha256": digest(value[0]), "planes": value[1]}
                                           for key, value in artifacts.items()})
            references = []
            if (mode, api) in conditions:
                references.append((conditions[(mode, api)], "repeat", "same-api-repeat"))
            if mode in modes and modes[mode][0] != api:
                references.append((modes[mode][1], "scene" if args.manifest else "fixture", "cross-api"))
            if api in apis and apis[api][0] != mode:
                references.append((apis[api][1], "repeat", "strict-disabled"))
            for previous, profile, kind in references:
                compare_sets(previous, artifacts, profile, kind, destination=summary["comparisons"])
            conditions.setdefault((mode, api), artifacts)
            modes.setdefault(mode, (api, artifacts))
            apis.setdefault(api, (mode, artifacts))
            write_json(output / "summary.json", summary)
            print(f"PASS {name}: {record['controller_seconds']:.1f}s", flush=True)
        observed = Counter((item["mode"], item["api"]) for item in summary["runs"])
        require(observed == Counter({(mode, api): args.repeats for mode in binaries for api in ("vulkan", "d3d12")}),
                "Incomplete acceptance matrix")
        summary["status"] = "passed"
    except Exception as error:
        summary.update(status="failed", error=str(error))
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
    parser.add_argument("--ibl-manifest", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--timeout", type=int, default=900)
    run(parser.parse_args())


if __name__ == "__main__":
    main()
