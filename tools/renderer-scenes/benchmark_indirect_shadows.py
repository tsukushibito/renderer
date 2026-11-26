"""Compare warmed DIS modes on identical poses. Run serially on an idle GPU."""

import argparse
import json
from pathlib import Path
import statistics
import subprocess

import numpy as np


def compare_captures(full, half, off, layout):
    def hdr(folder):
        return np.fromfile(folder / "hdr.bin", "<f2").reshape(-1, 4)[:, :3].astype(np.float32)

    baseline = hdr(full)
    difference = np.abs(hdr(half) - baseline)
    if layout == "single" and not np.any(difference):
        raise ValueError(f"Expected interpolation on the fixture's smooth floor at {half}")
    depth = (full / "depth.bin").read_bytes()
    if any((folder / "depth.bin").read_bytes() != depth for folder in (half, off)):
        raise ValueError(f"DIS changed depth at {half}")
    active = np.max(hdr(off) - baseline, axis=1) > 0.001
    if not active.any():
        raise ValueError(f"No shadow on visible receivers at {full}")
    return dict(hdr_mean=float(difference.mean()), hdr_max=float(difference.max()),
                hdr_p99=float(np.percentile(difference, 99)),
                shadowed_pixel_fraction=float(active.mean()),
                shadowed_hdr_mean=float(difference[active].mean()))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--api", choices=("vulkan", "d3d12"), required=True)
    parser.add_argument("--repeats", type=int, default=3)
    layouts = ("prop", "single", "offscreen", "spread", "dense")
    parser.add_argument("--layouts", nargs="+", choices=layouts, default=layouts)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    results = []
    quality = []
    for width, height in ((1280, 720), (1920, 1080)):
        for layout in args.layouts:
            for repeat in range(args.repeats):
                modes = ["off", "full", "half"]
                offset = repeat % len(modes)
                for mode in modes[offset:] + modes[:offset]:
                    dest = args.output / f"{width}-{layout}-{mode}-r{repeat + 1}"
                    subprocess.run([
                        args.executable.resolve(), "--api", args.api, "--pipeline", "forward",
                        "--indirect-shadow-demo", mode, "--shadow-layout", layout,
                        "--width", str(width), "--height", str(height),
                        "--warmup", "120", "--frames", "240", "--gpu-timing", "--output", dest,
                    ], check=True)
                    report = json.loads((dest / "report.json").read_bytes())
                    frames = report["measurement"]["frame_samples"]
                    gpu = [frame["gpu_ns"] / 1e6 for frame in frames]
                    collection = report["measurement"]["indirect_shadow_collection"]
                    if collection is None:
                        raise ValueError("build renderer-scenes with memory-profiling")
                    if not report["memory"]["normal"]["allocation_free"]:
                        raise ValueError(f"Unexpected steady-state allocation at {dest}")
                    results.append(dict(width=width, height=height, layout=layout, mode=mode,
                                        repeat=repeat, gpu_median_ms=statistics.median(gpu),
                                        gpu_p95_ms=float(np.percentile(gpu, 95)),
                                        collect_median_us=statistics.median(c["cpu_ns"] for c in collection) / 1e3,
                                        active_max=max(c["active"] for c in collection),
                                        tile_peak=max(c["busiest_tile"] for c in collection),
                                        memory=report["memory"]["normal"]))
                    (args.output / "timings.json").write_text(json.dumps(results, indent=2))
                if repeat == 0:
                    folders = [args.output / f"{width}-{layout}-{mode}-r1"
                               for mode in ("full", "half", "off")]
                    quality.append(dict(width=width, height=height, layout=layout,
                                        **compare_captures(*folders, layout)))
                    (args.output / "quality.json").write_text(json.dumps(quality, indent=2))


if __name__ == "__main__":
    main()
