import struct
import unittest

import numpy as np

from inspect_alpha import alpha_stats, decode_bc3_alpha, source_usage


def block(first, second, indices):
    packed = sum(value << (3*i) for i, value in enumerate(indices))
    return bytes((first, second)) + packed.to_bytes(6, "little") + b"\0" * 8


def dds(width, height, levels):
    header = bytearray(128)
    header[:4] = b"DDS "
    for offset, value in ((4, 124), (8, 0xA1007), (12, height), (16, width),
                          (20, ((width+3)//4)*((height+3)//4)*16), (28, len(levels)),
                          (76, 32), (80, 4), (108, 0x401008)):
        struct.pack_into("<I", header, offset, value)
    header[84:88] = b"DXT5"
    return bytes(header) + b"".join(levels)


class AlphaEvidenceTests(unittest.TestCase):
    def test_bc3_eight_alpha_order_and_little_endian_three_bit_indices(self):
        image = decode_bc3_alpha(dds(4, 4, [block(252, 0, list(range(8))*2)]))[0]
        expected = [252, 0, 216, 180, 144, 108, 72, 36]*2
        self.assertEqual(image.ravel().tolist(), expected)
        self.assertEqual(alpha_stats(image)["zero"], 2)

    def test_bc3_six_alpha_branch_exposes_zero_and_opaque_special_indices(self):
        image = decode_bc3_alpha(dds(4, 4, [block(0, 250, list(range(8))*2)]))[0]
        self.assertEqual(image.ravel().tolist(), [0, 250, 50, 100, 150, 200, 0, 255]*2)
        equal = decode_bc3_alpha(dds(4, 4, [block(17, 17, [7, 6]*8)]))[0]
        self.assertEqual(equal.ravel().tolist(), [255, 0]*8)

    def test_mip_edges_exclude_padding_and_blocks_follow_rows(self):
        zero = block(0, 0, [0]*16)
        # A 5x3 image has two horizontal blocks, not a 4x6 vertical arrangement.
        left = block(252, 0, [0]*16)
        right = block(0, 250, [7, 0, 0, 0]*4)
        # Only two of these 16 decoded samples are real texels in the 2x1 mip.
        tiny = block(0, 250, [1, 6] + [7]*14)
        levels = decode_bc3_alpha(dds(5, 3, [left+right, tiny, zero]))
        np.testing.assert_array_equal(levels[0], [[252]*4+[255]]*3)
        np.testing.assert_array_equal(levels[1], [[250, 0]])
        self.assertEqual(levels[2].shape, (1, 1))
        self.assertEqual(sum(alpha_stats(p)["opaque"] for p in levels), 3)

    def test_unexpected_headers_dimensions_mips_and_data_fail(self):
        original = dds(4, 4, [block(0, 0, [0]*16)])
        variants = [original[:127], original[:-1], original+b"x", b"BAD!"+original[4:]]
        for offset, value in ((4, 123), (8, 0x81003), (12, 0), (16, 16385), (20, 32),
                              (24, 1), (28, 4), (76, 31), (80, 0), (108, 0x200000), (112, 0x200)):
            bad = bytearray(original)
            struct.pack_into("<I", bad, offset, value)
            variants.append(bytes(bad))
        variants.append(original[:84]+b"DX10"+original[88:])
        for raw in variants:
            with self.subTest(header=raw[:128]), self.assertRaises(ValueError):
                decode_bc3_alpha(raw)

    def test_source_all_same_counts_all_triangles_instead_of_one_binding(self):
        audit = {"models": [{"id": "model", "name": "one", "material_ids": ["target"], "geometry_ids": ["g"]}],
                 "geometries": [{"id": "g", "polygons": 4, "polygon_sizes": {"3": 4},
                                  "material_layers": [{"fields": {"MappingInformationType": ["AllSame"],
                                                                        "ReferenceInformationType": ["IndexToDirect"]},
                                                       "material_index_counts": {"0": 1}}]}]}
        self.assertEqual(source_usage(audit, "target")[0]["triangles"], 4)
        audit["geometries"][0]["polygon_sizes"] = {"4": 4}
        with self.assertRaisesRegex(ValueError, "only source triangles"):
            source_usage(audit, "target")


if __name__ == "__main__":
    unittest.main()
