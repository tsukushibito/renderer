"""Compose hash-checked Bistro views and lighting alternatives into one image."""
import argparse
import hashlib
import json
import math
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont, PngImagePlugin


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def compose(directory, preview, report_path, config_path=None):
    render = json.loads((directory / "renders.json").read_text(encoding="utf-8"))
    config = None
    if config_path:
        config_bytes = config_path.read_bytes()
        if hashlib.sha256(config_bytes).hexdigest() != render["config_sha256"]:
            raise ValueError("Caption configuration differs from the rendered configuration")
        config = json.loads(config_bytes)
    selected = []
    alternatives = []
    measurements = []
    for record in render["views"]:
        images = {}
        for variant in record["variants"]:
            for kind in ("png", "exr", "noisy_exr") if "noisy_exr" in variant else ("png", "exr"):
                artifact = variant[kind]
                if digest(directory / artifact["file"]) != artifact["sha256"]:
                    raise ValueError(f"{kind} artifact hash differs: {artifact['file']}")
            pixels = np.asarray(Image.open(directory / variant["png"]["file"]).convert("RGB"))
            if list(pixels.shape[:2][::-1]) != render["settings"]["resolution"]:
                raise ValueError("Render image dimensions differ from report")
            images[variant["name"]] = pixels
            label = f"{record['view']['name']} | {variant['name']}"
            item = (label, pixels, variant)
            if variant["name"] == "selected":
                selected.append(item)
            else:
                alternatives.append(item)
        for name, pixels in images.items():
            if name != "selected":
                difference = np.abs(pixels.astype(np.int16) - images["selected"].astype(np.int16))
                measurements.append({"view": record["view"]["name"], "alternative": name,
                                     "mean_absolute_rgb8": float(difference.mean()),
                                     "maximum_rgb8": int(difference.max()),
                                     "pixels_any_rgb_difference_above_2": int((difference.max(axis=2) > 2).sum())})
    if not selected:
        raise ValueError("No selected views")
    tile_width, tile_height, row_height = 480, 270, 310
    selected_rows = math.ceil(len(selected) / 3)
    comparison = []
    if alternatives:
        by_view = {}
        for item in selected:
            by_view[item[0].split(" | ")[0]] = item
        for item in alternatives:
            view_name = item[0].split(" | ")[0]
            if not comparison or comparison[-1][0].split(" | ")[0] != view_name:
                if not any(i[0] == by_view[view_name][0] for i in comparison):
                    comparison.append(by_view[view_name])
            comparison.append(item)
    comparison_rows = math.ceil(len(comparison) / 3)
    sheet = Image.new("RGB", (tile_width * 3, 100 + (selected_rows + comparison_rows) * row_height + (30 if comparison else 0)), "#17191d")
    draw = ImageDraw.Draw(sheet)
    draw.text((12, 10), "Bistro fixed composition and lighting | complete exterior | CPU preview, not native GPU acceptance", fill="white")
    draw.text((12, 30), f"Cycles CPU {render['settings']['samples']} samples, seed {render['settings']['seed']} | Khronos PBR Neutral + one sRGB encoding", fill="white")
    draw.text((12, 50), f"{render['mesh_objects']} mesh objects, {render['triangles']:,} triangles, {render['materials']} materials retained | no alpha override", fill="white")
    denoise = " | CPU OIDN composition aid; noisy EXR retained" if render["settings"].get("denoising") else ""
    draw.text((12, 76), "FIXED VIEWS / selected sun + luminance-capped IBL" + denoise, fill="#99c8ff")
    def panel(item, index, top):
        label, pixels, variant = item
        column, row = index % 3, index // 3
        x, y = column * tile_width, top + row * row_height
        draw.text((x + 8, y), label, fill="white")
        draw.text((x + 8, y + 15), f"EV {variant['exposure_ev']:+g} | IBL {variant['ibl_strength']:g} | sun x{variant['sun_multiplier']:g}", fill="#b4bbc5")
        image = Image.fromarray(pixels).resize((tile_width, tile_height), Image.Resampling.LANCZOS)
        sheet.paste(image, (x, y + 34))
    for index, item in enumerate(selected):
        panel(item, index, 100)
    if len(selected) % 3 == 1:
        top = 100 + (selected_rows - 1) * row_height
        title_font = ImageFont.load_default(size=19)
        note_font = ImageFont.load_default(size=16)
        def notes(column, title, paragraphs):
            x, y = column * tile_width + 14, top + 16
            draw.text((x, y), title, fill="#99c8ff", font=title_font)
            y += 34
            for paragraph in paragraphs:
                line = ""
                for word in paragraph.split():
                    candidate = (line + " " + word).strip()
                    if draw.textbbox((0, 0), candidate, font=note_font)[2] > tile_width - 32 and line:
                        draw.text((x, y), line, fill="#cbd1da", font=note_font)
                        y += 21
                        line = word
                    else:
                        line = candidate
                if line:
                    draw.text((x, y), line, fill="#cbd1da", font=note_font)
                    y += 21
                y += 4
            if y > top + row_height:
                raise ValueError("Preview note text exceeds its tile")
        route_notes = ["Fixed logical-time tour; no walking physics."]
        if config:
            route = config["route"]
            route_notes = [f"{route['duration_seconds']:g} seconds / {route['fps']} Hz logical time"]
            route_notes += [f"{key['seconds']:g} s: {key['view']}" for key in route["keyframes"]]
            route_notes += [f"{render['route_clearance']['logical_frames']} frames / {len(route['capture_frames'])} scheduled captures"]
        clearance = render["route_clearance"]
        clear = not clearance["blocked_eye_path_segments"] and not clearance["near_plane_intrusions"]
        route_notes += [f"Eye segments and {clearance['near_plane_rays']:,} near-plane rays: {'clear' if clear else 'obstructed'}.",
                        "Point/ray audit, not continuous swept volume."]
        notes(1, "REPEATABLE CAMERA TOUR", route_notes)
        notes(2, "SCOPE OF THESE IMAGES", [
            "All geometry/materials retained. Frustum framing does not prove occlusion visibility.",
            "The label closeup is naturally dark; no special fill light was added.",
            "Color here cannot prove zero depth or shadow coverage: use alpha diagnostics and B2/B3 gates.",
            ("CPU OIDN aids composition. Original noisy HDR is retained; these are not native GPU acceptance images."
             if render["settings"].get("denoising") else "No denoising. These are not native GPU acceptance images.")])
    if comparison:
        top = 100 + selected_rows * row_height
        draw.text((12, top + 8), "LIGHTING ALTERNATIVES / identical camera and exposure; finite-sample differences are not runtime thresholds", fill="#99c8ff")
        for index, item in enumerate(comparison):
            panel(item, index, top + 30)
    metadata = PngImagePlugin.PngInfo()
    metadata.add_text("Attribution", "Amazon Lumberyard Bistro / NVIDIA ORCA / CC-BY 4.0; san_giuseppe_bridge by Greg Zaal / Poly Haven / CC0; converted and rendered for Renderer B0")
    preview.parent.mkdir(parents=True, exist_ok=True)
    sheet.save(preview, pnginfo=metadata)
    result = {"schema_version": 1, "render_report": render, "measurements": measurements,
              "preview": {"file": preview.name, "sha256": digest(preview), "size": preview.stat().st_size,
                          "resolution": list(sheet.size)},
              "compositor_sha256": digest(Path(__file__)),
              "interpretation": "Composition and lighting alternatives in a finite-sample CPU path tracer. No two-API raster image threshold, native depth/shadow proof, performance claim or guarantee of original author intent."}
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(result, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    return {"selected_views": len(selected), "alternatives": len(alternatives), "preview": result["preview"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("renders", type=Path)
    parser.add_argument("preview", type=Path)
    parser.add_argument("report", type=Path)
    parser.add_argument("--config", type=Path, help="Hash-matched config for route captions")
    args = parser.parse_args()
    print(json.dumps(compose(args.renders, args.preview, args.report, args.config), indent=2))


if __name__ == "__main__":
    main()
