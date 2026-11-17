"""Prepare and measure a pinned Godot release-template Bistro reference.

No global install, editor asset import, system setting change, or headless GPU
measurement. Runtime GLTFDocument import is outside ROI. PNG mip generation and
Godot materials differ from the Renderer DDS cook; this is a renderer reference,
not an isolated RenderGraph-overhead measurement.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import time


ROOT = Path(__file__).resolve().parents[2]
SOURCE = Path(__file__).resolve().parent


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def read(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def write(path, value):
    Path(path).write_text(json.dumps(value, indent=2, ensure_ascii=False, allow_nan=False) + "\n", encoding="utf-8")


def run_logged(command, log, timeout=600):
    environment = os.environ.copy()
    # Keep editor settings, release shader caches and user:// inside task output.
    environment["APPDATA"] = str(Path(log).resolve().parent / "appdata")
    environment["LOCALAPPDATA"] = str(Path(log).resolve().parent / "localappdata")
    with Path(log).open("w", encoding="utf-8") as stream:
        result = subprocess.run([str(x) for x in command], stdout=stream, stderr=subprocess.STDOUT, timeout=timeout, env=environment)
    if result.returncode:
        raise ValueError(f"Process failed ({result.returncode}); see {log}")
    text = Path(log).read_text(encoding="utf-8", errors="replace")
    if any(marker in text for marker in ("ERROR:", "SCRIPT ERROR:", "WARNING:", "Validation Error", "Validation Warning")):
        raise ValueError(f"Engine diagnostics require investigation; see {log}")


def prepare(args):
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    assets = args.asset_root.resolve()
    expected = read(ROOT / "assets/bistro/manifest.json")
    verified = []
    for item in expected["files"]:
        if not item["path"].startswith("assets/bistro/converted/"):
            continue
        path = assets / item["path"]
        if path.stat().st_size != item["size"] or digest(path) != item["sha256"]:
            raise ValueError(f"Input identity mismatch: {path}")
        verified.append(item)
    if len(verified) != 407:
        raise ValueError("Expected all 407 converted Bistro files")
    project = output / "project"
    project.mkdir()
    hashes = {}
    for name in ("project.godot", "main.gd", "main.tscn"):
        shutil.copyfile(SOURCE / name, project / name)
        hashes[name] = digest(project / name)
    shutil.copyfile(ROOT / "tools/bistro/conditions.json", output / "conditions.json")
    editor, template = args.editor.resolve(), args.template.resolve()
    # Custom template avoids any modification to the user's installed Godot.
    (project / "export_presets.cfg").write_text(
        '[preset.0]\nname="Windows"\nplatform="Windows Desktop"\nrunnable=true\n'
        'export_filter="all_resources"\ninclude_filter=""\nexclude_filter=""\n'
        '[preset.0.options]\ncustom_template/release="' + template.as_posix() + '"\n'
        'binary_format/architecture="x86_64"\napplication/modify_resources=false\n'
        'graphics/opengl3/include=false\ngraphics/d3d12/include=false\n', encoding="utf-8")
    run_logged([editor, "--headless", "--path", project, "--editor", "--import"], output / "import.log")
    run_logged([editor, "--headless", "--path", project, "--export-release", "Windows", output / "bistro-reference.exe"], output / "export.log")
    identity = {"schema_version": 1, "created_utc": datetime.now(timezone.utc).isoformat(),
                "editor_sha256": digest(editor), "template_sha256": digest(template),
                "binary_sha256": digest(output / "bistro-reference.exe"),
                "pack_sha256": digest(output / "bistro-reference.pck"), "project_sources": hashes,
                "conditions_sha256": digest(output / "conditions.json"), "inputs": verified,
                "gltf": str(assets / "assets/bistro/converted/BistroExterior.gltf"),
                "source_commit": subprocess.check_output(["git", "-c", f"safe.directory={ROOT.as_posix()}", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
                "benchmark_tool_sha256": digest(SOURCE / "benchmark.py"),
                "contract": "Forward+ Vulkan release template; runtime glTF full exterior; generated tangents and PNG mipmaps; no compression/IBL/shadows/AA/glow; constant ambient 0.03; linear tonemap exposure 1; no LOD/occlusion; optional frustum culling; windowed 1080p vsync off; fixed frame-indexed route. Godot's BRDF, depth prepass, batching, mip chain and presentation differ from Renderer."}
    write(output / "identity.json", identity)
    print(output / "identity.json")


def percentile(values, p):
    ordered = sorted(values)
    position = (len(ordered) - 1) * p
    lower = math.floor(position)
    upper = math.ceil(position)
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def validate_report(report, config):
    if report.get("config") != config or report.get("debug") is not False:
        raise ValueError("Run config or release identity mismatch")
    if report.get("driver") != "vulkan" or report.get("renderer") != "forward_plus":
        raise ValueError("Requested Vulkan Forward+ backend was not used")
    if report.get("inventory") != {"mesh_instances": 1296, "surfaces": 1591, "triangles": 2832120}:
        raise ValueError("Incomplete Bistro inventory")
    if report.get("import_inventory") != {"materials": 132, "images": 405}:
        raise ValueError("Incomplete Bistro material/image inventory")
    version = report.get("godot", {})
    if version.get("hash") != "ed1daf0bf001b61586d9930840f2f1394092c079":
        raise ValueError("Expected the pinned official Godot 4.7.2 release")
    if report.get("viewport_size") != [1920, 1080] or report.get("vsync") != 0 or report.get("max_fps") != 0:
        raise ValueError("Resolution or frame pacing mismatch")
    samples = report.get("samples", [])
    if len(samples) != config["frames"]:
        raise ValueError("Incomplete samples")
    metrics = ["frame_interval_ms"]
    if config["timing"]:
        metrics += ["cpu_setup_ms", "cpu_viewport_ms", "cpu_render_ms", "gpu_ms", "draw_calls", "primitives"]
    for index, sample in enumerate(samples):
        if sample.get("frame") != index:
            raise ValueError("Discontinuous frame identity")
        for metric in metrics:
            value = sample.get(metric)
            if type(value) not in (int, float) or not math.isfinite(value) or value <= 0:
                raise ValueError(f"Invalid or missing {metric} at frame {index}")
        if config["timing"] and abs(sample["cpu_render_ms"] - sample["cpu_setup_ms"] - sample["cpu_viewport_ms"]) > 1e-6:
            raise ValueError("CPU render total differs from setup + viewport")
        if not config["timing"] and any(key in sample for key in ("gpu_ms", "cpu_render_ms")):
            raise ValueError("Timing-disabled run contains timer readings")
    if abs(sum(x["frame_interval_ms"] for x in samples) - report["roi_wall_ms"]) > 0.01:
        raise ValueError("Frame intervals do not cover exact ROI")
    return {metric: {"mean": statistics.mean(x[metric] for x in samples),
                     "p50": percentile([x[metric] for x in samples], .5),
                     "p95": percentile([x[metric] for x in samples], .95),
                     "p99": percentile([x[metric] for x in samples], .99)} for metric in metrics}


def measure(args):
    prepared = args.prepared.resolve()
    identity = read(prepared / "identity.json")
    if digest(prepared / "bistro-reference.exe") != identity["binary_sha256"] or digest(prepared / "bistro-reference.pck") != identity["pack_sha256"]:
        raise ValueError("Prepared executable or pack was modified")
    if digest(prepared / "conditions.json") != identity["conditions_sha256"]:
        raise ValueError("Prepared conditions were modified")
    if args.frames <= 0 or args.warmup < 2 or args.repeats < 1:
        raise ValueError("frames/repeats must be positive and warmup >= 2")
    if not args.smoke and (args.frames != 1800 or args.warmup != 3600 or args.repeats < 3):
        raise ValueError("Formal baseline requires 3600 warmup, 1800 frames and >= 3 independent runs")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    runs = []
    for repetition in range(args.repeats):
        cases = [(culling, timing) for culling in args.culling for timing in (False, True)]
        offset = repetition % len(cases)
        for culling, timing in cases[offset:] + cases[:offset]:
            run_id = f"{culling}-timing-{int(timing)}-{repetition + 1}"
            folder = output / run_id
            folder.mkdir()
            config = {"gltf": identity["gltf"], "conditions": str(prepared / "conditions.json"),
                      "frames": args.frames, "warmup": args.warmup, "timing": timing, "culling": culling,
                      "output": str(folder / "report.json"), "capture": str(folder / "route0.png")}
            write(folder / "config.json", config)
            command = [prepared / "bistro-reference.exe", "--rendering-driver", "vulkan", "--rendering-method", "forward_plus",
                       "--resolution", "1920x1080", "--position", "0,0", "--disable-vsync", "--audio-driver", "Dummy",
                       "--log-file", folder / "engine.log"]
            if args.validation:
                command.append("--gpu-validation")
            command += ["--", folder / "config.json"]
            start = time.monotonic()
            run_logged(command, folder / "stdout.log", args.timeout)
            summary = validate_report(read(folder / "report.json"), config)
            captures = {}
            import struct
            for name in ["route0"] + [view["name"] for view in read(prepared / "conditions.json")["views"]]:
                path = folder / f"{name}.png"
                header = path.read_bytes()[:24]
                if header[:8] != b"\x89PNG\r\n\x1a\n" or struct.unpack(">II", header[16:24]) != (1920, 1080):
                    raise ValueError(f"Capture has wrong format/size: {path}")
                captures[name] = digest(path)
            runs.append({"run_id": run_id, "culling": culling, "timing": timing,
                         "controller_wall_seconds": time.monotonic() - start,
                         "command": [str(x) for x in command], "summary": summary,
                         "report_sha256": digest(folder / "report.json"), "captures_sha256": captures})
            write(output / "runs.json", {"identity": identity, "smoke": args.smoke, "validation": args.validation, "runs": runs})
            print(run_id, json.dumps(summary, ensure_ascii=False), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("prepare")
    build.add_argument("--editor", type=Path, required=True)
    build.add_argument("--template", type=Path, required=True)
    build.add_argument("--asset-root", type=Path, default=ROOT)
    build.add_argument("--output", type=Path, required=True)
    build.set_defaults(func=prepare)
    run = commands.add_parser("run")
    run.add_argument("--prepared", type=Path, required=True)
    run.add_argument("--output", type=Path, required=True)
    run.add_argument("--frames", type=int, default=1800)
    run.add_argument("--warmup", type=int, default=3600)
    run.add_argument("--repeats", type=int, default=3)
    run.add_argument("--culling", nargs="+", choices=("all", "frustum"), default=["all", "frustum"])
    run.add_argument("--timeout", type=int, default=600)
    run.add_argument("--validation", action="store_true")
    run.add_argument("--smoke", action="store_true")
    run.set_defaults(func=measure)
    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
