"""Compare cooked PBR lighting with a direct integral over every source EXR texel."""

import argparse
import json
from pathlib import Path

import numpy as np


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path)
    args = parser.parse_args()
    scene = json.loads((args.package/"scene.json").read_text())
    sky = scene["physical"]["sky"]
    env = scene["environment"]

    def image(meta):
        path = args.package/"objects"/meta["data"]["hash"]
        return np.memmap(path, dtype="<f2", mode="r")

    source = image(sky["equal_area_radiance"]).reshape(sky["equal_area_radiance"]["height"], -1, 4)
    rotate = np.array(sky["world_from_light"])[:3, :3]
    diffuse = env["maps"][0]
    w, h = diffuse["width"], diffuse["height"]
    irradiance = image(diffuse).reshape(h, w, 4)
    # Texel centres near zenith, nadir and the four horizon directions.
    probes = np.array([(w//2, 0), (w//2, h-1), (0, h//2), (w//4, h//2),
                       (w//2, h//2), (3*w//4, h//2)])
    phi = 2*np.pi*((probes[:, 0]+.5)/w-.5)
    theta = np.pi*(probes[:, 1]+.5)/h
    normals = np.array([np.sin(theta)*np.cos(phi), np.cos(theta), np.sin(theta)*np.sin(phi)]).T
    exact = np.zeros((len(normals), 3))
    size = source.shape[0]
    u = np.arange(size)[None, :]*(2/size) + 1/size-1
    for row in range(0, size, 32):
        stop = min(size, row+32)
        v = np.arange(row, stop)[:, None]*(2/size) + 1/size-1
        diagonal = 1-abs(u)-abs(v)
        r = 1-abs(diagonal)
        angle = np.pi/4*(1+np.divide(abs(v)-abs(u), r, where=r!=0, out=np.zeros_like(r)))
        radial = r*np.sqrt(2-r*r)
        direction = np.stack([radial*np.copysign(np.cos(angle), u), radial*np.copysign(np.sin(angle), v),
                              np.copysign(1-r*r, diagonal)], axis=-1).reshape(-1, 3) @ rotate
        weights = np.maximum(direction @ normals.T, 0.)
        exact += weights.T @ source[row:stop, :, :3].reshape(-1, 3).astype(np.float64)
    exact *= 4*np.pi/(size*size)*sky["scale"]
    actual = irradiance[probes[:, 1], probes[:, 0], :3].astype(np.float64)*env["nits_per_unit"]
    relative = np.abs(actual-exact)/np.maximum(exact, 1e-12)
    for pixel, expected, measured, delta in zip(probes, exact, actual, relative):
        print(pixel.tolist(), "irradiance", expected.round(4), "map", measured.round(4),
              "worst relative error", round(delta.max(), 5))


if __name__ == "__main__":
    main()
