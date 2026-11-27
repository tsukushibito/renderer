"""Illuminate the same Landscape leaf and grass cards from opposite sides at fixed ISO."""

import argparse
import json
import os
from pathlib import Path
import shutil

os.environ["OPENCV_IO_ENABLE_OPENEXR"] = "1"
import cv2
import numpy as np
from optics_fixture import ply


def generate(source, out):
    out.mkdir(parents=True, exist_ok=True)
    names = ["Aesculus_hippocastanum_lf_02_su_co_fr", "meadow_grass2_blos1b"]
    # Bright front hemisphere and a dim fill. Rotating the same light by 180
    # degrees changes only the direction it illuminates the cards from.
    axis = (np.arange(256) + .5)/128 - 1
    u, v = np.meshgrid(axis, axis)
    sky = np.where(abs(u) + abs(v) > 1, .8, .04).astype("float32")
    if not cv2.imwrite(str(out/"hemispheres.exr"), np.repeat(sky[:, :, None], 3, 2)):
        raise OSError("could not write the fixture light")
    vertices = [(x, y, 0, 0, 0, -1, u, v) for x, y, u, v in
                [(-1,-1,0,0), (-1,1,0,1), (1,1,1,1), (1,-1,1,0)]]
    ply(out/"card.ply", vertices, [(0,1,2), (2,3,0)])
    shapes = []
    for i, name in enumerate(names):
        shutil.copyfile(source/"textures"/f"{name}.png", out/f"{name}.png")
        h = f'"texture displacement" "mask{i}"' if i == 0 else ""
        shapes.append(f'''
AttributeBegin
Translate {(-1 if i == 0 else 1)*2} 0 10
Scale 1.75 1.75 1
Texture "color{i}" "spectrum" "imagemap" "string filename" "{name}.png"
Texture "mask{i}" "float" "imagemap" "string filename" "{name}.png"
Material "diffusetransmission" "texture reflectance" "color{i}"
    "texture transmittance" "color{i}" "float scale" {(.5 if i == 0 else .8)} {h}
Shape "plymesh" "string filename" "card.ply" "texture alpha" "mask{i}"
AttributeEnd
''')
    for name, angle in [("front", 0), ("back", 180)]:
        (out/f"{name}.pbrt").write_text(f'''LookAt 0 0 0  0 0 1  0 1 0
Camera "perspective" "float fov" 25
Film "rgb" "integer xresolution" 256 "integer yresolution" 128 "float iso" 100
Sampler "halton" "integer pixelsamples" 512
WorldBegin
AttributeBegin
Rotate {angle} 1 0 0
LightSource "infinite" "string filename" "hemispheres.exr"
AttributeEnd
{"".join(shapes)}''', encoding="ascii")
    (out/"settings.json").write_text(json.dumps(dict(
        views={}, sun_direction=[0,1,0], sun_color=[1,1,1], sun_lux=0,
        sky_color=[0,0,0], shadow_resolution=64, shadow_distance=20,
        pbrt=dict(metres_per_unit=.01))), encoding="ascii")


def compare(out):
    for light in ["front", "back"]:
        def load(renderer):
            image = cv2.imread(str(out/f"{light}-{renderer}.exr"), cv2.IMREAD_UNCHANGED)
            if image is None or image.shape != (128, 256, 3) or not np.isfinite(image).all():
                raise ValueError(f"invalid {light} {renderer} linear film")
            return image[:, :, ::-1]

        actual, reference = load("sashiko"), load("pbrt")
        for name, left in [("leaf", 20), ("grass", 136)]:
            rect = np.s_[16:112, left:left+100]
            error = actual[rect] - reference[rect]
            print(light, name, "linear RGB MAE", abs(error).mean((0,1)), "bias", error.mean((0,1)))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--compare", action="store_true")
    args = parser.parse_args()
    if args.compare:
        compare(args.output)
    else:
        generate(args.source, args.output)
