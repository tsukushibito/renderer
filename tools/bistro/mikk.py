"""Build/load the pinned, offline-only MikkTSpace corner tangent generator."""

from __future__ import annotations

import argparse
import _ctypes
import ctypes
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys

import numpy as np


UPSTREAM_COMMIT = "3e895b49d05ea07e4c2133156cfa94369e19e409"
SOURCE = Path(__file__).resolve().parent / "mikktspace"


def source_hashes():
    hashes = {name: hashlib.sha256((SOURCE / name).read_bytes()).hexdigest()
              for name in ("mikktspace.c", "mikktspace.h", "bridge.c")}
    provenance = json.loads((SOURCE / "provenance.json").read_text(encoding="utf-8"))
    if any(hashes[name] != expected for name, expected in provenance["upstream_files_unmodified"].items()):
        raise ValueError("Vendored MikkTSpace differs from its pinned upstream hashes")
    return hashes


def build(output, compiler=None):
    hashes = source_hashes()
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    compiler = compiler or shutil.which("clang")
    if not compiler and sys.platform == "win32":
        candidate = Path("C:/Program Files/LLVM/bin/clang.exe")
        if candidate.is_file():
            compiler = str(candidate)
    if not compiler:
        raise ValueError("Clang is required only to build the offline MikkTSpace helper")
    library = output / ("bistro_mikk.dll" if sys.platform == "win32" else "libbistro_mikk.so")
    flags = ["-O2", "-shared", "-fno-fast-math", "-std=c99"]
    if sys.platform != "win32":
        flags += ["-fPIC"]
    command = [str(compiler), *flags, str(SOURCE / "mikktspace.c"),
               str(SOURCE / "bridge.c"), "-o", str(library)]
    if sys.platform != "win32":
        command.append("-lm")
    subprocess.run(command, check=True, capture_output=True, text=True, encoding="utf-8", errors="replace")
    version = subprocess.run([str(compiler), "--version"], check=True,
                             capture_output=True, text=True, encoding="utf-8", errors="replace").stdout.splitlines()[0]
    record = {"upstream_commit": UPSTREAM_COMMIT, "source_sha256": hashes,
              "compiler": version, "flags": flags, "library": library.name,
              "library_sha256": hashlib.sha256(library.read_bytes()).hexdigest()}
    (output / "build.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
    return library


class Generator:
    def __init__(self, library):
        path = Path(library).resolve()
        record = json.loads((path.parent / "build.json").read_text(encoding="utf-8"))
        if record.get("upstream_commit") != UPSTREAM_COMMIT or record.get("source_sha256") != source_hashes():
            raise ValueError("MikkTSpace helper was not built from the current pinned sources")
        if record.get("library") != path.name or record.get("library_sha256") != hashlib.sha256(path.read_bytes()).hexdigest():
            raise ValueError("MikkTSpace helper binary differs from its build record")
        self.build_record = record
        self._library = ctypes.CDLL(str(path))
        self._handle = self._library._handle
        self._generate = self._library.bistro_mikktspace
        pointer = np.ctypeslib.ndpointer(dtype=np.float32, flags="C_CONTIGUOUS")
        self._generate.argtypes = [pointer, ctypes.c_int, pointer]
        self._generate.restype = ctypes.c_int

    def close(self):
        if self._handle is not None:
            handle, self._handle = self._handle, None
            del self._generate
            del self._library
            if sys.platform == "win32":
                _ctypes.FreeLibrary(handle)
            else:
                _ctypes.dlclose(handle)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()

    def __call__(self, vertices):
        if self._handle is None:
            raise ValueError("MikkTSpace generator is closed")
        vertices = np.ascontiguousarray(vertices, dtype=np.float32)
        if vertices.ndim != 2 or vertices.shape[1] != 8 or not len(vertices) or len(vertices) % 3:
            raise ValueError("MikkTSpace requires unindexed triangle pos3/normal3/UV2 corners")
        if len(vertices) > (2**31 - 1) // 8 or not np.isfinite(vertices).all():
            raise ValueError("MikkTSpace input exceeds bridge limits or is non-finite")
        output = np.full((len(vertices), 4), np.nan, dtype=np.float32)
        status = self._generate(vertices, len(vertices), output)
        if status != 1:
            raise ValueError(f"MikkTSpace generation failed (status {status}); not a fallback success")
        return output


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--compiler")
    arguments = parser.parse_args()
    print(build(arguments.output, arguments.compiler))
