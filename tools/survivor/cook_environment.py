"""Rebuild the lab's RGBA8 mip chains from the pinned Poly Haven material maps."""

import argparse
import hashlib
import io
import json
from pathlib import Path
from urllib.request import Request, urlopen

import numpy as np
from PIL import Image


ROOT = Path(__file__).resolve().parents[2]
OUTPUT = ROOT / "assets/survivor/environment"
ROLES = {"diff": "base", "nor_gl": "normal", "arm": "orm"}


def acquire(cache, base_url, asset, suffix, expected):
    filename = f"{asset}_{suffix}_1k.png"
    path = cache / filename
    if path.is_file():
        contents = path.read_bytes()
    else:
        url = f"{base_url}/{asset}/{filename}"
        request = Request(url, headers={"User-Agent": "Sashiko survivor material cook"})
        with urlopen(request, timeout=60) as response:
            contents = response.read()
    if hashlib.sha256(contents).hexdigest() != expected:
        raise ValueError(f"SHA-256 mismatch for {filename}")
    if not path.is_file():
        path.write_bytes(contents)
    return contents


def mip_chain(contents, role):
    with Image.open(io.BytesIO(contents)) as source:
        rgb = np.asarray(source.convert("RGB"), dtype=np.float64) / 255
    if rgb.shape != (1024, 1024, 3):
        raise ValueError(f"Expected a square 1K source, found {rgb.shape}")
    if role == "base":
        rgb = np.where(rgb <= 0.04045, rgb / 12.92, ((rgb + 0.055) / 1.055) ** 2.4)
    elif role == "normal":
        rgb = rgb * 2 - 1
    packed = bytearray()
    while rgb.shape[0] > 1:
        width = rgb.shape[0] // 2
        rgb = rgb.reshape(width, 2, width, 2, 3).mean(axis=(1, 3))
        if width > 256:
            continue
        if role == "base":
            encoded = np.where(rgb <= 0.0031308, rgb * 12.92, 1.055 * rgb ** (1 / 2.4) - 0.055)
        elif role == "normal":
            normals = rgb / np.maximum(np.linalg.norm(rgb, axis=2, keepdims=True), 1e-12)
            encoded = normals * 0.5 + 0.5
        else:
            encoded = rgb
        rgba = np.full((width, width, 4), 255, dtype=np.uint8)
        rgba[:, :, :3] = np.rint(encoded.clip(0, 1) * 255).astype(np.uint8)
        packed.extend(rgba.tobytes())
    return packed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, default=ROOT / "build/survivor-texture-sources")
    parser.add_argument("--check", action="store_true", help="Compare the rebuilt files with the game assets")
    args = parser.parse_args()
    args.cache.mkdir(parents=True, exist_ok=True)
    manifest = json.loads((OUTPUT / "sources.json").read_text(encoding="utf-8"))
    for name, item in manifest["surfaces"].items():
        for suffix, role in ROLES.items():
            data = acquire(args.cache, manifest["base_url"], item["asset"], suffix, item["sha256"][suffix])
            mip_data = mip_chain(data, role)
            dest = OUTPUT / f"{name}-{role}.rgba"
            if args.check:
                if dest.read_bytes() != mip_data:
                    raise SystemExit(f"Game asset differs from cook: {dest.name}")
            else:
                dest.write_bytes(mip_data)
            print(f"{dest.name} {len(mip_data)} bytes {hashlib.sha256(mip_data).hexdigest()}")


if __name__ == "__main__":
    main()
