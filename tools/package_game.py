"""Build a relocatable game directory from cooked content and one executable."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import tempfile


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def safe_member(value):
    path = PurePosixPath(value)
    if not path.parts or path.is_absolute() or ".." in path.parts or ":" in value or "\\" in value:
        raise ValueError(f"expected a relative package path, got {value!r}")
    return path


def package(project, executable, output):
    spec = json.loads(project.read_text(encoding="utf-8"))
    if spec["version"] != 1:
        raise ValueError("unsupported package recipe")
    root = project.resolve().parent
    mount = safe_member(spec["mount"])
    sources = {executable.name: executable}
    for pattern in spec["files"]:
        safe_member(pattern)
        paths = sorted(root.glob(pattern))
        if not paths:
            raise FileNotFoundError(f"{project}: missing required file group {pattern}")
        for path in paths:
            if not path.is_file() or not path.resolve().is_relative_to(root):
                raise ValueError(f"{path}: expected a regular asset inside the project")
            relative = (mount / path.relative_to(root).as_posix()).as_posix()
            sources[relative] = path
    if output.exists():
        raise FileExistsError(f"{output} already exists; choose a new package directory")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=output.parent) as temporary:
        stage = Path(temporary) / "package"
        stage.mkdir()
        hashes = {}
        for name, source in sorted(sources.items()):
            dest = stage / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, dest)
            hashes[name] = digest(dest)
            with dest.open("rb") as stream:
                if stream.read(42).startswith(b"version https://git-lfs.github.com/spec/"):
                    raise ValueError(f"{source} is an unhydrated Git LFS pointer")
        manifest = {"version": 1, "executable": executable.name, "sha256": hashes}
        (stage / "sashiko-package.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        stage.rename(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    package(args.project, args.exe, args.output)


if __name__ == "__main__":
    main()
