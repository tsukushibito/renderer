"""Frozen B3 Godot repeat tolerance, calibrated before formal collection.

B2's archived byte-exact result is unchanged. B3's initial pre-formal pilot
showed four isolated one-code RGB differences. The
pixel cap is four times that observed count; no edge or alpha mask is excluded.
This profile is not a Godot-versus-Renderer quality or algorithm equivalence test.
"""
from pathlib import Path

import numpy as np
from PIL import Image


PROFILE = {"name": "godot-b3-repeat", "version": 1,
           "maximum_rgb_code_difference": 1, "maximum_changed_pixels": 16,
           "alpha_exact": True, "expected_size": [1920, 1080],
           "calibration": "Pre-formal B3 pilot: four changed pixels, each one RGB code; cap=4x observed pixel count. Freeze before formal runs; a formal failure requires investigation, not a fitted tolerance."}


def compare_pixels(first, second):
    if (first.dtype != np.uint8 or second.dtype != np.uint8 or first.shape != second.shape
            or first.ndim != 3 or first.shape[2] != 4 or not all(first.shape)):
        raise ValueError("Godot repeat comparison requires matching nonempty RGBA8 images")
    rgb_difference = np.abs(first[:, :, :3].astype(np.int16) - second[:, :, :3].astype(np.int16))
    changed = np.any(rgb_difference != 0, axis=2)
    count = int(np.count_nonzero(changed))
    maximum = int(rgb_difference.max())
    alpha_changes = int(np.count_nonzero(first[:, :, 3] != second[:, :, 3]))
    passed = (maximum <= PROFILE["maximum_rgb_code_difference"]
              and count <= PROFILE["maximum_changed_pixels"] and alpha_changes == 0)
    points = []
    if 0 < count <= PROFILE["maximum_changed_pixels"]:
        for y, x in zip(*np.nonzero(changed)):
            points.append({"x": int(x), "y": int(y), "first": first[y, x].tolist(), "second": second[y, x].tolist()})
    return {"passed": passed, "pixels": first.shape[0] * first.shape[1],
            "changed_pixels": count, "maximum_rgb_code_difference": maximum,
            "alpha_changed_pixels": alpha_changes, "changed_pixel_values": points}


def compare_files(first, second):
    arrays = []
    for path in (first, second):
        with Image.open(Path(path)) as image:
            if list(image.size) != PROFILE["expected_size"]:
                raise ValueError("Godot repeat capture resolution differs from 1920x1080")
            arrays.append(np.array(image.convert("RGBA"), dtype=np.uint8))
    return compare_pixels(*arrays)
