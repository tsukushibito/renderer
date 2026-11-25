"""IBL boundary, analytic lighting, orientation and cook reproducibility checks."""
import hashlib
import math
from pathlib import Path
import tempfile
import unittest

import numpy as np

import ibl
from ibl_pilot import runtime_specular


def decode_rgbe(payload):
    """Independent small Radiance reader for the writer's canonical RLE stream."""
    header, pixels = payload.split(b"\n\n", 1)
    if b"FORMAT=32-bit_rle_rgbe" not in header:
        raise ValueError("Invalid format")
    resolution, pixels = pixels.split(b"\n", 1)
    words = resolution.decode("ascii").split()
    height, width = int(words[1]), int(words[3])
    rows = []
    cursor = 0
    for _ in range(height):
        if 8 <= width <= 32767:
            if pixels[cursor:cursor + 4] != bytes([2, 2, width >> 8, width & 255]):
                raise ValueError("Invalid scanline")
            cursor += 4
            channels = []
            for _ in range(4):
                channel = bytearray()
                while len(channel) < width:
                    count = pixels[cursor]
                    cursor += 1
                    if count > 128:
                        channel.extend([pixels[cursor]] * (count - 128))
                        cursor += 1
                    else:
                        channel.extend(pixels[cursor:cursor + count])
                        cursor += count
                if len(channel) != width:
                    raise ValueError("Bad RLE count")
                channels.append(np.frombuffer(channel, dtype=np.uint8))
            rows.append(np.stack(channels, axis=-1))
        else:
            rows.append(np.frombuffer(pixels[cursor:cursor + width * 4], dtype=np.uint8).reshape(width, 4))
            cursor += width * 4
    if cursor != len(pixels):
        raise ValueError("Trailing bytes")
    rgbe = np.stack(rows)
    return rgbe[:, :, :3].astype(np.float64) * np.exp2(rgbe[:, :, 3].astype(np.float64) - 136)[:, :, None]


