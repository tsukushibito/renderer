"""Measure a native 4K Landscape capture against the published view-0 PNG."""

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
from PIL import Image, ImageChops


# Pixel-edge coordinates on the upstream PNG, origin top left, end exclusive.
REGIONS = {
    "frame": (0, 0, 3840, 2160),
    "grass_heads": (0, 1290, 1200, 2020),
    "forked_trunk": (510, 550, 1010, 1310),
    "dark_woods": (1490, 910, 2470, 1180),
    "sky_gradient": (1640, 0, 2360, 700),
}
REFERENCE_SHA256 = "232eb4049cc965920cc694d74ba0224b704a7c6ece7510da448644a66c0df77f"


def lightness(image):
    rgb = np.asarray(image, dtype=np.float32) / 255
    linear = np.where(rgb <= 0.04045, rgb / 12.92, ((rgb + 0.055) / 1.055) ** 2.4)
    y = linear @ np.array([0.2126, 0.7152, 0.0722], dtype=np.float32)
    # CIE L* relative to the display white.
    return np.where(y > (6 / 29) ** 3, 116 * np.cbrt(y) - 16, (29 / 3) ** 3 * y)


def metrics(reference, capture):
    delta = np.asarray(ImageChops.difference(reference, capture), dtype=np.uint8)
    a, b = lightness(reference), lightness(capture)
    return {
        "mean_abs_rgb_u8": delta.mean(axis=(0, 1)).tolist(),
        "mean_abs_Lstar": float(np.abs(a - b).mean()),
        "mean_signed_Lstar": float((b - a).mean()),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("reference", type=Path)
    parser.add_argument("capture", type=Path)
    parser.add_argument("difference", type=Path)
    args = parser.parse_args()
    with args.reference.open("rb") as file:
        if hashlib.file_digest(file, "sha256").hexdigest() != REFERENCE_SHA256:
            parser.error("reference does not match pinned view-0.png")
    if args.difference.resolve() in (args.capture.resolve(), args.reference.resolve()):
        parser.error("choose a separate output for the difference image")
    with Image.open(args.reference) as ref, Image.open(args.capture) as capture:
        if capture.size != ref.size or capture.mode not in ("RGBA", "RGB"):
            parser.error("expected a full-frame 3840x2160 RGB or RGBA capture")
        rgb = capture.convert("RGB")
        result = {name: metrics(ref.crop(box), rgb.crop(box)) for name, box in REGIONS.items()}
        ImageChops.difference(ref, rgb).save(args.difference, format="PNG")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
