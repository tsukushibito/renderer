"""Cross-check cooked geometry, material texels and camera against three PBRT pixelmaterial hits."""

import argparse
import hashlib
import json
import struct
from pathlib import Path

import numpy as np
from PIL import Image

from prepare import CENTER


# First hit of the centre lens ray; PBRT reports centimetres in world space.
HITS = [
    ((962, 1372), (-2983.9524, 122.23373, -2804.4756), "DiffuseTransmission", "meadow_grass2_blos1b.png"),
    ((724, 1222), (-3058.038, 165.10266, -2456.9858), "CoatedDiffuse", "Betula_pendula_tr_06_co2.png"),
    ((1942, 1036), (397.8318, 663.4034, 4144.4053), "DiffuseTransmission", "Aesculus_hippocastanum_lf_02_su_co_fr.png"),
]


def top_level(path):
    with path.open("rb") as f:
        magic, w, h, _, srgb = struct.unpack("<8s4I", f.read(24))
        if magic != b"RIMAG001" or srgb != 1:
            raise ValueError(f"expected sRGB image at {path}")
        return hashlib.sha256(f.read(w*h*4)).digest()


def triangles(path):
    with path.open("rb") as f:
        magic, nv, ni = struct.unpack("<8sII", f.read(16))
        if magic != b"RMESH001":
            raise ValueError(path)
        vertices = np.fromfile(f, "<f4", nv*12).reshape(nv, 12)[:, :3]
        indices = np.fromfile(f, "<u4", ni).reshape(-1, 3)
        return vertices, indices, vertices.min(axis=0)-.001, vertices.max(axis=0)+.001


def match_surface(target, placement, vertices, indices, lower, upper):
    # Filter by bounding box before transforming triangles to world metres.
    transform = np.asarray(placement).T
    local = (np.linalg.inv(transform) @ np.append(target, 1))[:3]
    if np.any(local < lower) or np.any(local > upper):
        return np.inf
    world = vertices @ transform[:3, :3].T + transform[:3, 3]
    a, b, c = world[indices].transpose(1, 0, 2)
    n = np.cross(b-a, c-a)
    valid = np.linalg.norm(n, axis=1) > 0
    for start, end in ((a,b), (b,c), (c,a)):
        valid &= (np.cross(end-start, target-start)*n).sum(axis=1) >= 0
    distance = np.abs(((target-a[valid])*n[valid]).sum(axis=1)) / np.linalg.norm(n[valid], axis=1)
    return distance.min(initial=np.inf)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path)
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    scene = json.loads((args.package/"scene.json").read_text())
    objects = args.package/"objects"
    view = scene["settings"]["views"]["view-0"]
    z = np.array(view["forward"])
    x = np.cross(z, view["up"])
    x /= np.linalg.norm(x)
    y = np.cross(x, z)
    f = 1080/np.tan(np.radians(view["vertical_fov_degrees"])/2)
    materials = scene["physical"]["surfaces"]
    textures = {m["color"]["hash"] for m in materials if m["color"]}
    hashes = {k: top_level(objects/k) for k in textures}
    for pixel, pos, model, texture in HITS:
        target = (np.asarray(pos)-CENTER)*[-.01, .01, .01]
        ray = target - view["position"]
        screen = np.array([1920, 1080]) + f*np.array([x@ray, -y@ray])/(z@ray)
        error_px = np.linalg.norm(screen - (np.asarray(pixel)+.5))
        with Image.open(args.source/"textures"/texture) as im:
            digest = hashlib.sha256(im.convert("RGBA").tobytes()).digest()
        matches = {i for i, m in enumerate(materials) if m["scattering"] == model and m["color"]
                   and hashes[m["color"]["hash"]] == digest}
        meshes = {i: triangles(objects/scene["primitives"][i]["mesh"]["hash"]) for i in matches}
        best = min((match_surface(target, p["transform"], *meshes[p["primitive"]])
                    for p in scene["placements"] if p["primitive"] in meshes), default=np.inf)
        print(f"{pixel} {model} {texture}: {error_px:.5f} px, {best*1000:.5f} mm")
        if error_px >= .25 or best >= .001:
            raise ValueError("camera, surface or raw material texels disagree with PBRT")


if __name__ == "__main__":
    main()
