"""Capture and compare Bistro lighting terms and display transforms at the ORCA viewpoint."""
import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

from compare_daylight import PLATE_HASH, REGIONS, measure

SIZE = (1920, 1061)
LUMA = np.array([0.2126, 0.7152, 0.0722], dtype="f4")
PASSES = ("beauty", "base-color", "direct", "diffuse", "diffuse-no-ao", "specular",
          "local-specular", "emissive", "irradiance", "unshadowed")


def capture(args):
    common = [str(args.bistro.resolve()), "--api", "vulkan", "--pipeline", "forward",
              "--manifest", args.manifest, "--ibl-manifest", args.ibl_manifest,
              "--baked-lighting", args.baked_lighting, "--reflection-probes", args.reflection_probes,
              "--view", "bistro-corner", "--look", "lumberyard-day", "--scenario-blend", "0",
              "--width", str(SIZE[0]), "--height", str(SIZE[1]), "--warmup", "1", "--frames", "1",
              "--inflight", "1", "--aa-off"]
    for name in PASSES:
        flags = ["--lighting-pass", name]
        if name == "irradiance":
            flags = ["--gi-debug", name]
        if name == "unshadowed":
            flags = ["--lighting-pass", "direct", "--sun-shadows-off"]
        subprocess.run(common + flags + ["--output", args.captures / name], check=True, stdout=sys.stderr)


def read_buffer(directory, meta, key, fmt, dtype, channels):
    info = meta["captures"][key]
    data = (directory / (key + ".bin")).read_bytes()
    if ((info["width"], info["height"]) != SIZE or info["format"] != fmt
            or hashlib.sha256(data).hexdigest() != info["sha256"]):
        raise ValueError(f"Unexpected {directory}/{key}")
    return np.frombuffer(data, dtype=dtype).reshape(SIZE[1], SIZE[0], channels)


def neutral(x):
    # Same toe, shoulder and desaturation as output/tone_neutral.frag.hlsl.
    v = x.min(axis=2, keepdims=True)
    c = x - np.where(v < 0.08, v - 6.25 * v * v, 0.04)
    peak = c.max(axis=2, keepdims=True)
    high = np.maximum(peak, 0.76)
    new = 1 - 0.24 ** 2 / (high + 0.24 - 0.76)
    g = 1 - 1 / (1 + 0.15 * (high - new))
    return np.where(peak < 0.76, c, (c * (new / high)) * (1 - g) + new * g)


def display(rgb):
    c = np.clip(rgb, 0, 1)
    encoded = np.where(c <= 0.0031308, 12.92 * c, 1.055 * c ** (1 / 2.4) - 0.055)
    return Image.fromarray(np.rint(255 * encoded).astype("u1"))


