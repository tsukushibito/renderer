"""Compare a black sphere's indirect shadow against a cosine-weighted sky integral.

The orthographic camera sees a white floor. A bright opening subtends a fixed
cone at infinity. Only floor radiance is measured (including beneath the sphere).
"""
import argparse
import json
from pathlib import Path

import numpy as np
from PIL import Image


def smooth(x):
    t = np.clip(x, 0, 1)
    return t * t * (3 - 2 * t)


def evaluate(output, name, axes, opening, samples, wall=False):
    radius = 0.35
    coords = np.linspace(-1.6, 1.6, 65)
    x, y = np.meshgrid(coords, coords)
    to_center = np.stack((-x, -y, np.full_like(x, radius)), axis=-1)
    distance = np.linalg.norm(to_center, axis=-1)
    axis = to_center / distance[..., None]
    # Equal-solid-angle hemisphere quadrature. The mirrored azimuth pairs
    # make left/right comparisons independent of quadrature asymmetry.
    i = np.arange(samples // 2) + 0.5
    z = i / len(i)
    phi = 2 * np.pi * i * 0.6180339887498949
    xy = np.sqrt(1 - z*z)
    rays = np.column_stack((xy*np.cos(phi), xy*np.sin(phi), z))
    rays = np.concatenate((rays, rays * [-1, 1, 1]))
    sky = np.full(len(rays), 0.05)
    for light in axes:
        sky[(rays @ light) >= np.cos(np.deg2rad(opening))] = 1
    weights = sky * rays[:, 2] * (2 / len(rays))
    baseline = weights.sum()
    # L1 irradiance polynomial: c0 = pi * mean_sphere(L);
    # c1 = 2*pi * mean_sphere(L*w). The lower hemisphere is black.
    first = 2 * (sky @ rays) / sky.sum()
    strength = min(1, np.linalg.norm(first))
    direction = first / np.linalg.norm(first)
    base = np.full_like(distance, baseline)
    reference = np.empty_like(distance)
    if wall:
        # Infinite opaque wall x=-0.42 separates the left receivers from the sphere.
        # Static visibility is present in their baked baseline, but a single L1
        # direction sampled at the caster cannot encode this intervening wall.
        base[x < -0.42] = weights[rays[:, 0] <= 0].sum()
        base[x >= -0.42] = weights[rays[:, 0] >= 0].sum()
    for row in range(len(coords)):
        hits = (axis[row] @ rays.T) >= np.sqrt(np.maximum(0, 1 - (radius/distance[row])**2))[:, None]
        if wall:
            loss = hits @ (weights * (rays[:, 0] >= 0))
            reference[row] = base[row] - np.where(x[row] < -0.42, 0, loss)
        else:
            reference[row] = baseline - hits @ weights
    angular_radius = np.arcsin(np.minimum(1, radius / distance))
    separation = np.arccos(np.clip(axis @ direction, -1, 1))
    cone = 0.65
    overlap = smooth((angular_radius + cone - separation) / (2 * np.minimum(angular_radius, cone)))
    directional = overlap * np.minimum(1, (angular_radius / cone)**2)
    contact = (radius / distance)**2 * axis[..., 2]
    fade = 1 - smooth((distance / radius - 4) / 2)
    estimate = base * (1 - fade * ((1-strength)*contact + strength*directional))
    roi = (base-reference) > baseline * 0.02
    def centroid(loss):
        return float(np.sum(x*loss) / np.sum(loss))
    np.savez_compressed(output / (name+'.npz'), base=base, reference=reference,
                        estimate=estimate, reference_delta=reference-base, estimate_delta=estimate-base)
    panels = np.hstack((base, reference, estimate))
    Image.fromarray(np.uint8(255 * np.clip(panels, 0, 1)**(1/2.2))).save(output / (name+'.png'))
    return dict(cross_wall_max_loss=float((base-estimate)[x < -0.42].max()) if wall else None,
                base_mae=float(np.mean(abs(base-reference)[roi])),
                estimate_mae=float(np.mean(abs(estimate-reference)[roi])),
                reference_centroid_x=centroid(base-reference), estimate_centroid_x=centroid(base-estimate))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--samples', type=int, default=16384)
    args = parser.parse_args()
    if args.samples < 256 or args.samples % 2:
        parser.error('samples must be even and at least 256')
    args.output.mkdir(parents=True, exist_ok=True)
    left, right = (np.array([s*0.8, 0, 0.6]) for s in (-1, 1))
    results = {name: evaluate(args.output, name, axes, angle, args.samples)
               for name, axes, angle in [('broad_left', [left], 38), ('broad_right', [right], 38),
                                         ('narrow', [left], 10), ('opposed', [left, right], 25)]}
    results['thin_wall'] = evaluate(args.output, 'thin_wall', [right], 38, args.samples, wall=True)
    print(json.dumps(results, indent=2))
    for name, sign in [('broad_left', 1), ('broad_right', -1)]:
        case = results[name]
        if not (case['estimate_mae'] < case['base_mae'] and
                min(sign*case[k] for k in ('reference_centroid_x', 'estimate_centroid_x')) > 0):
            raise SystemExit('broad-opening acceptance failed')


if __name__ == '__main__':
    main()
