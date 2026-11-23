"""Capture the two Sponza floor controls and isolate direct rectangular lighting."""

import argparse
import json
import subprocess
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw


VIEWS = [
    ("gallery", (960, 540), (160, 425, 230, 478)),
    ("renderer", (1152, 888), (585, 770, 680, 842)),
]


def capture(args, view, size, mode, *, technique="forward", label=None, options=()):
    directory = args.output / (label or f"{view}-{mode}")
    subprocess.run([
        str(args.binary.resolve()), "--manifest", str(args.manifest),
        "--ibl-manifest", str(args.ibl_manifest), "--look", "o3de-atrium",
        "--api", args.api, "--pipeline", technique, "--view", view,
        "--width", str(size[0]), "--height", str(size[1]), "--inflight", "1",
        "--warmup", "2", "--frames", "4", "--gpu-timing",
        "--baked-lighting", str(args.baked_lighting),
        "--reflection-probes", str(args.reflection_probes),
        "--rect-lights", mode, "--output", str(directory), *options,
    ], check=True)
    report = json.loads((directory / "report.json").read_text())
    if not report["validation"]["clean"]:
        raise ValueError(f"backend validation failed in {directory}")
    width, height = size
    hdr = np.fromfile(directory / "hdr.bin", "<f2").reshape(height, width, 4)
    if not np.isfinite(hdr).all():
        raise ValueError(f"nonfinite HDR in {directory}")
    beauty = Image.frombytes("RGBA", size, (directory / "sdr.bin").read_bytes()).convert("RGB")
    return hdr[:, :, :3].astype(np.float32), beauty, (directory / "depth.bin").read_bytes()


def display_direct(direct):
    linear = np.clip(2.0 * direct, 0, 1)
    srgb = np.where(linear <= 0.0031308, 12.92 * linear, 1.055 * linear ** (1 / 2.4) - 0.055)
    return Image.fromarray(np.rint(255 * srgb).astype(np.uint8))


def verify_cache(args):
    view, size, roi = VIEWS[0]
    x, y, x_end, y_end = roi
    for technique in ("forward", "forward-plus", "deferred"):
        def take(mode, label, *options):
            return capture(args, view, size, mode, technique=technique,
                           label=f"{technique}-{label}", options=("--sun-shadows-off", *options))[0]

        lit = take("unshadowed", "sun-off-lit")
        shaded = take("shadowed", "sun-off-shadowed")
        if (lit[y:y_end, x:x_end] - shaded[y:y_end, x:x_end]).mean() < 0.01:
            raise ValueError(f"{technique}: missing rectangle shadow with sun shadows off")
        motion = ("--reflection-spheres", "-6", "1.5", "1", "--reflection-spheres-to", "-4", "1.5", "1")
        partial = take("shadowed", "partial", *motion)
        complete = take("shadowed", "full", *motion, "--redraw-shadows")
        if not np.array_equal(partial, complete):
            raise ValueError(f"{technique}: partial update differs from full redraw")
        measurement = json.loads((args.output / f"{technique}-partial/report.json").read_text())["measurement"]
        faces = measurement["rectangle_shadow_faces_rendered"]
        if not 0 < faces < 10 * measurement["frames"]:
            raise ValueError(f"{technique}: unexpected partial update count {faces}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ["binary", "manifest", "ibl-manifest", "baked-lighting", "reflection-probes", "output"]:
        parser.add_argument("--" + option, type=Path, required=True)
    parser.add_argument("--api", choices=["vulkan", "d3d12"], default="vulkan")
    parser.add_argument("--verify-cache", action="store_true", help="also compare partial and full updates for all PBR techniques")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    if args.verify_cache:
        verify_cache(args)
    sheet = Image.new("RGB", (1280, 568), "#192027")
    draw = ImageDraw.Draw(sheet)
    for column, title in enumerate(["Full / shadows off", "Full / shadows on",
                                     "Rectangle direct / shadows off", "Rectangle direct / shadows on"]):
        draw.text((320 * column + 8, 6), title, fill="white")
    results = {}
    for row, (view, size, roi) in enumerate(VIEWS):
        baseline, _, depth = capture(args, view, size, "off")
        panels = []
        direct_panels = []
        luminance = []
        for mode in ["unshadowed", "shadowed"]:
            hdr, beauty, current_depth = capture(args, view, size, mode)
            if depth != current_depth:
                raise ValueError(f"{view} geometry changed between captures")
            direct = hdr - baseline
            x, y, xmax, ymax = roi
            luminance.append(float((direct[y:ymax, x:xmax] @ [0.2126, 0.7152, 0.0722]).mean()))
            visual = display_direct(direct)
            visual.save(args.output / f"{view}-{mode}-direct.png")
            beauty.save(args.output / f"{view}-{mode}-beauty.png")
            panels.append(beauty)
            direct_panels.append(visual)
        if luminance[0] <= 0:
            raise ValueError(f"{view} has no unshadowed rectangle contribution")
        ratio = luminance[1] / luminance[0]
        results[view] = {"roi": roi, "unshadowed_direct_Y": luminance[0],
                         "shadowed_direct_Y": luminance[1], "ratio": ratio}
        for col, panel in enumerate(panels + direct_panels):
            preview = panel.copy()
            ImageDraw.Draw(preview).rectangle(roi, outline="#ffce59", width=4)
            preview.thumbnail((316, 226), Image.Resampling.LANCZOS)
            sheet.paste(preview, (col * 320 + 2, row * 268 + 45))
        title = "Side arcade" if view == "gallery" else "Central walkway"
        draw.text((8, row * 268 + 28), f"{title}    Direct luminance retained {ratio:.1%}", fill="white")
    sheet.save(args.output / "sponza-rectangle-shadows.jpg", quality=92)
    print(json.dumps(results, indent=2))
    if abs(results["gallery"]["ratio"]) > 0.01 or abs(results["renderer"]["ratio"] - 1) > 0.05:
        raise SystemExit("floor control outside tolerance")


if __name__ == "__main__":
    main()
