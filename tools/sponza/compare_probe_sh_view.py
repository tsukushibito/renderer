"""Reconstruct the Sponza courtyard probe lighting from its archived Vulkan captures.

The baker traces each used probe once. L1 and L2 then share the same samples,
surface normals, active corners and spatial weights. This diagnostic displays
linear diffuse irradiance divided by pi before receiver material and tonemapping.
The 1,024 sampled receiver pixels and nested 128-pixel high-ray subset are
independent checks of the interpolated result. Pass a new output directory.
"""

import argparse
import hashlib
import subprocess
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

from compare_probe_sh import HEIGHT, WIDTH, ProbeVolume, hdr, read_json, world_positions, write_json


def receiver_reference(baker, manifest, environment, out, points, eye, valid, normals, l1, l2):
    # Draw receiver pixels uniformly before computing either error. The seed
    # and nested subset fix both comparisons on identical camera samples.
    picks = np.random.default_rng(20261124).choice(len(normals), 1024, replace=False)
    yy, xx = np.unravel_index(np.flatnonzero(valid)[picks], valid.shape)
    # At silhouettes prefer the closer of the two one-sided screen tangents.
    # Offset outward by the same 0.3 mm as the archived patch study.
    p = points[yy, xx]
    def tangent(ay, ax):
        plus = points[(yy + ay).clip(0, HEIGHT - 1), (xx + ax).clip(0, WIDTH - 1)] - p
        minus = p - points[(yy - ay).clip(0, HEIGHT - 1), (xx - ax).clip(0, WIDTH - 1)]
        a, b = np.linalg.norm(plus, axis=1), np.linalg.norm(minus, axis=1)
        return np.where(((a > 0) & ((a < b) | (b == 0)))[:, None], plus, minus)
    face = np.cross(tangent(0, 1), tangent(1, 0))
    face *= np.where((face * (eye - p)).sum(axis=1) >= 0, 1, -1)[:, None]
    lengths = np.linalg.norm(face, axis=1)
    if np.min(lengths) <= 0:
        raise ValueError("Degenerate geometric normal at selected receiver")
    p = p + 0.0003 * face / lengths[:, None]
    receivers = [dict(position=position.tolist(), normal=normal.tolist())
                 for position, normal in zip(p, normals[picks])]
    references, errors = {}, {}
    for count, rays in ((1024, 2048), (128, 8192)):
        jobs = out / f"receivers-{count}.json"
        result = out / f"receiver-reference-{rays}.json"
        write_json(jobs, receivers[:count])
        subprocess.run([str(baker), "--diffuse-reference", str(jobs), "--day-only",
                        "--look", "o3de-atrium", "--backend", "cpu", "--samples", str(rays),
                        "--manifest", str(manifest), "--ibl-manifest", str(environment),
                        "--output", str(result)], check=True)
        reference = np.asarray(read_json(result)["irradiance"]) / np.pi
        references[rays] = reference
        a, b = (np.abs(field[picks[:count]] - reference).mean(axis=1) for field in (l1, l2))
        errors[str(rays)] = {"pixels": count, "l1_rgb_mae": a.mean(), "l2_rgb_mae": b.mean(),
                             "closer_with_l2": int((b < a).sum()), "equal": int((b == a).sum())}
    high = references[8192]
    low = references[2048][:len(high)]
    errors["subset_reference_rgb_mae_2048_vs_8192"] = np.abs(high - low).mean()
    errors["subset_2048_l1_rgb_mae"] = np.abs(l1[picks[:len(high)]] - low).mean()
    errors["subset_2048_l2_rgb_mae"] = np.abs(l2[picks[:len(high)]] - low).mean()
    return errors


