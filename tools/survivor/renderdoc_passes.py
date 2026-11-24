"""Capture one representative presented frame from each measured workload with RenderDoc 1.47.

Run in an x64 Visual Studio developer shell, using the headers for the installed RenderDoc release.
"""
import argparse
import csv
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
IMPORTS = ("AllocArrayMem", "FreeArrayMem", "InitialiseReplay", "ShutdownReplay",
           "CreateTargetControl", "ExecuteAndInject", "OpenCaptureFile")


def build_helper(output, headers):
    definition = output / "renderdoc.def"
    definition.write_text("LIBRARY renderdoc.dll\nEXPORTS\n" +
                          "".join(f"RENDERDOC_{name}\n" for name in IMPORTS), encoding="ascii")
    subprocess.run(["lib", "/nologo", "/machine:x64", f"/def:{definition}", "/out:renderdoc.lib"],
                   cwd=output, check=True)
    subprocess.run(["cl", "/nologo", "/EHsc", "/std:c++17", "/DRENDERDOC_PLATFORM_WIN32",
                    f"/I{headers}", str(Path(__file__).with_suffix(".cpp")), "renderdoc.lib",
                    "/Fe:renderdoc-passes.exe"], cwd=output, check=True)
    return output / "renderdoc-passes.exe"


def run(args):
    bench = args.benchmark.resolve(strict=True)
    summary = json.loads((bench / "summary.json").read_text(encoding="utf-8"))
    if summary["backend"] != "vulkan":
        raise ValueError("The marker capture recipe uses the Vulkan diagnostics build")
    exe = args.exe.resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    helper = build_helper(output, args.headers.resolve(strict=True))
    env = os.environ.copy()
    rd = str(args.renderdoc.resolve(strict=True))
    for key in ("PATH", "VK_ADD_IMPLICIT_LAYER_PATH"):
        env[key] = os.pathsep.join(filter(None, [rd, env.get(key)]))
    observations = []
    for run in summary["runs"]:
        if run["mode"] != "ordinary" or run["repetition"] != 1:
            continue
        scenario = run["scenario"]
        with (bench / run["csv"]).open(newline="", encoding="utf-8") as stream:
            rows = list(csv.DictReader(stream))
        row = next((r for r in rows if int(r["pulse"]) and int(r["shots"]) > 1),
                   rows[len(rows)//2]) if scenario == "overlap" else rows[len(rows)//2]
        # RenderDoc numbers presented frames starting at 1; CSV IDs start at 0 after warmup.
        frame = summary["warmup_presented"] + int(row["frame"]) + 1
        game = ["--window", "--backend", "vulkan", "--width", str(summary["extent"][0]),
                "--height", str(summary["extent"][1]), "--seed", str(summary["seed"]),
                "--scenario", "early" if scenario == "overlap" else scenario,
                "--frames", str(frame + 8)]
        if scenario == "overlap":
            game.extend(["--replay", str(args.replay.resolve(strict=True)),
                         "--replay-from", str(summary["replay"]["prefix"])])
        subprocess.run([helper, "capture", str(output / scenario), str(frame), exe,
                        ROOT, subprocess.list2cmdline(game)], env=env, check=True, timeout=600)
        capture, = output.glob(f"{scenario}_frame*.rdc")
        events = output / f"{scenario}-events.csv"
        subprocess.run([helper, "passes", capture, events], env=env, check=True, timeout=600)
        passes = {}
        with events.open(newline="", encoding="utf-8") as stream:
            for event in csv.DictReader(stream):
                key = event["marker_path"] or "(unmarked)"
                aggregate = passes.setdefault(key, {"events": 0, "gpu_ms": 0.0})
                aggregate["events"] += 1
                aggregate["gpu_ms"] += float(event["gpu_ms"])
        if "renderer.graph/scene_opaque" not in passes:
            raise ValueError("Capture lacks graph markers; use the Vulkan diagnostics binary")
        observations.append({"scenario": scenario, "capture_frame": frame,
                             "reference_simulation_tick": int(row["tick"]), "passes": passes})
        (output / "passes.json").write_text(json.dumps(observations, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    cli = argparse.ArgumentParser(description=__doc__)
    for flag in ("exe", "renderdoc", "headers", "benchmark", "replay", "output"):
        cli.add_argument("--" + flag, type=Path, required=True)
    run(cli.parse_args())
