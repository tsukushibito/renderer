"""Small CPU checks for the preview image's independent display transform."""
import tempfile
from pathlib import Path
import unittest

import numpy as np
from PIL import Image

from render_conditions import gltf_to_blender, neutral_tone_map, project_camera_points, srgb_encode, write_png


class DisplayTransformTests(unittest.TestCase):
    def test_neutral_gray_and_compression_boundary(self):
        gray = np.array([[0, 0, 0], [0.04] * 3, [0.18] * 3, [0.5] * 3, [0.8] * 3, [2] * 3, [1e6] * 3])
        output = neutral_tone_map(gray)
        np.testing.assert_allclose(output[:6], [[0] * 3, [0.01] * 3, [0.14] * 3, [0.46] * 3,
                                               [0.76] * 3, [0.96] * 3], atol=1e-15)
        self.assertTrue(np.all(output[-1] < 1))
        self.assertTrue(np.all(output[-1] > 0.999999))

    def test_bright_primary_desaturates_without_losing_channel_order(self):
        output = neutral_tone_map(np.array([[100, 0, 0], [0, 100, 0], [0, 0, 100]]))
        self.assertTrue(np.all(output >= 0))
        self.assertTrue(np.all(output < 1))
        self.assertTrue(np.all(output.min(axis=1) > 0))
        np.testing.assert_array_equal(output.argmax(axis=1), [0, 1, 2])

    def test_reject_negative_and_nonfinite_input(self):
        for bad in ([-1, 0, 0], [float("nan"), 1, 0], [0, 0, float("inf")]):
            with self.assertRaises(ValueError):
                neutral_tone_map(bad)

    def test_srgb_breakpoint_and_single_encoding(self):
        output = srgb_encode(np.array([0, 0.0031308, 0.18, 1]))
        np.testing.assert_allclose(output, [0, 0.040449936, 0.46135612950044164, 1], atol=1e-14)

    def test_png_preserves_rows_rgb_and_srgb_tag(self):
        pixels = np.array([[[255, 0, 0], [0, 255, 0]], [[0, 0, 255], [255, 255, 255]]], dtype=np.uint8)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "display.png"
            write_png(path, pixels)
            image = Image.open(path)
            self.assertEqual(image.info["srgb"], 0)
            np.testing.assert_array_equal(np.asarray(image), pixels)

    def test_coordinate_basis_preserves_handedness_and_up(self):
        self.assertEqual(gltf_to_blender((1, 2, 3)), (1, -3, 2))
        x, y, z = (np.array(gltf_to_blender(axis)) for axis in ((1, 0, 0), (0, 1, 0), (0, 0, 1)))
        np.testing.assert_array_equal(np.cross(x, y), z)

    def test_projection_uses_vertical_fov_aspect_and_negative_z_forward(self):
        points = [[0, 0, -2], [1.9, 0.9, -1], [-1.9, -0.9, -1]]
        ndc, depth, outside = project_camera_points(points, 90, 2, 0.02, 500)
        np.testing.assert_allclose(ndc, [[0, 0], [0.95, 0.9], [-0.95, -0.9]], atol=1e-14)
        np.testing.assert_array_equal(depth, [2, 1, 1])
        self.assertFalse(outside.any())

    def test_projection_detects_bottom_crop_clip_and_behind_camera(self):
        points = [[0, -1.1, -1], [0, 0, -0.01], [0, 0, -501], [0, 0, 1], [0, 0, 0]]
        _ndc, _depth, outside = project_camera_points(points, 90, 2, 0.02, 500)
        self.assertTrue(outside.all())


if __name__ == "__main__":
    unittest.main()
