import unittest

import numpy as np

from compare_alpha import compare


class AlphaComparisonTests(unittest.TestCase):
    def test_change_elsewhere_cannot_substitute_for_target_visibility(self):
        source = np.zeros((2, 2, 3), dtype=np.uint8)
        opaque = source.copy()
        opaque[0, 0] = 255
        with self.assertRaisesRegex(ValueError, "zero visible coverage"):
            compare(source, opaque, np.zeros((2, 2), dtype=np.uint8))
        mask = np.zeros((2, 2), dtype=np.uint8)
        mask[1, 1] = 255
        with self.assertRaisesRegex(ValueError, "no measured difference on target"):
            compare(source, opaque, mask)

    def test_full_range_difference_does_not_wrap_uint8(self):
        source = np.zeros((1, 1, 3), dtype=np.uint8)
        opaque = np.full_like(source, 255)
        difference, metrics = compare(source, opaque, np.full((1, 1), 255, dtype=np.uint8))
        self.assertEqual(difference.tolist(), [[[255, 255, 255]]])
        self.assertEqual(metrics["changed_pixels_on_coverage"], 1)
        self.assertEqual(metrics["mean_absolute_rgb_8bit"], 255)


if __name__ == "__main__":
    unittest.main()
