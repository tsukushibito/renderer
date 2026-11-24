"""Compare diffuse SH orders in three captured 44x32 Sponza probe-receiver patches.

Pass the archived arcade-measure directory as --captures. Run 'positions' to
write the XYZ list for bistro-bake --probe-sh-reference and use --receivers
to write the input for --diffuse-reference. Run both bakes with the same
scene, environment and daylight preset, then pass their JSON files to 'compare'.
Reported RGB values are diffuse irradiance divided by pi.
"""

import argparse
import hashlib
import json
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "bistro"))
from diagnose_daylight import display, neutral


WIDTH, HEIGHT = 960, 540
PATCH_W, PATCH_H = 44, 32
PATCHES = (
    ("D", "Arch underside", "courtyard", 464, 90),
    ("G", "Side aisle ceiling", "gallery", 300, 90),
    ("B", "Column rear face", "courtyard", 269, 445),
)
CORNERS = np.array([[i & 1, (i >> 1) & 1, i >> 2] for i in range(8)])


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2, allow_nan=False) + "\n", encoding="utf-8")


def hdr(directory, tag, bounds):
    data = np.memmap(directory / tag / "hdr.bin", dtype="<f2", mode="r", shape=(HEIGHT, WIDTH, 4))
    return data[bounds][:, :, :3].astype(float)


def world_positions(directory, bounds, camera, near, far):
    depth = np.memmap(directory / "beauty/depth.bin", dtype="<f4", mode="r", shape=(HEIGHT, WIDTH))
    z = near * far / (far - (far - near) * depth[bounds].astype(float))
    eye = np.array(camera["position"])
    forward = np.array(camera["target"]) - eye
    forward /= np.linalg.norm(forward)
    right = np.cross(forward, [0, 1, 0])
    right /= np.linalg.norm(right)
    up = np.cross(right, forward)
    yy, xx = np.mgrid[bounds]
    scale = HEIGHT * 0.5 / np.tan(np.deg2rad(camera["vertical_fov_degrees"] * 0.5))
    ray = (xx[..., None] + 0.5 - WIDTH / 2) * right - (yy[..., None] + 0.5 - HEIGHT / 2) * up
    return eye + z[..., None] * (forward + ray / scale)


class ProbeVolume:
    def __init__(self, atlas):
        data = atlas.read_bytes()
        offset = data.index(b"RBKLST01") + 8
        texels = np.frombuffer(data, dtype="<f4", offset=offset).reshape(-1, 4)
        self.origin = texels[0, :3].astype(float)
        self.size = float(texels[0, 3])
        nodes, count, _, keys = texels[1].astype(int)
        end = 4 + 3 * nodes + 6 * count
        if keys != 1 or len(texels) < end:
            raise ValueError("Expected the single-key Sponza probe atlas")
        self.nodes = texels[4 : 4 + 3 * nodes].reshape(-1, 3, 4)
        self.probes = texels[4 + 3 * nodes : end].reshape(-1, 6, 4)

    def sample_points(self, points):
        """Return the IDs, relocated positions and normalized runtime weights."""
        shape = points.shape[:-1]
        flat = points.reshape(-1, 3)
        origin = np.broadcast_to(self.origin, flat.shape).copy()
        size = np.full((len(flat), 1), self.size)
        if np.any(flat < origin) or np.any(flat > origin + size):
            raise ValueError("Receiver is outside the probe volume")
        node = np.zeros(len(flat), dtype=int)
        while True:
            first = self.nodes[node, 0, 0].astype(int)
            active = first != 0
            if not np.any(active):
                break
            size[active] *= 0.5
            side = flat[active] >= origin[active] + size[active]
            origin[active] += side * size[active]
            node[active] = first[active] + side @ np.array([1, 2, 4])
        frac = ((flat - origin) / size)[:, None, :]
        ids = self.nodes[node, 1:, :].reshape(-1, 8).astype(int)
        records = self.probes[ids]
        positions = origin[:, None, :] + CORNERS * size[:, None, :] + records[:, :, 2:5, 3]
        delta = flat[:, None, :] - positions
        distance = np.where(delta >= 0, records[:, :, 1, :3], records[:, :, 0, :3])
        visibility = np.clip(1 + (distance - np.abs(delta)) / (0.05 * size[:, None, :]), 0, 1).min(axis=-1)
        weights = np.where(CORNERS, frac, 1 - frac).prod(axis=-1) * records[:, :, 0, 3] * visibility
        weights /= np.maximum(weights.sum(axis=1, keepdims=True), 1e-6)
        return ids.reshape(*shape, 8), positions.reshape(*shape, 8, 3), weights.reshape(*shape, 8)


