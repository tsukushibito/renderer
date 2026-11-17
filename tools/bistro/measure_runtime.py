"""Run archived normal-release Bistro PBR measurements serially, without ETW.

Build/cook/review/validation are separate prerequisites. Each fresh process
reports its own steady-state QPC and OS CPU boundaries; controller duration is
retained separately and includes load, final capture and shutdown. GPU queries
and native CPU timers are enabled together by the opt-in profiling switch. The
combined CPU/frame cost is compared with that profiling disabled.
"""
import argparse
from datetime import datetime, timezone
import importlib.util
import json
import math
from pathlib import Path
import re
import statistics
import subprocess
import sys
import time

from compare_runtime import compare_reports, digest, load_json


ROOT = Path(__file__).resolve().parents[2]
PHASES = ("build", "compile", "submit", "poll_and_retire", "result_return")
DIAGNOSTICS = {"development-validation", "vulkan-diagnostics", "d3d12-diagnostics",
               "graph-diagnostics", "memory-profiling"}


class MeasurementError(ValueError):
    """A process or report failed the measurement contract."""


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2, ensure_ascii=False, allow_nan=False)
                          + "\n", encoding="utf-8")


def utc():
    return datetime.now(timezone.utc).isoformat()


def _integer(value, label, *, positive=False):
    if type(value) is not int or value < (1 if positive else 0):
        raise MeasurementError(f"{label} must be a {'positive' if positive else 'nonnegative'} integer")
    return value


def _number(value, label, *, positive=False):
    if (type(value) not in (int, float) or not math.isfinite(value)
            or value < 0 or (positive and value == 0)):
        raise MeasurementError(f"{label} must be finite and {'positive' if positive else 'nonnegative'}")
    return value


def verified_release(path):
    """Reuse the existing PE/PDB verifier; do not execute an unverified image."""
    module_path = ROOT / "tools/cpu_profile/identity.py"
    specification = importlib.util.spec_from_file_location("bistro_build_identity", module_path)
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    manifest_path = Path(path).resolve()
    manifest = module.verify_manifest(manifest_path)
    check_release_manifest(manifest)
    binary = (manifest_path.parent / manifest["binary"]["path"]).resolve()
    return manifest, binary


def check_release_manifest(manifest):
    build = manifest["build"]
    features = set(build.get("effective_features", build.get("features", [])))
    if build["profile"] != "release" or build["example"] != "bistro":
        raise MeasurementError("Measurements require an archived bistro normal-release build")
    if features & DIAGNOSTICS:
        raise MeasurementError("Normal-release measurements must compile out diagnostic features")
    if ("tracy" in features or build.get("instrumentation", "none") != "none"
            or build.get("artifact_class", "normal") != "normal"
            or build.get("instrumentation_dependencies")):
        raise MeasurementError("Normal-release measurements reject instrumented Tracy artifacts")
    if not {"vulkan", "d3d12"}.issubset(features):
        raise MeasurementError("One archived executable must contain both measured APIs")
    git = manifest.get("git", {})
    if git.get("dirty_status") != "":
        raise MeasurementError("Archive must identify a clean committed source tree")
    if not isinstance(git.get("commit"), str) or not re.fullmatch(r"[0-9a-f]{40}", git["commit"]):
        raise MeasurementError("Archive must identify its full source commit")
    if not isinstance(git.get("source_hashes"), dict) or not git["source_hashes"]:
        raise MeasurementError("Archive must identify the actual build source hashes")


def schedule(apis, gpu_modes, repeats):
    _integer(repeats, "repeats", positive=True)
    if repeats < 3:
        raise MeasurementError("At least three independent processes per condition are required")
    if len(set(apis)) != len(apis) or not apis or any(api not in ("vulkan", "d3d12") for api in apis):
        raise MeasurementError("Select distinct supported APIs")
    if (len(set(gpu_modes)) != len(gpu_modes) or not gpu_modes
            or any(mode not in ("off", "on") for mode in gpu_modes)):
        raise MeasurementError("Select distinct GPU timing modes off/on")
    conditions = [(api, mode) for mode in gpu_modes for api in apis]
    result = []
    for repetition in range(repeats):
        offset = repetition % len(conditions)
        rotated = conditions[offset:] + conditions[:offset]
        for api, mode in rotated:
            result.append({"api": api, "gpu_timing": mode,
                           "repetition": repetition + 1,
                           "run_id": f"{api}-gpu-{mode}-{repetition + 1}"})
    return result


