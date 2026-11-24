"""Compare matched floor/remedy reflection captures for Bistro and Sponza."""

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw

from awning_reference import BANDS, unit
from diagnose_daylight import LUMA


def pixels(directory, meta, key):
    dtype, fmt = {"hdr": ("<f2", "rgba16f-le"), "sdr": ("u1", "rgba8-unorm"),
                  "depth": ("<f4", "r32f-le")}[key]
    record = meta["captures"][key]
    data = (directory / (key + ".bin")).read_bytes()
    if record["format"] != fmt or hashlib.sha256(data).hexdigest() != record["sha256"]:
        raise ValueError(f"Invalid {key} capture in {directory}")
    return np.frombuffer(data, dtype).reshape(record["height"], record["width"], -1)


def load(root, scene, view, method):
    passes = {}
    common = atlas = None
    for mode in ("beauty", "specular", "weights"):
        path = root / f"{scene}-{method}-{view}" / mode
        report = json.loads((path / "report.json").read_text())
        ident = report["identity"]
        look = ident["look"]
        if (not report["validation"]["clean"] or ident["view"] != view or ident["aa"]
                or ident["pipeline"] != "forward" or not ident["shadows"] or ident["bloom"] != "off"
                or look["scenario_blend"] != 0
                or look["reflection_view"] != ("Weights" if mode == "weights" else "Shaded")
                or look["lighting_pass"] != ("Specular" if mode == "specular" else "Beauty")):
            raise ValueError(f"Unexpected render settings in {path}")
        signature = (ident["conditions_sha256"], ident["cook_manifest_sha256"],
                     ident["environment"]["manifest_sha256"], ident["shaders_sha256"],
                     report["captures"]["depth"]["sha256"], look["name"], look["exposure_ev"],
                     look["baked_lighting_sha256"])
        digest = look["reflection_probes_sha256"]
        if common is not None and (signature, digest) != (common, atlas):
            raise ValueError(f"Unmatched passes in {path}")
        common, atlas = signature, digest
        if mode == "beauty":
            passes["depth"] = pixels(path, report, "depth")[:, :, 0]
        passes[mode] = pixels(path, report, "sdr" if mode == "beauty" else "hdr")[:, :, :3]
    return passes, common, atlas


def sponza_regions(view, depth):
    conditions = json.loads((Path(__file__).resolve().parents[1] / "sponza/conditions.json").read_text())
    camera = next(v for v in conditions["views"] if v["name"] == view)
    eye = np.array(camera["position"])
    forward = unit(np.array(camera["target"]) - eye)
    right = unit(np.cross(forward, [0, 1, 0]))
    up = np.cross(right, forward)
    h, w = depth.shape
    y, x = np.indices((h, w))
    scale = h / (2 * np.tan(np.deg2rad(camera["vertical_fov_degrees"] / 2)))
    n, f = (conditions["camera"][k] for k in ("near_m", "far_m"))
    dist = n * f / (f - (f - n) * depth.astype(float))
    rays = forward + ((x + 0.5 - w / 2)[:, :, None] * right
                      - (y + 0.5 - h / 2)[:, :, None] * up) / scale
    xx, yy, zz = (eye + dist[:, :, None] * rays).transpose(2, 0, 1)
    interior = (depth < 1) & (abs(xx) < 15) & (abs(zz) < 8) & (yy >= -0.1)
    return {"atrium": interior & (abs(zz) <= 2.4) & (yy < 3.6),
            "side aisle": interior & (abs(zz) > 2.4) & (yy < 3.6),
            "upper gallery": interior & (abs(zz) > 2.4) & (yy >= 3.6) & (yy < 8.7)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bistro_captures", type=Path)
    parser.add_argument("sponza_captures", type=Path)
    parser.add_argument("image", type=Path)
    args = parser.parse_args()
    sheet = Image.new("RGB", (960, 4 * 300), "#202328")
    draw = ImageDraw.Draw(sheet)
    for row, (scene, view) in enumerate([("bistro", "bistro-corner"), ("sponza", "renderer"),
                                          ("sponza", "gallery")]):
        root = args.bistro_captures if scene == "bistro" else args.sponza_captures
        before, identity, old = load(root, scene, view, "floor")
        after, trial_identity, new = load(root, scene, view, "remedy")
        if identity != trial_identity or old == new:
            raise ValueError(f"Expected matched scenes and different atlases for {view}")
        if scene == "bistro":
            regions = {k: np.s_[y + 1:y + 10:3, 839:909:12] for k, y in BANDS.items()}
            regions.update({"painted facade": np.s_[312:418, 534:572],
                            "sign": np.s_[139:184, 740:942], "sunlit road": np.s_[878:962, 70:242],
                            "shaded road": np.s_[719:774, 1306:1440]})
        else:
            regions = sponza_regions(view, before["depth"])
        print(view, "mean linear specular RGB, mean local weight, mean per-pixel max SDR delta")
        delta = abs(after["beauty"].astype("i2") - before["beauty"]).max(axis=2)
        for name, region in regions.items():
            for method, data in [("floor", before), ("remedy", after)]:
                rgb = data["specular"][region].reshape(-1, 3).astype(float).mean(axis=0)
                local = 1 - data["weights"][region][..., 2].astype(float).mean()
                print(name, method, delta[region].size, rgb.round(6), round(rgb @ LUMA, 6),
                      round(local, 4), round(delta[region].mean(), 3))
        for col, (method, data) in enumerate([("floor", before), ("remedy", after)]):
            frame = Image.fromarray(data["beauty"])
            items = [(view, frame)] if row else [(view, frame), ("awning", frame.crop((600, 90, 1180, 410)))]
            for index, (label, img) in enumerate(items):
                x, y = col * 480, (row + int(row > 0) + index) * 300
                draw.text((x + 8, y + 4), f"{label} / {method}", fill="white")
                sheet.paste(img.resize((480, 265), Image.Resampling.LANCZOS), (x, y + 23))
    sheet.save(args.image, quality=91)


if __name__ == "__main__":
    main()
