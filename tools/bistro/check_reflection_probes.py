"""Check linear HDR captures of the 960x530 bistro-corner view (see examples.md)."""
import argparse
import json
from pathlib import Path

import numpy as np


def hdr(directory):
    values = np.fromfile(directory / "hdr.bin", "<f2").reshape(530, 960, 4)[..., :3]
    if not np.isfinite(values).all():
        raise ValueError(f"nonfinite lighting in {directory}")
    return values.astype(np.float64)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def expected_weights(directory, layout):
    # Independent reconstruction from the bistro-corner camera and opaque depth.
    eye = np.array([-23.01494, 6.42312, -0.15436])
    forward = np.array([-13.36295, 3.82032, 0.09966]) - eye
    forward /= np.linalg.norm(forward)
    right = np.cross(forward, [0, 1, 0])
    right /= np.linalg.norm(right)
    up = np.cross(right, forward)
    depth = np.fromfile(directory / "depth.bin", "<f4").reshape(530, 960).astype(float)
    scale = 265 / np.tan(np.deg2rad(37.83862) / 2)
    y, x = np.indices(depth.shape)
    ray = forward + ((x + .5 - 480) / scale)[..., None] * right - ((y + .5 - 265) / scale)[..., None] * up
    distance = .02 * 500 / (500 - (500 - .02) * depth)
    world = eye + distance[..., None] * ray
    boxes = json.loads(layout.read_text())
    require(len(boxes) == 2, "the Bistro comparison needs its two authored boxes")
    weights = np.stack([
        np.clip(np.minimum(world - box["min"], box["max"] - world).min(axis=2)
                / box["blend_distance"], 0, 1) for box in boxes
    ], axis=2)
    total = weights.sum(axis=2)
    expected = np.dstack((weights / np.maximum(total, 1)[..., None], np.clip(1 - total, 0, 1)))
    return expected, depth < 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ["control", "local", "weights", "radiance"]:
        parser.add_argument(arg, type=Path)
    parser.add_argument("--layout", type=Path, default=Path(__file__).with_name("reflection-probes.json"))
    args = parser.parse_args()
    for directory in [args.control, args.local, args.weights, args.radiance]:
        report = json.loads((directory / "report.json").read_text())
        identity = report["identity"]
        require((identity["view"], identity["width"], identity["height"]) == ("bistro-corner", 960, 530),
                "use the documented corner view and extent")
        require(report["validation"]["clean"], f"native validation failed in {directory}")
    control, local = hdr(args.control), hdr(args.local)
    road = np.s_[439:481, 35:121]
    require(np.array_equal(control[road], local[road]), "global fallback changed on the road")
    require(local[213:255,605:660].mean() < control[213:255,605:660].mean() * .9,
            "local capture did not reduce the rear seating reflection")
    expected, opaque = expected_weights(args.weights, args.layout)
    error = np.abs(hdr(args.weights) - expected).max(axis=2)
    groups = {"overlap": expected[..., :2].min(axis=2) > .05,
              "edge": (expected[..., 2] > .03) & (expected[..., 2] < .97)}
    for name, group in groups.items():
        group &= opaque
        # Transparent fragments can have a different position from opaque depth.
        require(group.sum() > 500, f"insufficient {name} coverage")
        p99 = np.percentile(error[group], 99)
        require(p99 < .01, f"{name} weight error {p99}")
        print(f"{name}: {group.sum()} pixels, 99th percentile absolute weight error {p99:.5f}")
    day, mid, dusk = (hdr(args.radiance / path) for path in [".", "midpoint", "dusk"])
    surface = opaque & (expected[..., 2] < .95)
    require(np.abs(day - dusk)[surface].mean() > .01, "identical reflection scenarios")
    difference = np.abs(mid - (day+dusk)/2)
    require(np.all(difference[surface] <= .001 + .001 * np.maximum(day, dusk)[surface]),
            "specular scenario blend is not linear in HDR")
    print(f"road unchanged; midpoint maximum absolute radiance error {difference[surface].max():.6f}")


if __name__ == "__main__":
    main()
