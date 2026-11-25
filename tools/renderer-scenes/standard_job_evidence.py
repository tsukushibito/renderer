"""Compact matched StandardPipeline cohorts and apply the measured FPS/p95 adoption rule."""

import argparse
import itertools
import json
from pathlib import Path
import statistics


def compare(reference, candidate, long_run):
    # The harness rotates stage order but preserves one independent process per repeat.
    def by_repeat(runs):
        result = {int(run["name"].rsplit("-r", 1)[1]): run for run in runs}
        if len(result) != len(runs):
            raise ValueError("Repeated process for the same repeat and stage")
        return result

    control, proposed = by_repeat(reference), by_repeat(candidate)
    if control.keys() != proposed.keys():
        raise ValueError("Stage runs are not paired by repeat")
    def ratio(metric):
        return statistics.median(r[metric] for r in proposed.values()) / statistics.median(
            r[metric] for r in control.values())

    fps, p95 = ratio("completed_fps"), ratio("interval_p95_ms")
    wins = sum(proposed[k]["completed_fps"] > control[k]["completed_fps"] for k in control)
    eligible = long_run and len(control) >= 3
    return {
        "fps_change_percent": round(100 * (fps - 1), 2),
        "p95_change_percent": round(100 * (p95 - 1), 2),
        "paired_fps_wins": wins,
        "paired_processes": len(control),
        "passes_adoption_rule": (fps >= 1.05 and p95 <= 1.02 and wins == len(control)) if eligible else None,
    }


def compact(source):
    plan = json.loads((source / "plan.json").read_text(encoding="utf-8"))
    runs = json.loads((source / "summary.json").read_text(encoding="utf-8"))
    options = plan["options"]
    stages = options["stages"]
    long_run = options["frames"] >= 600 and options["warmup"] >= 120 and not any(
        options.get(mode) for mode in ("profile", "strict", "capture", "memory"))
    groups = {}
    for key, conditions in runs["groups"].items():
        if set(conditions) != set(stages):
            raise ValueError(f"Missing or unexpected stages for {key}")
        metrics = {}
        for stage, condition in conditions.items():
            if len(condition["runs"]) != options["repeats"]:
                raise ValueError(f"Missing repeats for {key} / {stage}")
            metrics[stage] = {
                "median": {k: round(v, 5) for k, v in condition["median"].items()},
                "fps": [round(r["completed_fps"], 3) for r in condition["runs"]],
                "p95_ms": [round(r["interval_p95_ms"], 5) for r in condition["runs"]],
            }
        comparisons = {}
        for a, b in itertools.combinations(stages, 2):
            comparisons[f"{a}_to_{b}"] = compare(conditions[a]["runs"], conditions[b]["runs"], long_run)
        groups[key] = {"stages": metrics, "comparisons": comparisons}
    expected = len(options["apis"]) * len(options["objects"]) * len(options["visible"]) * len(options["workloads"])
    if len(groups) != expected:
        raise ValueError(f"{source}: expected {expected} independent scene configurations")
    return {"binary_sha256": plan["sha256"], "cpu_affinity_mask": plan["cpu_policy"]["affinity_mask"],
            "devices": runs["devices"], "options": options, "groups": groups, "captures": runs.get("captures", {})}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("cohorts", type=Path, nargs="+", help="completed standard_job_benchmark.py output directories")
    args = parser.parse_args()
    summaries = {folder.name: compact(folder) for folder in args.cohorts}
    if len(summaries) != len(args.cohorts):
        parser.error("cohort directories must have distinct names")
    result = {"schema": 1, "cohorts": summaries}
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2, allow_nan=False) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
