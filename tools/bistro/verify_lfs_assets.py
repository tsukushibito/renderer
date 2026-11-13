"""Verify Bistro LFS payload hashes, or unhydrated pointers in lightweight CI."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re


def verify(root, *, pointers=False):
    root = root.resolve()
    manifest = json.loads((root / "assets/bistro/manifest.json").read_text(encoding="utf-8"))
    if manifest["schema_version"] != 1:
        raise ValueError("Unsupported Bistro manifest version")
    expected = set()
    total = 0
    for entry in manifest["files"]:
        name, size, digest = entry["path"], entry["size"], entry["sha256"]
        relative = PurePosixPath(name)
        if relative.is_absolute() or ".." in relative.parts or "\\" in name or ":" in name:
            raise ValueError(f"Invalid manifest path: {name}")
        if name in expected or not re.fullmatch(r"[0-9a-f]{64}", digest) or type(size) is not int or size < 0:
            raise ValueError(f"Invalid or duplicate manifest entry: {name}")
        expected.add(name)
        path = (root / name).resolve()
        if not path.is_relative_to(root):
            raise ValueError(f"Asset escapes repository: {name}")
        if pointers:
            pointer = f"version https://git-lfs.github.com/spec/v1\noid sha256:{digest}\nsize {size}\n".encode()
            if path.stat().st_size != len(pointer) or path.read_bytes() != pointer:
                raise ValueError(f"LFS pointer differs from manifest: {name}")
        else:
            if path.stat().st_size != size:
                raise ValueError(f"Asset size differs (run git lfs pull if still a pointer): {name}")
            with path.open("rb") as stream:
                if hashlib.file_digest(stream, "sha256").hexdigest() != digest:
                    raise ValueError(f"Asset SHA-256 differs: {name}")
        total += size
    actual = {path.relative_to(root).as_posix()
              for pattern in ("assets/bistro/source/*.zip", "assets/bistro/converted/**/*",
                              "assets/bistro/environment/*.hdr", "docs/verification/bistro-assets-preview.png")
              for path in root.glob(pattern) if path.is_file()}
    if actual != expected:
        raise ValueError(f"Manifest coverage differs: missing={sorted(expected - actual)}, extra={sorted(actual - expected)}")
    return {"passed": True, "mode": "pointers" if pointers else "payloads", "files": len(expected), "bytes": total}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--pointers", action="store_true", help="Require canonical pointers instead of downloading assets")
    args = parser.parse_args()
    try:
        print(json.dumps(verify(args.root, pointers=args.pointers)))
    except (ValueError, OSError, KeyError, TypeError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
