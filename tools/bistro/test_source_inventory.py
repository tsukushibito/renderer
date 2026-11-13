"""Focused contract tests for original FBX polygon accounting."""

import unittest
from collections import namedtuple

from source_inventory import classify_duplicate_surfaces, polygon_audit


Element = namedtuple("Element", "id props elems")


def field(name, value):
    return Element(name, [value], [])


class PolygonAuditTests(unittest.TestCase):
    def setUp(self):
        self.vertices = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]

    def test_opposite_faces_are_reported_and_both_counted(self):
        result = polygon_audit([0, 1, -3, 2, 1, -1], self.vertices)
        self.assertEqual(result["fan_triangles"], 2)
        self.assertEqual(result["duplicate_index_set_polygons"], [
            {"polygon": 1, "matches_polygon": 0, "same_winding": False},
        ])
        self.assertEqual(result["exact_zero_area_polygons"], [])

    def test_cyclic_order_has_same_winding(self):
        result = polygon_audit([0, 1, -3, 1, 2, -1], self.vertices)
        self.assertTrue(result["duplicate_index_set_polygons"][0]["same_winding"])

    def test_collinear_faces_are_distinct_from_repeated_positions(self):
        result = polygon_audit([0, 1, -3], [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 2.0, 0.0, 0.0])
        self.assertEqual(result["exact_zero_area_polygons"], [0])
        self.assertEqual(result["repeated_position_polygons"], [])
        self.assertEqual(result["repeated_vertex_index_polygons"], [])

    def test_repeated_positions_and_indices_remain_separate(self):
        vertices = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
        result = polygon_audit([0, 1, -3, 0, 1, -2], vertices)
        self.assertEqual(result["repeated_position_polygons"], [0, 1])
        self.assertEqual(result["repeated_vertex_index_polygons"], [1])

    def test_unterminated_polygon_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "unterminated"):
            polygon_audit([0, 1, 2], self.vertices)

    def test_out_of_range_index_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "exceeds"):
            polygon_audit([0, 1, -4], self.vertices)

    def test_nonfinite_vertex_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "non-finite"):
            polygon_audit([0, 1, -3], self.vertices[:6] + [float("nan"), 1.0, 0.0])

    def test_duplicate_faces_retain_different_corner_shading_and_material(self):
        indices = [0, 1, -3, 0, 1, -3]
        normals = Element(b"LayerElementNormal", [0], [
            field(b"MappingInformationType", b"ByPolygonVertex"),
            field(b"ReferenceInformationType", b"Direct"),
            field(b"Normals", [0.0, 0.0, 1.0] * 3 + [0.0, 0.0, -1.0] * 3),
        ])
        materials = Element(b"LayerElementMaterial", [0], [
            field(b"MappingInformationType", b"ByPolygon"),
            field(b"Materials", [0, 1]),
        ])
        geometry = Element(b"Geometry", [], [field(b"PolygonVertexIndex", indices), normals, materials])
        result = polygon_audit(indices, self.vertices)
        classify_duplicate_surfaces(geometry, result)
        duplicate = result["duplicate_index_set_polygons"][0]
        self.assertEqual(result["fan_triangles"], 2)
        self.assertEqual(duplicate["normal_equal_per_vertex"], [False])
        self.assertEqual(duplicate["material_slots"], [{"first": 0, "duplicate": 1}])


if __name__ == "__main__":
    unittest.main()