def reconstruct(captures, baker, manifest, environment, out):
    scene = captures / "courtyard"
    bounds = np.s_[0:HEIGHT, 0:WIDTH]
    config = read_json(captures / "views.json")
    camera = next(v for v in config["views"] if v["name"] == "courtyard")
    points = world_positions(scene, bounds, camera, config["camera"]["near_m"], config["camera"]["far_m"])
    depth = np.fromfile(scene / "beauty/depth.bin", "<f4").reshape(HEIGHT, WIDTH)
    depth_hash = hashlib.sha256(depth.tobytes()).hexdigest()
    for tag in ("sources", "normals", "irradiance", "beauty"):
        report = read_json(scene / tag / "report.json")
        blob = (scene / tag / "hdr.bin").read_bytes()
        if (report["captures"]["depth"]["sha256"] != depth_hash
                or report["captures"]["hdr"]["sha256"] != hashlib.sha256(blob).hexdigest()):
            raise ValueError(f"Unmatched capture: {tag}")
    atlas = captures.parent / "light.gi"
    # The legacy capture host hashes the payload after the 64-byte cook identity.
    atlas_hash = hashlib.sha256(atlas.read_bytes()[64:]).hexdigest()
    if atlas_hash != report["identity"]["look"]["baked_lighting_sha256"]:
        raise ValueError("Capture and probe atlas differ")
    volume = ProbeVolume(atlas)
    sources = hdr(scene, "sources", bounds)
    normal = 2 * hdr(scene, "normals", bounds) - 1
    length = np.linalg.norm(normal, axis=-1)
    probe_pixels = (sources[..., 2] > 0.7) & (depth < 1)
    valid = (probe_pixels & (abs(length - 1) < 0.007)
             & ((points >= volume.origin) & (points <= volume.origin + volume.size)).all(axis=-1))
    normals = normal[valid] / length[valid, None]

    flat = points[valid]
    ids = np.empty((len(flat), 8), dtype=np.int32)
    weights = np.empty((len(flat), 8))
    origins = np.full((len(volume.probes), 3), np.nan, dtype=np.float32)
    batch = 8192
    for start in range(0, len(flat), batch):
        window = slice(start, start + batch)
        probe_ids, positions, w = volume.sample_points(flat[window])
        ids[window], weights[window] = probe_ids, w
        used = w > 0
        origins[probe_ids[used]] = positions[used]
    used_ids = np.flatnonzero(np.isfinite(origins[:, 0]))
    positions_path = out / "positions.json"
    write_json(positions_path, origins[used_ids].tolist())
    coefficients_path = out / "coefficients.json"
    subprocess.run([str(baker), "--probe-sh-reference", str(positions_path),
                    "--day-only", "--look", "o3de-atrium", "--backend", "cpu", "--samples", "128",
                    "--manifest", str(manifest), "--ibl-manifest", str(environment),
                    "--output", str(coefficients_path)], check=True)
    sampled = read_json(coefficients_path)
    coefficients = np.zeros((len(volume.probes), 9, 3))
    coefficients[used_ids] = np.asarray(sampled["coefficients"]).reshape(len(used_ids), 9, 3)
    archive = hdr(scene, "irradiance", bounds)
    l1, l2 = archive.copy(), archive.copy()
    old_result, l1_result, l2_result = (np.empty_like(normals) for _ in range(3))
    for start in range(0, len(flat), batch):
        window = slice(start, start + batch)
        x, y, z = normals[window].T
        basis = np.column_stack([np.ones_like(x), x, y, z, x*y, y*z, x*z, x*x-y*y, 3*z*z-1])
        w, idx = weights[window], ids[window]
        weighted = np.einsum("pi,pikc->pkc", w, coefficients[idx])
        old = np.einsum("pi,pikc->pkc", w, volume.probes[idx, 2:6, :3])
        for dest, coeff in ((old_result, old), (l1_result, weighted[:, :4]), (l2_result, weighted)):
            value = np.einsum("pk,pkc->pc", basis[:, :coeff.shape[1]], coeff)
            dest[window] = np.maximum(0, value) / np.pi
    l1[valid], l2[valid] = l1_result, l2_result
    delta = np.abs(l1_result - l2_result)
    metrics = {
        "camera": "courtyard", "dimensions": [WIDTH, HEIGHT], "rays_per_probe": sampled["samples"],
        "atlas_payload_sha256": atlas_hash, "bake_identity": sampled["identity"], "units": "linear RGB E/pi",
        "probe_pixels": int(probe_pixels.sum()), "reconstructed_pixels": int(valid.sum()),
        "zero_weight_pixels": int((weights.sum(axis=1) == 0).sum()), "sampled_probes": len(used_ids),
    }
    for name, residual in (("stored_l1_vs_gpu", np.abs(old_result - archive[valid])),
                           ("rebaked_l1_vs_gpu", np.abs(l1_result - archive[valid])),
                           ("l2_vs_l1", delta)):
        max_channel = residual.max(axis=1)
        row, col = np.unravel_index(np.flatnonzero(valid)[np.argmax(max_channel)], valid.shape)
        metrics[name] = {"rgb_mae": residual.mean(), "p50_max_channel": np.median(max_channel),
                         "p95_max_channel": np.percentile(max_channel, 95),
                         "p99_max_channel": np.percentile(max_channel, 99), "max_channel": max_channel.max(),
                         "pixels_over_0_001": int((max_channel > 0.001).sum()), "max_xy": [int(col), int(row)]}
    if not all(np.isfinite(frame).all() for frame in (l1, l2)):
        raise ValueError("Non-finite reconstruction")
    metrics["receiver_error"] = receiver_reference(baker, manifest, environment, out, points,
        np.array(camera["position"]), valid, normals, l1_result, l2_result)
    write_json(out / "metrics.json", metrics)
    return l1, l2, scene


def srgb(rgb):
    rgb = np.clip(rgb, 0, 1)
    return Image.fromarray(np.uint8(np.rint(255 * np.where(
        rgb <= 0.0031308, 12.92 * rgb, 1.055 * rgb ** (1 / 2.4) - 0.055))))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("captures", "baker", "manifest", "environment", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True)
    l1, l2, scene = reconstruct(args.captures, args.baker, args.manifest, args.environment, args.out)
    beauty = Image.open(scene / "beauty/display.png").convert("RGB")
    width, height, label = 576, 324, 36
    sheet = Image.new("RGB", (2 * width, 2 * (height + label)), "#181b20")
    draw = ImageDraw.Draw(sheet)
    panels = [
        ("Captured scene  /  daylight", beauty),
        ("Absolute L2 - L1 difference  /  8x linear gain", srgb(8 * abs(l2 - l1))),
        ("L1   Diffuse irradiance  /  E divided by pi", srgb(l1)),
        ("L2   Diffuse irradiance  /  E divided by pi", srgb(l2)),
    ]
    for i, (caption, panel) in enumerate(panels):
        x, y = i % 2 * width, i // 2 * (height + label)
        draw.text((x + 10, y + 7), caption, font=ImageFont.load_default(size=19), fill="white")
        sheet.paste(panel.resize((width, height), Image.Resampling.LANCZOS), (x, y + label))
    sheet.save(args.out / "courtyard.png")


if __name__ == "__main__":
    main()
