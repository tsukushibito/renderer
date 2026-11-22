"""Compare the published ORCA exterior plate with matched Bistro daylight captures."""
import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont


# Coordinates on the 960 x 530 comparison frame, away from geometry boundaries.
REGIONS = {
    "sunlit_cobbles": (35, 439, 121, 481),
    "shaded_cobbles": (653, 359, 720, 387),
    "painted_facade": (267, 156, 286, 209),
    "awning": (365, 108, 473, 131),
    "potted_foliage": (315, 229, 347, 258),
}
PLATE_HASH = "35c3c824fe342e92549a525d11e7b80ea4e1b39fe3d4ef1323e4a688253579dc"


def capture(directory, look, size=(960, 530)):
    report = json.loads((directory / "report.json").read_text(encoding="utf-8"))
    identity = report["identity"]
    if (identity["view"], identity["look"]["name"]) != ("bistro-corner", look):
        raise ValueError("Expected bistro-corner and the indicated look")
    info = report["captures"]["sdr"]
    data = (directory / "sdr.bin").read_bytes()
    if (info["width"], info["height"]) != size or hashlib.sha256(data).hexdigest() != info["sha256"]:
        raise ValueError(f"Expected verified {size[0]} x {size[1]} capture")
    return Image.frombytes("RGBA", size, data).convert("RGB")


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


def comparison(plate, current, output, details):
    # Coordinates on the original 1920 x 1061 plate. Both columns use the same
    # rectangles without resampling, exposure changes or registration warps.
    images = [plate, current]
    labels = ["Amazon Lumberyard Bistro  /  ORCA Exterior 1", "Renderer  /  Bistro daylight"]
    crops = [
        ("Menu board and sidewalk", (0, 405, 800, 745)),
        ("Awning and cafe seating", (920, 175, 1720, 515)),
        ("Streetlamp shadow and cobbles", (630, 721, 1430, 1061)),
    ]
    width, height = plate.size
    sheet = Image.new("RGB", (2 * width, height + 60), "#202328")
    draw = ImageDraw.Draw(sheet)
    for col, (img, label) in enumerate(zip(images, labels)):
        draw.text((col * width + 14, 9), label, font=ImageFont.load_default(size=34), fill="white")
        sheet.paste(img, (col * width, 60))
    sheet.save(output)
    if details:
        sheet = Image.new("RGB", (1600, 40 + len(crops) * 380), "#202328")
        draw = ImageDraw.Draw(sheet)
        font = ImageFont.load_default(size=23)
        for col, (img, label) in enumerate(zip(images, labels)):
            draw.text((800 * col + 10, 5), label, font=font, fill="white")
            for row, (subject, rect) in enumerate(crops):
                top = 40 + row * 380
                draw.text((800 * col + 10, top + 5), subject, font=font, fill="white")
                sheet.paste(img.crop(rect), (800 * col, top + 40))
        sheet.save(details)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("plate", type=Path)
    parser.add_argument("before", type=Path, nargs="?", help="Optional legacy 960 x 530 reference capture")
    parser.add_argument("after", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--details", type=Path, help="Also save matched crops of the native-size pair")
    args = parser.parse_args()
    if hashlib.sha256(args.plate.read_bytes()).hexdigest() != PLATE_HASH:
        parser.error("Expected the published Bistro_Exterior_1.png")
    plate = Image.open(args.plate).convert("RGB")
    if args.before is None:
        comparison(plate, capture(args.after, "lumberyard-day", plate.size), args.output, args.details)
        return
    if args.details:
        parser.error("--details is available for the two-image comparison")
    plate = plate.resize((960, 530), Image.Resampling.LANCZOS)
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
