"""Check that packaged Pipeline shaders match their generated manifest without an SDK."""

import argparse
import hashlib
import json
from pathlib import Path
import re

from build_material_programs import hash_file, object_pairs


SHADERS = Path("crates/sashiko-renderer/shaders")
RENDERER_SHADERS = Path("crates/sashiko-renderer-core/shaders")
ARTIFACT_SUFFIXES = ("spv", "dxil", "reflection.txt", "spirv-reflection.json")
ASSET_SUFFIXES = (".hlsl", ".hlsli", *(f".{suffix}" for suffix in ARTIFACT_SUFFIXES))


def output_hashes(renderer_manifest: dict) -> dict[str, str]:
    """The shared GLSL generator records these fragments alongside the fullscreen vertex stage."""
    expected = {}
    prefix = "../../sashiko-renderer/shaders/output/"
    for key, record in renderer_manifest["artifacts"].items():
        if not key.startswith(prefix):
            continue
        variant = key.removeprefix(prefix)
        if not re.fullmatch(r"(?:tone_neutral|sdr(?:_fxaa)?(?:_linear)?)\.frag", variant):
            raise ValueError(f"Unknown standard output variant: {variant}")
        source = variant.replace("_linear.frag", ".frag")
        name = f"output/{source}"
        if expected.setdefault(name, record["source_utf8_lf"]) != record["source_utf8_lf"]:
            raise ValueError(f"Variants disagree on the SHA-256 of {name}")
        expected[f"output/{variant}.spv"] = record["spirv"]
        for suffix, field in [
            ("hlsl", "hlsl"),
            ("dxil", "dxil"),
            ("reflection.txt", "reflection"),
            ("spirv-reflection.json", "spirv_reflection"),
        ]:
            expected[f"output/dxil/{variant}.{suffix}"] = record[field]
    for license_name, digest in renderer_manifest["licenses"].items():
        if not re.fullmatch(r"[A-Za-z0-9.-]+\.txt", license_name):
            raise ValueError(f"Invalid standard output license name: {license_name}")
        expected[f"output/licenses/{license_name}"] = digest
    return expected


def verify(root: Path) -> tuple[int, int]:
    shaders = root / SHADERS
    renderer_shaders = root / RENDERER_SHADERS
    manifest = json.loads(
        (shaders / "manifest.json").read_text(encoding="utf-8"),
        object_pairs_hook=object_pairs,
    )
    if type(manifest["schema"]) is not int or manifest["schema"] != 2:
        raise ValueError("Expected Pipeline shader manifest schema 2")
    artifacts = manifest["artifacts"]
    includes = manifest["includes"]
    if not isinstance(artifacts, dict) or not artifacts or not isinstance(includes, dict):
        raise ValueError("Pipeline manifest requires an artifact table and an include table")

    sources = {}
    generated = {}
    for name, artifact in artifacts.items():
        if not re.fullmatch(r"[a-zA-Z0-9_-]+\.(vert|frag|comp)", name):
            raise ValueError(f"Invalid shader variant name: {name!r}")
        source = artifact["source"]
        stage = name.rsplit(".", 1)[1]
        if not re.fullmatch(rf"[a-zA-Z0-9_-]+\.{stage}\.hlsl", source):
            raise ValueError(f"Invalid source for {name}: {source!r}")
        source_hash = artifact["source_utf8_lf"]
        if sources.setdefault(source, source_hash) != source_hash:
            raise ValueError(f"Variants disagree on the SHA-256 of {source}")
        for suffix in ARTIFACT_SUFFIXES:
            filename = f"{name}.{suffix}"
            path = filename if suffix == "spv" else f"dxil/{filename}"
            generated[path] = artifact[suffix]
    for name in includes:
        if not re.fullmatch(r"[a-zA-Z0-9_-]+\.hlsli", name):
            raise ValueError(f"Invalid shader include name: {name!r}")

    for field, path in (
        ("toolchain_sha256", renderer_shaders / "dxil-toolchain.json"),
        ("renderer_shaders_sha256", renderer_shaders / "dxil/manifest.json"),
    ):
        if hash_file(path) != manifest[field]:
            raise ValueError(f"SHA-256 differs from the Pipeline shader manifest: {path.relative_to(root)}")
    renderer_manifest = json.loads(
        (renderer_shaders / "dxil/manifest.json").read_text(encoding="utf-8"),
        object_pairs_hook=object_pairs,
    )
    expected = sources | includes | generated | output_hashes(renderer_manifest)
    output = shaders / "output"
    actual = {
        path.relative_to(shaders).as_posix()
        for path in shaders.rglob("*")
        if path.is_file() and (path.name.lower().endswith(ASSET_SUFFIXES) or path.is_relative_to(output))
    }
    if expected.keys() != actual:
        missing = sorted(expected.keys() - actual)
        unlisted = sorted(actual - expected.keys())
        raise ValueError(f"Pipeline shader inventory differs: missing={missing}, unlisted={unlisted}")

    for name, digest in expected.items():
        path = shaders / name
        if path.suffix in (".spv", ".dxil"):
            actual_digest = hash_file(path)
        else:
            # The generator writes UTF-8 without a BOM and normalizes CRLF to LF.
            data = path.read_bytes().decode("utf-8-sig").replace("\r\n", "\n").encode("utf-8")
            actual_digest = hashlib.sha256(data).hexdigest()
        if actual_digest != digest:
            raise ValueError(f"SHA-256 differs from the Pipeline shader manifest: {SHADERS / name}")

    return sum(name.endswith(".spv") for name in expected), len(includes)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    try:
        artifacts, includes = verify(args.root)
    except (KeyError, OSError, TypeError, ValueError) as error:
        parser.exit(1, f"{error}\nRegenerate and verify Pipeline shaders with the pinned SDK.\n")
    print(f"Verified Pipeline shader manifest for {artifacts} variants and {includes} includes.")


if __name__ == "__main__":
    main()
