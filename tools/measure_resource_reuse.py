"""Compare prepared resource selectors; no GPU or native resources are used."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
VULKAN_CACHE = "crates/renderer/src/backends/vulkan/graph/cache.rs"
VULKAN_BENCH = "crates/renderer/benches/vulkan_resource_cache.rs"
D3D12_BENCH = "crates/renderer/benches/d3d12_reuse.rs"


def run(command, **kwargs):
    return subprocess.run(command, cwd=ROOT, check=True, **kwargs)


def capture(command):
    return run(command, stdout=subprocess.PIPE, encoding="utf-8").stdout


def digest(text):
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", default="483e304")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--output", type=Path, default=Path(".tools/resource-reuse/comparison.json"))
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    output = (ROOT / args.output).resolve()
    build = output.parent / "reuse-build"
    build.mkdir(parents=True, exist_ok=True)
    suffix = ".exe" if os.name == "nt" else ""

    baseline = capture(["git", "rev-parse", args.baseline]).strip()
    old_cache = capture(["git", "show", f"{baseline}:{VULKAN_CACHE}"])
    before_cache = build / "cache-before.rs"
    before_cache.write_text(old_cache, encoding="utf-8", newline="\n")
    harness = (ROOT / VULKAN_BENCH).read_text(encoding="utf-8")
    include = '../src/backends/vulkan/graph/cache.rs'
    if harness.count(include) != 1:
        raise RuntimeError("Vulkan harness must include exactly one production cache module")
    before_harness = build / "before-harness.rs"
    before_harness.write_text(harness.replace(include, before_cache.as_posix()), encoding="utf-8", newline="\n")
    before = build / ("vulkan-before" + suffix)
    after = build / ("vulkan-after" + suffix)
    # Build all executables before collecting any timing samples.
    run(["rustc", "--edition=2024", "-O", str(before_harness), "-o", str(before)])
    run(["rustc", "--edition=2024", "-O", str(ROOT / VULKAN_BENCH), "-o", str(after)])
    env = dict(os.environ, CARGO_INCREMENTAL="0")
    build_output = run([
        "cargo", "build", "--locked", "--release", "-p", "renderer", "--no-default-features",
        "--bench", "d3d12_reuse", "--message-format=json",
    ], env=env, stdout=subprocess.PIPE, encoding="utf-8").stdout
    executables = [
        event["executable"] for line in build_output.splitlines()
        if (event := json.loads(line)).get("reason") == "compiler-artifact"
        and event.get("target", {}).get("name") == "d3d12_reuse" and event.get("executable")
    ]
    if len(executables) != 1:
        raise RuntimeError(f"Expected one D3D12 CPU benchmark executable, got {executables}")
    d3d12 = executables[0]
    report = {
        "baseline_commit": baseline,
        "implementation_commit": capture(["git", "rev-parse", "HEAD"]).strip(),
        "worktree_status": capture(["git", "status", "--porcelain"]),
        "rustc": capture(["rustc", "-vV"]),
        "host": {"system": platform.platform(), "machine": platform.machine(),
                 "processor_identifier": os.environ.get("PROCESSOR_IDENTIFIER")},
        "profile": "Vulkan: rustc -O; D3D12: Cargo release; CARGO_INCREMENTAL=0",
        "scope": "CPU selector acquisition/return; D3D12 native ownership boundary uses Arc<()>; no GPU/native time",
        "sampling": "Each sample is a batch mean in ns/cycle, not an individual-frame latency sample",
        "baseline_note": "Vulkan compiles original Git source; D3D12 benchmark --reference retains the original entries.iter().position predicate",
        "source_sha256_utf8_lf": {
            "baseline_vulkan_cache": digest(old_cache),
            **{path: digest((ROOT / path).read_text(encoding="utf-8")) for path in [
                VULKAN_CACHE, VULKAN_BENCH, D3D12_BENCH,
                "crates/renderer/src/backends/d3d12/registry/reuse.rs",
                "tools/measure_resource_reuse.py",
            ]},
        },
        "runs": [],
    }
    commands = {"vulkan": {"before": [str(before)], "after": [str(after)]},
                "d3d12": {"before": [d3d12, "--reference"], "after": [d3d12]}}
    for api, versions in commands.items():
        for ordinal in range(1, args.runs + 1):
            order = ["before", "after"] if ordinal % 2 else ["after", "before"]
            for version in order:
                print(f"Measuring {api} run {ordinal} {version}", file=sys.stderr, flush=True)
                measured = json.loads(capture(versions[version]))
                report["runs"].append({"api": api, "ordinal": ordinal, "version": version,
                                       "command": versions[version], "result": measured})
                output.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(output)


if __name__ == "__main__":
    main()
