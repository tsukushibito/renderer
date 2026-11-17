import unittest

import numpy as np

from repeat import compare_pixels


class RepeatToleranceTests(unittest.TestCase):
    def test_allows_only_small_one_code_variation_without_masking_alpha(self):
        first = np.full((2, 10, 4), 128, dtype=np.uint8)
        first[:, :, 3] = 255
        second = first.copy()
        second.reshape(-1, 4)[:16, 1] += 1
        result = compare_pixels(first, second)
        self.assertTrue(result["passed"])
        self.assertEqual(result["changed_pixels"], 16)
        second.reshape(-1, 4)[16, 1] += 1
        self.assertFalse(compare_pixels(first, second)["passed"])
        second = first.copy()
        second[0, 0, 2] += 2
        self.assertFalse(compare_pixels(first, second)["passed"])
        second = first.copy()
        second[0, 0, 3] -= 1
        self.assertFalse(compare_pixels(first, second)["passed"])

    def test_identical_images_and_invalid_shapes_are_distinct(self):
        image = np.zeros((2, 3, 4), dtype=np.uint8)
        self.assertEqual(compare_pixels(image, image)["changed_pixels"], 0)
        for invalid in (image[:, :, :3], image[:1], image.astype(np.float32)):
            with self.assertRaises(ValueError):
                compare_pixels(image, invalid)


if __name__ == "__main__":
    unittest.main()
