"""Verify PBRT scalar-channel selection and cutout mip coverage in a cooked Landscape."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import struct

import numpy as np
from PIL import Image


def levels(path):
    with path.open("rb") as f:
        tag, width, height, count, srgb = struct.unpack("<8s4I", f.read(24))
        if tag != b"RIMAG001":
            raise ValueError(f"invalid image {path}")
        for i in range(count):
            w, h = max(1, width >> i), max(1, height >> i)
            yield srgb, np.frombuffer(f.read(w*h*4), dtype=np.uint8).reshape(h, w, 4)


def digest(a):
    return (a.shape, hashlib.sha256(a.tobytes()).digest())


def expected_masks(source):
    scene = (source/"geometry/geometry.pbrt").read_text()
    images = dict(re.findall(r'Texture\s+"([^"]+)"\s+"float"\s+"imagemap"\s+'
                             r'"string filename"\s*\[?\s*"([^"]+)"', scene))
    used = set(re.findall(r'"texture alpha"\s*\[\s*"([^"]+)"', scene))
    expected = {}
    for path in sorted({images[name] for name in used}):
        with Image.open(source/path) as im:
            rgba = np.asarray(im.convert("RGBA"))
            if im.mode == "RGBA" and np.any(rgba[:,:,3] != 255):
                channels = rgba[:,:,3:4]
            else:
                channels = rgba[:,:,:3]
            c = channels.astype(np.float64) / 255
            c = np.where(c <= .04045, c/12.92, ((c+.055)/1.055)**2.4)
            alpha = np.floor(c.mean(axis=2)*255+.5).astype(np.uint8)
            expected[digest(alpha)] = path
    return expected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path)
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    scene = json.loads((args.package/"scene.json").read_bytes())
    objects = args.package/"objects"
    sources = expected_masks(args.source)
    verified = {}
    pairs = set()
    errors = []
    for part, mat in zip(scene["primitives"], scene["physical"]["surfaces"], strict=True):
        if mat["alpha"] is None:
            continue
        alpha = mat["alpha"]["hash"]
        if alpha not in verified:
            srgb, scalar = next(levels(objects/alpha))
            key = digest(scalar[:,:,0])
            if srgb or key not in sources:
                raise ValueError(f"scalar map {alpha} differs from source bytes after decoding")
            verified[alpha] = key
        preview = part["images"][0]["hash"]
        if (alpha, preview) in pairs:
            continue
        pairs.add((alpha, preview))
        chain = levels(objects/preview)
        srgb, image = next(chain)
        mask = image[:,:,3]
        if mask.shape == verified[alpha][0]:
            expected = verified[alpha]
        else:
            _, scalar = next(levels(objects/alpha))
            h, w = mask.shape
            sh, sw = scalar.shape[:2]
            rows = (2*np.arange(h)+1)*sh//(2*h)
            cols = (2*np.arange(w)+1)*sw//(2*w)
            expected = digest(scalar[rows[:,None], cols, 0])
        if not srgb or digest(mask) != expected:
            raise ValueError(f"preview mask disagrees with source {sources[verified[alpha]]}")
        base_coverage = (mask >= 128).mean()
        for level, (_, mip) in enumerate(chain, start=1):
            mip_alpha = mip[:,:,3]
            coverage = (mip_alpha >= 128).mean()
            errors.append((mip_alpha.size, abs(coverage-base_coverage), level, sources[verified[alpha]]))
    if set(verified.values()) != set(sources):
        raise ValueError("cooked package omits source opacity maps")
    print(f"{len(verified)} unique source masks, {len(pairs)} preview pairs, {len(errors)} mip levels")
    for minimum in (256, 16, 1):
        retained = [v for v in errors if v[0] >= minimum]
        worst = max(retained, key=lambda v: v[1])
        print(f">= {minimum} texels: p95 {np.percentile([v[1] for v in retained],95):.5f}, "
              f"max {worst[1]:.5f} ({worst[3]}, mip {worst[2]}, {worst[0]} texels)")


if __name__ == "__main__":
    main()
