"""Measure and compose Bistro alpha alternatives; reject invisible/no-change views."""
import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, PngImagePlugin


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def compare(a, b, mask):
    if a.shape != b.shape or a.ndim != 3 or a.shape[2] != 3 or mask.shape != a.shape[:2]:
        raise ValueError("Alpha comparison image/mask dimensions differ")
    difference = np.abs(a.astype(np.int16) - b.astype(np.int16))
    visible = mask > 0
    changed = np.max(difference, axis=2) > 2
    if not np.any(visible):
        raise ValueError("Target has zero visible coverage; choose another view")
    if not np.any(changed & visible):
        raise ValueError("Opaque alternative has no measured difference on target coverage")
    return difference, {"coverage_pixels": int(visible.sum()),
                        "pixels_any_rgb_difference_above_2": int(changed.sum()),
                        "changed_pixels_on_coverage": int((changed & visible).sum()),
                        "mean_absolute_rgb_8bit": float(difference.mean()),
                        "maximum_rgb_8bit": int(difference.max())}


def compose(directory, preview, report_path):
    report = json.loads((directory / "renders.json").read_text(encoding="utf-8"))
    records = []
    columns = ("source alpha (selected)", "force opaque (diagnostic)", "absolute RGB difference x4", "target visible coverage")
    sheet = Image.new("RGB", (1920, 80 + 300 * len(report["views"])), "#17191d")
    draw = ImageDraw.Draw(sheet)
    draw.text((12, 10), "Bistro alpha interpretation | all scene meshes retained | CPU diagnostic, not Renderer/GPU acceptance", fill="white")
    draw.text((12, 30), f"Cycles CPU, {report['settings']['samples']} samples, seed 0 | forced opacity changes only the named material", fill="white")
    for row, view in enumerate(report["views"]):
        images = {}
        for variant, item in view["images"].items():
            path = directory / item["file"]
            if digest(path) != item["sha256"]:
                raise ValueError(f"Render hash differs: {path}")
            images[variant] = np.asarray(Image.open(path).convert("RGB"))
        a, b = images["source-alpha"], images["force-opaque"]
        difference, metrics = compare(a, b, images["coverage"][:, :, 0])
        records.append(dict(view=view["name"], material=view["material"], resolution=[a.shape[1], a.shape[0]], **metrics))
        draw.text((12, 68 + row * 300), f"{view['name']} | {view['material']}", fill="white")
        panels = (a, b, np.minimum(difference * 4, 255).astype(np.uint8), images["coverage"])
        for column, pixels in enumerate(panels):
            x, y = column * 480, 88 + row * 300
            draw.text((x + 8, y), columns[column], fill="white")
            sheet.paste(Image.fromarray(pixels).resize((480, 270), Image.Resampling.LANCZOS), (x, y + 20))
    metadata = PngImagePlugin.PngInfo()
    metadata.add_text("Attribution", "Amazon Lumberyard Bistro / NVIDIA ORCA / CC-BY 4.0; converted and rendered for Renderer alpha QA")
    preview.parent.mkdir(parents=True, exist_ok=True)
    sheet.save(preview, pnginfo=metadata)
    result = {"schema_version": 1, "render_report": report, "measurements": records,
              "preview": {"file": preview.name, "sha256": digest(preview)},
              "compositor_sha256": digest(Path(__file__)),
              "interpretation": "Controlled finite-sample CPU comparison. Coverage proves the target is in view; opacity changes produce direct and indirect differences. These statistics are not a GPU acceptance threshold or proof of artist intent."}
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(result, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    return records


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("renders", type=Path)
    parser.add_argument("preview", type=Path)
    parser.add_argument("report", type=Path)
    args = parser.parse_args()
    print(json.dumps(compose(args.renders, args.preview, args.report), indent=2))


if __name__ == "__main__":
    main()
