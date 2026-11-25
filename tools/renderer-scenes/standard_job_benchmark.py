"""Run matched StandardPipeline frame-throughput, allocation, and parity comparisons on Windows."""

import argparse
import hashlib
import itertools
import json
import statistics
import subprocess
import sys
from pathlib import Path

from raster_job_benchmark import ROOT, check_device_and_build, host_policy, write_json

STAGES = {"serial": (), "graph": ("graph",), "scene": ("scene",), "both": ("scene", "graph")}
WORKLOADS = ("opaque", "textured", "blend", "shadow-static", "shadow-dynamic")
PHASES = ("build", "compile", "input_update", "prepare", "record", "submit", "retire", "gpu")


def measurements(exe, args, output, policy):
    groups = list(itertools.product(args.apis, args.objects, args.visible, args.workloads))
    for repeat in range(args.repeats):
        start = repeat % len(args.stages)
        for api, objects, visible, workload in groups[::1 if repeat % 2 == 0 else -1]:
            for stage in args.stages[start:] + args.stages[:start]:
                key = f"{api}-{objects}-{visible}-{workload}"
                name = f"{key}-{stage}-r{repeat + 1}"
                folder = output / name
                folder.mkdir()
                command = [str(exe), "--api", api, "--objects", str(objects), "--visible", str(visible),
                           "--workload", workload, "--warmup", str(args.warmup), "--frames", str(args.frames),
                           "--extent", args.extent, "--participants", str(args.participants),
                           "--report", str(folder / "report.json")]
                command += [f"--{s}-jobs" for s in STAGES[stage]]
                if args.native:
                    command.append("--native-jobs")
                if args.profile:
                    command.append("--profile")
                if args.capture:
                    command.append("--capture")
                print(name, flush=True)
                with (folder / "process.log").open("w", encoding="utf-8") as log, subprocess.Popen(
                    command, stdout=log, stderr=subprocess.STDOUT, cwd=ROOT,
                    creationflags=subprocess.CREATE_NO_WINDOW | 0x4,
                ) as process:
                    try:
                        observed = host_policy.apply(process.pid, policy)
                        code = process.wait(timeout=900)
                    except BaseException:
                        process.kill()
                        process.wait()
                        raise
                write_json(folder / "process.json", {"command": command, "cpu_policy": observed, "exit": code})
                if code:
                    raise RuntimeError(f"{name} exited with {code}; see {folder / 'process.log'}")
                report = json.loads((folder / "report.json").read_text(encoding="utf-8"))
                expected = dict(api=api, objects=objects, visible=visible, workload=workload,
                                profile=args.profile, frames=args.frames, warmup=args.warmup, in_flight=2,
                                extent=list(map(int, args.extent.split("x"))), native_jobs=args.native,
                                scene_jobs="scene" in STAGES[stage], graph_jobs="graph" in STAGES[stage],
                                participants=args.participants)
                if any(report[k] != value for k, value in expected.items()):
                    raise ValueError(f"{name} does not match the requested input")
                yield key, stage, name, report


def metrics(report):
    samples = report["samples"]
    if len(samples) != report["frames"] or samples[0]["interval_ns"] != 0:
        raise ValueError("invalid frame samples")
    intervals = [s["interval_ns"] / 1e6 for s in samples[1:]]
    q = statistics.quantiles(intervals, n=100, method="inclusive")
    values = {"completed_fps": len(samples) * 1e9 / report["wall_ns"],
              "interval_p95_ms": q[94], "interval_p99_ms": q[98]}
    for phase in PHASES:
        ns = [s[phase + "_ns"] for s in samples]
        if any(v is not None for v in ns):
            values[phase + "_p50_ms"] = statistics.median(ns) / 1e6
    return values


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--apis", nargs="+", choices=("vulkan", "d3d12"), default=["vulkan", "d3d12"])
    parser.add_argument("--objects", nargs="+", type=int, choices=(1024, 10000, 50000), default=[50000])
    parser.add_argument("--visible", nargs="+", type=int, choices=range(101), default=[10, 50, 100])
    parser.add_argument("--workloads", nargs="+", choices=WORKLOADS, default=["opaque"])
    parser.add_argument("--stages", nargs="+", choices=STAGES, default=list(STAGES))
    parser.add_argument("--participants", type=int, default=4)
    parser.add_argument("--extent", default="512x512")
    parser.add_argument("--native", action="store_true", help="request native batch recording in all compared stages")
    parser.add_argument("--profile", action="store_true")
    parser.add_argument("--capture", action="store_true", help="compare two frames of SDR/HDR/depth/shadows after timing")
    parser.add_argument("--memory", action="store_true", help="require zero steady-state events from a memory-profiling binary")
    parser.add_argument("--strict", action="store_true", help="require a development-validation binary for correctness")
    parser.add_argument("--frames", type=int, default=600)
    parser.add_argument("--warmup", type=int, default=120)
    parser.add_argument("--repeats", type=int, default=3)
    args = parser.parse_args()
    if min(args.repeats, args.participants) < 1 or args.frames < 3 or args.warmup < 0:
        parser.error("expected positive participants/repeats, >=3 frames and nonnegative warmup")
    if args.capture and "serial" not in args.stages:
        parser.error("captures require the serial reference")
    for items in (args.apis, args.objects, args.visible, args.workloads, args.stages):
        if len(set(items)) != len(items):
            parser.error("a comparison condition is repeated")
    exe = args.exe.resolve(strict=True)
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    policy = host_policy.plan("performance-cores")
    with exe.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    write_json(out / "plan.json", {"exe": str(exe), "sha256": digest, "cpu_policy": policy,
                                  "options": {k: v for k, v in vars(args).items() if k not in ("exe", "output")}})
    groups = {}
    devices = {}
    captures = {}
    for key, stage, name, report in measurements(exe, args, out, policy):
        if not args.strict:
            check_device_and_build({"api": report["api"], "name": name}, report, devices)
        elif (report["validation"] != {"clean": True, "strict": True}
              or devices.setdefault(report["api"], report["device"]) != report["device"]):
            raise ValueError(f"{name}: unexpected validation or device")
        if args.capture:
            if not report["capture"] or captures.setdefault(key, report["capture"]) != report["capture"]:
                raise ValueError(f"{name}: readback mismatch")
        if args.memory and not (report["memory"] and report["memory"]["allocation_free"]):
            raise ValueError(f"{name}: nonzero or unmeasured memory events: {report['memory']}")
        groups.setdefault(key, {}).setdefault(stage, []).append({"name": name, **metrics(report)})
    results = {}
    for key, stages in groups.items():
        results[key] = {}
        for stage, runs in stages.items():
            median = {k: statistics.median(row[k] for row in runs) for k in runs[0] if k != "name"}
            results[key][stage] = {"runs": runs, "median": median}
    write_json(out / "summary.json", {"devices": devices, "groups": results, "captures": captures,
                                      "fps_scope": "Frames divided by wall time including final GPU drain."})


if __name__ == "__main__":
    main()
