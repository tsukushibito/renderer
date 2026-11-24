"""Encode the padded linear RGBA32F atlas supplied by bistro-bake on stdin."""
import sys

import ispc_texcomp
import numpy as np


def encode(data, width, height):
    if width % 4 or height % 4 or width <= 0 or height <= 0:
        raise ValueError("BC6H probe planes need whole blocks")
    pixels = np.frombuffer(data, dtype="<f4").reshape(height, width, 4)
    if not np.isfinite(pixels).all() or pixels.min() < 0 or pixels.max() > 65504:
        raise ValueError("Probe exceeds the finite unsigned BC6H range")
    surface = ispc_texcomp.RGBASurface(pixels.astype("<f2").tobytes(), width, height, width * 8)
    settings = ispc_texcomp.BC6HEncSettings.from_profile("slow")
    blocks = ispc_texcomp.compress_blocks_bc6h(surface, settings)
    if len(blocks) != width * height:
        raise ValueError("Unexpected BC6H payload length")
    return blocks


if __name__ == "__main__":
    sys.stdout.buffer.write(encode(sys.stdin.buffer.read(), int(sys.argv[1]), int(sys.argv[2])))