def sheet(panels, path):
    width, height, header = 600, 332, 38
    sheet = Image.new("RGB", (width * 3, (height + header) * ((len(panels) + 2) // 3)), "#202328")
    draw = ImageDraw.Draw(sheet)
    for i, (label, image) in enumerate(panels):
        x, y = i % 3 * width, i // 3 * (height + header)
        draw.text((x + 10, y + 5), label, font=ImageFont.load_default(size=22), fill="white")
        sheet.paste(image.resize((width, height), Image.Resampling.LANCZOS), (x, y + header))
    sheet.save(path, quality=94, subsampling=0)


def region_bounds(bounds):
    return tuple(round(v * SIZE[j % 2] / (960, 530)[j % 2]) for j, v in enumerate(bounds))


def detail_sheet(variants, path):
    crops = [("Shaded cobbles", (1240, 695, 1510, 805)), ("Painted facade", (490, 300, 620, 410)),
             ("Red awning", (704, 195, 964, 305))]
    width, height, header, gutter = 310, 200, 43, 150
    page = Image.new("RGB", (gutter + width * len(variants), header + height * len(crops)), "#202328")
    pen = ImageDraw.Draw(page)
    font = ImageFont.load_default(size=19)
    for i, (name, img) in enumerate(variants):
        x = gutter + i * width
        pen.text((x + 6, 9), name, font=font, fill="white")
        for j, (label, bounds) in enumerate(crops):
            y = header + j * height
            if i == 0:
                pen.text((6, y + height // 2 - 12), label, font=font, fill="white")
            crop = img.crop(bounds)
            crop.thumbnail((width - 4, height - 4), Image.Resampling.LANCZOS)
            page.paste(crop, (x + (width - crop.width) // 2, y + (height - crop.height) // 2))
    page.save(path, quality=96, subsampling=0)


def analyze(args):
    if hashlib.sha256(args.plate.read_bytes()).hexdigest() != PLATE_HASH:
        raise ValueError("Expected ORCA Bistro_Exterior_1.png")
    plate = Image.open(args.plate).convert("RGB")
    hdr = {}
    for name in PASSES:
        directory = args.captures / name
        report = json.loads((directory / "report.json").read_text(encoding="utf-8"))
        identity = report["identity"]
        look = identity["look"]
        settings = (identity["view"], look["name"], look["baked_lighting_sha256"],
                    look["reflection_probes_sha256"], look["scenario_blend"], look["exposure_ev"],
                    report["captures"]["depth"]["sha256"])
        if name == "beauty":
            baseline = settings
            if settings[:2] != ("bistro-corner", "lumberyard-day") or settings[4:6] != (0, 1):
                raise ValueError("Expected the ORCA daylight camera and exposure")
            covered = read_buffer(directory, report, "depth", "r32f-le", "<f4", 1)[:, :, 0] < 1
            gpu = Image.fromarray(read_buffer(directory, report, "sdr", "rgba8-unorm", "u1", 4)[:, :, :3])
        expected = {"irradiance": "beauty", "unshadowed": "direct"}.get(name, name)
        if (settings != baseline or identity["aa"] or identity["bloom"] != "off"
                or identity["pipeline"] != "forward" or identity["shadows"] != (name != "unshadowed")
                or look["lighting_pass"].lower() != expected.replace("-", "")
                or look["gi_view"] != ("Irradiance" if name == "irradiance" else "Shaded")):
            raise ValueError(f"Inconsistent lighting capture {name}")
        hdr[name] = read_buffer(directory, report, "hdr", "rgba16f-le", "<f2", 4)[:, :, :3].astype("f4")
        if not np.isfinite(hdr[name]).all():
            raise ValueError(f"Nonfinite radiance in {name}")

    args.output.mkdir(parents=True, exist_ok=True)
    x = hdr["beauty"] * 2
    replay = display(neutral(x).astype("f2").astype("f4"))
    tones = [("ORCA / published SDR", plate), ("GPU / Neutral EV +1", gpu),
             ("Neutral EV +2", display(neutral(2 * x))),
             ("Reinhard by luminance / EV +1", display(x / (1 + (x @ LUMA)[:, :, None]))),
             ("Linear clip / EV +1", display(x))]
    annotated = gpu.copy()
    pen = ImageDraw.Draw(annotated)
    for i, bounds in enumerate(REGIONS.values(), 1):
        x0, y0, x1, y1 = region_bounds(bounds)
        pen.rectangle((x0, y0, x1, y1), outline="#ffdb40", width=4)
        pen.text((x0, y0 - 40), str(i), font=ImageFont.load_default(size=40), fill="#ffdb40",
                 stroke_width=2, stroke_fill="black")
    sheet(tones + [("Measured regions / GPU", annotated)], args.output / "bistro-tonemapping.jpg")

    hdr["global-specular"] = np.maximum(0, hdr["specular"] - hdr["local-specular"])
    # All radiance panels share the production display transform. Albedo is linear to sRGB only.
    panels = [("Beauty / Neutral EV +1", gpu), ("Base color / sRGB", display(hdr["base-color"]))]
    labels = [("direct", "Sun / shadows on"), ("unshadowed", "Sun / shadows off"),
              ("irradiance", "White diffuse receiver / E divided by pi"), ("diffuse", "Diffuse GI / material applied"),
              ("global-specular", "Global specular / weighted"), ("local-specular", "Local specular / weighted"),
              ("emissive", "Material emission")]
    for name, label in labels:
        img = hdr[name]
        if name == "irradiance":
            img = np.where(covered[:, :, None], img, 0)
        panels.append((label, display(neutral(img * 2))))
    sheet(panels, args.output / "bistro-lighting-passes.jpg")

    # Adjust only the weighted global reflection in scene-linear space, then replay the same display.
    details = [("ORCA", plate), ("GPU / current", gpu)]
    for scale in (0.5, 0):
        changed = np.maximum(0, hdr["beauty"] - (1 - scale) * hdr["global-specular"])
        details.append((f"Global specular x{scale}", display(neutral(changed * 2))))
    detail_sheet(details, args.output / "bistro-shadow-details.jpg")

    residual = abs(hdr["beauty"] - sum(hdr[k] for k in ("direct", "diffuse", "specular", "emissive")))[covered]
    replay_error = abs(np.asarray(replay).astype("i2") - np.asarray(gpu))
    radiance = {}
    region_rgb = {}
    for region, bounds in REGIONS.items():
        x0, y0, x1, y1 = region_bounds(bounds)
        region_rgb[region] = {name: img[y0:y1, x0:x1].mean(axis=(0, 1), dtype="f8").round(6).tolist()
                              for name, img in hdr.items()}
        radiance[region] = {k: round(float(np.mean(hdr[k][y0:y1, x0:x1] @ LUMA)), 6)
                            for k in ("beauty", "base-color", "direct", "unshadowed", "irradiance", "diffuse",
                                      "global-specular", "local-specular", "emissive")}
    print(json.dumps({
        "covered_pixels": int(covered.sum()),
        "recomposition_channel_abs_error_mean_p99_max": [float(residual.mean()), float(np.percentile(residual, 99)), float(residual.max())],
        "gpu_replay_srgb8_abs_error_p99_max": [float(np.percentile(replay_error, 99)), int(replay_error.max())],
        "diffuse_ao_bypass_identical": np.array_equal(hdr["diffuse"], hdr["diffuse-no-ao"]),
        "scene_linear_region_mean_Y": radiance,
        "scene_linear_mean_RGB": region_rgb,
        "global_specular_control": {k: measure(im.resize((960, 530), Image.Resampling.LANCZOS))
                                    for k, im in details[2:]},
        "display_region_medians": {k: measure(im.resize((960, 530), Image.Resampling.LANCZOS)) for k, im in tones},
    }, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("plate", type=Path)
    parser.add_argument("captures", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--bistro", type=Path, help="Run the captures before reading them")
    for flag in ("manifest", "ibl-manifest", "baked-lighting", "reflection-probes"):
        parser.add_argument("--" + flag, type=Path)
    args = parser.parse_args()
    if args.bistro:
        if not all((args.manifest, args.ibl_manifest, args.baked_lighting, args.reflection_probes)):
            parser.error("--bistro requires both manifests and both baked lighting assets")
        capture(args)
    analyze(args)


if __name__ == "__main__":
    main()
