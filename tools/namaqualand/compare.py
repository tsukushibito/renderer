"""Compare three lossless views and matching fixed-time movies from measure.py."""
import argparse
import json
from pathlib import Path

import cv2
import numpy as np


# Pixel coordinates at 1920 x 1080 for a rock face, flower bed and distant vegetation.
REGIONS = {"rock": (700, 440, 580, 430), "flowers": (1330, 305, 570, 435),
           "vista": (330, 350, 700, 340)}


def metrics(a, b):
    diff = cv2.absdiff(a, b)
    return dict(mean_abs_rgb=float(diff.mean()), pixels_over_16_percent=float(
        100 * np.mean(diff.max(axis=2) > 16)))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    args = parser.parse_args()
    path = args.evidence
    summary = {"still": {}, "movie": {}}
    rows = []
    for name, (x, y, w, h) in REGIONS.items():
        normal, high = [cv2.imread(str(path / f"{mode}-{name}.png")) for mode in ("normal", "highest")]
        if any(image is None or image.shape != (1080, 1920, 3) for image in (normal, high)):
            raise ValueError(f"Missing native view {name}")
        region = np.s_[y:y+h, x:x+w]
        summary["still"][name] = dict(full=metrics(normal, high), roi_xywh=[x, y, w, h],
                                       roi=metrics(normal[region], high[region]))
        if name == "flowers":
            def petals(image):
                blue, green, red = cv2.split(image[region].astype(np.int16))
                return (red > green + 25) & (blue > green + 12)
            n, hmask = petals(normal), petals(high)
            summary["still"][name]["magenta_petals"] = dict(
                normal_pixels=int(n.sum()), highest_pixels=int(hmask.sum()),
                intersection_over_union=float((n & hmask).sum() / max(1, (n | hmask).sum())))
        diff = cv2.convertScaleAbs(cv2.absdiff(normal, high), alpha=4)
        crops = [image[region] for image in (high, normal, diff)]
        width, height = min(400, w), min(250, h)
        crops = [c[(h-height)//2:(h+height)//2,(w-width)//2:(w+width)//2] for c in crops]
        rows.append(np.hstack(crops))
    header = np.full((35, rows[0].shape[1], 3), 27, dtype=np.uint8)
    for column, text in enumerate(("Highest detail", "Distance LOD", "4x absolute RGB difference")):
        cv2.putText(header, text, (column * width + 8, 23), cv2.FONT_HERSHEY_SIMPLEX,
                    0.56, (245, 245, 245), 1, cv2.LINE_AA)
    if not cv2.imwrite(str(path / "native-crops.png"), np.vstack([header, *rows])):
        raise OSError("could not write native crop comparison")
    captures = [cv2.VideoCapture(str(path / f"{mode}.mp4")) for mode in ("normal", "highest")]
    if not all(c.isOpened() for c in captures):
        raise ValueError("missing comparison movie")
    fps = captures[0].get(cv2.CAP_PROP_FPS)
    if fps != captures[1].get(cv2.CAP_PROP_FPS):
        raise ValueError("movie rates differ")
    count = round(30 * fps)
    if any(round(c.get(cv2.CAP_PROP_FRAME_COUNT)) != count for c in captures):
        raise ValueError("unexpected movie duration")
    for c in captures:
        c.release()
    arrays = []
    for mode in ("normal", "highest"):
        raw = path.parent / f"{mode}.rgb8"
        if raw.stat().st_size != count * 480 * 270 * 3:
            raise ValueError("incomplete raw comparison stream")
        arrays.append(np.memmap(raw, dtype=np.uint8, mode="r", shape=(count, 270, 480, 3)))
    errors, temporal = [], []
    prev = None
    for normal, high in zip(*arrays):
        diff = normal.astype(np.int16) - high
        errors.append(float(np.abs(diff).mean()))
        if prev is not None:
            temporal.append(float(np.abs(diff - prev).mean()))
        prev = diff
    summary["movie"] = dict(fps=fps, frames=len(errors), raw_comparison_extent=[480, 270],
                            rgb_mae=errors, frame_difference_mae=temporal,
                            max_rgb_mae_second=int(np.argmax(errors))/fps,
                            max_temporal_mae_second=(int(np.argmax(temporal))+1)/fps)
    (path / "comparison.json").write_text(json.dumps(summary, indent=2), encoding="utf8")


if __name__ == "__main__":
    main()
