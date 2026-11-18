"""Measure the integrated Bistro scene on an ordinary Windows host."""

import argparse
from pathlib import Path
import statistics
import subprocess
import time

from b3_benchmark import distribution, observe_target, ordinary_host_token, renderer_metrics
from compare_runtime import digest, load_json
import host_policy
from resource_acceptance import require, write_json


APIS = ("vulkan", "d3d12")
SCENARIOS = (("offscreen", False), ("offscreen", True), ("window-vsync", True),
             ("window-tearing", True))
FRAMES = {"view": "route", "width": 1920, "height": 1080, "frames": 5400,
          "warmup": 3600, "inflight": 2}
WINDOW_FRAMES = {**FRAMES, "frames": 600, "warmup": 120}


def series(values):
    require(all(type(value) is int and value >= 0 for value in values), "Invalid raw timing")
    return distribution([value / 1e6 for value in values])


def adapter_state():
    command = ["nvidia-smi",
               "--query-gpu=name,temperature.gpu,clocks.current.graphics,clocks.current.memory",
               "--format=csv,noheader"]
    raw = subprocess.check_output(command, text=True, timeout=10)
    matches = [line.split(", ") for line in raw.splitlines() if "RTX 3060" in line]
    require(len(matches) == 1, "The measurement host must expose one RTX 3060")
    fields = ("adapter", "temperature_c", "graphics_clock", "memory_clock")
    return dict(zip(fields, matches[0], strict=True))


def summarize_window(report, frames):
    count = frames["frames"]
    stats = report["timing"]
    require(report["presentation"] and report["warmup"] == frames["warmup"]
            and report["accepted"] == count + report["warmup"],
            "Incomplete Window run")
    require((report["initial_width"], report["initial_height"]) ==
            (frames["width"], frames["height"]), "Window output dimensions changed")
    require(report["pending_on_exit"] > 0, "Window did not exercise pending shutdown")
    require(len(stats["frame_intervals_ns"]) == len(stats["frame_phases_ns"]) == count,
            "Incomplete caller Window timings")
    require(len(stats["graph_samples"]) == count and len(stats["present_samples"]) >= count,
            "Incomplete native Window timings")
    result = {"frame_interval_ms": series(stats["frame_intervals_ns"]),
              "mean_wall_ms": stats["wall_ns"] / 1e6 / count,
              "present_calls": stats["present_calls"],
              "mean_present_call_ms": stats["present_elapsed_ns"] / 1e6 / stats["present_calls"],
              "poll_per_frame_ms": stats["graph_poll_elapsed_ns"] / 1e6 / count,
              "cold_phases_ns": report["setup_phases_ns"],
              "native_memory_at_drain": report["native_memory"]}
    for index, phase in enumerate(stats["phase_order"]):
        result[phase + "_ms"] = series([sample[index] for sample in stats["frame_phases_ns"]])
    for group in ("graph", "present"):
        samples = stats[group + "_samples"]
        result[group] = {key.removesuffix("_ns") + "_ms": series([item[key] for item in samples])
                         for key in samples[0]}
    return result


def run(args, policy, api, scenario, timed, destination):
    windowed = scenario != "offscreen"
    frames = WINDOW_FRAMES if windowed else FRAMES
    command = [str(args.binary), "--api", api, "--manifest", str(args.manifest), "--ibl-manifest",
               str(args.ibl_manifest), "--bloom", "tent", "--output", str(destination)]
    for key, value in frames.items():
        command += [f"--{key}", str(value)]
    if timed:
        command += ["--gpu-timing"]
    if windowed:
        command += ["--windowed", "--vsync", "on" if scenario == "window-vsync" else "off"]
    else:
        command += ["--steady-state-cpu"]
    destination.mkdir(exist_ok=False)
    write_json(destination / "command.json", command)
    before = adapter_state()
    started = time.monotonic_ns()
    with (destination / "stdout.log").open("wb") as out, (destination / "stderr.log").open("wb") as err:
        # Start suspended so affinity and QoS are confirmed before any setup or timing.
        process = subprocess.Popen(command, stdout=out, stderr=err,
                                   creationflags=4 | subprocess.CREATE_NO_WINDOW)
        try:
            token = observe_target(process.pid, policy)
            status = process.wait(timeout=900)
        except BaseException:
            if process.poll() is None:
                process.kill()
            process.wait()
            raise
    require(status == 0, f"{destination.name} failed with status {status}")
    elapsed = time.monotonic_ns() - started
    after = adapter_state()
    path = destination / "report.json"
    report = load_json(path)
    require(report["runtime"]["api"] == api and report["validation"]["mode"] == "disabled"
            and report["validation"]["clean"], f"Wrong runtime or diagnostics {destination}")
    if windowed:
        require(report["bloom"] == "tent" and report["vsync"] == (scenario == "window-vsync"),
                "Window quality or pacing request changed")
        metrics = summarize_window(report, frames)
    else:
        require(report["identity"]["bloom"] == "tent", "Offscreen quality changed")
        metrics = renderer_metrics(report, {"variant": api, "timing": timed}, FRAMES)
        metrics["cold_phases_ns"] = report["setup_phases_ns"]
        metrics["native_memory_at_boundaries"] = report["native_memory_at_boundaries"]
    return {"report": str(path), "report_sha256": digest(path), "process_elapsed_ns": elapsed,
            "token": token, "before": before, "after": after,
            "metrics": metrics, "runtime": report["runtime"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "manifest", "ibl-manifest", "output"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    parser.add_argument("--resume", action="store_true", help="reuse validated completed runs")
    args = parser.parse_args()
    ordinary_host_token()
    policy = host_policy.plan("performance-cores")
    settings = {"offscreen": FRAMES, "window": WINDOW_FRAMES}
    identity = {name: digest(getattr(args, name)) for name in ("binary", "manifest", "ibl_manifest")}
    progress = args.output / "progress.json"
    if args.resume:
        result = load_json(progress)
        require(result["identity"] == identity and result["settings"] == settings
                and result["cpu_policy"] == policy, "Resume identity differs")
        for run_id, record in result["runs"].items():
            report = Path(record["report"])
            require(report.resolve().is_relative_to(args.output.resolve())
                    and digest(report) == record["report_sha256"], f"Stored report differs: {run_id}")
    else:
        args.output.mkdir(exist_ok=False)
        power_scheme = subprocess.check_output(["powercfg", "/getactivescheme"]).decode("oem").strip()
        result = {"identity": identity, "settings": settings, "cpu_policy": policy,
                  "power_scheme": power_scheme, "runs": {}}
    write_json(progress, result)
    runs = result["runs"]
    for scenario, timed in SCENARIOS:
        for repeat in range(1, 4):
            for api in APIS if repeat % 2 else reversed(APIS):
                label = f"{scenario}-{'timed' if timed else 'normal'}-{api}-{repeat}"
                if label in runs:
                    continue
                print(label, flush=True)
                destination = args.output / label
                attempt = 1
                while destination.exists():
                    attempt += 1
                    destination = args.output / f"{label}-attempt{attempt}"
                runs[label] = run(args, policy, api, scenario, timed, destination)
                write_json(progress, result)
    result["median_window_wall_ms"] = {
        f"{scenario}/{api}": statistics.median(
            record["metrics"]["mean_wall_ms"] for label, record in runs.items()
            if label.startswith(scenario) and f"-{api}-" in label)
        for scenario in ("window-vsync", "window-tearing") for api in APIS
    }
    require({name: digest(getattr(args, name)) for name in identity} == identity,
            "Benchmark executable or input identity changed")
    write_json(args.output / "summary.json", result)


if __name__ == "__main__":
    main()
