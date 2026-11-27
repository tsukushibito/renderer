"""Prepare the official sky, cook recipe and distance-LOD geometry for the exported section."""
import argparse
import json
from pathlib import Path
import subprocess

import cv2
import numpy as np


def write_sky(path, out, size=1024):
    source = cv2.imread(str(path), cv2.IMREAD_ANYDEPTH | cv2.IMREAD_COLOR)
    if source is None:
        raise ValueError(f"Missing HDR panorama {path}")
    grid = (np.arange(size, dtype=np.float32) + 0.5) * (2 / size) - 1
    x, y = np.meshgrid(grid, grid)
    diagonal = 1 - abs(x) - abs(y)
    r = 1 - abs(diagonal)
    phi = np.pi / 4 * ((abs(y) - abs(x)) / np.maximum(r, 1e-10) + 1)
    radial = r * np.sqrt(2 - r * r)
    dx = np.copysign(radial * np.cos(phi), x)
    dy = np.copysign(radial * np.sin(phi), y)
    dz = np.copysign(1 - r * r, diagonal)
    u = (0.5 + np.arctan2(dz, dx) / (2 * np.pi)) * source.shape[1] - 0.5
    v = np.arccos(np.clip(dy, -1, 1)) * (source.shape[0] / np.pi) - 0.5
    rgb = cv2.remap(source, u, v.clip(0, source.shape[0] - 1), cv2.INTER_LINEAR,
                    borderMode=cv2.BORDER_WRAP)[:, :, ::-1]
    rgba = np.ones((size, size, 4), dtype="<f2")
    # The separate directional light supplies the solar disc. Bound its residual
    # energy in the environment used for both display and indirect lighting.
    rgba[:, :, :3] = np.clip(rgb, 0, 16)
    rgba.tofile(out)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path, help="task directory containing gltf/section.gltf")
    parser.add_argument("panorama", type=Path)
    parser.add_argument("meshoptimizer", type=Path, help="installed meshoptimizer 0.23.0 directory")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    subprocess.run(["node", str(Path(__file__).with_name("reduce.mjs")), str(args.directory / "gltf"),
                    str(args.meshoptimizer.resolve())], check=True)
    write_sky(args.panorama, args.directory / "sky.rgba16f")
    recipe = json.loads((repo / "assets/namaqualand/section-settings.json").read_text())
    gltf = json.loads((args.directory / "gltf/section.gltf").read_bytes())
    thresholds = {}
    for index, material in enumerate(gltf["materials"]):
        name = material["name"]
        if name.startswith("flower_") or name in ("tiny_purple_succulant", "crystalline_iceplant"):
            thresholds[index] = 900
        elif name.startswith("searsia_lucida"):
            thresholds[index] = 550
        elif name.startswith("namaqualand_boulder_"):
            thresholds[index] = 950
        elif name in ("landscape_main", "landscape_background", "damp_sand"):
            thresholds[index] = 1
    recipe.update(backdrop=dict(width=1024, height=1024, mip_levels=1, data="sky.rgba16f"),
                  reduced_gltf=dict(path="gltf/reduced.gltf", min_pixels=3000, material_min_pixels=thresholds))
    (args.directory / "recipe.json").write_text(json.dumps(recipe), encoding="utf8")


if __name__ == "__main__":
    main()
