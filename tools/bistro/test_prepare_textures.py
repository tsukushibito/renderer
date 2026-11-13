import json
from pathlib import Path
import struct
import tempfile
import unittest

import numpy as np
from PIL import Image

from prepare_textures import prepare


def bc5_fixture(path):
    header = bytearray(128)
    header[:4] = b"DDS "
    for offset, value in ((4, 124), (8, 0x81007), (12, 8), (16, 4), (20, 32),
                          (28, 1), (76, 32), (80, 4), (108, 0x1000)):
        struct.pack_into("<I", header, offset, value)
    header[84:88] = b"ATI2"
    # Two distinct rows of blocks distinguish normal green inversion from image flipping.
    block = lambda value: bytes((value, value)) + b"\0" * 6
    path.write_bytes(header + block(128) + block(32) + block(128) + block(224))


class TextureConversionTests(unittest.TestCase):
    def test_bc5_normal_sign_and_positive_z_preserve_spatial_orientation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bc5_fixture(root / "normal.dds")
            report = prepare(root, [{"path": "normal.dds", "role": "normal"}], root / "out")
            image = np.asarray(Image.open(root / "out/normal.png"))
            self.assertTrue(np.all(image[:, :, 0] == 128))
            self.assertTrue(np.all(image[:4, :, 1] == 223))
            self.assertTrue(np.all(image[4:, :, 1] == 31))
            self.assertTrue(np.all(image[:, :, 2] > 200))
            self.assertTrue(report["textures"][0]["pixel_roundtrip"])

    def test_base_color_alpha_and_orm_channels_are_not_reinterpreted(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pixels = np.array([[[12, 89, 203, 0], [230, 45, 17, 127]]], dtype=np.uint8)
            Image.fromarray(pixels).save(root / "base.png")
            Image.fromarray(pixels[:, :, :3]).save(root / "orm.png")
            report = prepare(root, [{"path": "base.png", "role": "base_color"},
                                    {"path": "orm.png", "role": "orm"}], root / "out")
            self.assertTrue(np.array_equal(np.asarray(Image.open(root / "out/base.png")), pixels))
            self.assertTrue(np.array_equal(np.asarray(Image.open(root / "out/orm.png")), pixels[:, :, :3]))
            self.assertEqual(report["textures"][0]["alpha"]["fractional"], 1)
            self.assertEqual(report["textures"][1]["colorspace"], "linear-data")


if __name__ == "__main__":
    unittest.main()
