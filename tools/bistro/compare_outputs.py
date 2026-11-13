"""Compare every file linked by two independently regenerated glTF exports."""
import argparse
import hashlib
import json
from pathlib import Path
from urllib.parse import unquote, urlsplit


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def compare(first, second):
    model = json.loads(first.read_text(encoding="utf-8"))
    paths = {unquote(item["uri"]) for item in model.get("buffers", []) + model.get("images", [])}
    pairs = [(first.name, first, second)]
    for name in sorted(paths):
        uri = urlsplit(name)
        if uri.scheme or uri.netloc or uri.query or uri.fragment:
            raise ValueError(f"Unsupported external resource: {name}")
        a, b = (first.parent / name).resolve(), (second.parent / name).resolve()
        if not a.is_relative_to(first.parent.resolve()) or not b.is_relative_to(second.parent.resolve()):
            raise ValueError(f"Resource escapes export directory: {name}")
        pairs.append((name, a, b))
    files = []
    for name, a, b in pairs:
        first_hash, second_hash = digest(a), digest(b)
        files.append({"path": name, "size": b.stat().st_size, "sha256": second_hash,
                      "reproduced": first_hash == second_hash})
    return {"passed": all(item["reproduced"] for item in files), "files": files,
            "bytes": sum(item["size"] for item in files),
            "scope": "All glTF/linked buffer/image bytes; excludes host-only blend/NPZ/debug metadata and paths"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("first", type=Path)
    parser.add_argument("second", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    report = compare(args.first, args.second)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"passed": report["passed"], "files": len(report["files"]), "bytes": report["bytes"]}))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
