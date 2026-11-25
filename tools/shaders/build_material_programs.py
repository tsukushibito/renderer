"""Build user-defined HLSL Raster templates with material shader functions."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
from tempfile import TemporaryDirectory


TOOLCHAIN = Path(__file__).resolve().parents[2] / "crates/sashiko-renderer-core/shaders/dxil-toolchain.json"
SPIRV_ARGUMENTS = ["-spirv", "-fspv-target-env=vulkan1.1", "-fvk-stage-io-order=decl"]
PROFILES = {
    "vertex": ("vs_6_0", "vert", "; Vertex Shader"),
    "fragment": ("ps_6_0", "frag", "; Pixel Shader"),
}


def object_pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def expect_fields(value, required, optional=()):
    if not isinstance(value, dict):
        raise ValueError("expected an object in the material build manifest")
    missing = set(required) - value.keys()
    extra = value.keys() - set(required) - set(optional)
    if missing or extra:
        raise ValueError(f"missing manifest fields {sorted(missing)}, unknown fields {sorted(extra)}")


def source_path(root, value, *, directory=False):
    if not isinstance(value, str) or not value:
        raise ValueError("source paths must be non-empty strings")
    relative = Path(value)
    if relative.is_absolute() or relative.drive:
        raise ValueError(f"use a path relative to the build manifest: {value!r}")
    path = (root / relative).resolve(strict=True)
    if (directory and not path.is_dir()) or (not directory and not path.is_file()):
        raise ValueError(f"expected a {'directory' if directory else 'file'}: {path}")
    if any(character in path.as_posix() for character in ('"', '\n', '\r')):
        raise ValueError(f"path cannot be represented as an HLSL include: {path}")
    return path


def read_plan(path):
    data = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=object_pairs)
    expect_fields(data, ("schema_version", "templates", "materials", "programs"), ("include_dirs",))
    if type(data["schema_version"]) is not int or data["schema_version"] != 1:
        raise ValueError("expected material build manifest schema_version 1")
    if not all(isinstance(data[key], dict) for key in ("templates", "materials")):
        raise ValueError("templates and materials must be objects keyed by logical name")
    root = path.parent
    templates = {}
    for name, sources in data["templates"].items():
        if not name.strip():
            raise ValueError("template logical names cannot be empty")
        expect_fields(sources, ("vertex",), ("fragment",))
        templates[name] = {stage: source_path(root, value) for stage, value in sources.items()}
    materials = {}
    for name, value in data["materials"].items():
        if not name.strip():
            raise ValueError("material logical names cannot be empty")
        materials[name] = source_path(root, value)
    include_dirs = data.get("include_dirs", [])
    if not isinstance(include_dirs, list):
        raise ValueError("include_dirs must be an array of paths")
    includes = [source_path(root, directory, directory=True) for directory in include_dirs]
    if not isinstance(data["programs"], list) or not data["programs"]:
        raise ValueError("programs must be a non-empty array of template/material pairs")
    pairs = set()
    jobs = []
    for selection in data["programs"]:
        expect_fields(selection, ("template", "material"))
        template, material = selection["template"], selection["material"]
        if not isinstance(template, str) or template not in templates:
            raise ValueError(f"unknown template {template!r}")
        if not isinstance(material, str) or material not in materials:
            raise ValueError(f"unknown material {material!r}")
        if (template, material) in pairs:
            raise ValueError(f"duplicate program for {template!r} and {material!r}")
        pairs.add((template, material))
        jobs.append((template, material, templates[template], materials[material]))
    return jobs, includes


def hash_file(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def run(executable, arguments, *, revision=False):
    result = subprocess.run([executable, *arguments], capture_output=True, check=False)
    output = result.stdout.decode("utf-8", errors="replace")
    errors = result.stderr.decode("utf-8", errors="replace")
    if result.returncode or (errors.strip() and not revision):
        raise ValueError(f"{executable.name} failed ({result.returncode})\n{output}{errors}")
    return (output + errors).replace("\r\n", "\n")


def pinned_tools(sdk_root, lock):
    tools = {}
    for filename in ("dxc.exe", "dxcompiler.dll", "spirv-val.exe", "spirv-cross.exe"):
        path = sdk_root / "Bin" / filename
        if not path.is_file() or hash_file(path) != lock["files"][filename]:
            raise ValueError(f"missing or changed pinned tool: {path}")
        tools[filename] = path
    for tool, expected, flag in (
        ("dxc.exe", lock["dxc_version"], "--version"),
        ("spirv-cross.exe", lock["spirv_cross_revision"], "--revision"),
    ):
        if run(tools[tool], [flag], revision=True).strip() != expected:
            raise ValueError(f"unexpected revision for {tool}")
    return tools


def preprocessed_hash(dxc, arguments, source, output):
    run(dxc, ["-P", "-Fi", output, *arguments, source])
    # The compiler includes absolute paths in #line directives. The expanded
    # source hash is portable across checkout paths while covering transitive
    # include contents and the backend's predefined macros.
    source_text = output.read_text(encoding="utf-8")
    source_text = re.sub(r"^#line [0-9]+[^\n]*\n", "", source_text, flags=re.M)
    return hashlib.sha256(source_text.encode("utf-8")).hexdigest()


def build_stage(stage, template, material, location, bundle, scratch, arguments, tools):
    profile, spirv_stage, dxil_stage = PROFILES[stage]
    dxc = tools["dxc.exe"]
    wrapper = scratch / "compose.hlsl"
    wrapper.write_text(
        f'#define RENDERER_MATERIAL_INCLUDE "{material.as_posix()}"\n'
        f'#include "{template.as_posix()}"\n',
        encoding="utf-8",
        newline="\n",
    )
    arguments = [*arguments, "-T", profile]
    spirv_arguments = [*arguments, *SPIRV_ARGUMENTS]
    spirv = location / f"{stage}.spv"
    dxil = location / f"{stage}.dxil"
    spirv_layout = location / f"{stage}.spirv-reflection.json"
    dxil_layout = location / f"{stage}.dxil-reflection.txt"
    expanded = scratch / "expanded.hlsl"
    spirv_source_sha256 = preprocessed_hash(dxc, spirv_arguments, wrapper, expanded)
    run(dxc, [*spirv_arguments, expanded, "-Fo", spirv])
    with spirv.open("rb") as stream:
        if stream.read(8) != bytes.fromhex("0302230700030100"):
            raise ValueError("DXC did not emit little-endian SPIR-V 1.3")
    run(tools["spirv-val.exe"], ["--target-env", "vulkan1.3", spirv])
    reflection = run(tools["spirv-cross.exe"], [spirv, "--reflect"])
    entry_points = json.loads(reflection)["entryPoints"]
    if entry_points != [{"name": "main", "mode": spirv_stage}]:
        raise ValueError(f"unexpected {stage} SPIR-V entry point(s): {entry_points!r}")
    spirv_layout.write_text(reflection, encoding="utf-8", newline="\n")
    dxil_source_sha256 = preprocessed_hash(dxc, arguments, wrapper, expanded)
    run(dxc, [*arguments, expanded, "-Fo", dxil])
    dump = run(dxc, ["-dumpbin", dxil])
    reflection, separator, _ = dump.partition("target datalayout = ")
    if not separator or "!dx.valver = " not in dump or "; shader hash: " not in reflection:
        raise ValueError("DXIL validator or reflection metadata missing")
    reflection_lines = reflection.splitlines()
    if dxil_stage not in reflection_lines or "; EntryFunctionName: main" not in reflection_lines:
        raise ValueError(f"unexpected {stage} DXIL entry point or shader stage")
    dxil_layout.write_text(
        re.sub(r"[ \t]+$", "", reflection, flags=re.M), encoding="utf-8", newline="\n"
    )
    return {
        "spv": spirv.relative_to(bundle).as_posix(),
        "dxil": dxil.relative_to(bundle).as_posix(),
        "spv_sha256": hash_file(spirv),
        "dxil_sha256": hash_file(dxil),
        "spirv_reflection": spirv_layout.relative_to(bundle).as_posix(),
        "dxil_reflection": dxil_layout.relative_to(bundle).as_posix(),
        "spirv_reflection_sha256": hash_file(spirv_layout),
        "dxil_reflection_sha256": hash_file(dxil_layout),
        "spirv_source_sha256": spirv_source_sha256,
        "dxil_source_sha256": dxil_source_sha256,
    }


def build(plan, output, sdk_root):
    jobs, includes = read_plan(plan)
    lock = json.loads(TOOLCHAIN.read_text(encoding="utf-8"))
    tools = pinned_tools(sdk_root.resolve(strict=True), lock)
    output = output.absolute()
    if output.exists() or output.is_symlink():
        raise ValueError(f"output bundle already exists: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    selected_include_dirs = [argument for directory in includes for argument in ("-I", directory)]
    arguments = [*lock["dxc_arguments"], *selected_include_dirs]
    manifest = {
        "schema_version": 1,
        "toolchain_sha256": hash_file(TOOLCHAIN),
        "dxc_arguments": lock["dxc_arguments"],
        "spirv_arguments": SPIRV_ARGUMENTS,
        "programs": [],
    }
    # Stage on the same volume and publish the complete bundle in one rename.
    with TemporaryDirectory(prefix="renderer-material-", dir=output.parent) as temporary:
        scratch = Path(temporary)
        bundle = scratch / "bundle"
        for index, (template, material, stages, material_file) in enumerate(jobs):
            location = bundle / "programs" / str(index)
            location.mkdir(parents=True)
            program = {"template": template, "material": material, "vertex": None, "fragment": None}
            for stage, source in stages.items():
                try:
                    program[stage] = build_stage(
                        stage, source, material_file, location, bundle, scratch, arguments, tools
                    )
                except (ValueError, OSError) as error:
                    raise ValueError(f"{template} / {material} / {stage}: {error}") from error
            manifest["programs"].append(program)
        (bundle / "manifest.json").write_text(
            json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n"
        )
        bundle.rename(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True, type=Path, help="material build inputs (JSON)")
    parser.add_argument("--output", required=True, type=Path, help="new bundle directory")
    parser.add_argument("--sdk-root", type=Path, default=os.environ.get("VULKAN_SDK"))
    args = parser.parse_args()
    if args.sdk_root is None:
        parser.error("set VULKAN_SDK or pass --sdk-root for the pinned Vulkan SDK")
    try:
        build(args.manifest.resolve(strict=True), args.output, args.sdk_root)
    except (OSError, ValueError) as error:
        parser.exit(1, f"material build: {error}\n")


if __name__ == "__main__":
    main()
