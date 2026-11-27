"""Calibrate or evaluate full-film and regional gates at native resolution."""

import argparse
import hashlib
import json
import math
from pathlib import Path

from PIL import Image
from calibrate import compare_patch, cv2, np
from compare import REGIONS, REFERENCE_SHA256

PUBLISHED_EXR_SHA256 = "c1415dacd4b884684eb3fb4eb66eed0d2a2396f156fa7a235018bcae8d8d61b9"


def digest(path):
    with path.open("rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


def rgb(path):
    if path.suffix.lower() == ".exr":
        bgr = cv2.imread(str(path), cv2.IMREAD_UNCHANGED)
        if bgr is None or bgr.shape != (2160, 3840, 3) or not np.isfinite(bgr).all():
            raise ValueError(f"expected finite 4K linear RGB EXR at {path}")
        linear = bgr[:, :, ::-1].clip(0, 1)
        srgb = np.where(linear <= 0.0031308, 12.92 * linear, 1.055 * linear ** (1 / 2.4) - 0.055)
        return np.rint(255 * srgb).astype(np.uint8)
    with Image.open(path) as image:
        if image.size != (3840, 2160) or image.mode not in ("RGB", "RGBA"):
            raise ValueError(f"expected full-frame RGB at {path}")
        return np.asarray(image.convert("RGB"))


def sky_profile(image):
    # Compare the shape of the display gradient separately from its mean color.
    bands = np.array([band.mean(axis=(0, 1)) for band in np.array_split(image, 16)])
    return bands - bands.mean(axis=0)


def errors(reference, candidate, sky):
    m = compare_patch(reference, candidate)
    result = {key: m[key] for key in ("rgb_mae_u8", "Lstar_mae")}
    if sky:
        result["sky_profile_mae_u8"] = float(np.abs(sky_profile(reference) - sky_profile(candidate)).mean())
    else:
        result["one_minus_ssim"] = m["one_minus_ssim"]
    return result


def crop(image, box):
    x0, y0, x1, y1 = box
    return image[y0:y1, x0:x1]


def main():
    cv2.setNumThreads(2)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("reference", type=Path, help="pinned published view-0.png")
    parser.add_argument("gates", type=Path)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--calibrate", nargs=2, type=Path, metavar=("PUBLISHED_EXR", "INDEPENDENT_EXR"))
    action.add_argument("--evaluate", type=Path, metavar="CAPTURE")
    args = parser.parse_args()
    if digest(args.reference) != REFERENCE_SHA256:
        parser.error("reference digest mismatch")
    ref = rgb(args.reference)
    if args.evaluate:
        manifest = json.loads(args.gates.read_text(encoding="utf-8"))
        if (manifest["reference"] != REFERENCE_SHA256 or manifest["published_exr"] != PUBLISHED_EXR_SHA256
                or manifest["version"] != 1 or set(manifest["regions"]) != set(REGIONS)):
            parser.error("invalid gate manifest")
        image = rgb(args.evaluate)
        results = {}
        passed = True
        for region, box in REGIONS.items():
            record = manifest["regions"][region]
            values = errors(crop(ref, box), crop(image, box), region == "sky_gradient")
            if list(box) != record["box"] or set(record["limits"]) != set(values):
                parser.error(f"gate contract mismatch for {region}")
            if not all(math.isfinite(limit) and limit >= 0 for limit in record["limits"].values()):
                parser.error(f"invalid threshold for {region}")
            failed = [key for key in values if not values[key] <= record["limits"][key]]
            results[region] = {"measured": values, "failed": failed}
            passed = passed and not failed
        print(json.dumps(results, indent=2, allow_nan=False))
        return 0 if passed else 1

    published, independent = args.calibrate
    published_hash = digest(published)
    independent_hash = digest(independent)
    if published_hash != PUBLISHED_EXR_SHA256 or independent_hash == published_hash:
        parser.error("expected pinned published EXR and a separately rendered EXR")
    published_rgb, independent_rgb = rgb(published), rgb(independent)
    regions = {}
    for region, box in REGIONS.items():
        patch = crop(ref, box)
        sky = region == "sky_gradient"
        original = crop(published_rgb, box)
        rerender = crop(independent_rgb, box)
        positives = {
            "published_png_vs_published_exr": errors(patch, original, sky),
            "published_png_vs_independent": errors(patch, rerender, sky),
            "published_exr_vs_independent": errors(original, rerender, sky),
        }
        darkened = np.maximum(patch.astype(np.int16) - 24, 0).astype(np.uint8)
        negative = errors(patch, darkened, sky)
        if sky:
            flat = np.broadcast_to(np.rint(patch.mean(axis=(0, 1))).astype(np.uint8), patch.shape)
            negative["sky_profile_mae_u8"] = errors(patch, flat, True)["sky_profile_mae_u8"]
        else:
            blur = cv2.GaussianBlur(patch, (0, 0), 4)
            negative["one_minus_ssim"] = errors(patch, blur, False)["one_minus_ssim"]
        limits = {}
        for key, bad in negative.items():
            high = max(measured[key] for measured in positives.values())
            if not high < bad:
                raise ValueError(f"cannot distinguish full-reference variation from degradation: {region}/{key}")
            limits[key] = (high + bad) / 2
        regions[region] = {"box": box, "positive": positives, "negative": negative, "limits": limits}
    with args.gates.open("x", encoding="utf-8") as f:
        json.dump({"version": 1, "reference": REFERENCE_SHA256, "published_exr": published_hash,
                   "independent_exr": independent_hash, "regions": regions}, f, indent=2, allow_nan=False)
        f.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