class IblTests(unittest.TestCase):
    def test_cap_precedes_filtering_and_preserves_chromaticity(self):
        source = np.array([[[100., 20., 1.], [.1, .2, .3]]])
        actual = ibl.capped_source(source)
        luminance = actual @ np.array([.2126, .7152, .0722])
        self.assertLessEqual(luminance.max(), 4.000001)
        np.testing.assert_allclose(actual[0, 0] / source[0, 0], [actual[0, 0, 0] / 100] * 3, rtol=1e-7)
        np.testing.assert_allclose(actual[0, 1], source[0, 1], atol=1e-8)
        self.assertFalse(np.allclose(actual.mean(axis=1), ibl.capped_source(source.mean(axis=1, keepdims=True))[:, 0]))

    def test_hammersley_sequence_is_pinned(self):
        np.testing.assert_array_equal(ibl.hammersley(4), [[0, 0], [.25, .5], [.5, .25], [.75, .75]])
        for count in (0, -1, True):
            with self.assertRaises(ValueError):
                ibl.hammersley(count)

    def test_latlong_cardinals_seam_poles_and_row_order(self):
        # Red changes only with longitude, green only with latitude.
        image = np.zeros((4, 8, 3))
        image[:, :, 0] = np.arange(8)[None, :]
        image[:, :, 1] = np.arange(4)[:, None]
        directions = np.array([[1, 0, 0], [0, 0, 1], [0, 0, -1], [0, 1, 0], [0, -1, 0], [-1, 0, 0]])
        actual = ibl.sample_environment(image, directions)
        np.testing.assert_allclose(actual[:3, 0], [3.5, 5.5, 1.5])
        np.testing.assert_allclose(actual[:3, 1], [1.5] * 3)
        self.assertEqual(actual[3, 1], 0)
        self.assertEqual(actual[4, 1], 3)
        self.assertEqual(actual[5, 0], 3.5)
        normals = ibl.directions(8, 4)
        np.testing.assert_allclose(ibl.sample_environment(image, normals), image, atol=2e-15)
        self.assertTrue((normals[0, :, 1] > 0).all())
        self.assertTrue((normals[-1, :, 1] < 0).all())

    def test_constant_environment_has_analytic_diffuse_and_every_specular_mip(self):
        color = np.array([.2, .5, 1.])
        source = np.broadcast_to(color, (8, 16, 3)).copy()
        diffuse = ibl.irradiance(source, 8, 4, samples=1024, chunk=3)
        np.testing.assert_allclose(diffuse, np.broadcast_to(math.pi * color, diffuse.shape), rtol=2e-14)
        levels = ibl.prefilter(source, 16, 8, samples=1024, chunk=3)
        self.assertEqual([level.shape[:2] for level in levels], [(8, 16), (4, 8), (2, 4), (1, 2), (1, 1)])
        for level in levels:
            np.testing.assert_allclose(level, np.broadcast_to(color, level.shape), rtol=2e-14)
        packed = np.frombuffer(ibl.rgba16_bytes([diffuse]), dtype="<f2").reshape(4, 8, 4)
        np.testing.assert_array_equal(packed[:, :, :3], np.broadcast_to((math.pi * color).astype("<f2"), (4, 8, 3)))

    def test_chunk_size_does_not_change_cook_bytes(self):
        direction = ibl.directions(16, 8)
        source = .6 + .5 * direction
        a = [ibl.irradiance(source, 8, 4, samples=64, chunk=1)]
        a += ibl.prefilter(source, 16, 8, samples=64, chunk=1)
        b = [ibl.irradiance(source, 8, 4, samples=64, chunk=7)]
        b += ibl.prefilter(source, 16, 8, samples=64, chunk=7)
        self.assertEqual(ibl.rgba16_bytes(a), ibl.rgba16_bytes(b))

    def test_ggx_and_brdf_are_finite_over_grazing_and_roughness_extremes(self):
        xi = ibl.hammersley(1024)
        for roughness in (0., .01, .1, .5, 1.):
            half = ibl.importance_ggx(xi, roughness)
            np.testing.assert_allclose(np.linalg.norm(half, axis=1), 1., atol=1e-14)
            value = ibl.brdf_values([1/512, .1, .5, 1.], roughness, xi)
            self.assertTrue(np.isfinite(value).all())
            self.assertTrue((value >= 0).all())
            self.assertLess(value.max(), 1.01)
        value = ibl.brdf_values([1.], 0, xi)[0]
        self.assertGreater(value[0], .999)
        self.assertLess(value[1], 1e-6)

    def test_small_runtime_brdf_fixture_matches_reviewed_cook(self):
        root = Path(__file__).resolve().parents[2]
        checked_in = root / "examples/renderer-scenes/src/shared/ibl-fixture-brdf.rgba16f"
        generated = ibl.rgba16_bytes([ibl.brdf_lut(16, 16, samples=1024)])
        self.assertEqual(checked_in.read_bytes(), generated)

    def test_roughness_one_keeps_directional_light_after_runtime_sampling(self):
        normals = np.array([[1,0,0],[-1,0,0],[0,1,0],[0,-1,0],[0,0,1],[0,0,-1]],dtype=np.float64)
        source = .6 + ibl.directions(256,128)*np.array([.3,.2,.1])
        xi = ibl.hammersley(1024)
        actual = np.array([runtime_specular(source,normal,1.,xi,max_lod=4) for normal in normals])
        # At alpha=1 the normalized prefilter is a cosine integral of this
        # linear spherical function, whose exact mean direction is 2*N/3.
        reference = .6 + (2/3)*normals*np.array([.3,.2,.1])
        np.testing.assert_allclose(actual,reference,rtol=0,atol=1/64)
        self.assertGreater(actual[0,0]-actual[1,0],.39)
        legacy = np.array([runtime_specular(source,normal,1.,xi,max_lod=10) for normal in normals])
        self.assertGreater(np.max(np.abs(legacy-reference)),.39)

    def test_rgbe_writer_and_godot_quarter_turn_keep_source_linear(self):
        source = np.maximum(ibl.directions(32, 16), 0.) * 3.
        rotated = np.roll(source, -source.shape[1] // 4, axis=1)
        decoded = decode_rgbe(ibl.radiance_bytes(rotated))
        bound = np.max(rotated, axis=-1)[:, :, None] / 128 + 1e-20
        self.assertTrue((np.abs(decoded - rotated) <= bound).all())
        self.assertEqual(decoded.shape, (16, 32, 3))
        for x in (0, 7, 8, 31):
            np.testing.assert_array_equal(rotated[:, x], source[:, (x + 8) % 32])

    def test_record_hash_size_alpha_and_little_endian(self):
        levels = [np.ones((2, 4, 3)), np.zeros((1, 2, 3)), np.full((1, 1, 3), .5)]
        with tempfile.TemporaryDirectory() as temporary:
            record = ibl.image_record(Path(temporary), "probe", levels)
            raw = (Path(temporary) / record["path"]).read_bytes()
            self.assertEqual(record["mip_levels"], 3)
            self.assertEqual(record["byte_length"], 88)
            self.assertEqual(record["sha256"], hashlib.sha256(raw).hexdigest())
            self.assertEqual(raw[:8], b"\x00\x3c" * 4)
            self.assertTrue((np.frombuffer(raw, "<f2").reshape(-1, 4)[:, 3] == 1).all())

    def test_invalid_radiance_and_half_overflow_are_rejected(self):
        for value in (-1., float("inf"), float("nan")):
            with self.assertRaises(ValueError):
                ibl.capped_source([[[value, 0., 0.]]])
        with self.assertRaises(ValueError):
            ibl.rgba16_bytes([np.full((1, 1, 3), 65505.)])


if __name__ == "__main__":
    unittest.main()
