"""Consumer cook regressions: tangent seams, world basis, source mips and opacity."""

import hashlib
import json
from pathlib import Path
import struct
import tempfile
import unittest

import numpy as np
from PIL import Image

import texture_codec as codec
from cook import cook, cook_materials, cook_primitive, dds_levels, relative_file
from mikk import Generator, build


class TextureCodecTests(unittest.TestCase):
    def test_format_precedence_and_semantics(self):
        policy = {"default": "rgba8", "roles": {"normal": "bc5"}, "images": {"detail.png": "bc7"}}
        codec.validate_policy(policy, ["detail.png"])
        self.assertEqual(codec.choose_format(policy, "detail.png", "normal", None), "bc7")
        self.assertEqual(codec.choose_format(policy, "other.png", "normal", None), "bc5")
        self.assertEqual(codec.choose_format(policy, "other.png", "orm", None), "rgba8")
        self.assertEqual(codec.choose_format({}, "color.png", "base_color", "bc3"), "bc3")
        with self.assertRaises(ValueError):
            codec.choose_format({"default": "bc5"}, "color.png", "orm", None)
        with self.assertRaises(ValueError):
            codec.validate_policy(policy, [])

    def test_reflection_policy_and_hdr_encoding(self):
        from reflection_codec import encode
        for fmt in ("bc6h", "rgba32f"):
            codec.validate_policy({"reflection_probes": fmt}, [])
        with self.assertRaises(ValueError):
            codec.validate_policy({"reflection_probes": "bc7"}, [])
        pixels = np.full((8, 4, 4), 8.0, dtype="<f4")
        pixels[:4, :, 0] = 0.125
        data = encode(pixels.tobytes(), 4, 8)
        self.assertEqual(len(data), 32)
        self.assertNotEqual(data[:16], data[16:])
        pixels[0, 0, 0] = 70000
        with self.assertRaises(ValueError):
            encode(pixels.tobytes(), 4, 8)

    def test_partial_edge_blocks_and_channel_order(self):
        pixels = np.full((5, 9, 4), 255, dtype=np.uint8)
        y, x = np.indices(pixels.shape[:2])
        pixels[:, :, 0] = (x // 4) * 80
        pixels[:, :, 1] = (y // 4) * 180
        pixels[:, :, 2] = 100
        for fmt in ("bc1", "bc3", "bc5", "bc7"):
            with self.subTest(fmt=fmt):
                block = codec.encode(pixels, fmt)
                self.assertEqual(len(block), 6 * (8 if fmt == "bc1" else 16))
                actual = codec.decode(block, fmt, 9, 5)
                channels = 2 if fmt == "bc5" else 3
                error = np.abs(actual[:, :, :channels].astype(int) - pixels[:, :, :channels])
                self.assertLessEqual(error.max(), 8)
        pixels[:, :, 3] = (x // 4) * 90
        for fmt in ("bc3", "bc7"):
            decoded = codec.decode(codec.encode(pixels, fmt), fmt, 9, 5)
            self.assertLessEqual(np.abs(decoded[:, :, 3].astype(int) - pixels[:, :, 3]).max(), 1)
        with self.assertRaises(ValueError):
            codec.encode(pixels, "bc1")


class Geometry:
    def __init__(self, positions, normals, uv, indices):
        self.values = [np.asarray(positions, dtype=np.float32), np.asarray(normals, dtype=np.float32),
                       np.asarray(uv, dtype=np.float32), np.asarray(indices, dtype=np.uint32).reshape(-1, 1)]
        self.data = {"accessors": [{}, {}, {}, {"type": "SCALAR", "componentType": 5125}]}

    def accessor(self, index):
        return self.values[index].copy()


PRIMITIVE = {"attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2}, "indices": 3}


def triangle():
    return Geometry([[0, 0, 0], [1, 0, 0], [0, 1, 0]], [[0, 0, 2]] * 3,
                    [[0, 0], [1, 0], [0, 1]], [0, 1, 2])


def dds_dxt1(width, height, blocks):
    header = bytearray(128)
    header[:4] = b"DDS "
    struct.pack_into("<I", header, 4, 124)
    struct.pack_into("<I", header, 8, 0xA1007)
    struct.pack_into("<II", header, 12, height, width)
    struct.pack_into("<I", header, 28, len(blocks))
    struct.pack_into("<II", header, 76, 32, 4)
    header[84:88] = b"DXT1"
    struct.pack_into("<I", header, 108, 0x401008)
    return bytes(header) + b"".join(blocks)


class CookTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory()
        cls.library = build(Path(cls.temporary.name) / "mikk")
        cls.generator = Generator(cls.library)

    @classmethod
    def tearDownClass(cls):
        cls.generator.close()
        cls.temporary.cleanup()

    def result(self, geometry, transform=None):
        vertices, indices, record, _ = cook_primitive(geometry, PRIMITIVE,
                                                  np.eye(4) if transform is None else transform,
                                                  self.generator)
        return np.frombuffer(vertices, "<f4").reshape(-1, 12), np.frombuffer(indices, "<u4"), record

    def test_authored_lightmap_is_independent_of_material_uv(self):
        from lightmap_uv import parameterize
        geo = triangle()
        geo.values.append(geo.values[2].copy())
        geo.values[2][:] = 3.5
        prim = dict(PRIMITIVE, attributes=dict(PRIMITIVE["attributes"], TEXCOORD_1=4))
        uv, extent, source = parameterize(geo, prim, np.eye(4))
        self.assertEqual((source, extent), ("authored", [4, 4]))
        vertices, indices, _, secondary = cook_primitive(geo, prim, np.eye(4), self.generator, uv)
        np.testing.assert_array_equal(np.frombuffer(vertices, "<f4").reshape(-1, 12)[:, 10:], 3.5)
        np.testing.assert_array_equal(np.frombuffer(secondary, "<f4").reshape(-1, 2)[np.frombuffer(indices, "<u4")], uv[0])

    def test_overlapping_secondary_uv_is_unwrapped(self):
        from lightmap_uv import parameterize
        geo = Geometry([[0,0,0], [1,0,0], [0,1,0], [2,0,0], [3,0,0], [2,1,0]],
                       [[0,0,1]] * 6, [[0,0], [1,0], [0,1]] * 2, list(range(6)))
        prim = dict(PRIMITIVE, attributes=dict(PRIMITIVE["attributes"], TEXCOORD_1=2))
        uv, _, source = parameterize(geo, prim, np.eye(4))
        self.assertEqual(source, "xatlas")
        self.assertFalse(np.array_equal(uv[0], uv[1]))

    def test_mirrored_uv_splits_source_shared_corner(self):
        geometry = Geometry([[0, 0, 0], [1, 0, 0], [0, 1, 0], [-1, 0, 0]],
                            [[0, 0, 1]] * 4, [[0, 0], [1, 0], [0, 1], [1, 0]], [0, 1, 2, 0, 2, 3])
        vertices, indices, _ = self.result(geometry)
        center = vertices[np.all(vertices[:, :3] == 0, axis=1)]
        self.assertEqual(len(center), 2)
        self.assertEqual(set(center[:, 9]), {-1, 1})
        self.assertEqual(len(indices), 6)

    def test_nonuniform_reflection_preserves_world_normal_winding_and_sign(self):
        transform = np.diag([-2, 3, 4, 1])
        transform[:3, 3] = [5, 6, 7]
        vertices, indices, record = self.result(triangle(), transform)
        corners = vertices[indices, :3]
        self.assertGreater(np.cross(corners[1] - corners[0], corners[2] - corners[0])[2], 0)
        np.testing.assert_allclose(vertices[:, 3:6], [[0, 0, 1]] * 3)
        np.testing.assert_allclose(vertices[:, 6:9], [[-1, 0, 0]] * 3)
        np.testing.assert_allclose(vertices[:, 9], -1)
        self.assertTrue(record["reflected_transform"])

    def test_degenerate_uv_keeps_triangle_and_builds_non_z_normal_fallback(self):
        geometry = Geometry([[0, 0, 0], [0, 1, 0], [0, 0, 1]], [[1, 0, 0]] * 3,
                            [[0, 0]] * 3, [0, 1, 2])
        vertices, indices, record = self.result(geometry)
        self.assertEqual(len(indices), 3)
        self.assertEqual(record["degenerate_uv_faces"], 1)
        self.assertEqual(record["tangent_fallback_corners"], 3)
        np.testing.assert_allclose(np.linalg.norm(vertices[:, 6:9], axis=1), 1)
        np.testing.assert_allclose(np.sum(vertices[:, 3:6] * vertices[:, 6:9], axis=1), 0)

    def test_invalid_authored_normal_and_singular_transform_fail(self):
        bad = triangle()
        bad.values[1][0] = 0
        with self.assertRaisesRegex(ValueError, "authored normal"):
            self.result(bad)
        with self.assertRaisesRegex(ValueError, "Singular"):
            self.result(triangle(), np.diag([1, 0, 1, 1]))

    def test_real_bistro_projected_angle_collapse_keeps_handedness(self):
        # Reduced source mesh1/primitive1/face1634. Its authored corner normal
        # nearly lies in the face plane; Mikk's float32 projected-edge cosine
        # rounds to >=1, giving zero angular weight and hence a zero tangent.
        corners = np.array([
            [-597.8322143554688, 271.4437255859375, -1265.41845703125,
             -0.5104933381080627, 0.10521619021892548, -0.8534202575683594, 1.6943359375, -1.0556640625],
            [-598.0751953125, 288.71331787109375, -1262.376708984375,
             0.8552923798561096, 0.043355900794267654, 0.5163285732269287, 1.9521484375, -1.572265625],
            [-596.8197021484375, 271.5013122558594, -1264.8048095703125,
             0.9796546697616577, 0.20065216720104218, -0.003934299573302269, 1.361328125, -1.0556640625],
        ], dtype=np.float32)
        np.testing.assert_array_equal(self.generator(corners)[1], [0, 0, 0, -1])
        geometry = Geometry(corners[:, :3], corners[:, 3:6], corners[:, 6:8], [0, 1, 2])
        vertices, _, record = self.result(geometry)
        self.assertEqual(record["degenerate_uv_faces"], 0)
        self.assertEqual(record["degenerate_geometry_faces"], 0)
        self.assertGreater(record["collapsed_mikk_basis_corners"], 0)
        np.testing.assert_allclose(np.linalg.norm(vertices[:, 6:9], axis=1), 1)
        np.testing.assert_allclose(np.sum(vertices[:, 3:6] * vertices[:, 6:9], axis=1), 0, atol=1e-7)
        np.testing.assert_array_equal(vertices[:, 9], -1)

    def test_actual_dds_mips_are_independently_decoded_without_resampling(self):
        # 4x4 red, 2x2 green, 1x1 blue: generated downsampling would stay red.
        blocks = [struct.pack("<HHI", color, 0, 0) for color in (0xF800, 0x07E0, 0x001F)]
        raw = dds_dxt1(4, 4, blocks)
        metadata, levels = dds_levels(raw, "base_color")
        pixels = list(levels)
        self.assertEqual(metadata["mip_levels"], 3)
        self.assertEqual([item.shape[:2] for item in pixels], [(4, 4), (2, 2), (1, 1)])
        self.assertEqual([item[0, 0, :3].tolist() for item in pixels],
                         [[255, 0, 0], [0, 255, 0], [0, 0, 255]])
        with self.assertRaisesRegex(ValueError, "payload length"):
            dds_levels(raw[:-1], "base_color")

    def test_alpha_zero_and_missing_orm_scalar_are_not_repaired(self):
        data = {"materials": [{"name": "labels", "alphaMode": "BLEND", "doubleSided": True,
                               "pbrMetallicRoughness": {"baseColorFactor": [1, 1, 1, 0],
                                                       "metallicFactor": 0, "roughnessFactor": 0.5}}]}
        materials, roles = cook_materials(data)
        self.assertEqual(materials[0]["base_factor"][3], 0)
        self.assertEqual(materials[0]["alpha_mode"], "BLEND")
        self.assertEqual(materials[0]["roughness"], 0.5)
        self.assertIsNone(materials[0]["orm_texture"])
        self.assertEqual(roles, {})

    def test_source_uri_cannot_escape_asset_root(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "escapes"):
                relative_file(Path(directory), "%2e%2e/private.bin")

    def test_small_cook_rebuild_has_identical_manifest_and_binary_hashes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            geometry = triangle()
            chunks = [value.astype("<u4" if index == 3 else "<f4").tobytes()
                      for index, value in enumerate(geometry.values)]
            (root / "input.bin").write_bytes(b"".join(chunks))
            views, accessors, offset = [], [], 0
            for index, chunk in enumerate(chunks):
                views.append({"buffer": 0, "byteOffset": offset, "byteLength": len(chunk)})
                accessors.append({"bufferView": index, "componentType": 5125 if index == 3 else 5126,
                                  "type": ["VEC3", "VEC3", "VEC2", "SCALAR"][index], "count": 3})
                offset += len(chunk)
            material = {"name": "fixture", "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}
            Image.new("RGBA", (1, 1), (255, 128, 64, 0)).save(root / "color.png")
            data = {"asset": {"version": "2.0"}, "scene": 0, "scenes": [{"nodes": [0, 1]}],
                    "nodes": [{"name": "triangle", "mesh": 0},
                              {"name": "copy", "mesh": 0, "translation": [1,2,3], "scale": [2,2,2]}],
                    "meshes": [{"primitives": [{**PRIMITIVE, "material": 0}]}], "materials": [material],
                    "textures": [{"source": 0}], "images": [{"uri": "color.png"}],
                    "buffers": [{"uri": "input.bin", "byteLength": offset}],
                    "bufferViews": views, "accessors": accessors}
            (root / "input.gltf").write_text(json.dumps(data), encoding="utf-8")
            manifests = [cook(root / "input.gltf", root / name, self.library) for name in ("a", "b")]
            self.assertEqual(manifests[0], manifests[1])
            self.assertEqual(manifests[0]["summary"]["triangles"], 2)
            first, second = [p["lightmap"] for p in manifests[0]["primitives"]]
            self.assertEqual(first["uv_offset"], second["uv_offset"])
            self.assertEqual(second["extent"], [2*n for n in first["extent"]])
            self.assertEqual(manifests[0]["images"][0]["mips"][0]["alpha_max"], 0)
            for relative in ("manifest.json", "vertices.bin", "indices.bin", "lightmap_uv.bin", "images/0000.bin"):
                self.assertEqual(hashlib.sha256((root / "a" / relative).read_bytes()).digest(),
                                 hashlib.sha256((root / "b" / relative).read_bytes()).digest())


if __name__ == "__main__":
    unittest.main()
