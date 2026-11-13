"""Save conversion-QA previews and measured differences without claiming GPU image acceptance."""
import argparse
import json
import math
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, PngImagePlugin


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("converted", type=Path)
    parser.add_argument("views", type=Path)
    parser.add_argument("preview", type=Path)
    parser.add_argument("report", type=Path)
    args = parser.parse_args()
    views = json.loads(args.views.read_text(encoding="utf-8"))["views"]
    sheet = Image.new("RGB", (1920, 40 + 384 * len(views)), "#17191d")
    draw = ImageDraw.Draw(sheet)
    draw.text((12, 12), "Bistro conversion QA | Source PBR recipe / glTF reimport / absolute RGB difference x8 | Cycles CPU, 8 samples", fill="white")
    records = []
    for row, view in enumerate(views):
        name = view["name"]
        a = np.asarray(Image.open(args.source / f"{name}.png").convert("RGB"))
        b = np.asarray(Image.open(args.converted / f"{name}.png").convert("RGB"))
        if a.shape != b.shape:
            raise ValueError(f"Preview dimensions differ: {name}")
        difference = np.abs(a.astype(np.int16) - b.astype(np.int16))
        mse = float(np.mean(difference.astype(np.float64) ** 2))
        records.append({"view": name, "resolution": [a.shape[1], a.shape[0]],
                        "mean_absolute_rgb_8bit": float(difference.mean()), "maximum_rgb_8bit": int(difference.max()),
                        "rmse_rgb_8bit": math.sqrt(mse), "psnr_db": 10 * math.log10(255 ** 2 / mse) if mse else None,
                        "pixels_any_channel_difference_above_2": int(np.count_nonzero(np.max(difference, axis=2) > 2))})
        images = (Image.fromarray(a), Image.fromarray(b), Image.fromarray(np.minimum(difference * 8, 255).astype(np.uint8)))
        for column, image in enumerate(images):
            x, y = column * 640, 40 + row * 384
            draw.text((x + 8, y + 5), f"{name} | {('source recipe', 'glTF reimport', 'difference x8')[column]}", fill="white")
            sheet.paste(image.resize((640, 360), Image.Resampling.LANCZOS), (x, y + 24))
    metadata = PngImagePlugin.PngInfo()
    metadata.add_text("Attribution", "Amazon Lumberyard Bistro, NVIDIA ORCA, CC-BY 4.0; converted and rendered for Renderer asset QA")
    args.preview.parent.mkdir(parents=True, exist_ok=True)
    sheet.save(args.preview, pnginfo=metadata)
    report = {"schema_version": 1, "views": records, "preview": args.preview.name,
              "interpretation": "Measured low-sample CPU conversion QA; noise and Blender packed-normal/import behavior affect pixels. Inspect images with independent geometry and material checks; not a final runtime threshold or Vulkan/D3D12 acceptance."}
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(records, indent=2))


if __name__ == "__main__":
    main()
