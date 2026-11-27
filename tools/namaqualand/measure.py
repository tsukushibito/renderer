"""Measure matching WSI routes and optionally record native fixed-time route videos."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time

import numpy as np


def highest_package(package, output):
    """Reuse immutable payloads and remove only the alternate geometry bindings."""
    manifest = json.loads((package / "scene.json").read_bytes())
    if not manifest.pop("mesh_lods"):
        raise ValueError("scene has no reduced geometry")
    def blobs(value):
        if isinstance(value, list):
            for item in value:
                yield from blobs(item)
        elif isinstance(value, dict):
            if value.keys() == {"hash", "size"}:
                yield value["hash"]
            else:
                for item in value.values():
                    yield from blobs(item)
    objects = output / "objects"
    objects.mkdir(parents=True, exist_ok=True)
    for digest in set(blobs(manifest)):
        target = objects / digest
        if not target.exists():
            os.link(package / "objects" / digest, target)
    (output / "scene.json").write_text(json.dumps(manifest), encoding="utf8")


def benchmark(exe, package, route, output, profile):
    import ctypes
    import psutil
    import win32pdh as pdh
    env = dict(os.environ)
    if profile:
        env["SASHIKO_ROUTE_PROFILE"] = "1"
    else:
        env.pop("SASHIKO_ROUTE_PROFILE", None)
    t0 = time.monotonic()
    process = subprocess.Popen([exe, package, "rock", "--benchmark-route", route, output], env=env)
    memory = []
    gpu_samples = []
    query = pdh.OpenQuery()
    counter = pdh.AddEnglishCounter(query, r'\GPU Process Memory(*)\Dedicated Usage')
    proc = psutil.Process(process.pid)
    # Keep the display available during an unattended WSI run. This request
    # belongs to the measuring thread and is released when the run ends.
    kernel = ctypes.windll.kernel32
    kernel.SetThreadExecutionState(0x80000003)
    try:
        while process.poll() is None:
            try:
                rss = proc.memory_info().rss
            except psutil.NoSuchProcess:
                break
            card = subprocess.check_output(["nvidia-smi", "--query-gpu=memory.used,utilization.gpu,clocks.gr,temperature.gpu,pstate",
                                            "--format=csv,noheader,nounits"], text=True)
            used, util, clock, temp, state = [field.strip() for field in card.splitlines()[0].split(",")]
            gpu_samples.append([time.monotonic() - t0, int(util), int(clock), int(temp), state])
            pdh.CollectQueryData(query)
            samples = pdh.GetFormattedCounterArray(counter, pdh.PDH_FMT_LARGE)
            matching = [value for key, value in samples.items() if key.startswith(f"pid_{process.pid}_")]
            memory.append([time.monotonic() - t0, rss, int(used),
                           sum(matching) if matching else None])
            time.sleep(1)
    finally:
        kernel.SetThreadExecutionState(0x80000000)
        pdh.CloseQuery(query)
        if process.wait():
            raise RuntimeError(f"benchmark failed for {package}")
    if not memory:
        raise ValueError("missing memory samples")
    data = json.loads(output.read_bytes())
    frames = np.asarray(data["presented_seconds_and_interval_ms"])
    laps = []
    for lap in range(1, 2 if profile else 4):
        start = lap * data["lap_seconds"]
        mask = (frames[:, 0] - frames[:, 1] / 1000 >= start) & (frames[:, 0] < start + data["lap_seconds"])
        intervals = frames[mask, 1]
        laps.append(dict(frames=len(intervals), fps=1000 / intervals.mean(),
                         p95_ms=np.percentile(intervals, 95), worst_ms=intervals.max()))
    data.update(laps=laps, memory_seconds_working_set_bytes_device_used_MiB_process_dedicated_bytes=memory,
                gpu_seconds_utilization_percent_clock_MHz_temperature_C_pstate=gpu_samples)
    output.write_text(json.dumps(data, separators=(",", ":")), encoding="utf8")


def record(exe, package, route, output, fps):
    import cv2
    import imageio_ffmpeg
    cmd = [imageio_ffmpeg.get_ffmpeg_exe(), "-loglevel", "error", "-y", "-f", "rawvideo",
           "-pix_fmt", "rgba", "-s", "1920x1080", "-framerate", str(fps), "-i", "pipe:0",
           "-c:v", "libx264", "-preset", "fast", "-crf", "22", "-threads", "3",
           "-maxrate", "18M", "-bufsize", "36M", "-pix_fmt", "yuv420p", "-movflags", "+faststart", output]
    with subprocess.Popen([exe, package, "rock", "--route-rgba", route, str(fps)],
                          stdout=subprocess.PIPE) as source, subprocess.Popen(cmd, stdin=subprocess.PIPE) as encoder:
        with (output.parent.parent / f"{output.stem}.rgb8").open("wb") as samples:
            while pixels := source.stdout.read(1920 * 1080 * 4):
                if len(pixels) != 1920 * 1080 * 4:
                    raise ValueError("truncated native frame")
                rgba = np.frombuffer(pixels, np.uint8).reshape(1080, 1920, 4)
                small = cv2.resize(rgba, (480, 270), interpolation=cv2.INTER_AREA)
                samples.write(small[:, :, :3].tobytes())
                encoder.stdin.write(pixels)
        encoder.stdin.close()
        if source.wait() or encoder.wait():
            raise RuntimeError("route capture or movie encoding failed")



def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("viewer", type=Path)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--mode", choices=["still", "timing", "profile", "movie"], required=True)
    parser.add_argument("--fps", type=int, default=24)
    parser.add_argument("--detail", choices=["normal", "highest", "both"], default="both")
    args = parser.parse_args()
    route = Path(__file__).resolve().parents[2] / "assets/namaqualand/walk.json"
    directory = args.directory.resolve()
    package = directory / "package"
    highest_package(package, directory / "highest")
    evidence = directory / "evidence"
    evidence.mkdir(exist_ok=True)
    modes = ["normal", "highest"] if args.detail == "both" else [args.detail]
    for name in modes:
        src = package if name == "normal" else directory / "highest"
        if args.mode == "still":
            for view in ["rock", "flowers", "vista"]:
                subprocess.run([args.viewer, src, view, "--capture-1080p",
                                evidence / f"{name}-{view}.png"], check=True)
        elif args.mode == "movie":
            record(args.viewer, src, route, evidence / f"{name}.mp4", args.fps)
        else:
            benchmark(args.viewer, src, route, evidence / f"{name}-{args.mode}.json", args.mode == "profile")


if __name__ == "__main__":
    main()
