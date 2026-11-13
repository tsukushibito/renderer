"""Contract tests for original-source to imported-baseline comparisons."""

from collections import namedtuple
from pathlib import Path
import tempfile
import unittest

import numpy as np

from compare_source_bindings import (
    compare_geometry_arrays, expand_material_slots, load_name_rule, unique_names,
)


Element = namedtuple("Element", "id props elems")


def field(name, value):
    return Element(name, [value], [])


def fixture(mapping=b"AllSame", material_slots=(0,)):
    vertices = np.array([[0, 0, 0], [1, 0, 0], [0, 1, 0]], dtype=np.float32)
    normals = np.array([[0, 0, 1]] * 6, dtype=np.float32)
    uv = np.array([[0, 0], [1, 0], [0, 1]], dtype=np.float32)
    geometry = Element(b"Geometry", [10, b"\0\1Geometry", b"Mesh"], [
        field(b"Vertices", vertices.ravel()),
        field(b"PolygonVertexIndex", np.array([0, 1, -3, 0, 1, -3])),
        Element(b"LayerElementMaterial", [0], [
            field(b"MappingInformationType", mapping), field(b"Materials", material_slots),
        ]),
        Element(b"LayerElementNormal", [0], [
            field(b"MappingInformationType", b"ByPolygonVertex"),
            field(b"ReferenceInformationType", b"Direct"), field(b"Normals", normals.ravel()),
        ]),
        Element(b"LayerElementUV", [0], [
            field(b"MappingInformationType", b"ByPolygonVertex"),
            field(b"ReferenceInformationType", b"IndexToDirect"), field(b"Name", b"TextureUV"),
            field(b"UV", uv.ravel()), field(b"UVIndex", [0, 1, 2, 0, 1, 2]),
        ]),
    ])
    saved = {
        "positions": np.concatenate((vertices, vertices)),
        "triangles": np.array([[0, 1, 2], [3, 4, 5]]),
        "triangle_loops": np.arange(6).reshape(-1, 3),
        "material_indices": np.array([0, 0] if mapping == b"AllSame" else material_slots),
        "source_loop_normals": normals.copy(),
        "loop_uv": np.concatenate((uv, uv)),
    }
    return geometry, saved


class SourceBindingTests(unittest.TestCase):
    def test_all_same_expands_to_every_source_polygon(self):
        np.testing.assert_array_equal(expand_material_slots(b"AllSame", [2], 8, 3), [2] * 8)

    def test_invalid_source_material_slot_is_rejected(self):
        for slots in ([-1], [2]):
            with self.subTest(slots=slots), self.assertRaisesRegex(ValueError, "exceeds bindings"):
                expand_material_slots(b"AllSame", slots, 3, 2)

    def test_separated_duplicate_retains_source_corners(self):
        geometry, saved = fixture()
        report = compare_geometry_arrays("source", geometry, saved, 1)
        self.assertEqual(report["triangles"], 2)
        self.assertEqual(report["duplicate_faces"], 1)
        self.assertEqual(report["material_face_counts"], {"0": 2})
        self.assertEqual(report["baseline_vertices"], 6)

    def test_swapped_per_face_materials_are_detected_despite_equal_counts(self):
        geometry, saved = fixture(b"ByPolygon", (0, 1))
        saved["material_indices"] = np.array([1, 0])
        with self.assertRaisesRegex(ValueError, "ordered polygon material indices"):
            compare_geometry_arrays("source", geometry, saved, 2)

    def test_changed_position_normal_or_uv_is_detected(self):
        for field_name in ("positions", "source_loop_normals", "loop_uv"):
            with self.subTest(field=field_name):
                geometry, saved = fixture()
                saved[field_name][0, 0] = 0.25
                with self.assertRaisesRegex(ValueError, "source values differ"):
                    compare_geometry_arrays("source", geometry, saved, 1)

    def test_reordered_triangle_corners_are_detected(self):
        geometry, saved = fixture()
        saved["triangles"][1] = [3, 5, 4]
        with self.assertRaisesRegex(ValueError, "ordered triangle corner positions"):
            compare_geometry_arrays("source", geometry, saved, 1)

    def test_name_collision_is_rejected(self):
        elements = [Element(b"Model", [i, name, b"Mesh"], [])
                    for i, name in enumerate((b"one\0\1Model", b"two\0\1Model"))]
        with self.assertRaisesRegex(ValueError, "name collision"):
            unique_names(elements, lambda name: "same")

    def test_loading_name_function_does_not_import_bpy_module(self):
        with tempfile.TemporaryDirectory() as directory:
            importer = Path(directory) / "import_fbx.py"
            importer.write_text("import nonexistent_bpy\n\ndef validate_blend_names(name):\n    return name.decode('utf-8')\n",
                                encoding="utf-8")
            rule, digest = load_name_rule(importer)
            self.assertEqual(rule(b"mesh"), "mesh")
            self.assertEqual(len(digest), 64)


if __name__ == "__main__":
    unittest.main()
