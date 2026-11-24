"""Run the Windows training-yard acceptance matrix and retain reproducible evidence."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import shutil
import struct
import subprocess
import sys
import zipfile


ROOT = Path(__file__).resolve().parents[2]
REFERENCE = ROOT / "crates/training-yard/tests/reference.zip"
COLOR_DELTA = 1
DEPTH_DELTA = 1e-5
HEADER = b"P6\n48 24\n255\n"


def artifacts():
    for step in range(29):
        yield f"headless/frame-{step:02}.ppm", len(HEADER) + 48 * 24 * 3
    host_files = {
        "main.rgba8": 48 * 24 * 4, "composed.rgba8": 48 * 24 * 4,
        "outlined.rgba8": 48 * 24 * 4, "main.depth32": 48 * 24 * 4,
        "parts.u32": 48 * 24 * 4, "preview.rgba8": 12 * 6 * 4,
        **{f"shadow{index}.depth32": 32 * 32 * 4 for index in range(4)},
    }
    for step in range(24):
        for name, size in host_files.items():
            yield f"host/frame-{step:02}/{name}", size
        yield f"host/frame-{step:02}/input.txt", None


def compare(name, actual, reference):
    if len(actual) != len(reference):
        return {"actual_bytes": len(actual), "expected_bytes": len(reference)}
    if actual == reference:
        return None
    if name.endswith(".depth32"):
        pairs = zip(struct.iter_unpack("<f", actual), struct.iter_unpack("<f", reference))
        for pixel, ((value,), (expected,)) in enumerate(pairs):
            if not math.isfinite(value) or abs(value - expected) > DEPTH_DELTA:
                return {"pixel": pixel, "actual": str(value), "expected": expected}
    elif name.endswith((".ppm", ".rgba8")):
        if name.endswith(".ppm"):
            if not actual.startswith(HEADER):
                return {"error": "unexpected PPM header"}
            actual, reference = actual[len(HEADER):], reference[len(HEADER):]
        for offset, (value, expected) in enumerate(zip(actual, reference)):
            if abs(value - expected) > COLOR_DELTA:
                return {"component_offset": offset, "actual": value, "expected": expected}
    else:
        offset = next(i for i, (a, b) in enumerate(zip(actual, reference)) if a != b)
        return {"byte_offset": offset, "actual": actual[offset], "expected": reference[offset]}
    return None


def run(output):
    output.mkdir(parents=True, exist_ok=False)
    metadata = subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"], text=True)
    target = Path(json.loads(metadata)["target_directory"])
    executable = target / "release/training-yard.exe"
    report = {
        "revision": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        "worktree_changes": subprocess.check_output(["git", "status", "--porcelain"], text=True).splitlines(),
        "rustc": subprocess.check_output(["rustc", "-Vv"], text=True),
        "platform": platform.platform(), "comparison": {"color": COLOR_DELTA, "depth": DEPTH_DELTA, "ids": "exact"},
        "reference_sha256": hashlib.sha256(REFERENCE.read_bytes()).hexdigest(), "commands": [], "differences": [],
    }

    def execute(args, log, *, cwd=ROOT, env=None):
        record = {"args": [str(arg) for arg in args], "log": str(log.relative_to(output))}
        report["commands"].append(record)
        with log.open("w", encoding="utf-8") as stream:
            result = subprocess.run(args, cwd=cwd, env=env, stdout=stream, stderr=subprocess.STDOUT)
        record["status"] = result.returncode
        if result.returncode:
            raise RuntimeError(f"{args[0]} failed, see {log}")

    try:
        with zipfile.ZipFile(REFERENCE) as baseline:
            for profile, feature in [("strict", "development-validation"), ("shipping", "memory-profiling")]:
                build = output / profile
                build.mkdir()
                features = f"presentation,vulkan,d3d12,{feature}"
                execute(["cargo", "build", "--locked", "--release", "-p", "training-yard",
                         "--features", features], build / "build.log")
                report[profile + "_exe_sha256"] = hashlib.sha256(executable.read_bytes()).hexdigest()
                package = build / "package"
                package.mkdir()
                app = shutil.copy2(executable, package)
                distribution = package / "assets/equipment/dist"
                execute(["cargo", "run", "--locked", "--release", "-p", "renderer-asset-cook", "--",
                         "cook", ROOT / "crates/training-yard/assets/equipment/source", distribution,
                         "--cache", output / "cook-cache"], build / "package-assets.log")
                report[profile + "_asset_generation"] = (distribution / "current").read_text().strip()
                environment = os.environ.copy()
                environment.pop("TRAINING_YARD_ASSETS", None)
                for api in ["vulkan", "d3d12"]:
                    case = build / api
                    case.mkdir()
                    for mode in ["window", "headless", "host", "host-measured"]:
                        command = [app, "--api", api, "--mode", mode.split("-")[0]]
                        if mode in ("headless", "host"):
                            command += ["--output", case / mode]
                        execute(command, case / (mode + ".log"), cwd=package, env=environment)
                    for name, size in artifacts():
                        expected = baseline.read(name)
                        if size is not None and len(expected) != size:
                            raise ValueError(f"invalid reference for {name}")
                        delta = compare(name, (case / name).read_bytes(), expected)
                        if delta is not None:
                            report["differences"].append({"case": f"{profile}/{api}", "file": name, **delta})
                environment["TRAINING_YARD_ASSETS"] = str(distribution)
                # Exercise resize/suspension, core-only compute, external devices and frame ownership.
                for crate, targets, log in [
                    ("training-yard", ["--tests"], "integration"),
                    ("renderer-core", ["--test", "d3d12_host"], "d3d12-host"),
                    ("renderer-core", ["--lib", "host_device_survives_adopted_graphs_and_failed_adoption"], "vulkan-host"),
                ]:
                    execute(["cargo", "test", "--locked", "--release", "-p", crate, "--features", features,
                             *targets, "--", "--ignored", "--test-threads=1"], build / (log + ".log"), env=environment)
        if report["differences"]:
            raise ValueError(f"capture mismatch, see {output / 'report.json'}")
        report["passed"] = True
    finally:
        (output / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="new directory for captures, commands, differences and logs")
    args = parser.parse_args()
    if sys.platform != "win32":
        parser.error("the acceptance matrix requires Windows with Vulkan and D3D12")
    os.chdir(ROOT)
    run(args.output.resolve())
