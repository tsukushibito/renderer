"""Separate angular approximation error from the effect of interpolating probe locations.

Use --probe-ray-output from the same bistro-bake invocation as --coefficients.
The comparison uses the same normals, corner weights and clamp as compare_probe_sh.
"""

import argparse
from pathlib import Path

import numpy as np

from compare_probe_sh import collect, evaluate, read_json, write_json


def load_rays(path, coeff):
    samples = coeff["samples"]
    expected = np.asarray(coeff["coefficients"])
    probes = len(expected)
    data = np.memmap(path, dtype="<f4", mode="r")
    if data.size != (probes + 1) * samples * 3:
        raise ValueError("Probe ray file size disagrees with the coefficient bake")
    directions = data[:samples * 3].reshape(samples, 3).astype(float)
    radiance = data[samples * 3:].reshape(probes, samples, 3)
    if not np.allclose(np.linalg.norm(directions, axis=1), 1, atol=1e-6):
        raise ValueError("Expected unit sphere samples")
    # Check position order, ray order, units and the coefficient convention before
    # using the independent positive cosine integral as an angular reference.
    x, y, z = directions.T
    basis = np.stack((np.ones(samples), 2*x, 2*y, 2*z, 3.75*x*y, 3.75*y*z,
                      3.75*x*z, 0.9375*(x*x-y*y), 0.3125*(3*z*z-1)), axis=0)
    projected = (basis @ radiance) * (np.pi / samples)
    if not np.allclose(projected, expected, rtol=2e-5, atol=2e-6):
        raise ValueError("Ray file does not reproduce the supplied SH coefficients")
    return directions, radiance


def integrate_corners(patch, directions, radiance):
    normals = patch["normal"].reshape(-1, 3)
    ids = patch["indices"].reshape(-1, 8)
    weights = patch["weights"].reshape(-1, 8)
    integral = np.zeros_like(normals)
    for index in np.unique(ids[weights > 0]):
        weight = np.where(ids == index, weights, 0).sum(axis=1)
        active = np.flatnonzero(weight > 0)
        # Limit the working set for high ray counts. E/pi under uniform sphere
        # sampling is 4 times the mean of incoming radiance times positive cosine.
        for start in range(0, len(active), 128):
            pixels = active[start:start + 128]
            cosine = np.maximum(normals[pixels] @ directions.T, 0)
            integral[pixels] += (cosine @ radiance[index]) * weight[pixels, None]
    return (integral * (4 / len(directions))).reshape(patch["normal"].shape)


def statistics(l1, l2, cosine, receiver):
    def mae(a, b):
        return float(np.abs(a - b).mean())

    return {
        "pixels": l1.size // 3,
        "l1_vs_probe_cosine_mae": mae(l1, cosine),
        "l2_vs_probe_cosine_mae": mae(l2, cosine),
        "probe_cosine_vs_receiver_mae": mae(cosine, receiver),
        "l1_vs_receiver_mae": mae(l1, receiver),
        "l2_vs_receiver_mae": mae(l2, receiver),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("captures", "coefficients", "positions", "rays", "reference", "out"):
        parser.add_argument("--" + name, required=True, type=Path)
    args = parser.parse_args()
    _, positions, patches = collect(args.captures)
    if not np.array_equal(positions, np.asarray(read_json(args.positions), dtype=np.float32)):
        raise ValueError("Probe positions differ from the patch lookup")
    coeff = read_json(args.coefficients)
    ref = read_json(args.reference)
    if ref["identity"] != coeff["identity"]:
        raise ValueError("Mismatched scene or illumination")
    sh = np.asarray(coeff["coefficients"])
    dirs, radiance = load_rays(args.rays, coeff)
    ref_rgb = np.asarray(ref["irradiance"]).reshape(len(patches), *patches[0]["normal"].shape)
    results, rows = {}, []
    for patch, target in zip(patches, ref_rgb / np.pi, strict=True):
        values = sh[patch["indices"]]
        n, weights = patch["normal"], patch["weights"]
        row = (evaluate(values[..., :4, :], n, weights), evaluate(values, n, weights),
               integrate_corners(patch, dirs, radiance), target)
        results[patch["key"]] = statistics(*row)
        rows.append(row)
    write_json(args.out, {
        "scene_lighting_identity": coeff["identity"],
        "samples_per_probe": coeff["samples"],
        "samples_per_receiver": ref["samples"],
        "units": "linear RGB irradiance / pi; mean over pixels and color channels",
        "patches": results,
        "combined": statistics(*(np.concatenate(x) for x in zip(*rows))),
    })


if __name__ == "__main__":
    main()
