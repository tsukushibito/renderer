"""Fetch the pinned PBRT reference inputs and verify their Git blob identities."""

from concurrent.futures import ThreadPoolExecutor
import argparse
import hashlib
import json
from pathlib import Path
from shutil import copyfileobj
from tempfile import NamedTemporaryFile
from urllib.parse import quote
from urllib.request import urlopen

from prepare import REV


def matches(path, blob):
    if not path.is_file() or path.stat().st_size != blob["size"]:
        return False
    digest = hashlib.sha1(f"blob {blob['size']}\0".encode())
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest() == blob["sha"]


def fetch(root, blob):
    name = blob["path"]
    relative = Path(name).relative_to("landscape" if name.startswith("landscape/") else "images")
    if ".." in relative.parts:
        raise ValueError(name)
    path = root / (relative if name.startswith("landscape/") else Path("reference") / relative)
    if matches(path, blob):
        return
    path.parent.mkdir(exist_ok=True, parents=True)
    with NamedTemporaryFile(dir=path.parent, delete=False) as tmp:
        staged = Path(tmp.name)
        try:
            url = f"https://raw.githubusercontent.com/mmp/pbrt-v4-scenes/{REV}/{quote(name)}"
            with urlopen(url, timeout=120) as source:
                copyfileobj(source, tmp)
            tmp.close()
            if not matches(staged, blob):
                raise ValueError(f"Git blob mismatch for {name}")
            staged.replace(path)
        finally:
            tmp.close()
            staged.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, default=Path("build/landscape-cache"))
    args = parser.parse_args()
    api = f"https://api.github.com/repos/mmp/pbrt-v4-scenes/git/trees/{REV}?recursive=1"
    with urlopen(api, timeout=90) as response:
        tree = json.load(response)
    if tree["truncated"]:
        raise ValueError("incomplete upstream tree")
    blobs = [item for item in tree["tree"] if item["type"] == "blob"
             and item["path"].startswith(("landscape/", "images/landscape/"))]
    root = args.cache / REV
    with ThreadPoolExecutor(max_workers=6) as pool:
        for _ in pool.map(lambda blob: fetch(root, blob), blobs):
            pass
    print(f"Verified {len(blobs)} upstream files in {root}")


if __name__ == "__main__":
    main()