def make_plan(args):
    _integer(args.frames, "frames", positive=True)
    _integer(args.warmup, "warmup", positive=True)
    _integer(args.inflight, "inflight", positive=True)
    _integer(args.width, "width", positive=True)
    _integer(args.height, "height", positive=True)
    if not 0 < args.timeout_seconds <= 3600:
        raise MeasurementError("timeout-seconds must be in (0,3600]")
    manifest, binary = verified_release(args.build_manifest)
    output = Path(args.output).resolve()
    if output.exists():
        raise MeasurementError("Output must be a new directory; stale run artifacts are not reused")
    if not args.fixture and not args.manifest:
        raise MeasurementError("Select a cooked manifest or the fixture")
    if not args.fixture and not args.ibl_manifest:
        raise MeasurementError("Full B3 measurements require --ibl-manifest")
    runs = schedule(args.apis, args.gpu_modes, args.repeats)
    for run in runs:
        command = [str(binary), "--api", run["api"], "--view", args.view,
                   "--width", str(args.width), "--height", str(args.height),
                   "--frames", str(args.frames), "--warmup", str(args.warmup),
                   "--inflight", str(args.inflight), "--output", str(output / run["run_id"])]
        command += ["--fixture"] if args.fixture else ["--manifest", str(Path(args.manifest).resolve())]
        if not args.fixture:
            command += ["--ibl-manifest", str(Path(args.ibl_manifest).resolve())]
        if run["gpu_timing"] == "on":
            command.append("--gpu-timing")
        run["command"] = command
    return {"schema_version": 1, "created_utc": utc(), "output": str(output),
            "cwd": str(Path(args.cwd).resolve()), "build_manifest": str(Path(args.build_manifest).resolve()),
            "build_manifest_sha256": digest(args.build_manifest), "binary": manifest["binary"],
            "source": manifest.get("git"), "frames": args.frames, "warmup": args.warmup,
            "inflight": args.inflight, "width": args.width, "height": args.height,
            "view": "fixture" if args.fixture else args.view, "fixture": args.fixture,
            "timeout_seconds": args.timeout_seconds, "runs": runs,
            "contract": "Serial independent normal-release processes; no build/cook/ETW in measurement; final capture outside target ROI; no global OS settings changed."}


def validate_report(report, condition, plan):
    """Reject partial/mislabeled timing instead of filling absent costs with zero."""
    if type(report.get("schema_version")) is not int or report["schema_version"] != 1:
        raise MeasurementError("Runner report schema_version must be 1")
    if report["runtime"]["api"] != condition["api"]:
        raise MeasurementError("Runner API differs from requested API")
    if report["runtime"].get("validation_mode") != "disabled":
        raise MeasurementError("Native validation must be explicitly disabled for normal timing")
    identity = report["identity"]
    for field in ("view", "width", "height", "inflight"):
        if identity[field] != plan[field]:
            raise MeasurementError(f"Runner identity {field} differs from requested input")
    measurement = report["measurement"]
    for field, requested in (("frames", "frames"), ("warmup_frames", "warmup"), ("inflight", "inflight")):
        _integer(measurement[field], field)
        if measurement[field] != plan[requested]:
            raise MeasurementError(f"Runner {field} differs from requested measurement")
    if measurement.get("readback_outside_measurement") is not True:
        raise MeasurementError("Final verification readback must be outside the measured window")
    frames = plan["frames"]
    wall = _integer(measurement["wall_ns"], "wall_ns", positive=True)
    cpu = measurement["cpu"]
    start = _integer(cpu["qpc_start"], "qpc_start")
    end = _integer(cpu["qpc_end"], "qpc_end")
    frequency = _integer(cpu["qpc_frequency"], "qpc_frequency", positive=True)
    ticks = _integer(cpu["qpc_elapsed_ticks"], "qpc_elapsed_ticks", positive=True)
    if end - start != ticks:
        raise MeasurementError("QPC elapsed ticks do not match recorded boundaries")
    qpc_ns = ticks * 1_000_000_000 / frequency
    if abs(qpc_ns - wall) > max(1_000_000, wall * 0.01):
        raise MeasurementError("QPC and wall measurement windows disagree")
    process_cpu = sum(_integer(cpu[f"process_{kind}_100ns"], f"process_{kind}_100ns")
                      for kind in ("kernel", "user")) * 100
    thread_cpu = sum(_integer(cpu[f"main_thread_{kind}_100ns"], f"main_thread_{kind}_100ns")
                     for kind in ("kernel", "user")) * 100
    phases = measurement["phase_totals_ns"]
    phase_metrics = {name + "_ns_per_frame": _integer(phases[name], name) / frames for name in PHASES}
    gpu = measurement["gpu"]
    expected_enabled = condition["gpu_timing"] == "on"
    if gpu.get("enabled") is not expected_enabled:
        raise MeasurementError("GPU query mode differs from the requested condition")
    gpu_mean = None
    if expected_enabled:
        if _integer(gpu["samples"], "gpu.samples") != frames:
            raise MeasurementError("GPU query results are missing; do not report them as zero")
        total = _number(gpu["total_ns"], "gpu.total_ns", positive=True)
        minimum = _number(gpu["min_ns"], "gpu.min_ns", positive=True)
        maximum = _number(gpu["max_ns"], "gpu.max_ns", positive=True)
        if not minimum <= total / frames <= maximum:
            raise MeasurementError("GPU mean lies outside reported min/max")
        gpu_mean = total / frames
    elif gpu.get("samples", 0) != 0:
        raise MeasurementError("Disabled GPU queries cannot contain measured samples")
    result = {"process_cpu_ns_per_frame": process_cpu / frames,
              "main_thread_cpu_ns_per_frame": thread_cpu / frames,
              "wall_ns_per_frame": wall / frames,
              "throughput_frames_per_second": frames * 1_000_000_000 / wall,
              "gpu_mean_ns_per_frame": gpu_mean, **phase_metrics}
    samples = measurement.get("frame_wall_ns")
    if samples is not None:
        if not isinstance(samples, list) or len(samples) != frames:
            raise MeasurementError("Frame latency sample count differs from measured frame count")
        checked = [_number(value, "frame_wall_ns", positive=True) for value in samples]
        scope = measurement.get("frame_wall_scope")
        if not isinstance(scope, str) or not scope:
            raise MeasurementError("Per-frame wall samples require an explicit measurement scope")
        result["individual_frame_wall_ns"] = {"samples": len(checked), "scope": scope,
            "p50": percentile(checked, 0.5), "p95": percentile(checked, 0.95),
            "minimum": min(checked), "maximum": max(checked)}
    else:
        result["individual_frame_wall_ns"] = None
    return result


