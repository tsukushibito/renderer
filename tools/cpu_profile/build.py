"""Build and archive a release-derived Windows profiling executable and its PDB.

The archive is an explicit build boundary: capture tools run these copied bytes,
not an executable that a subsequent Cargo invocation can replace. This helper
does not collect traces or establish sampled stack/source attribution.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tomllib
import uuid

from identity import IdentityError, inspect_pair, read_pe_identity, verify_manifest


ROOT = Path(__file__).resolve().parents[2]
DIAGNOSTICS = {"development-validation", "vulkan-diagnostics", "d3d12-diagnostics",
               "graph-diagnostics", "memory-profiling"}
RELEASE_DEFAULTS = {"opt-level": 3, "debug": False, "strip": "none",
                    "debug-assertions": False, "overflow-checks": False,
                    "lto": False, "panic": "unwind", "incremental": False,
                    "codegen-units": 16}


class BuildError(ValueError):
    """The requested build cannot establish a reproducible profiling boundary."""


def run(command, **kwargs):
    return subprocess.run(command, cwd=ROOT, check=True, text=True,
                          encoding="utf-8", errors="replace", **kwargs)


def git(*args):
    return run(["git", *args], capture_output=True).stdout.rstrip("\n")


def parse_features(value, instrumentation="none"):
    features = sorted(set(value.replace(",", " ").split()))
    forbidden = DIAGNOSTICS.intersection(features)
    if forbidden:
        raise BuildError("Shipping profiling excludes diagnostics: " + ", ".join(sorted(forbidden)))
    if instrumentation not in ("none", "tracy"):
        raise BuildError("Unknown instrumentation mode")
    if instrumentation == "none" and "tracy" in features:
        raise BuildError("Tracy requires explicit --instrumentation tracy")
    if instrumentation == "tracy":
        features = sorted(set(features) | {"tracy"})
    return features


def check_environment(environment):
    # Cargo environment/config can override manifest profiles. Refuse those
    # overrides rather than silently claiming release-equivalent optimization.
    exact = {"RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS",
             "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_TARGET",
             "CARGO_BUILD_RUSTC", "CARGO_BUILD_RUSTC_WRAPPER",
             "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"}
    overrides = [name for name, value in environment.items() if value and
                 (name in exact or name.startswith("CARGO_PROFILE_") or
                  name.startswith("TRACY_CLIENT_") or name == "CRATE_CC_NO_DEFAULTS" or
                  name in ("CL", "_CL_") or
                  re.search(r"(^|_)(CC|CXX|CFLAGS|CPPFLAGS|CXXFLAGS)(_|$)", name, re.I) or
                  (name.startswith("CARGO_TARGET_") and name.endswith("_RUSTFLAGS")))]
    if overrides:
        raise BuildError("Unset build/profile overrides before archiving: " + ", ".join(sorted(overrides)))


def configuration_files(root, environment):
    paths = []
    for directory in [root, *root.parents]:
        for name in ("config", "config.toml"):
            candidate = directory / ".cargo" / name
            if candidate.is_file():
                paths.append(candidate)
    cargo_home = Path(environment.get("CARGO_HOME", str(Path.home() / ".cargo")))
    for name in ("config", "config.toml"):
        candidate = cargo_home / name
        if candidate.is_file() and candidate not in paths:
            paths.append(candidate)
    return paths


def check_configuration(data, path):
    if "profile" in data or "include" in data or data.get("build", {}).get("rustflags"):
        raise BuildError(f"Cargo profile/rustflags/include override in {path}")
    if any(target.get("rustflags") for target in data.get("target", {}).values()):
        raise BuildError(f"Cargo target rustflags override in {path}")
    build = data.get("build", {})
    if any(build.get(key) for key in ("rustc", "rustc-wrapper", "rustc-workspace-wrapper", "target")):
        raise BuildError(f"Cargo compiler/target override in {path}")
    # [env] can introduce the same flags after our own process has started.
    check_environment({key: str(value) for key, value in data.get("env", {}).items()})


def configuration_snapshot(root, environment):
    records = []
    for path in configuration_files(root, environment):
        content = path.read_bytes()
        check_configuration(tomllib.loads(content.decode("utf-8")), path)
        records.append({"path": str(path.resolve()), "sha256": hashlib.sha256(content).hexdigest()})
    return records


def profile_settings(root, name):
    data = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    profiles = data.get("profile", {})
    release = {**RELEASE_DEFAULTS, **profiles.get("release", {})}
    selected = profiles.get(name, {})
    if name == "profiling":
        if selected.get("inherits") != "release":
            raise BuildError("profiling must inherit the release profile")
        if set(selected) - {"inherits", "debug", "strip"}:
            raise BuildError("profiling may only change release debug information and stripping")
        if selected.get("debug") not in (2, "full") or selected.get("strip", "none") not in (False, "none"):
            raise BuildError("profiling requires full, unstripped debug information")
    return {**release, **selected}


def source_snapshot():
    names = git("ls-files", "--cached", "--others", "--exclude-standard", "-z", "--",
                "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo", "crates/renderer",
                "tools/cpu_profile", "tools/tracy", "tools/bistro/conditions.json",
                "tools/bistro/ibl.py").split("\0")
    result = {}
    for name in sorted(set(filter(None, names))):
        path = ROOT / name
        if not path.is_file():
            result[name] = {"deleted": True}
            continue
        content = path.read_bytes()
        raw_sha256 = hashlib.sha256(content).hexdigest()
        try:
            content = content.decode("utf-8").replace("\r\n", "\n").encode("utf-8")
            normalization = "utf8-lf"
        except UnicodeDecodeError:
            normalization = "raw"
        result[name] = {"sha256": hashlib.sha256(content).hexdigest(), "normalization": normalization,
                        "raw_sha256": raw_sha256}
    return result


def select_artifact(messages, example):
    matches = []
    for line in messages.splitlines():
        try:
            item = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (item.get("reason") == "compiler-artifact" and item.get("target", {}).get("name") == example
                and "example" in item.get("target", {}).get("kind", []) and item.get("executable")):
            matches.append(item)
    if len(matches) != 1:
        raise BuildError(f"Expected one Cargo executable artifact for {example}; found {len(matches)}")
    return matches[0]


def cargo_build(command, environment):
    def diagnostics(messages):
        for line in messages.splitlines():
            try:
                item = json.loads(line)
            except json.JSONDecodeError:
                continue
            if item.get("reason") == "compiler-message":
                rendered = item.get("message", {}).get("rendered")
                if rendered:
                    print(rendered, file=sys.stderr, end="")

    try:
        result = run(command, env=environment, stdout=subprocess.PIPE)
    except subprocess.CalledProcessError as error:
        diagnostics(error.stdout or "")
        raise
    diagnostics(result.stdout)
    return result.stdout


def check_artifact(artifact, settings, instrumentation="none"):
    observed = artifact["profile"]
    for cargo_key, profile_key in (("opt_level", "opt-level"), ("debug_assertions", "debug-assertions"),
                                   ("overflow_checks", "overflow-checks")):
        if str(observed[cargo_key]).lower() != str(settings[profile_key]).lower():
            raise BuildError(f"Observed Cargo {cargo_key} differs from the selected release-derived profile")
    if settings.get("debug") in (2, "full") and observed.get("debuginfo") != 2:
        raise BuildError("Cargo did not emit the requested full debug information")
    forbidden = DIAGNOSTICS.intersection(artifact.get("features", []))
    if forbidden:
        raise BuildError("Cargo enabled diagnostics transitively: " + ", ".join(sorted(forbidden)))
    if ("tracy" in artifact.get("features", [])) != (instrumentation == "tracy"):
        raise BuildError("Cargo Tracy features differ from the explicit instrumentation mode")


def instrumentation_dependencies(messages, instrumentation):
    """Record features actually compiled, rather than Cargo's dev-unified graph."""
    result = []
    for line in messages.splitlines():
        try:
            item = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (item.get("reason") == "compiler-artifact"
                and item.get("target", {}).get("name") in ("tracy_client", "tracy_client_sys")):
            record = {"package_id": item["package_id"], "target": item["target"]["name"],
                      "features": sorted(item.get("features", []))}
            if record not in result:
                result.append(record)
    if instrumentation == "none" and result:
        raise BuildError("Normal build unexpectedly compiled Tracy instrumentation")
    if instrumentation == "tracy" and {item["target"] for item in result} != {"tracy_client", "tracy_client_sys"}:
        raise BuildError("Tracy build is missing actual client dependency feature proof")
    return result


