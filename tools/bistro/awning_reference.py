"""Measure specular transport at visible fabric pixels in the ORCA daylight camera."""

import argparse
from functools import lru_cache
import hashlib
import json
import subprocess
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

from compare_daylight import PLATE_HASH
from diagnose_daylight import SIZE, display, neutral, read_buffer
from ibl import sample_environment
from texture_codec import decode

EYE = np.array([-23.01494, 6.42312, -0.15436])
BANDS = {"Wall": 205, "Middle": 238, "Edge": 265}


def unit(x):
    return x / np.linalg.norm(x)


def ray(x, y):
    forward = unit(np.array([-13.36295, 3.82032, 0.09966]) - EYE)
    right = unit(np.cross(forward, [0, 1, 0]))
    up = np.cross(right, forward)
    scale = SIZE[1] / (2 * np.tan(np.deg2rad(37.83862 / 2)))
    return forward + ((x + 0.5 - SIZE[0] / 2) * right - (y + 0.5 - SIZE[1] / 2) * up) / scale


def fabric_triangles(cook, root):
    vertices = np.memmap(root / cook["vertices"]["path"], "<f4", "r").reshape(-1, 12)
    indices = np.memmap(root / cook["indices"]["path"], "<u4", "r")
    triangles, materials = [], []
    for p in cook["primitives"]:
        if "awning" not in cook["materials"][p["material"]]["name"].lower():
            continue
        i = p["index_offset"] // 4
        idx = indices[i : i + p["index_count"]].reshape(-1, 3)
        triangles.append(vertices[p["vertex_offset"] // 48 + idx])
        materials.extend([p["material"]] * len(idx))
    return np.concatenate(triangles).astype(float), np.array(materials)


def intersection(tris, normal, x, y):
    # Barycentrics for the intersection with each fabric triangle's plane.
    a, b, c = tris[:, 0, :3], tris[:, 1, :3], tris[:, 2, :3]
    direction = ray(x, y)
    with np.errstate(divide="ignore", invalid="ignore"):
        distance = np.einsum("ij,ij->i", a - EYE, normal) / (normal @ direction)
        p = EYE + distance[:, None] * direction
        denom = np.einsum("ij,ij->i", normal, normal)
        v = np.einsum("ij,ij->i", np.cross(p - a, c - a), normal) / denom
        w = np.einsum("ij,ij->i", np.cross(b - a, p - a), normal) / denom
    return distance, np.stack([1 - v - w, v, w], axis=1)


@lru_cache(maxsize=8)
def read_level(path, offset, length, fmt, w, h):
    with path.open("rb") as stream:
        stream.seek(offset)
        return decode(stream.read(length), fmt, w, h)


def texture(image, root, uv, grad):
    """Repeating trilinear texture lookup at the receiver's screen-space footprint."""
    chain = image["mips"]
    footprint = np.linalg.norm(grad * [image["width"], image["height"]], axis=1).max()
    lod = np.clip(np.log2(max(1.0, footprint)), 0, len(chain) - 1)

    def level(m):
        h, w = m["height"], m["width"]
        data = read_level(root / image["path"], m["offset"], m["byte_length"], image.get("format", "rgba8"), w, h)
        x, y = uv * [w, h] - 0.5
        ix, iy = int(np.floor(x)), int(np.floor(y))
        fx, fy = x - ix, y - iy
        return (
            sum(
                data[(iy + j) % h, (ix + i) % w].astype(float) * wx * wy
                for i, wx in enumerate([1 - fx, fx])
                for j, wy in enumerate([1 - fy, fy])
            )
            / 255
        )

    lo = int(lod)
    value = level(chain[lo]) * (1 - lod + lo) + level(chain[min(lo + 1, len(chain) - 1)]) * (lod - lo)
    encoding = image.get("normal_encoding", "xyz")
    if encoding in ("xy", "xy_directx"):
        if encoding == "xy_directx":
            value[1] = 1 - value[1]
        xy = value[:2] * 2 - 1
        value[2] = 0.5 + 0.5 * np.sqrt(max(0.0, 1.0 - xy @ xy))
    return value


def receiver(cook, root, tris, faces, materials, depth, base, x, y):
    distances, bary = intersection(tris, faces, x, y)
    valid = (bary.min(axis=1) >= 0) & (distances > 0)
    if not valid.any():
        raise ValueError(f"No awning covers {(x,y)}")
    i = np.argmin(np.where(valid, distances, np.inf))
    z = float(depth[y, x, 0])
    depth_distance = 0.02 * 500 / (500 - (500 - 0.02) * z)
    if abs(depth_distance - distances[i]) > 0.004:
        raise ValueError(f"GPU depth and fabric hit disagree at {(x,y)}")
    hit = bary[i] @ tris[i]
    mat = cook["materials"][materials[i]]
    uv = hit[10:12]
    grad = np.array(
        [
            intersection(tris[i : i + 1], faces[i : i + 1], u, v)[1][0] @ tris[i, :, 10:12] - uv
            for u, v in [(x + 1, y), (x, y + 1)]
        ]
    )

    def sample(slot):
        tid = mat[slot]
        return texture(cook["images"][cook["textures"][tid]["source"]], root, uv, grad)

    orm = sample("orm_texture")
    metallic = mat["metallic"] * orm[2]
    roughness = mat["roughness"] * orm[1]
    n = unit(hit[3:6])
    t = unit(hit[6:9] - n * np.dot(n, hit[6:9]))
    b = np.cross(n, t) * np.sign(hit[9])
    mapped = 2 * sample("normal_texture")[:3] - 1
    mapped[:2] *= mat["normal_scale"]
    shading = unit(t * mapped[0] + b * mapped[1] + n * mapped[2])
    view = unit(EYE - hit[:3])
    face = unit(faces[i])
    if np.dot(face, view) < 0:
        face = -face
    if np.dot(n, face) < 0:
        shading = -shading
    f0 = 0.04 * (1 - metallic) + metallic * base[y, x, :3].astype(float)
    return mat["name"], dict(
        position=hit[:3].tolist(),
        geometric_normal=face.tolist(),
        shading_normal=shading.tolist(),
        view=view.tolist(),
        f0=f0.tolist(),
        roughness=roughness,
    )


def capture_buffers(args):
    result = {}
    identity = None
    for name, label in [("beauty", "Beauty"), ("specular", "Specular"), ("base-color", "BaseColor")]:
        directory = args.captures / name
        report = json.loads((directory / "report.json").read_text())
        meta = report["identity"]
        look = meta["look"]
        settings = (
            meta["cook_manifest_sha256"],
            meta["environment"]["manifest_sha256"],
            report["captures"]["depth"]["sha256"],
            look["baked_lighting_sha256"],
            look["reflection_probes_sha256"],
        )
        if identity is None:
            identity = settings
            result["depth"] = read_buffer(directory, report, "depth", "r32f-le", "<f4", 1)
            result["ibl_strength"] = look["ibl_strength"]
            if look["ibl_yaw_degrees"] != 0:
                raise ValueError("Expected the daylight panorama orientation")
        if (
            settings != identity
            or meta["view"] != "bistro-corner"
            or look["name"] != "lumberyard-day"
            or look["scenario_blend"] != 0
            or look["lighting_pass"] != label
            or meta["aa"]
            or meta["bloom"] != "off"
            or look["exposure_ev"] != 1
            or meta["pipeline"] != "forward"
        ):
            raise ValueError("Expected matched daylight lighting-pass captures")
        result[name] = read_buffer(directory, report, "hdr", "rgba16f-le", "<f2", 4)
    if identity[:2] != tuple(hashlib.sha256(p.read_bytes()).hexdigest() for p in (args.manifest, args.ibl)):
        raise ValueError("Cook or environment differs from the GPU capture")
    return result


def run_bake(args, tmp, inputs, count):
    input_path = tmp / "receivers.json"
    input_path.write_text(json.dumps(inputs))
    output = tmp / f"reference-{count}.json"
    subprocess.run(
        [
            args.baker,
            "--manifest",
            args.manifest,
            "--ibl-manifest",
            args.ibl,
            "--reflection-reference",
            input_path,
            "--samples",
            str(count),
            "--output",
            output,
        ],
        check=True,
    )
    return json.loads(output.read_text())["results"]


def replay_global(path, inputs, strength):
    """Check recovered receiver inputs against GPU pixels with the prefiltered maps and BRDF LUT."""
    manifest = json.loads(path.read_text())

    def levels(name):
        info = manifest["images"][name]
        raw = np.fromfile(path.parent / info["path"], "<f2").reshape(-1, 4)
        w, h, offset = info["width"], info["height"], 0
        result = []
        for _ in range(info["mip_levels"]):
            result.append(raw[offset : offset + w * h].reshape(h, w, 4))
            offset += w * h
            w, h = max(1, w // 2), max(1, h // 2)
        return result

    maps = levels("specular")
    lut = levels("brdf")[0]
    h, w = lut.shape[:2]
    result = []
    for s in inputs:
        n, v = np.array(s["shading_normal"]), np.array(s["view"])
        nv = np.dot(n, v)
        reflected = 2 * nv * n - v
        lod = s["roughness"] * manifest["max_prefilter_lod"]
        lo = int(lod)
        color = (1 - lod + lo) * sample_environment(maps[lo], reflected) + (lod - lo) * sample_environment(
            maps[min(lo + 1, len(maps) - 1)], reflected
        )
        x, y = np.clip([nv * w - 0.5, s["roughness"] * h - 0.5], [0, 0], [w - 1, h - 1])
        ix, iy = int(x), int(y)
        fx, fy = x - ix, y - iy
        a, b = sum(
            lut[min(h - 1, iy + j), min(w - 1, ix + i), :2].astype(float) * wx * wy
            for i, wx in enumerate((1 - fx, fx))
            for j, wy in enumerate((1 - fy, fy))
        )
        result.append(strength * color[:3] * (np.array(s["f0"]) * a + b))
    return np.array(result)


def compare(args):
    if args.samples < 2 or hashlib.sha256(args.plate.read_bytes()).hexdigest() != PLATE_HASH:
        raise ValueError("Expected two or more samples and the original ORCA exterior plate")
    plate = Image.open(args.plate).convert("RGB")
    data = capture_buffers(args)
    cook = json.loads(args.manifest.read_text())
    tris, material = fabric_triangles(cook, args.manifest.parent)
    faces = np.cross(tris[:, 1, :3] - tris[:, 0, :3], tris[:, 2, :3] - tris[:, 0, :3])
    pixels, inputs, names = [], [], set()
    for start in BANDS.values():
        for y in range(start + 1, start + 10, 3):
            for x in range(839, 909, 12):
                name, s = receiver(
                    cook, args.manifest.parent, tris, faces, material, data["depth"], data["base-color"], x, y
                )
                names.add(name)
                inputs.append(s)
                pixels.append((x, y))
    if len(names) != 1:
        raise ValueError(f"Mixed receiver materials: {names}")
    with tempfile.TemporaryDirectory() as tmp:
        low = run_bake(args, Path(tmp), inputs, args.samples // 2)
        high = run_bake(args, Path(tmp), inputs, args.samples)
    count = len(inputs) // len(BANDS)
    report = {
        "samples_per_pixel": args.samples,
        "pixels_per_band": count,
        "material": names.pop(),
        "bands": {},
    }
    replay = replay_global(args.ibl, inputs, data["ibl_strength"])
    swatches = []
    for row, band in enumerate(BANDS):
        sl = slice(row * count, (row + 1) * count)
        x, y = np.array(pixels[sl]).T
        roughness = [s["roughness"] for s in inputs[sl]]

        def term(run, key):
            return np.array([r[key] for r in run[sl]])

        sky, geometry = term(high, "sky"), term(high, "geometry")
        reference = sky + geometry
        runtime = data["specular"][y, x, :3].astype(float)
        beauty = data["beauty"][y, x, :3].astype(float)
        revised = beauty - runtime + reference

        def rgb8(a):
            return (
                np.median(np.asarray(display(neutral(2 * np.maximum(a, 0)[:, None, :])))[:, 0, :], axis=0)
                .round()
                .astype(int)
            )

        colors = [np.median(np.asarray(plate)[y, x], axis=0).round().astype(int), rgb8(beauty), rgb8(revised)]
        swatches.append(colors)
        nr = term(high, "sky_rays").sum()
        report["bands"][band] = {
            "roughness_mean": float(np.mean(roughness)),
            "sky_fraction": float(nr / (nr + term(high, "surface_rays").sum())),
            "runtime_rgb": runtime.mean(axis=0).tolist(),
            "reference_rgb": reference.mean(axis=0).tolist(),
            "split_sum_replay_max_channel_error": float(abs(replay[sl] - runtime).max()),
            "sky_rgb": sky.mean(axis=0).tolist(),
            "geometry_rgb": geometry.mean(axis=0).tolist(),
            "unobstructed_rgb": term(high, "unobstructed").mean(axis=0).tolist(),
            "half_sample_delta_max_channel": float(
                abs((term(low, "sky") + term(low, "geometry") - reference).mean(axis=0)).max()
            ),
            "srgb8_median_orca_runtime_replaced": [c.tolist() for c in colors],
        }
    args.output.mkdir(parents=True, exist_ok=True)
    (args.output / "bistro-awning-rays.json").write_text(json.dumps(report, indent=2) + "\n")
    figure(plate, pixels, swatches, report["bands"], args.output / "bistro-awning-rays.png")
    print(json.dumps(report, indent=2))


def figure(plate, pixels, swatches, bands, path):
    page = Image.new("RGB", (880, 310), "#202328")
    draw = ImageDraw.Draw(page)
    font = ImageFont.load_default(size=17)
    crop = plate.crop((675, 165, 1105, 340))
    marks = ImageDraw.Draw(crop)
    for x, y in pixels:
        marks.ellipse((x - 676, y - 166, x - 674, y - 164), fill="#ffda63")
    page.paste(crop, (12, 83))
    draw.text((12, 50), "ORCA / measured fabric pixels", font=font, fill="white")
    fractions = "   ".join(f"{name} {stats['sky_fraction']:.1%}" for name, stats in bands.items())
    draw.text((12, 270), "Sky rays    " + fractions, font=font, fill="white")
    draw.text((455, 9), "Median display RGB / matched pixels", font=font, fill="white")
    for i, title in enumerate(["ORCA", "Renderer", "Ray reference"]):
        draw.text((534 + 114 * i, 47), title, font=font, fill="white")
    for i, (name, colors) in enumerate(zip(BANDS, swatches)):
        y = 82 + 70 * i
        draw.text((454, y + 7), name, font=font, fill="white")
        for j, color in enumerate(colors):
            x = 534 + 114 * j
            draw.rectangle((x, y, x + 97, y + 26), fill=tuple(color))
            draw.text((x, y + 30), " ".join(map(str, color)), font=font, fill="white")
    page.save(path)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("manifest", "ibl", "captures", "baker", "plate", "output"):
        parser.add_argument(key, type=Path)
    parser.add_argument("--samples", type=int, default=8192)
    compare(parser.parse_args())
