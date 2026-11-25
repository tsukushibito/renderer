"""Compare headless raster throughput for a fixed draw count and different job layouts."""

import argparse
import hashlib
import itertools
import json
from pathlib import Path
import statistics
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "bistro"))
import host_policy


# (passes, batches per pass, Graph dependency)
CONDITIONS = {
    "single": (1, 1, "independent"),
    "independent": (4, 1, "independent"),
    "chain": (4, 1, "chain"),
    "in-pass": (1, 4, "independent"),
    "mixed": (4, 4, "independent"),
    "in-pass-16": (1, 16, "independent"),
}


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, allow_nan=False) + "\n", encoding="utf-8")


def make_plan(args):
    profiles = {"both": [False, True], "off": [False], "on": [True]}[args.profile]
    groups = list(itertools.product(args.apis, args.draws, args.words, args.conditions, profiles))
    teams = list(itertools.product(args.workers, args.native_workers))
    runs = []
    for repeat in range(args.repeats):
        offset = repeat % len(teams)
        # Rotate worker counts within matched groups. Reverse group order every other run.
        for api, draws, words, condition, profile in (reversed(groups) if repeat % 2 else groups):
            passes, batches, topology = CONDITIONS[condition]
            for workers, native_workers in teams[offset:] + teams[:offset]:
                mode = "profile" if profile else "ordinary"
                group = f"{api}-{draws}-{words}-{condition}-w{workers}-n{native_workers}-{mode}"
                runs.append({
                    "group": group, "name": f"{group}-r{repeat + 1}", "api": api,
                    "draws": draws, "words": words, "passes": passes, "batches_per_pass": batches,
                    "topology": topology, "workers": workers, "native_workers": native_workers, "profile": profile,
                    "frames": args.frames, "warmup": args.warmup,
                })
    return runs


def check_device_and_build(run, report, devices):
    validation = report["validation"]
    if validation["clean"] is not True or validation["strict"] is not False:
        raise ValueError(f"{run['name']}: performance measurements require a clean shipping configuration")
    device = report["device"]
    if (device["api"] != run["api"] or not device["device"]
            or device != devices.setdefault(run["api"], device)):
        raise ValueError(f"{run['name']}: unexpected device identity {device}")


def run_process(exe, root, run, policy, devices):
    destination = root / run["name"]
    destination.mkdir()
    # Publish the report only after the measured process has exited successfully.
    pending = destination / "pending.json"
    command = [str(exe)]
    for field in ("api", "draws", "words", "passes", "batches_per_pass", "topology",
                  "workers", "warmup", "frames"):
        command.extend(["--" + field.replace("_", "-"), str(run[field])])
    command.extend(["--native-workers", str(run["native_workers"])])
    command.extend(["--report", str(pending)])
    if run["profile"]:
        command.append("--profile")
    print(run["name"], flush=True)
    with (destination / "process.log").open("w", encoding="utf-8") as log, subprocess.Popen(
        command, stdout=log, stderr=subprocess.STDOUT, cwd=ROOT,
        creationflags=subprocess.CREATE_NO_WINDOW | 0x4,  # CREATE_SUSPENDED
    ) as process:
        try:
            observed = host_policy.apply(process.pid, policy)
            result = process.wait(timeout=900)
        except BaseException:
            if process.poll() is None:
                process.kill()
            process.wait()
            raise
    write_json(destination / "process.json", {"command": command, "cpu_policy": observed, "exit": result})
    if result:
        raise RuntimeError(f"{run['name']} failed with exit code {result}: {destination / 'process.log'}")
    check_device_and_build(run, json.loads(pending.read_text(encoding="utf-8")), devices)
    pending.rename(destination / "report.json")


