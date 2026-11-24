"""Measure the presented survivor game in separate ordinary and GPU-timestamp runs."""

import argparse
import csv
import ctypes
from ctypes import wintypes
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import statistics
import subprocess
import time


ROOT = Path(__file__).resolve().parents[2]
CPU = ("interval_ms", "update_ms", "draw_ms")
GPU = ("graph_gpu_ms", "present_gpu_ms")
NATIVE_CPU = ("graph_prepare_ms", "graph_record_ms", "graph_submit_ms", "graph_retire_ms")
COUNTS = ("active", "visible", "drawn", "shots", "drops", "pulse", "rapid_level", "pierce_level", "pulse_level")


def distribution(values):
    ordered = sorted(values)
    def percentile(q):
        rank = (len(ordered) - 1) * q
        lower = math.floor(rank)
        upper = math.ceil(rank)
        return ordered[lower] + (ordered[upper] - ordered[lower]) * (rank - lower)
    return {"min": ordered[0], "median": statistics.median(ordered), "p95": percentile(.95),
            "p99": percentile(.99), "max": ordered[-1]}


def summarize(csv_path, frames, gpu_profile, scenario):
    with csv_path.open(newline="", encoding="utf-8") as stream:
        rows = list(csv.DictReader(stream))
    if len(rows) != frames:
        raise ValueError(f"{csv_path}: expected {frames} presented frames, got {len(rows)}")
    frame_ids = [int(row["frame"]) for row in rows]
    if any(b != a + 1 for a, b in zip(frame_ids, frame_ids[1:])):
        raise ValueError(f"{csv_path}: nonconsecutive frame numbers")
    counts = {key: [int(row[key]) for row in rows] for key in COUNTS}
    workload = hashlib.sha256()
    for row in rows:
        # Include the entire measured activity trace, without hardware timings.
        trace = [int(row[key]) for key in ("tick", *COUNTS)]
        workload.update((",".join(map(str, trace)) + "\n").encode("ascii"))
    for active, visible, drawn in zip(counts["active"], counts["visible"], counts["drawn"]):
        if not 0 <= drawn <= active or not 0 <= visible <= active:
            raise ValueError(f"{csv_path}: invalid enemy counts")
    if scenario in ("endgame", "overlap") and any(value != 500 for value in counts["active"]):
        raise ValueError(f"{csv_path}: measured segment must sustain 500 live enemies")
    overlap = sum(int(row["shots"]) > 1 and int(row["pulse"]) == 1 for row in rows)
    if scenario == "overlap" and (not overlap or min(counts["rapid_level"]) < 5 or min(counts["pierce_level"]) < 2):
        raise ValueError(f"{csv_path}: did not sustain the earned upgrades or show overlapping attacks")
    timings = {}
    for key in (*CPU, *GPU, *NATIVE_CPU):
        if key not in CPU and not gpu_profile:
            if any(row[key].strip() for row in rows):
                raise ValueError(f"{csv_path}: native timing populated in an unprofiled run")
            continue
        values = [float(row[key]) for row in rows]
        if any(not math.isfinite(value) or value < 0 for value in values):
            raise ValueError(f"{csv_path}: invalid {key}")
        timings[key] = distribution(values)
    return {"timings": timings, "counts": {key: distribution(values) for key, values in counts.items()},
            "shot_and_pulse_frames": overlap, "workload_sha256": workload.hexdigest()}


class ProcessMemoryCounters(ctypes.Structure):
    _fields_ = [("cb", wintypes.DWORD), ("page_faults", wintypes.DWORD),
                ("peak_working_set", ctypes.c_size_t), ("working_set", ctypes.c_size_t),
                ("peak_paged_pool", ctypes.c_size_t), ("paged_pool", ctypes.c_size_t),
                ("peak_nonpaged_pool", ctypes.c_size_t), ("nonpaged_pool", ctypes.c_size_t),
                ("pagefile", ctypes.c_size_t), ("peak_pagefile", ctypes.c_size_t)]


def measured_process(command, log):
    """Sample native process-lifetime peaks, including load, warmup and play."""
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    query = kernel.K32GetProcessMemoryInfo
    query.argtypes = [wintypes.HANDLE, ctypes.POINTER(ProcessMemoryCounters), wintypes.DWORD]
    query.restype = wintypes.BOOL
    peak = 0
    with log.open("w", encoding="utf-8") as stream, subprocess.Popen(
        command, cwd=ROOT, stdout=stream, stderr=subprocess.STDOUT
    ) as process:
        try:
            while process.poll() is None:
                counters = ProcessMemoryCounters()
                counters.cb = ctypes.sizeof(counters)
                if query(int(process._handle), ctypes.byref(counters), counters.cb):
                    peak = max(peak, counters.peak_working_set)
                elif process.poll() is None:
                    raise ctypes.WinError(ctypes.get_last_error())
                time.sleep(.25)
        except BaseException:
            process.kill()
            raise
        if process.returncode:
            raise RuntimeError(f"Game exited {process.returncode}; inspect {log}")
    if not peak:
        raise RuntimeError(f"No working-set sample for {log}")
    return peak


