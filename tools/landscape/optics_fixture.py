"""Generate matched PBRT/Sashiko sphere and depth-of-field controls in metres."""

import argparse
import json
import math
import os
from pathlib import Path
import struct

os.environ["OPENCV_IO_ENABLE_OPENEXR"] = "1"
import cv2
import numpy as np


def ply(path, vertices, faces):
    with path.open("wb") as out:
        fields = "".join(f"property float {k}\n" for k in "x y z nx ny nz u v".split())
        out.write((f"ply\nformat binary_little_endian 1.0\nelement vertex {len(vertices)}\n"
                   f"{fields}element face {len(faces)}\nproperty list uchar int vertex_indices\n"
                   "end_header\n").encode("ascii"))
        for vertex in vertices:
            out.write(struct.pack("<8f", *vertex))
        for face in faces:
            out.write(struct.pack("<B3i", 3, *face))


def sphere(path):
    rings, segments = 64, 128
    vertices, faces = [], []
    for i in range(rings + 1):
        theta = math.pi * i / rings
        for j in range(segments + 1):
            phi = math.tau * j / segments
            xyz = (math.sin(theta)*math.cos(phi), math.cos(theta), math.sin(theta)*math.sin(phi))
            vertices.append((*xyz, *xyz, j/segments, i/rings))
            if i < rings and j < segments:
                a = i*(segments+1) + j
                b = a + segments + 1
                if i > 0:
                    faces.append((a, a+1, b))
                if i+1 < rings:
                    faces.append((a+1, b+1, b))
    ply(path, vertices, faces)


def direction_map(path):
    # RGB encodes the three signed direction cosines in PBRT's light space.
    # All channels remain inside the film gamut under the fixture's exposure.
    axis = (np.arange(256) + .5) / 128 - 1
    u, v = np.meshgrid(axis, axis)
    diagonal = 1 - abs(u) - abs(v)
    r = 1 - abs(diagonal)
    phi = np.pi/4 * (1 + (abs(v)-abs(u))/r)
    radial = r*np.sqrt(2-r*r)
    direction = np.stack((radial*np.copysign(np.cos(phi), u), radial*np.copysign(np.sin(phi), v),
                          np.copysign(1-r*r, diagonal)), axis=-1)
    rgb = (.45 + .32*direction).astype("float32")
    if not cv2.imwrite(str(path), rgb[:, :, ::-1]):
        raise OSError(f"could not write {path}")


def scene(out, name, lens, geometry, iso=100):
    text = f'''LookAt 0 0 0  0 0 1  0 1 0
Camera "perspective" "float fov" 50 "float lensradius" {lens} "float focaldistance" 5
Sampler "sobol" "integer pixelsamples" 256
PixelFilter "box"
Film "rgb" "integer xresolution" 480 "integer yresolution" 270 "float iso" {iso}
Integrator "path" "integer maxdepth" 2
WorldBegin
LightSource "infinite" "string filename" "direction.exr"
{geometry}
'''
    (out/f"{name}.pbrt").write_text(text, encoding="ascii")


