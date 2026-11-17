"""Verify archived runs and publish raw evidence plus conservative reference budgets."""
import argparse
import json
import math
from pathlib import Path
import statistics

if __package__:
    from .benchmark import digest, read, validate_report
    from .profiles import compare_settings, orientation_fixture, settings
else:
    from benchmark import digest, read, validate_report
    from profiles import compare_settings, orientation_fixture, settings


def aggregate(runs):
    expected = {f"{c}-timing-{t}-{r}" for c in ("all", "frustum") for t in (0, 1) for r in (1, 2, 3)}
    if len(runs) != 12 or {r["run_id"] for r in runs} != expected:
        raise ValueError("Require exactly three independent runs for each of four conditions")
    groups = {}
    for culling in ("all", "frustum"):
        for timing in (False, True):
            selected = [r for r in runs if r["culling"] == culling and r["timing"] is timing]
            if len(selected) != 3:
                raise ValueError("Mislabeled run conditions")
            if any(not r["run_id"].startswith(f"{culling}-timing-{int(timing)}-") for r in selected):
                raise ValueError("Run identifier differs from condition")
            metrics = {}
            for metric in selected[0]["summary"]:
                metrics[metric] = {}
                for statistic in ("mean", "p50", "p95", "p99"):
                    values = [r["summary"][metric][statistic] for r in selected]
                    if any(not math.isfinite(v) or v <= 0 for v in values):
                        raise ValueError("Invalid aggregate input")
                    metrics[metric][statistic] = {"run_median": statistics.median(values), "run_min": min(values), "run_max": max(values)}
            groups[f"{culling}-timing-{int(timing)}"] = metrics
    # Reference ceilings are rounded up from the slowest run p95, not selected
    # fast runs, an average FPS, or a percentile of run averages.
    all_timed = groups["all-timing-1"]
    budgets = {metric: math.ceil(all_timed[metric]["p95"]["run_max"] * 2) / 2
               for metric in ("cpu_render_ms", "gpu_ms")}
    return groups, budgets


def assemble(path):
    directory = Path(path).resolve()
    manifest = read(directory / "runs.json")
    if manifest.get("smoke") is not False or manifest.get("validation") is not False:
        raise ValueError("Smoke or validation runs are not normal performance evidence")
    identity = manifest["identity"]
    if identity.get("profile") not in ("b2", "b3"):
        raise ValueError("Archive requires an explicit B2 or B3 profile")
    conditions = read(directory / "conditions.json")
    if digest(directory / "conditions.json") != identity["conditions_sha256"]:
        raise ValueError("Archived conditions differ from the prepared source")
    compare_settings(identity["quality"], settings(identity["profile"], conditions))
    if identity["profile"] == "b3":
        if (digest(directory / "ibl-manifest.json") != identity["ibl"]["manifest_sha256"]
                or digest(directory / "orientation-fixture.json") != identity["ibl"]["orientation_fixture_sha256"]
                or read(directory / "orientation-fixture.json") != orientation_fixture()):
            raise ValueError("Archived IBL cook or orientation fixture was modified")
    runs = []
    capture_sets = {}
    for entry in manifest["runs"]:
        folder = (directory / entry["run_id"]).resolve()
        # Do not accept arbitrary relative paths from a modified manifest.
        if folder.parent != directory:
            raise ValueError("Run path escapes archive")
        report = read(folder / "report.json")
        config = read(folder / "config.json")
        if config["frames"] != 1800 or config["warmup"] != 3600:
            raise ValueError("Wrong formal measurement window")
        if config["culling"] != entry["culling"] or config["timing"] != entry["timing"]:
            raise ValueError("Condition mismatch")
        if config["profile"] != identity["profile"]:
            raise ValueError("Archive contains mixed quality profiles")
        compare_settings(config["quality"], identity["quality"])
        if identity["profile"] == "b3" and config["ibl"] != {
                "sha256": identity["ibl"]["panorama"]["sha256"], "width": 4096, "height": 2048}:
            raise ValueError("Archive contains a different IBL cook")
        summary = validate_report(report, config)
        if summary != entry["summary"] or digest(folder / "report.json") != entry["report_sha256"]:
            raise ValueError("Report or summary changed after collection")
        if set(entry["captures_sha256"]) != {"route0", "overview", "street", "material-detail", "glass-detail", "label-detail", "street-mid", "street-end"}:
            raise ValueError("Incomplete captures")
        for name, expected in entry["captures_sha256"].items():
            if name not in ("route0", "overview", "street", "material-detail", "glass-detail", "label-detail", "street-mid", "street-end"):
                raise ValueError("Unexpected capture")
            if digest(folder / f"{name}.png") != expected:
                raise ValueError("Capture changed after collection")
            capture_sets.setdefault(name, set()).add(expected)
        runs.append({**entry, "report": report, "engine_log_sha256": digest(folder / "engine.log"),
                     "stdout_log_sha256": digest(folder / "stdout.log")})
    groups, budgets = aggregate(runs)
    return {"schema_version": 2, "identity": identity, "conditions": conditions, "groups": groups,
            "reference_p95_budgets_ms": budgets,
            "capture_distinct_hashes": {name: sorted(values) for name, values in capture_sets.items()},
            "scope": "Godot renderer reference for the explicitly archived B2/B3 profile, not isolated RenderGraph overhead or pixel parity with Renderer. B3 retains native Godot AgX, pre-tonemap FXAA, BRDF/prefilter/shadow-filter differences versus Renderer Neutral/post-tonemap FXAA. CPU = setup + viewport, excludes scripts/physics/process-wide CPU; GPU = viewport observed query. Frame intervals = post-draw callback gaps, not scanout. Query observations may lag route frame numbers. Timers off/on are independent processes; differences include run variability. Godot memory monitors are not residency or allocation event counts; static_memory_bytes=0 is an unavailable release counter, not zero memory use.",
            "runs": runs}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("runs", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    evidence = assemble(args.runs)
    with args.output.open("x", encoding="utf-8") as stream:
        json.dump(evidence, stream, ensure_ascii=False, allow_nan=False, separators=(",", ":"))
        stream.write("\n")
    print(json.dumps({"groups": evidence["groups"], "budgets": evidence["reference_p95_budgets_ms"]}, indent=2))


if __name__ == "__main__":
    main()
