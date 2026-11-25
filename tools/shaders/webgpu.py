"""Regenerate the checked-in browser shaders from the native shader build inputs."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import struct
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
CORE = ROOT / "crates/sashiko-renderer-core/shaders"
STANDARD = ROOT / "crates/sashiko-renderer/shaders"
MANIFEST = Path(__file__).with_name("webgpu-manifest.json")


def run(args):
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            encoding="utf-8", errors="replace")
    if result.returncode:
        raise RuntimeError(f"{' '.join(map(str, args))}\n{result.stdout}")
    return result.stdout.strip()


def generate(module, directory, args, recipe):
    name = module.stem
    out = directory / (name + ".wgsl")
    # DXC lowers isfinite(float3) in the lighting include to a vector OpLogicalOr,
    # which Naga 30 rejects. Slang compiles those stages directly from the same HLSL.
    if name.endswith(".frag") and module.parent == STANDARD and (
        name.startswith(("pbr", "deferred_", "gbuffer", "toon"))
    ):
        shader = recipe[name]
        # The CPU writes columns as consecutive HLSL rows. Set the default
        # explicitly for matrix arrays as well as individual row_major members.
        run([args.slang, STANDARD / shader["source"], "-entry", "main", "-stage", "fragment",
             "-target", "wgsl", "-matrix-layout-row-major", "-D__spirv__=1",
             *["-D" + d for d in shader["defines"]], "-o", out])
    elif name == "frustum_visibility.comp":
        run([args.slang, STANDARD / recipe[name]["source"], "-entry", "main",
             "-stage", "compute", "-target", "wgsl", "-matrix-layout-row-major", "-o", out])
    elif (module.parent == CORE and name.endswith(".comp")) or name == "skin.comp":
        # The storage buffer shader only writes. WebGPU exposes read_write
        # as the declaration and has no write-only storage buffer type.
        assembly = run(["spirv-dis", module])
        assembly = re.sub(r"(?m)^.*Op(?:Member)?Decorate .* NonReadable\s*$", "", assembly)
        assembly_file = directory / "storage.spvasm"
        adjusted = directory / "storage.spv"
        assembly_file.write_text(assembly, encoding="utf-8")
        run(["spirv-as", "--target-env", "spv1.3", assembly_file, "-o", adjusted])
        run([args.naga, adjusted, out])
    else:
        run([args.naga, module, out])
    source = out.read_text(encoding="utf-8")
    # Inline words use a per-draw aligned uniform slice on WebGPU.
    source = source.replace("var<immediate>", "@group(0) @binding(255) var<uniform>")
    # This Slang revision prints its HLSL qualifier verbatim, but WGSL
    # has no precise qualifier. Keep each multiplication/addition explicit.
    source = source.replace("precise var ", "var ")
    # Slang can duplicate an interpolation qualifier onto a private helper
    # struct. Only the actual stage IO (with a location) has interpolation.
    source = re.sub(r"(?m)^( *)@interpolate\(flat\) +(?=\w+\s*:)", r"\1", source)
    source = "\n".join(line.rstrip() for line in source.splitlines()).rstrip() + "\n"
    out.write_text(source, encoding="utf-8", newline="\n")
    return out.read_bytes()


def shader_interface(module):
    """Read only the ABI instructions from a compiler-produced SPIR-V module."""
    data = module.read_bytes()
    words = struct.unpack(f"<{len(data) // 4}I", data)
    groups, bindings = {}, {}
    has_inline = False
    stage = local_size = None
    position = 5  # little-endian module header
    while position < len(words):
        header = words[position]
        word_count, opcode = header >> 16, header & 0xffff
        if not word_count or position + word_count > len(words):
            raise ValueError(f"invalid instruction in {module}")
        operands = words[position + 1: position + word_count]
        position += word_count
        if opcode == 15:  # OpEntryPoint, execution model
            stage = operands[0]
        elif opcode == 16 and operands[1] == 17:  # OpExecutionMode LocalSize
            local_size = operands[2:]
        elif opcode == 71 and operands[1] == 33:  # OpDecorate Binding
            bindings[operands[0]] = operands[2]
        elif opcode == 71 and operands[1] == 34:  # OpDecorate DescriptorSet
            groups[operands[0]] = operands[2]
        elif opcode == 59 and operands[2] == 9:  # OpVariable PushConstant
            has_inline = True
    slots = {(groups[id_], binding) for id_, binding in bindings.items()}
    if has_inline:
        slots.add((0, 255))
    return stage, local_size, slots


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--naga", default="naga")
    parser.add_argument("--slang", default="slangc")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="regenerate and compare every module")
    mode.add_argument("--validate-packaged", action="store_true",
                      help="check source/output digests and validate WGSL with Naga alone")
    args = parser.parse_args()
    if run([args.naga, "--version"]) != "30.0.0":
        parser.error("naga-cli 30.0.0 required")
    if not args.validate_packaged and run([args.slang, "-version"]) != "2026.1-52-gc8ddf20bb":
        parser.error("slangc from Vulkan SDK 1.4.341.1 required")
    recipe = json.loads((STANDARD / "manifest.json").read_bytes())["artifacts"]
    modules = [
        module for folder in (CORE, STANDARD, STANDARD / "output")
        for module in sorted(folder.glob("*.spv"))
        if not module.name.startswith("bake_") and module.name != "composite.frag.spv"
    ]
    # The native manifest fixes the HLSL file and defines for each Slang stage.
    # Include source contents as well, so checking doesn't depend on a native rebuild.
    inputs = [Path(__file__).resolve(), STANDARD / "manifest.json", *modules,
              *sorted(STANDARD.glob("*.hlsl")), *sorted(STANDARD.glob("*.hlsli"))]
    outputs = [module.with_suffix(".wgsl") for module in modules]
    with tempfile.TemporaryDirectory() as temp:
        for module, dest in zip(modules, outputs):
            if not args.validate_packaged:
                data = generate(module, Path(temp), args, recipe)
                if args.check and dest.read_bytes() != data:
                    raise SystemExit(f"stale WGSL: {dest.relative_to(ROOT)}")
                if not args.check:
                    dest.write_bytes(data)
            # Compile WGSL independently, then compare reflected resource slots,
            # stage and local size to the previously reviewed native module.
            compiled = Path(temp) / "wgsl-validation.spv"
            run([args.naga, dest, compiled])
            native, browser = shader_interface(module), shader_interface(compiled)
            if native != browser:
                raise SystemExit(f"{dest.name}: incompatible WGSL interface: {browser} != {native}")
    manifest = {"input_sha256": digest(inputs), "wgsl_sha256": digest(outputs)}
    if args.check or args.validate_packaged:
        if manifest != json.loads(MANIFEST.read_bytes()):
            raise SystemExit("stale WGSL package; run tools/shaders/webgpu.py")
    else:
        MANIFEST.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(f"Validated {len(modules)} packaged WGSL modules.")


def digest(files):
    index = hashlib.sha256()
    for path in sorted(files, key=lambda p: p.as_posix()):
        data = path.read_bytes()
        if path.suffix != ".spv":
            data = data.replace(b"\r\n", b"\n")
        index.update(path.relative_to(ROOT).as_posix().encode() + b"\0" + hashlib.sha256(data).digest())
    return index.hexdigest()


if __name__ == "__main__":
    main()