def collect(captures):
    volume = ProbeVolume(captures.parent / "light.gi")
    config = read_json(captures / "views.json")
    cameras = {v["name"]: v for v in config["views"]}
    patches = []
    for key, name, view, x, y in PATCHES:
        directory = captures / view
        bounds = (slice(y - PATCH_H // 2, y + PATCH_H // 2), slice(x - PATCH_W // 2, x + PATCH_W // 2))
        points = world_positions(
            directory, bounds, cameras[view], config["camera"]["near_m"], config["camera"]["far_m"]
        )
        ids, positions, weights = volume.sample_points(points)
        normal = 2 * hdr(directory, "normals", bounds) - 1
        length = np.linalg.norm(normal, axis=-1, keepdims=True)
        if np.max(np.abs(length - 1)) > 0.007 or not np.all(hdr(directory, "sources", bounds)[:, :, 2] > 0.7):
            raise ValueError(f"{key}: expected valid probe-lit receiver normals throughout")
        patches.append(
            dict(
                key=key,
                name=name,
                directory=directory,
                bounds=bounds,
                ids=ids,
                positions=positions,
                weights=weights,
                normal=normal / length,
                point=points,
            )
        )
    # Stable row order for the external coefficient sampler. Corner relocation
    # is evaluated at single precision by the baker.
    positions = np.unique(
        np.concatenate([p["positions"][p["weights"] > 0] for p in patches]).astype("f4"), axis=0
    )
    lookup = {tuple(pos): i for i, pos in enumerate(positions)}
    for patch in patches:
        indices = np.zeros(patch["ids"].shape, dtype=int)
        active = patch["weights"] > 0
        indices[active] = [lookup[tuple(pos)] for pos in patch["positions"][active].astype("f4")]
        patch["indices"] = indices
    return volume, positions, patches


def evaluate(coefficients, normal, weights):
    x, y, z = normal.transpose(2, 0, 1)
    basis = np.stack([np.ones_like(x), x, y, z, x * y, y * z, x * z, x * x - y * y, 3 * z * z - 1], axis=-1)
    count = coefficients.shape[-2]
    irradiance = np.einsum("hwc,hwb,hwcbr->hwr", weights, basis[..., :count], coefficients)
    return np.maximum(irradiance, 0) / np.pi


def montage(output, title, subtitle, columns, rows):
    # Integer enlargement makes a single source pixel visible in the 44x32 crops.
    tile_w, tile_h = 220, 160
    margin, gap = 18, 12
    label_w = 168
    start_x, start_y = margin + label_w, 119
    fig = Image.new(
        "RGB",
        (
            start_x + len(columns) * (tile_w + gap) + margin - gap,
            start_y + len(rows) * (tile_h + gap) + margin - gap,
        ),
        "#182127",
    )
    pen = ImageDraw.Draw(fig)
    large = ImageFont.load_default(size=26)
    body = ImageFont.load_default(size=17)
    small = ImageFont.load_default(size=14)
    pen.text((margin, 14), title, font=large, fill="white")
    pen.text((margin, 53), subtitle, font=small, fill="#c4d3d9")
    for column, label in enumerate(columns):
        pen.text((start_x + column * (tile_w + gap), 88), label, fill="white", font=body)
    for j, (patch, images) in enumerate(rows):
        y = start_y + j * (tile_h + gap)
        ys, xs = patch["bounds"]
        pen.text((margin, y + 47), f'{patch["key"]}   {patch["name"]}', fill="white", font=body)
        pen.text((margin, y + 78), patch["directory"].name, fill="#c4d3d9", font=small)
        pen.text(
            (margin, y + 99),
            f"x {xs.start}-{xs.stop-1}, y {ys.start}-{ys.stop-1}",
            fill="#c4d3d9",
            font=small,
        )
        for column, img in enumerate(images):
            fig.paste(
                img.resize((tile_w, tile_h), Image.Resampling.NEAREST), (start_x + column * (tile_w + gap), y)
            )
    fig.save(output)


def statistics(l1, l2, reference, display1, display2):
    err1, err2 = np.abs(l1 - reference), np.abs(l2 - reference)
    delta = np.abs(l2 - l1)
    code_delta = np.abs(display2.astype(float) - display1)
    return {
        "pixels": int(l1.size // 3),
        "rgb_mae_l1": float(err1.mean()),
        "rgb_mae_l2": float(err2.mean()),
        "rgb_mae_reduction_percent": float(100 * (1 - err2.mean() / err1.mean())),
        "mean_abs_l2_minus_l1_rgb": float(delta.mean()),
        "luminance_mean": {
            key: float(np.mean(rgb @ [0.2126, 0.7152, 0.0722]))
            for key, rgb in [("l1", l1), ("l2", l2), ("receiver", reference)]
        },
        "final_srgb8_mean_abs_channel_change": float(code_delta.mean()),
        "final_srgb8_p95_max_channel_change_per_pixel": float(np.percentile(code_delta.max(axis=-1), 95)),
        "final_srgb8_max_channel_change": int(code_delta.max()),
    }


def compare(args, volume, positions, patches):
    positions_file = np.array(read_json(args.positions), dtype=np.float32)
    if not np.array_equal(positions, positions_file):
        raise ValueError("Sampler input must match this capture's relocated probe positions and order")
    bake = read_json(args.coefficients)
    reference_bake = read_json(args.reference)
    if (
        not bake.get("identity")
        or reference_bake["identity"] != bake["identity"]
        or not (bake["samples"] > 0 and reference_bake["samples"] > 0)
    ):
        raise ValueError(
            "Coefficient and receiver bakes must share a scene/lighting identity and valid sample counts"
        )
    coeff = np.array(bake["coefficients"])
    if coeff.shape != (len(positions), 9, 3) or not np.all(np.isfinite(coeff)):
        raise ValueError("Expected nine finite RGB irradiance coefficients per input position")
    receivers = np.array(read_json(args.captures / "patch-receivers.json")).reshape(
        -1, PATCH_H, PATCH_W, 2, 3
    )
    references = np.array(reference_bake["irradiance"]).reshape(-1, PATCH_H, PATCH_W, 3) / np.pi
    if not np.all(np.isfinite(references)) or np.any(references < 0):
        raise ValueError("Invalid receiver irradiance")
    results = {}
    irradiance_rows, shaded_rows, data = [], [], []
    for patch, receiver, reference in zip(patches, receivers, references, strict=True):
        normal, weights = patch["normal"], patch["weights"]
        # The reference rays start 0.3 mm along the geometric normal to avoid self-hits.
        if (
            np.max(np.linalg.norm(receiver[..., 0, :] - patch["point"], axis=-1)) > 0.00031
            or np.max(
                np.abs(
                    receiver[..., 1, :] / np.linalg.norm(receiver[..., 1, :], axis=-1, keepdims=True) - normal
                )
            )
            > 1e-6
        ):
            raise ValueError(f'{patch["key"]}: receiver positions or normal order differ')
        directory, bounds = patch["directory"], patch["bounds"]
        old = hdr(directory, "irradiance", bounds)
        atlas_l1 = evaluate(volume.probes[patch["ids"]][:, :, :, 2:6, :3], normal, weights)
        captured_diffuse = hdr(directory, "diffuse", bounds)
        base = hdr(directory, "base-color", bounds)
        raw = hdr(directory, "beauty/aa-off", bounds)
        if np.max(np.abs(old * base - captured_diffuse)) > 0.00025:
            raise ValueError("Captured base color and diffuse term disagree")
        selected = coeff[patch["indices"]]
        l1 = evaluate(selected[..., :4, :], normal, weights)
        l2 = evaluate(selected, normal, weights)
        shaded = [
            display(neutral(2 * np.maximum(0, raw - captured_diffuse + base * irradiance)))
            for irradiance in (l1, l2, reference)
        ]
        displays = np.array(shaded[0]), np.array(shaded[1])
        item = statistics(l1, l2, reference, *displays)
        item["atlas_l1_vs_gpu_max_abs_rgb"] = float(np.max(np.abs(atlas_l1 - old)))
        if item["atlas_l1_vs_gpu_max_abs_rgb"] > 0.0007:
            raise ValueError("Spatial lookup of captured coefficients does not reproduce the GPU irradiance")
        item["baked_l1_vs_atlas_mae_rgb"] = float(np.abs(l1 - atlas_l1).mean())
        item["baked_l1_vs_atlas_max_abs_rgb"] = float(np.abs(l1 - atlas_l1).max())
        item["pixels_with_zero_probe_weight"] = int(np.count_nonzero(weights.sum(axis=-1) == 0))
        results[patch["key"]] = item
        data.append((l1, l2, reference, *displays))
        irradiance_rows.append(
            (patch, [display(8 * v) for v in (l1, l2, reference)] + [display(32 * np.abs(l2 - l1))])
        )
        shaded_rows.append((patch, shaded))
    args.out.mkdir(parents=True, exist_ok=True)
    inputs = [
        args.positions,
        args.coefficients,
        args.captures.parent / "light.gi",
        args.captures / "views.json",
        args.reference,
        args.captures / "patch-receivers.json",
    ]
    write_json(
        args.out / "metrics.json",
        {
            "scene_lighting_identity": bake["identity"],
            "probe_rays": bake["samples"],
            "receiver_rays": reference_bake["samples"],
            "unique_contributing_probes": len(positions),
            "units": "linear RGB irradiance / pi; equally weighted pixels and RGB channels for MAE",
            "source_sha256": {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in inputs},
            "patches": results,
            "combined": statistics(*(np.concatenate(x) for x in zip(*data))),
        },
    )
    montage(
        args.out / "irradiance.png",
        "Sponza   /   Diffuse probe irradiance",
        f'{bake["samples"]:,} rays per probe.  Receiver reference: {reference_bake["samples"]:,} rays per pixel.  Irradiance / pi x8, then sRGB.  Difference x32.',
        ("L1", "L2", "Receiver reference", "|L2 - L1|"),
        irradiance_rows,
    )
    montage(
        args.out / "shaded.png",
        "Sponza   /   Indirect diffuse in the shaded image",
        f'{bake["samples"]:,} rays per probe.  Identical direct light, reflections and material.  EV +1, PBR Neutral, sRGB.  AA off.',
        ("L1", "L2", "Receiver reference"),
        shaded_rows,
    )
    print(json.dumps(results, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--captures", type=Path, required=True)
    modes = parser.add_subparsers(dest="mode", required=True)
    prepare = modes.add_parser("positions")
    prepare.add_argument("--out", type=Path, required=True)
    prepare.add_argument(
        "--receivers", type=Path, help="Also write the input for bistro-bake --diffuse-reference"
    )
    report = modes.add_parser("compare")
    report.add_argument("--positions", type=Path, required=True)
    report.add_argument("--coefficients", type=Path, required=True)
    report.add_argument("--reference", type=Path, required=True)
    report.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    volume, positions, patches = collect(args.captures)
    if args.mode == "positions":
        write_json(args.out, positions.tolist())
        if args.receivers:
            receiver_pairs = read_json(args.captures / "patch-receivers.json")
            write_json(args.receivers, [dict(position=p, normal=n) for p, n in receiver_pairs])
        print(f"Wrote {len(positions)} unique relocated positions to {args.out}")
    else:
        compare(args, volume, positions, patches)


if __name__ == "__main__":
    main()