def verify(out):
    def linear(name):
        image = cv2.imread(str(out/f"{name}.exr"), cv2.IMREAD_UNCHANGED)
        if image is None or image.shape != (270, 480, 3) or not np.isfinite(image).all():
            raise ValueError(f"invalid 480×270 linear film {name}")
        return image[:, :, ::-1]

    rendered, reference = [linear(f"spheres-{source}") for source in ("renderer", "pbrt")]
    black = linear("black-renderer")
    twice = linear("spheres-double-iso-renderer")
    if twice.max() <= 1 or not np.allclose(twice, 2*rendered, atol=.001):
        raise ValueError("ISO response clipped or nonlinear")
    results = {"iso_200_peak": float(twice.max())}
    # Means over 9x9 windows retain directional gradients and average spectral noise.
    for name, x, subtract in [("gray", 150, 0), ("diffuse_only", 150, black), ("mirror", 330, 0)]:
        difference = rendered - subtract - reference
        patches = [difference[131+dy:140+dy, x-4+dx:x+5+dx].mean((0, 1))
                   for dx, dy in [(0,0), (-21,0), (21,0), (0,-21), (0,21)]]
        results[f"{name}_max_patch_mean_rgb_error"] = float(np.abs(patches).max())

    def edge(image, x):
        g = image[90:180, x-25:x+25, 1].mean(0)
        contrast = g[-8:].mean() - g[:8].mean()
        if contrast < .4:
            raise ValueError("missing black/sky edge")
        profile = (g - g[:8].mean()) / contrast
        # Find the first crossing, with interpolation between adjacent pixel centres.
        def crossing(level):
            i = int(np.flatnonzero(profile >= level)[0])
            return i - 25.5 + (level-profile[i-1])/(profile[i]-profile[i-1])
        return np.array([crossing(.5), crossing(.9)-crossing(.1)])

    focus, oracle = [linear(f"focus-{source}") for source in ("renderer", "pbrt")]
    edges = []
    for x in (96, 240, 384):
        actual, expected = edge(focus, x), edge(oracle, x)
        edges.append({"film_x": x, "centre_width": actual.tolist(), "pbrt": expected.tolist()})
        if max(abs(actual-expected)) > .5:
            raise ValueError(f"lens edge disagrees by more than half a film pixel at {x}")
    results["focus_edges_px"] = edges
    print(json.dumps(results, indent=2))
    if (results["gray_max_patch_mean_rgb_error"] > .015
            or results["diffuse_only_max_patch_mean_rgb_error"] > .003
            or results["mirror_max_patch_mean_rgb_error"] > .005):
        raise ValueError("fixed-exposure light or orientation mismatch")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--verify", action="store_true", help="compare rendered linear EXRs")
    args = parser.parse_args()
    out = args.directory
    if args.verify:
        verify(out)
        return
    out.mkdir(parents=True, exist_ok=True)
    direction_map(out/"direction.exr")
    sphere(out/"sphere.ply")
    balls = []
    for x, material in [(-1.55, '"diffuse" "rgb reflectance" [.18 .18 .18]'),
                        (1.55, '"conductor" "rgb reflectance" [.999 .999 .999]')]:
        balls.append(f'AttributeBegin\nTranslate {x} 0 5\nScale .8 .8 .8\nMaterial {material}\n'
                     'Shape "plymesh" "string filename" "sphere.ply"\nAttributeEnd')
    scene(out, "spheres", 0, "\n".join(balls))
    scene(out, "spheres-double-iso", 0, "\n".join(balls), iso=200)
    # Subtracting this matched render isolates diffuse irradiance from the
    # standard preview material's interface reflection at fixed exposure.
    scene(out, "black", 0, "\n".join(balls).replace("[.18 .18 .18]", "[0 0 0]"))

    # Each card ends at the same nominal film edge regardless of depth. Black
    # reflects no light, so the edge measures only coverage and the lens PSF.
    vertices = [(x,y,0, 0,0,-1, u,v) for x,y,u,v in
                [(-1,-1,0,0), (-1,1,0,1), (0,1,1,1), (0,-1,1,0)]]
    ply(out/"edge.ply", vertices, [(0,1,2), (2,3,0)])
    cards = ['Material "diffuse" "rgb reflectance" [0 0 0]']
    focal_pixels = 135 / math.tan(math.radians(25))
    for x, z in [(96, 2.5), (240, 5), (384, 10)]:
        scale = z/focal_pixels
        cards.append(f'AttributeBegin\nTranslate {(x-240)*scale} 0 {z}\n'
                     f'Scale {60*scale} {65*scale} 1\n'
                     'Shape "plymesh" "string filename" "edge.ply"\nAttributeEnd')
    scene(out, "focus", .14, "\n".join(cards))
    recipe = dict(views={}, sun_direction=[0,1,0], sun_color=[1,1,1], sun_lux=0,
                  sky_color=[0,0,0], shadow_resolution=64, shadow_distance=20)
    (out/"settings.json").write_text(json.dumps(recipe, indent=2), encoding="ascii")


if __name__ == "__main__":
    main()
