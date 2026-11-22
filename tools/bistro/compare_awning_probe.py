"""Compare the two-probe daylight capture and the upper-awning trial against the ray reference."""

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

from awning_reference import BANDS
from compare_daylight import PLATE_HASH
from diagnose_daylight import LUMA, read_buffer


def load(directory, mode):
    report = json.loads((directory / "report.json").read_text())
    identity = report["identity"]
    look = identity["look"]
    debug = "Weights" if mode == "weights" else "Shaded"
    shading = "Specular" if mode == "specular" else "Beauty"
    if (
        identity["view"] != "bistro-corner"
        or look["name"] != "lumberyard-day"
        or look["scenario_blend"] != 0
        or look["exposure_ev"] != 1
        or look["reflection_view"] != debug
        or look["lighting_pass"] != shading
        or identity["aa"]
        or identity["bloom"] != "off"
        or identity["pipeline"] != "forward"
        or not identity["shadows"]
        or not report["validation"]["clean"]
    ):
        raise ValueError(f"Unexpected daylight settings in {directory}")
    common = (
        identity["conditions_sha256"],
        identity["cook_manifest_sha256"],
        identity["shaders_sha256"],
        identity["environment"]["manifest_sha256"],
        look["baked_lighting_sha256"],
        report["captures"]["depth"]["sha256"],
    )
    kind = ("sdr", "rgba8-unorm", "u1") if mode == "beauty" else ("hdr", "rgba16f-le", "<f2")
    return read_buffer(directory, report, *kind, 4)[:, :, :3], common, look["reflection_probes_sha256"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("plate", "baseline", "trial", "reference", "image"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    if hashlib.sha256(args.plate.read_bytes()).hexdigest() != PLATE_HASH:
        parser.error("Expected the ORCA exterior plate")
    plate = Image.open(args.plate).convert("RGB")
    reference = json.loads(args.reference.read_text())["bands"]
    before, common, old_atlas = load(args.baseline / "beauty", "beauty")
    after, settings, new_atlas = load(args.trial / "beauty", "beauty")
    if settings != common or old_atlas == new_atlas:
        raise ValueError("Expected different reflection atlases with matched scene, lighting and depth")
    radiance = []
    for directory, name, expected in [
        (args.baseline, "specular", old_atlas),
        (args.trial, "specular", new_atlas),
        (args.trial, "weights", new_atlas),
    ]:
        values, settings, atlas = load(directory / name, name)
        if settings != common or atlas != expected:
            raise ValueError(f"Unmatched capture in {directory / name}")
        radiance.append(values)
    stats = {}
    for band, y in BANDS.items():
        # Same 18 fabric pixels per band as the independent GGX ray reference.
        pixels = np.s_[y + 1 : y + 10 : 3, 839:909:12]
        old, new, weights = (v[pixels].astype(float).mean(axis=(0, 1)) for v in radiance)
        if not np.allclose(old, reference[band]["runtime_rgb"], atol=1e-4, rtol=0):
            raise ValueError(f"Baseline does not reproduce the ray study in {band}")
        stats[band] = {
            "before_Y": float(old @ LUMA),
            "trial_Y": float(new @ LUMA),
            "reference_Y": float(reference[band]["reference_rgb"] @ LUMA),
            "local_fraction": float(1 - weights[2]),
            "trial_srgb8_median": np.median(after[pixels], axis=(0, 1)).tolist(),
        }
    delta = np.abs(after.astype("i2") - before).max(axis=2)
    controls = {
        "sunlit_road": (70, 878, 242, 962),
        "shaded_road": (1306, 719, 1440, 774),
        "painted_facade": (534, 312, 572, 418),
        "sign_above_awning": (740, 139, 942, 184),
    }
    for name, (x, y, x1, y1) in controls.items():
        d = delta[y:y1, x:x1]
        stats[name] = {"max_srgb8_delta": int(d.max()), "mean_max_channel_delta": float(d.mean())}
    stats["frame_fraction_delta_over_1_srgb8"] = float(np.mean(delta > 1))
    print(json.dumps(stats, indent=2))

    width = 580
    sheet = Image.new("RGB", (3 * width, 692), "#202328")
    draw = ImageDraw.Draw(sheet)
    font = ImageFont.load_default(size=22)
    for i, (label, img) in enumerate(
        [
            ("ORCA / published SDR", plate),
            ("Renderer / two original probes", Image.fromarray(before)),
            ("Renderer / added upper-awning probe", Image.fromarray(after)),
        ]
    ):
        x = i * width
        draw.text((x + 10, 7), label, font=font, fill="white")
        sheet.paste(img.resize((width, 321), Image.Resampling.LANCZOS), (x, 40))
        sheet.paste(img.crop((600, 90, 1180, 410)), (x, 372))
    sheet.save(args.image, quality=95, subsampling=0)


if __name__ == "__main__":
    main()