def percentile(values, quantile):
    ordered = sorted(values)
    position = (len(ordered) - 1) * quantile
    first, last = math.floor(position), math.ceil(position)
    return ordered[first] + (ordered[last] - ordered[first]) * (position - first)


def aggregate(runs):
    result = {}
    for run in runs:
        key = run["api"] + "/gpu-" + run["gpu_timing"]
        result.setdefault(key, []).append(run["metrics"])
    summaries = {}
    for key, observations in result.items():
        metrics = {}
        for name, value in observations[0].items():
            if isinstance(value, dict) or value is None:
                continue
            values = [item[name] for item in observations]
            metrics[name] = {"values": values, "median_of_runs": statistics.median(values),
                             "minimum_run": min(values), "maximum_run": max(values)}
        summaries[key] = {"independent_processes": len(observations), "metrics": metrics,
                          "frame_wall_by_run": [item["individual_frame_wall_ns"] for item in observations]}
    return summaries


def query_cost_observations(summaries):
    """Ratios describe observed runs; they do not establish causal overhead."""
    result = {}
    for api in ("vulkan", "d3d12"):
        off, on = summaries.get(api + "/gpu-off"), summaries.get(api + "/gpu-on")
        if off is None or on is None:
            continue
        ratios = {}
        for metric in ("process_cpu_ns_per_frame", "main_thread_cpu_ns_per_frame", "wall_ns_per_frame"):
            baseline = off["metrics"][metric]["median_of_runs"]
            observed = on["metrics"][metric]["median_of_runs"]
            ratios[metric] = {"off_median": baseline, "on_median": observed,
                             "on_over_off_percent_change": (observed / baseline - 1) * 100 if baseline else None}
        result[api] = {"scope": "Descriptive ratios of independent-run medians for native CPU timers plus GPU timestamp queries; variability is retained; not a guaranteed or isolated causal query overhead.",
                       "metrics": ratios}
    return result


