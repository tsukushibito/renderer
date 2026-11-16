"""Resolve actual sampled instruction pointers with a verified Rust EXE/PDB.

The caller must obtain the ASLR image base from the captured process/image
lifetime. A PE preferred base or a decoded function entry is not a substitute.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys


def resolve_addresses(manifest_path, image_base, addresses, symbolizer, runner=subprocess.run):
    from identity import verify_manifest
    manifest_path = Path(manifest_path).resolve()
    manifest = verify_manifest(manifest_path)
    binary = (manifest_path.parent / manifest["binary"]["path"]).resolve()
    size = manifest["binary"]["size_of_image"]
    env = dict(os.environ)
    env.pop("LLVM_SYMBOLIZER_OPTS", None)
    results = []
    for address in addresses:
        ip = int(address, 0)
        record = {"ip": f"0x{ip:x}", "captured_image_base": f"0x{image_base:x}"}
        if not image_base <= ip < image_base + size:
            record["status"] = "outside_verified_executable"
            results.append(record)
            continue
        rva = ip - image_base
        command = [str(symbolizer), "--obj=" + str(binary), "--relative-address", "--inlines", "--no-debuginfod", "--output-style=JSON", hex(rva)]
        completed = runner(command, check=False, capture_output=True, text=True, encoding="utf-8", timeout=30, env=env)
        if completed.returncode:
            raise ValueError(f"llvm-symbolizer failed ({completed.returncode}): {completed.stderr}")
        decoded = json.loads(completed.stdout)
        # Positional addresses produce a JSON array (stdin uses one object per
        # input line). Require one exact address result instead of guessing.
        if not isinstance(decoded, list) or len(decoded) != 1 or not isinstance(decoded[0], dict):
            raise ValueError("Unsupported llvm-symbolizer JSON schema")
        decoded = decoded[0]
        if int(decoded.get("Address", "-1"), 0) != rva or not isinstance(decoded.get("Symbol"), list):
            raise ValueError("llvm-symbolizer returned a different instruction address")
        if os.path.normcase(str(Path(decoded.get("ModuleName", "")).resolve())) != os.path.normcase(str(binary)):
            raise ValueError("llvm-symbolizer returned a different module")
        frames = decoded["Symbol"]
        record.update(rva=f"0x{rva:x}", frames=frames,
                      status="line_available" if any(frame.get("FileName") and frame.get("Line", 0) > 0 for frame in frames)
                      else "source_line_unresolved")
        results.append(record)
    return {"schema": 1, "manifest": str(manifest_path), "binary_sha256": manifest["binary"]["sha256"],
            "pdb_sha256": manifest["pdb"]["sha256"], "addresses": results,
            "limits": ["Image base must identify this executable in the target process at the sample timestamp.",
                       "Optimized/inlined source locations may be ambiguous; preserve raw IP and RVA.",
                       "Outside-module and unresolved source locations remain explicit."]}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--image-base", type=lambda value: int(value, 0), required=True,
                        help="Actual ASLR base from target Image_Load/lifetime evidence, never PE preferred base")
    parser.add_argument("--summary", type=Path, required=True)
    parser.add_argument("--top", type=int, default=50)
    parser.add_argument("--symbolizer", type=Path, default=Path("C:/Program Files/LLVM/bin/llvm-symbolizer.exe"))
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        if args.image_base <= 0 or args.top <= 0:
            raise ValueError("--image-base and --top must be positive")
        summary = json.loads(args.summary.read_text(encoding="utf-8"))
        entries = summary["sampled"]["instruction_pointers"][:args.top]
        addresses = [entry["name"] for entry in entries]
        report = resolve_addresses(args.manifest, args.image_base, addresses, args.symbolizer)
        for resolved, entry in zip(report["addresses"], entries):
            resolved["sample_weight_ms"] = entry["weight_ms"]
        report["selected_ip_weight_ms"] = sum(entry["weight_ms"] for entry in entries)
        report["total_sample_weight_ms"] = summary["sampled"]["weight_ms"]
        args.output.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(args.output)
        return 0
    except (ValueError, OSError, subprocess.SubprocessError, KeyError) as error:
        print(f"cpu-profile symbolize: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
