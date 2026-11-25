"""Run matched packed-geometry visibility comparisons on a physical Windows GPU."""
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

VARIANTS = {
    "cpu": ["--route", "cpu"],
    "cpu-scene4": ["--route", "cpu", "--workers", "4"],
    "cpu-native4": ["--route", "cpu", "--workers", "4", "--native"],
    "cpu-all4": ["--route", "cpu", "--workers", "4", "--graph-jobs", "--native"],
    "ranges": ["--route", "cpu-ranges"],
    "ranges4": ["--route", "cpu-ranges", "--workers", "4"],
    "direct-all": ["--route", "direct-all"],
    "gpu": ["--route", "gpu"],
}


def measure(command, policy):
    with subprocess.Popen(
        command, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        creationflags=subprocess.CREATE_NO_WINDOW | 0x4,
    ) as process:
        try:
            observed = host_policy.apply(process.pid, policy)
            stdout, stderr = process.communicate(timeout=900)
        except BaseException:
            process.kill()
            process.wait()
            raise
    if process.returncode:
        raise RuntimeError(f"{command} exited {process.returncode}: {stderr}\n{stdout}")
    return json.loads(stdout), observed


def summarize(results):
    summaries = {}
    for condition, runs in results.items():
        routes = {}
        for run in runs:
            routes.setdefault(run["variant"], []).append(run)
        medians = {
            name: {field: statistics.median(row[field] for row in rows)
                   for field in ("fps", "p95_ms", "p99_ms")} for name, rows in routes.items()
        }
        summary = {"median": medians}
        if "gpu" in routes and len(routes) > 1:
            fastest = max((v for v in routes if v != "gpu"), key=lambda v: medians[v]["fps"])
            baseline = medians[fastest]
            gpu = medians["gpu"]
            matched = {row["repeat"]: row for row in routes[fastest]}
            gpu_runs = routes["gpu"]
            pairs = [row["fps"] / matched[row["repeat"]]["fps"] for row in gpu_runs
                     if row["repeat"] in matched]
            confirmation = (len(pairs) >= 3 and len(pairs) == len(matched) == len(gpu_runs)
                            and all(row["warmup"] >= 120 and row["frames"] >= 600
                                    and not row["strict"] and not row["profile"] and "memory" not in row
                                    for row in runs))
            summary.update(cpu_reference=fastest, gpu_fps_ratio=gpu["fps"] / baseline["fps"],
                           gpu_p95_ratio=gpu["p95_ms"] / baseline["p95_ms"],
                           matched_fps_ratios=pairs,
                           meets_gate=confirmation and min(pairs) > 1 and
                           gpu["fps"] >= baseline["fps"] * 1.05 and gpu["p95_ms"] <= baseline["p95_ms"] * 1.02)
        summaries[condition] = summary
    return summaries


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--apis", nargs="+", choices=("vulkan", "d3d12"), default=["vulkan", "d3d12"])
    parser.add_argument("--objects", nargs="+", type=int, default=[50000])
    parser.add_argument("--visible", nargs="+", type=int, default=[10, 50, 100])
    parser.add_argument("--moving", nargs="+", type=int, default=[0])
    parser.add_argument("--groups", nargs="+", type=int, default=[1])
    parser.add_argument("--variants", nargs="+", choices=VARIANTS, default=list(VARIANTS))
    parser.add_argument("--warmup", type=int, default=120)
    parser.add_argument("--frames", type=int, default=600)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--profile", action="store_true")
    args = parser.parse_args()
    if args.frames < 2 or args.warmup < 0 or args.repeats < 1:
        parser.error("frames >= 2, warmup >= 0, repeats >= 1 required")
    args.output.mkdir(parents=True, exist_ok=False)
    policy = host_policy.plan("performance-cores")
    exe = args.exe.resolve(strict=True)
    with exe.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    common = {"binary_sha256": digest, "policy": policy}
    results, expected, adapters = {}, {}, {}
    cases = list(itertools.product(args.apis, args.objects, args.visible, args.moving, args.groups))
    summary_path = args.output / "summary.json"
    for repeat in range(args.repeats):
        start = repeat % len(args.variants)
        for api, count, visible, moving, groups in (reversed(cases) if repeat % 2 else cases):
            if moving > count or groups > count:
                continue
            key = f"{api}-{count}-visible{visible}-moving{moving}-groups{groups}"
            for variant in args.variants[start:] + args.variants[:start]:
                cmd = [str(exe), "--api", api, "--objects", str(count), "--visible", str(visible),
                       "--moving", str(moving), "--groups", str(groups), "--warmup", str(args.warmup),
                       "--frames", str(args.frames), *VARIANTS[variant]]
                if args.profile:
                    cmd.append("--profile")
                row, observed = measure(cmd, policy)
                hashes = row["hdr_depth_sha256"]
                if len(hashes) != 6 or expected.setdefault(key, hashes) != hashes:
                    raise ValueError(f"HDR/depth parity failure for {key}/{variant}")
                if moving:
                    crossed = row["boundary_hdr_depth_sha256"]
                    if len(crossed) != 6 or expected.setdefault(key+"/boundary", crossed) != crossed:
                        raise ValueError(f"clip-plane crossing parity failure for {key}/{variant}")
                if adapters.setdefault(api, row["adapter"]) != row["adapter"]:
                    raise ValueError(f"adapter changed for {key}/{variant}")
                row.update(variant=variant, repeat=repeat+1, profile=args.profile)
                run = f"{key}-{variant}-r{repeat+1}"
                (args.output / f"{run}.json").write_text(json.dumps(
                    {"command": cmd, "observed": observed, "report": row}, indent=2)+"\n", encoding="utf-8")
                results.setdefault(key, []).append(row)
                print(f"{run}: {row['fps']:.1f} FPS, p95 {row['p95_ms']:.3f} ms", flush=True)
        summary_path.write_text(json.dumps({**common, "adapters": adapters, "conditions": summarize(results)},
                                           indent=2)+"\n", encoding="utf-8")


if __name__ == "__main__":
    main()