def run_batch(plan):
    output = Path(plan["output"])
    output.mkdir(parents=True, exist_ok=False)
    (output / "logs").mkdir()
    write_json(output / "plan.json", plan)
    summary = {"schema_version": 1, "status": "running", "started_utc": utc(),
               "plan_sha256": digest(output / "plan.json"), "runs": [],
               "scope": "Target steady-state CPU/GPU counters and target captures; controller duration includes setup/capture/teardown and is not Renderer CPU time."}
    write_json(output / "summary.json", summary)
    first_report = {}
    common_identity = None
    try:
        for condition in plan["runs"]:
            if (digest(plan["build_manifest"]) != plan["build_manifest_sha256"]
                    or digest(condition["command"][0]) != plan["binary"]["sha256"]):
                raise MeasurementError("Archived build identity changed after planning")
            record = {key: condition[key] for key in ("run_id", "api", "gpu_timing", "repetition")}
            summary["runs"].append(record)
            stdout_path = output / "logs" / (condition["run_id"] + ".stdout.txt")
            stderr_path = output / "logs" / (condition["run_id"] + ".stderr.txt")
            record.update({"status": "running", "started_utc": utc(),
                           "stdout": str(stdout_path), "stderr": str(stderr_path)})
            write_json(output / "summary.json", summary)
            started = time.perf_counter_ns()
            with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
                process = subprocess.Popen(condition["command"], cwd=plan["cwd"],
                    stdout=stdout, stderr=stderr,
                    creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
                record["pid"] = process.pid
                try:
                    record["returncode"] = process.wait(timeout=plan["timeout_seconds"])
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                    record["status"] = "timeout"
                    raise MeasurementError(f"Target timed out: {condition['run_id']}")
                except BaseException:
                    # Own only this process. Interruption must not leave a GPU
                    # workload running while later work begins.
                    if process.poll() is None:
                        process.kill()
                    process.wait()
                    raise
                finally:
                    record["controller_wall_ns"] = time.perf_counter_ns() - started
            if record["returncode"] != 0:
                raise MeasurementError(f"Target failed with exit {record['returncode']}: {condition['run_id']}")
            if "panicked at" in stderr_path.read_text(encoding="utf-8", errors="replace"):
                raise MeasurementError("Target emitted a Rust panic despite a successful exit code")
            report_path = output / condition["run_id"] / "report.json"
            if not report_path.is_file():
                raise MeasurementError("Target did not produce a fresh report.json")
            report = load_json(report_path)
            if common_identity is None:
                common_identity = report["identity"]
            elif report["identity"] != common_identity:
                raise MeasurementError("Rendering identity differs across measured API/query conditions")
            record["metrics"] = validate_report(report, condition, plan)
            # Final capture occurs after the ROI. Enforce content agreement for
            # repeated identical inputs, including query-enabled/disabled runs.
            reference = first_report.setdefault(condition["api"], report_path)
            comparison = compare_reports(reference, report_path, profile="repeat")
            record["final_capture_consistency"] = comparison
            if not comparison["passed"]:
                raise MeasurementError("Final capture changed between repeated identical inputs")
            record["report_sha256"] = digest(report_path)
            record["report"] = str(report_path)
            record["status"] = "passed"
            record["finished_utc"] = utc()
            write_json(output / "summary.json", summary)
        summary["aggregates"] = aggregate(summary["runs"])
        summary["gpu_query_cost_observations"] = query_cost_observations(summary["aggregates"])
        summary["status"] = "passed"
    except (Exception, KeyboardInterrupt) as error:
        summary["status"] = "failed"
        summary["failure"] = f"{type(error).__name__}: {error}"
        if summary["runs"] and summary["runs"][-1]["status"] == "running":
            summary["runs"][-1]["status"] = "failed"
        raise
    finally:
        summary["finished_utc"] = utc()
        write_json(output / "summary.json", summary)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("plan", "run"))
    parser.add_argument("--build-manifest", type=Path, required=True)
    asset = parser.add_mutually_exclusive_group(required=True)
    asset.add_argument("--manifest", type=Path)
    asset.add_argument("--fixture", action="store_true")
    parser.add_argument("--ibl-manifest", type=Path)
    parser.add_argument("--apis", nargs="+", default=["vulkan", "d3d12"])
    parser.add_argument("--gpu-modes", nargs="+", choices=("off", "on"), default=["off", "on"])
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--view", default="street")
    parser.add_argument("--width", type=int, default=1920)
    parser.add_argument("--height", type=int, default=1080)
    parser.add_argument("--frames", type=int, default=1800)
    parser.add_argument("--warmup", type=int, default=3600)
    parser.add_argument("--inflight", type=int, default=2)
    parser.add_argument("--timeout-seconds", type=float, default=180)
    parser.add_argument("--cwd", type=Path, default=ROOT)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        plan = make_plan(args)
        if args.action == "plan":
            print(json.dumps(plan, indent=2, ensure_ascii=False, allow_nan=False))
        else:
            result = run_batch(plan)
            print(json.dumps({"status": result["status"], "runs": len(result["runs"]),
                              "summary": str(Path(plan["output"]) / "summary.json")}))
    except (ValueError, KeyError, OSError) as error:
        print(f"Measurement failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
