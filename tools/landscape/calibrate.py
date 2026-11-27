"""Compare two independent 512-spp PBRT samples and controlled degradations.

All input rectangles use the full film's pixel coordinates. No resampling,
registration or exposure fit is applied to a rendered sample.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

os.environ["OPENCV_IO_ENABLE_OPENEXR"] = "1"
import cv2
import numpy as np
from PIL import Image

from compare import REFERENCE_SHA256, lightness


# 64x64 windows inside the four full-resolution evaluation regions.
WINDOWS = {"grass": (930, 1340), "trunk": (692, 1190), "woods": (1910, 1004), "sky": (1980, 450)}
CAMERA_SHA256 = "beb6395ba68054022a86df38852714468c0f2021fd821c163951e4a6c86d4883"

# Limits for the 64x64 probes only. For color and L* the negative control
# subtracts 24 display codes. For structure it blurs by sigma 4; for mask
# disagreement it shifts by eight pixels. The sky probe measures color only.
PROBE_LIMITS = {
    "grass": {"rgb_mae_u8": 16, "Lstar_mae": 6.5, "one_minus_ssim": .23, "bright_mask_disagreement": .10},
    "trunk": {"rgb_mae_u8": 15, "Lstar_mae": 6.4, "one_minus_ssim": .38, "bright_mask_disagreement": .18},
    "woods": {"rgb_mae_u8": 4.8, "Lstar_mae": 2.2, "one_minus_ssim": .21, "bright_mask_disagreement": .20},
    "sky": {"rgb_mae_u8": 1, "Lstar_mae": .4},
}


def encode_exr(path):
    image = cv2.imread(str(path), cv2.IMREAD_UNCHANGED)
    if image is None or image.shape != (64, 64, 3) or not np.isfinite(image).all():
        raise ValueError(f"invalid 64x64 RGB EXR {path}")
    rgb = image[:, :, ::-1].clip(0, 1)
    srgb = np.where(rgb <= 0.0031308, 12.92 * rgb, 1.055 * rgb ** (1 / 2.4) - 0.055)
    return np.rint(255 * srgb).astype(np.uint8)


def compare_patch(reference, candidate):
    a, b = lightness(reference), lightness(candidate)

    def smooth(x):
        return cv2.GaussianBlur(x, (11, 11), 1.5)

    ma, mb = smooth(a), smooth(b)
    va = np.maximum(smooth(a * a) - ma * ma, 0)
    vb = np.maximum(smooth(b * b) - mb * mb, 0)
    cov = smooth(a * b) - ma * mb
    # SSIM on display L* (range 100), discard the filter border.
    similarity = (2 * ma * mb + 1) * (2 * cov + 9) / ((ma * ma + mb * mb + 1) * (va + vb + 9))
    threshold = np.median(ma)
    mask_a, mask_b = ma >= threshold, mb >= threshold
    return {
        "rgb_mae_u8": float(np.abs(reference.astype(float) - candidate).mean()),
        "Lstar_mae": float(np.abs(a - b).mean()),
        "one_minus_ssim": float(1 - similarity[5:-5, 5:-5].mean()),
        "bright_mask_area_difference": float(abs(mask_a.mean() - mask_b.mean())),
        "bright_mask_disagreement": float((mask_a != mask_b).mean()),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="verified upstream Landscape directory")
    parser.add_argument("output", type=Path)
    parser.add_argument("--render-with", type=Path, help="generate fresh crop EXRs with this PBRT executable")
    parser.add_argument("--threads", type=int, default=6)
    parser.add_argument("--capture", type=Path, help="score fixed probes on a candidate 4K PNG")
    args = parser.parse_args()
    if args.render_with and args.capture:
        parser.error("choose either calibration renders or a candidate capture")
    src = args.source / "view-0.pbrt"
    ref = args.source / "reference/landscape/view-0.png"
    for path, expected in [(src, CAMERA_SHA256), (ref, REFERENCE_SHA256)]:
        if hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            parser.error(f"unexpected reference revision at {path}")
    args.output.mkdir(exist_ok=True, parents=True)
    with Image.open(ref) as image:
        reference = np.array(image)
    if args.capture:
        with Image.open(args.capture) as image:
            if image.size != (3840, 2160) or image.mode not in ("RGB", "RGBA"):
                parser.error("expected an RGB or RGBA 4K capture")
            capture = np.asarray(image.convert("RGB"))
        report = {}
        passed = True
        for name, (x, y) in WINDOWS.items():
            value = compare_patch(reference[y:y+64, x:x+64], capture[y:y+64, x:x+64])
            limits = PROBE_LIMITS[name]
            failed = [k for k, limit in limits.items() if value[k] > limit]
            report[name] = {"metrics": value, "limits": limits, "failed": failed}
            passed = passed and not failed
        print(json.dumps(report, indent=2, allow_nan=False))
        return 0 if passed else 1
    report = {}
    for name, (x, y) in WINDOWS.items():
        patch = reference[y:y + 64, x:x + 64]
        samples = []
        for seed, prefix in [(0, "calibration0"), (13, "calibration")]:
            path = args.output / f"{prefix}-{name}.exr"
            if args.render_with:
                subprocess.run([str(args.render_with.resolve()), "--quiet", "--spp", "512",
                                "--seed", str(seed), "--nthreads", str(args.threads),
                                "--pixelbounds", f"{x},{x+64},{y},{y+64}",
                                "--outfile", str(path.resolve()), str(src.resolve())], check=True)
            samples.append(encode_exr(path))
        # Derive all controls from the published image, including neighboring pixels
        # for the shift control. Keep the measured renderer out of calibration.
        controls = {
            "published_vs_seed0": samples[0],
            "published_vs_seed13": samples[1],
            "shift_8px": reference[y:y + 64, x + 8:x + 72],
            "blur_sigma4": cv2.GaussianBlur(reference[y-16:y+80, x-16:x+80], (0, 0), 4)[16:-16, 16:-16],
            "display_codes_minus_24": np.maximum(patch.astype(int) - 24, 0).astype(np.uint8),
        }
        report[name] = {key: compare_patch(patch, value) for key, value in controls.items()}
        report[name]["seed0_vs_seed13"] = compare_patch(*samples)
        negatives = {"rgb_mae_u8": "display_codes_minus_24", "Lstar_mae": "display_codes_minus_24",
                     "one_minus_ssim": "blur_sigma4", "bright_mask_disagreement": "shift_8px"}
        for metric, limit in PROBE_LIMITS[name].items():
            observed = report[name]
            positives = [observed[key][metric] for key in
                         ("published_vs_seed0", "published_vs_seed13", "seed0_vs_seed13")]
            if not max(positives) <= limit < observed[negatives[metric]][metric]:
                raise ValueError(f"{name} {metric} cannot separate noise from its negative control")
    print(json.dumps(report, indent=2, allow_nan=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