def find_pdb(binary):
    adjacent = binary.with_suffix(".pdb")
    embedded = Path(read_pe_identity(binary)["rsds"]["pdb_path"])
    failures = []
    for path in dict.fromkeys((adjacent, embedded)):
        if path.is_file():
            try:
                inspect_pair(binary, path)
                return path
            except IdentityError as error:
                failures.append(str(error))
    raise BuildError("Matching PDB not found beside the executable or at its RSDS path: " + "; ".join(failures))


def build(args):
    if sys.platform != "win32":
        raise BuildError("This helper archives Windows MSVC PE/PDB artifacts")
    check_environment(os.environ)
    config = configuration_snapshot(ROOT, os.environ)
    settings = profile_settings(ROOT, args.profile)
    dirty = git("status", "--porcelain=v1", "--untracked-files=all")
    if dirty and not args.allow_dirty:
        raise BuildError("Source checkout is dirty; commit it or explicitly pass --allow-dirty")
    output = args.output.resolve()
    if output.exists() and (not output.is_dir() or any(output.iterdir())):
        raise BuildError("Archive output must be a new or empty directory")
    output.mkdir(parents=True, exist_ok=True)
    sources = source_snapshot()
    commit = git("rev-parse", "HEAD")
    instrumentation = getattr(args, "instrumentation", "none")
    features = parse_features(args.features, instrumentation)
    command = ["cargo", "build", "--locked", "-p", "renderer", "--no-default-features",
               "--profile", args.profile, "--example", args.example,
               "--features", ",".join(features), "--message-format=json"]
    # Cargo's JSON reports an artifact path, not its byte hash. A shared target
    # directory could be replaced by another Cargo build after ours exits. Own
    # a fresh target directory through archival so metadata and bytes belong to
    # this invocation. CARGO_TARGET_DIR selects its parent location only.
    target_base = Path(os.environ.get("CARGO_TARGET_DIR", str(ROOT / "target" / "cpu-profile-build")))
    if not target_base.is_absolute():
        target_base = ROOT / target_base
    target_base = target_base.resolve()
    target = target_base / ("build-" + uuid.uuid4().hex)
    target.mkdir(parents=True, exist_ok=False)
    environment = dict(os.environ, CARGO_INCREMENTAL="0", CARGO_TARGET_DIR=str(target))
    messages = cargo_build(command, environment)
    artifact = select_artifact(messages, args.example)
    check_artifact(artifact, settings, instrumentation)
    dependencies = instrumentation_dependencies(messages, instrumentation)
    if sources != source_snapshot() or config != configuration_snapshot(ROOT, os.environ):
        raise BuildError("Source or Cargo configuration changed during the build; no manifest was written")
    binary = Path(artifact["executable"])
    pdb = find_pdb(binary)
    binary_copy, pdb_copy = output / f"{args.example}.exe", output / f"{args.example}.pdb"
    shutil.copy2(binary, binary_copy)
    shutil.copy2(pdb, pdb_copy)
    pair = inspect_pair(binary_copy, pdb_copy)
    # Re-read the original too: a competing build must not silently replace the
    # source artifacts while they are being copied.
    original = inspect_pair(binary, pdb)
    if any(pair[role]["sha256"] != original[role]["sha256"] for role in ("binary", "pdb")):
        raise BuildError("Cargo artifacts changed while archiving; no manifest was written")
    pair["binary"]["path"], pair["pdb"]["path"] = binary_copy.name, pdb_copy.name
    manifest = {"schema": 1, **pair,
                "build": {"profile": args.profile, "features": features,
                          "instrumentation": instrumentation,
                          "artifact_class": "normal" if instrumentation == "none" else "tracy-instrumented",
                          "instrumentation_dependencies": dependencies,
                          "effective_features": artifact.get("features", []), "example": args.example,
                          "command": command, "rustc": run(["rustc", "-vV"], capture_output=True).stdout.strip(),
                          "cargo": run(["cargo", "-V"], capture_output=True).stdout.strip(),
                          "host": platform.platform(), "cargo_profile": artifact["profile"],
                          "profile_settings": settings, "cargo_configuration": config,
                          "incremental": False, "cargo_target_dir": str(target),
                          "cargo_target_base": str(target_base), "target_directory_ownership": "private-per-build"},
                "git": {"commit": commit, "dirty_status": dirty, "source_hashes": sources}}
    manifest_path = output / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    verify_manifest(manifest_path)
    print(manifest_path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("profiling", "release"), default="profiling")
    parser.add_argument("--example", default="windowed")
    parser.add_argument("--features", default="vulkan,d3d12,presentation")
    parser.add_argument("--instrumentation", choices=("none", "tracy"), default="none",
                        help="Explicit opt-in; Tracy archives are never normal measurement artifacts")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--allow-dirty", action="store_true", help="Record actual dirty source hashes explicitly")
    args = parser.parse_args()
    try:
        build(args)
    except (BuildError, IdentityError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"build: {error}\n")


if __name__ == "__main__":
    main()
