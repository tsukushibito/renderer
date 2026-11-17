"""Build the benchmark-only Godot CPU clock offline with MSVC x64.

The caller must hold the shared B3 exclusive-work lock during compilation and
Godot commands. This helper never downloads a compiler, engine, or dependency.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


SOURCE = Path(__file__).resolve().parent
COMMIT = "ed1daf0bf001b61586d9930840f2f1394092c079"
ABI_SIZES = {"Variant": 24, "StringName": 8, "String": 8}
ABI_FILES = {"gdextension_interface.h", "gdextension_interface.json", "gdextension_interface_header_generator.cpp", "LICENSE.txt"}


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def record(path, relative_to):
    return {"path": Path(path).relative_to(relative_to).as_posix(),
            "sha256": digest(path), "byte_length": Path(path).stat().st_size}


def write(path, value):
    Path(path).write_text(json.dumps(value, ensure_ascii=False, allow_nan=False, indent=2) + "\n", encoding="utf-8")


def compiler_environment():
    vswhere = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Microsoft Visual Studio/Installer/vswhere.exe"
    install = subprocess.check_output([str(vswhere), "-latest", "-products", "*", "-requires",
        "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "installationPath"], text=True).strip()
    if not install:
        raise ValueError("MSVC x64 build tools were not found")
    setup = Path(install) / "VC/Auxiliary/Build/vcvars64.bat"
    # Installation path is an argument to one cmd invocation, never a list of
    # shell-built file operations. Reject cmd metacharacters before using it.
    if any(character in str(setup) for character in '\"&|<>^%\r\n'):
        raise ValueError("Unsupported compiler setup path")
    command = f'call "{setup}" >nul && set'
    # Pass cmd's command tail as native command text. Python list2cmdline uses
    # CRT quote escaping, which cmd's batch parser does not understand.
    shell_path = os.environ.get("COMSPEC", "cmd.exe")
    if any(character in shell_path for character in '\"&|<>^%\r\n'):
        raise ValueError("Unsupported command processor path")
    result = subprocess.check_output(f'"{shell_path}" /d /s /c "{command}"', text=True)
    environment = {key.upper(): value for key, value in os.environ.items()}
    environment.update((key.upper(), value) for key, value in
        (line.split("=", 1) for line in result.splitlines() if "=" in line and not line.startswith("=")))
    environment["VSLANG"] = "1033"
    compiler = shutil.which("cl.exe", path=environment["PATH"])
    if not compiler:
        raise ValueError("MSVC compiler was not found after vcvars64")
    return environment, Path(compiler)


def verify_abi_sources():
    provenance = json.loads((SOURCE / "vendor/provenance.json").read_text(encoding="utf-8"))
    if (provenance["godot_commit"] != COMMIT or provenance["builtin_sizes"] != ABI_SIZES
            or provenance["builtin_configuration"] != "float_64"
            or {entry["path"] for entry in provenance["files"]} != ABI_FILES
            or len(provenance["files"]) != len(ABI_FILES)):
        raise ValueError("CPU clock ABI provenance differs from the pinned engine")
    for entry in provenance["files"]:
        candidate = (SOURCE / "vendor" / entry["path"]).resolve()
        if candidate.parent != (SOURCE / "vendor").resolve() or record(candidate, SOURCE / "vendor") != entry:
            raise ValueError("Vendored CPU clock ABI source changed")
    return provenance


def build(output):
    if sys.platform != "win32":
        raise ValueError("CPU clock DLL requires Windows x86_64 MSVC")
    output = Path(output).resolve()
    if output.exists():
        raise ValueError("CPU clock build output must be a new directory")
    provenance = verify_abi_sources()
    environment, compiler = compiler_environment()
    output.mkdir(parents=True)
    binary = output / "b3_cpu_clock.dll"
    command = [str(compiler), "/nologo", "/TC", "/std:c11", "/W4", "/WX", "/O2", "/MD", "/GS",
        "/guard:cf", "/LD", "/Brepro", str(SOURCE / "b3_cpu_clock.c"),
        f"/Fo{output / 'b3_cpu_clock.obj'}", f"/Fe{binary}", "/link", "/MACHINE:X64",
        "/INCREMENTAL:NO", "/OPT:REF", "/OPT:ICF", "/DYNAMICBASE", "/NXCOMPAT", "/guard:cf", "/Brepro"]
    result = subprocess.run(command, env=environment, cwd=output, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, check=False)
    (output / "build.log").write_bytes(result.stdout)
    if result.returncode:
        raise ValueError(f"CPU clock compilation failed ({result.returncode}); see {output / 'build.log'}")
    version = subprocess.run([str(compiler), "/Bv"], env=environment, encoding="utf-8", errors="replace",
                             stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False)
    shutil.copyfile(SOURCE / "b3_cpu_clock.gdextension", output / "b3_cpu_clock.gdextension")
    sources = [record(path, SOURCE) for path in sorted(SOURCE.rglob("*"))
               if path.is_file() and "__pycache__" not in path.parts]
    manifest = {"schema_version": 1, "kind": "b3-benchmark-only-cpu-clock", "godot_version": "4.7.2",
        "godot_commit": COMMIT, "abi_provenance": provenance,
        "files": [record(output / name, output) for name in ("b3_cpu_clock.dll", "b3_cpu_clock.gdextension")],
        "compiler": {"path": str(compiler), "sha256": digest(compiler), "version_output": version.stdout,
                     "command": command}, "sources": sources,
        "snapshot_fields": ["qpc", "qpc_frequency", "process_kernel_100ns", "process_user_100ns",
                            "caller_thread_kernel_100ns", "caller_thread_user_100ns", "caller_thread_id"],
        "boundary_order": "QPC_then_frequency_then_process_times_then_caller_thread_times",
        "failure_contract": "negative qpc field and Win32 error in field 1; Godot emits ERROR; never a valid sample",
        "scope": "Two ROI-boundary calls only; process includes all process threads. Caller-thread is not all rendering workers. CPU counters are charged time, not elapsed/wait; no per-frame CPU distribution."}
    write(output / "manifest.json", manifest)
    return output / "manifest.json"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    print(build(args.output))


if __name__ == "__main__":
    main()
