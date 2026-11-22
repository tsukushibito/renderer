"""Compare opaque regions of the ORCA exterior plate and two matched Bistro captures."""
import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw


# Coordinates on the 960 x 530 comparison frame, away from geometry boundaries.
REGIONS = {
    "sunlit_cobbles": (35, 439, 121, 481),
    "shaded_cobbles": (653, 359, 720, 387),
    "painted_facade": (267, 156, 286, 209),
    "awning": (365, 108, 473, 131),
    "potted_foliage": (315, 229, 347, 258),
}
PLATE_HASH = "35c3c824fe342e92549a525d11e7b80ea4e1b39fe3d4ef1323e4a688253579dc"


def capture(directory, look):
    report = json.loads((directory / "report.json").read_text(encoding="utf-8"))
    identity = report["identity"]
    if (identity["view"], identity["look"]["name"]) != ("bistro-corner", look):
        raise ValueError("Expected bistro-corner and the indicated look")
    info = report["captures"]["sdr"]
    data = (directory / "sdr.bin").read_bytes()
    if (info["width"], info["height"]) != (960, 530) or hashlib.sha256(data).hexdigest() != info["sha256"]:
        raise ValueError("Expected verified 960 x 530 capture")
    return Image.frombytes("RGBA", (960, 530), data).convert("RGB")


def measure(image):
    regions = {}
    for name, bounds in REGIONS.items():
        srgb = np.asarray(image.crop(bounds), dtype=float) / 255
        linear = np.where(srgb <= 0.04045, srgb / 12.92, ((srgb + 0.055) / 1.055) ** 2.4)
        regions[name] = {
            "median_srgb8": np.rint(np.median(srgb, axis=(0, 1)) * 255).astype(int).tolist(),
            "median_display_luminance": round(float(np.median(linear @ [0.2126, 0.7152, 0.0722])), 5),
        }
    return regions


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("plate", "before", "after", "output"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    if hashlib.sha256(args.plate.read_bytes()).hexdigest() != PLATE_HASH:
        parser.error("Expected the published Bistro_Exterior_1.png")
    plate = Image.open(args.plate).convert("RGB").resize((960, 530), Image.Resampling.LANCZOS)
    images = [plate, capture(args.before, "reference"), capture(args.after, "lumberyard-day")]
    names = ["ORCA exterior 1", "reference", "lumberyard-day"]
    sheet = Image.new("RGB", (1440, 290))
    draw = ImageDraw.Draw(sheet)
    for i, (name, im) in enumerate(zip(names, images)):
        sheet.paste(im.resize((480, 265), Image.Resampling.LANCZOS), (i * 480, 25))
        draw.text((i * 480 + 8, 6), name, fill="white")
    sheet.save(args.output)
    print(json.dumps({name: measure(im) for name, im in zip(names, images)}, indent=2))


if __name__ == "__main__":
    main()