def run_metrics(run, report):
    for field in ("draws", "words", "passes", "batches_per_pass", "topology", "workers",
                  "warmup", "frames", "profile"):
        if report[field] != run[field]:
            raise ValueError(f"{run['name']}: expected {field}={run[field]}, got {report[field]}")
    if report.get("native_workers", 0) != run.get("native_workers", 0):
        raise ValueError(f"{run['name']}: native recording team mismatch")
    samples = report["samples"]
    if len(samples) != run["frames"] or report["wall_ns"] <= 0 or samples[0]["interval_ns"] != 0:
        raise ValueError(f"{run['name']}: unexpected measurement window or samples")
    # The first measured frame has no preceding start. Use inclusive linear percentiles.
    intervals = [s["interval_ns"] / 1e6 for s in samples[1:]]
    percentiles = statistics.quantiles(intervals, n=100, method="inclusive")
    metrics = {
        "completed_fps": run["frames"] * 1e9 / report["wall_ns"],
        "interval_p50_ms": statistics.median(intervals), "interval_p95_ms": percentiles[94],
        "interval_p99_ms": percentiles[98],
    }
    phases = ["active", "build", "compile"]
    if run["workers"]:
        phases.append("packet_prepare")
    if run["profile"]:
        phases.extend(["prepare", "record", "submit", "gpu"])
    for phase in phases:
        values = [s[phase + "_ns"] for s in samples]
        if any(type(value) is not int or value < 0 for value in values):
            raise ValueError(f"{run['name']}: missing or invalid {phase} timing")
        metrics[phase + "_p50_ms"] = statistics.median(values) / 1e6
    # Subtract nested timers per frame before taking the median. Direct draws
    # are generated inside the Graph builder and have no separate packet phase.
    remaining = [s["build_ns"] - (s["packet_prepare_ns"] or 0) for s in samples]
    if min(remaining) < 0:
        raise ValueError(f"{run['name']}: packet preparation exceeds its enclosing build time")
    metrics["build_excluding_packet_p50_ms"] = statistics.median(remaining) / 1e6
    return metrics


def summarize(output):
    plan = json.loads((output / "plan.json").read_text(encoding="utf-8"))
    groups = {}
    devices = {}
    completed = 0
    for run in plan["runs"]:
        path = output / run["name"] / "report.json"
        if path.is_file():
            report = json.loads(path.read_text(encoding="utf-8"))
            check_device_and_build(run, report, devices)
            metrics = run_metrics(run, report)
            groups.setdefault(run["group"], []).append({"name": run["name"], **metrics})
            completed += 1
    results = {}
    for group, runs in groups.items():
        results[group] = {
            "runs": runs,
            "median_of_runs": {key: statistics.median(run[key] for run in runs)
                               for key in runs[0] if key != "name"},
        }
    write_json(output / "summary.json", {
        "planned_runs": len(plan["runs"]), "completed_runs": completed, "devices": devices, "groups": results,
        "completed_fps_scope": "Measured frames divided by elapsed time including final GPU drain; headless.",
    })
    print(f"{completed}/{len(plan['runs'])} processes completed. {output / 'summary.json'}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--summarize", action="store_true", help="only aggregate reports from an existing run")
    parser.add_argument("--apis", nargs="+", choices=("vulkan", "d3d12"), default=["vulkan", "d3d12"])
    parser.add_argument("--draws", nargs="+", type=int, choices=(1024, 10000, 50000), default=[50000])
    parser.add_argument("--words", nargs="+", type=int, choices=(0, 20), default=[0, 20])
    parser.add_argument("--conditions", nargs="+", choices=CONDITIONS, default=list(CONDITIONS))
    parser.add_argument("--workers", nargs="+", type=int, default=[0, 1, 4],
                        help="0 direct, 1 serial packets, >1 participants including caller")
    parser.add_argument("--native-workers", nargs="+", type=int, default=[0], help="0 direct, 1 serial native batches, >1 native recording team size")
    parser.add_argument("--warmup", type=int, default=120)
    parser.add_argument("--frames", type=int, default=600)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--profile", choices=("off", "on", "both"), default="both")
    args = parser.parse_args()
    output = args.output.resolve()
    if args.summarize:
        summarize(output)
        return
    if not args.exe or args.warmup < 0 or args.frames < 3 or args.repeats < 1 or min(args.workers + args.native_workers) < 0:
        parser.error("supply --exe, frames >= 3, repeats >= 1 and nonnegative workers and warmup")
    for values in (args.apis, args.draws, args.words, args.conditions, args.workers, args.native_workers):
        if len(set(values)) != len(values):
            parser.error("repeated values would duplicate a measurement condition")
    exe = args.exe.resolve(strict=True)
    policy = host_policy.plan("performance-cores")
    runs = make_plan(args)
    output.mkdir(parents=True, exist_ok=False)
    with exe.open("rb") as binary:
        digest = hashlib.file_digest(binary, "sha256").hexdigest()
    write_json(output / "plan.json", {"exe": str(exe), "sha256": digest, "cpu_policy": policy, "runs": runs})
    devices = {}
    try:
        for run in runs:
            run_process(exe, output, run, policy, devices)
    finally:
        summarize(output)


if __name__ == "__main__":
    main()