def run(args):
    exe = args.exe.resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    gpu_info = subprocess.check_output([
        "nvidia-smi", "--query-gpu=name,driver_version,memory.total", "--format=csv,noheader"
    ], text=True).strip()
    with exe.open("rb") as binary:
        executable_digest = hashlib.file_digest(binary, "sha256").hexdigest()
    scenarios = ["early", "endgame"]
    overlap_args = []
    if args.overlap_replay:
        replay = args.overlap_replay.resolve(strict=True)
        with replay.open(encoding="utf-8") as stream:
            tape = json.load(stream)
        inputs = tape if isinstance(tape, list) else tape["inputs"]
        start = len(inputs) - 720
        if args.seed != 506 or start < 0 or args.frames + args.warmup > 720:
            raise ValueError("The generated overlap replay requires seed 506 and at most 720 presented frames")
        overlap_args = ["--replay", str(replay), "--replay-from", str(start)]
        scenarios.append("overlap")
    summary = {
        "revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "executable_sha256": executable_digest,
        "os": platform.platform(), "processor": platform.processor(), "gpu_inventory": gpu_info,
        "backend": args.backend, "seed": args.seed, "extent": [1920, 1080],
        "warmup_presented": args.warmup, "measured_presented": args.frames,
        "vsync": False, "frame_budget_ms": 1000 / 60,
        "expected_runs": len(scenarios) * 2 * args.repeats,
        "runs": [],
    }
    if overlap_args:
        summary["replay"] = {"prefix": start, "sha256": hashlib.sha256(replay.read_bytes()).hexdigest()}
    summary_path = output / "summary.json"
    if summary_path.exists():
        raise FileExistsError(f"Choose a new output directory for this experiment: {output}")
    workloads = {}
    for repetition in range(1, args.repeats + 1):
        for gpu_profile in (False, True):
            for scenario in scenarios:
                mode = "timestamps" if gpu_profile else "ordinary"
                stem = f"{scenario}-{mode}-{repetition}"
                csv_path = output / f"{stem}.csv"
                if csv_path.exists():
                    raise FileExistsError(csv_path)
                command = [str(exe), "--window", "--scenario", "early" if scenario == "overlap" else scenario, "--seed", str(args.seed),
                           "--backend", args.backend, "--width", "1920", "--height", "1080",
                           "--warmup", str(args.warmup), "--frames", str(args.frames),
                           "--metrics", str(csv_path)]
                if scenario == "overlap":
                    command.extend(overlap_args)
                if gpu_profile:
                    command.append("--gpu-profile")
                print(stem, flush=True)
                peak = measured_process(command, output / f"{stem}.log")
                stats = summarize(csv_path, args.frames, gpu_profile, scenario)
                digest = stats["workload_sha256"]
                if digest != workloads.setdefault(scenario, digest):
                    raise ValueError(f"{stem}: game activity differs from the first {scenario} run")
                summary["runs"].append({"scenario": scenario, "mode": mode, "repetition": repetition,
                                        "csv": csv_path.name, "command": command,
                                        "sampled_peak_process_working_set_bytes": peak, **stats})
                summary_path.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    if overlap_args:
        # Query memory outside the timed launches. Keep capacities separate from live usage.
        memory = output / "overlap-memory.json"
        command = [str(exe), "--window", "--backend", args.backend, "--scenario", "early",
                   "--width", "1920", "--height", "1080", "--warmup", str(args.warmup),
                   "--frames", str(args.frames), "--memory-samples", str(memory), *overlap_args]
        measured_process(command, output / "overlap-memory.log")
        summary["memory_snapshots"] = json.loads(memory.read_text(encoding="utf-8"))
        summary_path.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(summary_path)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--overlap-replay", type=Path, help="Recording produced by the overlap_replay example")
    parser.add_argument("--backend", choices=["vulkan", "d3d12"], default="vulkan")
    parser.add_argument("--seed", type=int, default=506)
    parser.add_argument("--warmup", type=int, default=120)
    parser.add_argument("--frames", type=int, default=600)
    parser.add_argument("--repeats", type=int, default=3)
    options = parser.parse_args()
    if os.name != "nt":
        parser.error("This benchmark requires the Windows display host")
    if min(options.warmup, options.frames, options.repeats) < 1:
        parser.error("warmup, frames and repeats must be positive")
    run(options)
