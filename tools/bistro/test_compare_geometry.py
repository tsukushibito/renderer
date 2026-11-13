"""Small conversion fixtures exercise correspondence and deliberate corruption."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

import numpy as np


spec = importlib.util.spec_from_file_location("compare_geometry", Path(__file__).with_name("compare_geometry.py"))
geometry = importlib.util.module_from_spec(spec)
spec.loader.exec_module(geometry)


class GeometryComparisonTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.source = self.directory / "source.json"
        self.target = self.directory / "scene.gltf"
        self.positions = np.array([[0, 0, 0], [2, 0, 0], [0, 0, 3], [2, 1, 3]], dtype=np.float32)
        self.triangles = np.array([[0, 1, 2], [1, 3, 2]], dtype=np.int32)
        self.loops = np.arange(6, dtype=np.int32).reshape(2, 3)
        self.normals = np.tile(np.array([0, 1, 0], dtype=np.float32), (6, 1))
        self.uv = np.array([[0, 0], [1, 0], [0, 1], [1, 0], [1, 1], [0, 1]], dtype=np.float32)
        self.uv1 = self.uv * 0.25 + 0.125
        self.matrix = np.array([[0, -2, 0, 5], [1, 0, 0, 6], [0, 0, 3, 7], [0, 0, 0, 1]], dtype=np.float64)
        self.write_source()

    def write_source(self, duplicate=False):
        triangles = self.triangles
        loops = self.loops
        indices = np.array([0, 1], dtype=np.int32)
        if duplicate:
            triangles = np.concatenate((triangles, triangles[:1]))
            loops = np.concatenate((loops, loops[:1]))
            indices = np.array([0, 1, 0], dtype=np.int32)
        np.savez(self.directory / "source.npz", positions=self.positions, triangles=triangles,
                 triangle_loops=loops, material_indices=indices, loop_normals=self.normals,
                 loop_uv=self.uv, loop_uv_1=self.uv1)
        self.source.write_text(json.dumps({"objects": [{"name": "Building", "matrix_world": self.matrix.tolist(),
                                                       "materials": ["Brick", "Glass"], "geometry": "source.npz",
                                                       "uv_layers": ["TextureUV", "LightMapUV"]}]}),
                               encoding="utf-8")

    def export(self, *, reordered=False, corrupt=None, interleaved=False):
        data = {"asset": {"version": "2.0"}, "buffers": [], "bufferViews": [], "accessors": [],
                "materials": [{"name": "Brick"}, {"name": "Glass"}], "meshes": [{"primitives": []}],
                "nodes": [{"name": "Building", "mesh": 0,
                           "matrix": (geometry.AXIS @ self.matrix @ geometry.AXIS.T).T.ravel().tolist()}],
                "scenes": [{"nodes": [0]}], "scene": 0}
        binary = bytearray()

        def accessor(values, kind, component=5126):
            values = np.asarray(values, dtype=geometry.COMPONENTS[component])
            while len(binary) % 4:
                binary.append(0)
            offset = len(binary)
            view = {"buffer": 0, "byteOffset": offset, "byteLength": values.nbytes}
            if interleaved and kind == "VEC3":
                padded = np.zeros((len(values), 4), dtype=np.float32)
                padded[:, :3] = values
                binary.extend(padded.tobytes())
                view.update(byteStride=16, byteLength=padded.nbytes)
            else:
                binary.extend(values.tobytes())
            data["bufferViews"].append(view)
            data["accessors"].append({"bufferView": len(data["bufferViews"]) - 1, "componentType": component,
                                      "count": len(values), "type": kind})
            return len(data["accessors"]) - 1

        for face in ([1, 0] if reordered else [0, 1]):
            positions = self.positions[self.triangles[face]] @ geometry.AXIS[:3, :3].T
            normals = self.normals[self.loops[face]] @ geometry.AXIS[:3, :3].T
            uv = self.uv[self.loops[face]].astype(np.float64)
            uv[:, 1] = 1 - uv[:, 1]
            uv1 = self.uv1[self.loops[face]].astype(np.float64)
            uv1[:, 1] = 1 - uv1[:, 1]
            order = [2, 0, 1] if reordered else [0, 1, 2]
            if corrupt == "position" and face == 0:
                positions[0, 0] += 0.25
            if corrupt == "normal" and face == 0:
                normals[0] *= -1
            if corrupt == "normal_rounding" and face == 0:
                normals[0, 0] += 1e-7
            if corrupt == "uv" and face == 0:
                uv[0, 0] += 0.25
            if corrupt == "uv1" and face == 0:
                uv1[0, 0] += 0.25
            attributes = {"POSITION": accessor(positions[order], "VEC3"),
                          "NORMAL": accessor(normals[order], "VEC3"),
                          "TEXCOORD_0": accessor(uv[order], "VEC2"),
                          "TEXCOORD_1": accessor(uv1[order], "VEC2")}
            if corrupt == "missing_uv" and face == 0:
                del attributes["TEXCOORD_0"]
            indices = [0, 2, 1] if corrupt == "winding" and face == 0 else [0, 1, 2]
            data["meshes"][0]["primitives"].append({"attributes": attributes,
                                                     "indices": accessor(indices, "SCALAR", 5123),
                                                     "material": 1 - face if corrupt == "material" else face})
        if corrupt == "missing_object":
            del data["nodes"][0]["mesh"]
        if corrupt == "unexpected_object":
            data["nodes"].append({"name": "Unexpected", "mesh": 0})
            data["scenes"][0]["nodes"].append(1)
        if corrupt == "matrix":
            data["nodes"][0]["matrix"][12] += 1
        data["buffers"] = [{"uri": "scene.bin", "byteLength": len(binary)}]
        (self.directory / "scene.bin").write_bytes(binary)
        self.target.write_text(json.dumps(data), encoding="utf-8")

    def test_reordered_vertices_triangles_and_primitives_preserve_world_geometry(self):
        self.export(reordered=True, interleaved=True)
        report = geometry.compare(self.source, self.target)
        self.assertTrue(report["ok"], report["errors"])
        self.assertEqual(report["source_triangles"], 2)
        self.assertEqual(report["export_triangles"], 2)
        self.assertEqual(report["objects"][0]["max_position_diff"], 0)

    def test_missing_or_unexpected_instance_fails(self):
        for corruption, message in (("missing_object", "missing mesh object"),
                                    ("unexpected_object", "unexpected mesh object")):
            with self.subTest(corruption=corruption):
                self.export(corrupt=corruption)
                report = geometry.compare(self.source, self.target)
                self.assertFalse(report["ok"])
                self.assertTrue(any(message in error for error in report["errors"]))

    def test_geometry_material_uv_normal_and_transform_corruption_fail(self):
        for corruption, message in (("position", "position mismatch"), ("material", "position mismatch"),
                                    ("uv", "uv_0 mismatch"), ("uv1", "uv_1 mismatch"), ("normal", "normal mismatch"),
                                    ("matrix", "world transform mismatch"), ("winding", "position mismatch"),
                                    ("missing_uv", "missing required attributes")):
            with self.subTest(corruption=corruption):
                self.export(corrupt=corruption)
                report = geometry.compare(self.source, self.target)
                self.assertFalse(report["ok"], corruption)
                self.assertTrue(any(message in error for error in report["errors"]), report["errors"])

    def test_duplicate_triangle_multiplicity_is_not_discarded(self):
        self.write_source(duplicate=True)
        self.export()
        report = geometry.compare(self.source, self.target)
        self.assertFalse(report["ok"])
        self.assertTrue(any("triangle count differs (2 vs 1)" in error for error in report["errors"]))

    def test_small_normal_rounding_does_not_reorder_distinct_shared_vertex_faces(self):
        self.triangles = np.array([[0, 1, 2], [0, 2, 3]], dtype=np.int32)
        self.write_source()
        source = json.loads(self.source.read_text(encoding="utf-8"))
        source["objects"][0]["materials"] = ["Brick", "Brick"]
        self.source.write_text(json.dumps(source), encoding="utf-8")
        self.export(corrupt="normal_rounding")
        target = json.loads(self.target.read_text(encoding="utf-8"))
        target["materials"][1]["name"] = "Brick"
        self.target.write_text(json.dumps(target), encoding="utf-8")
        report = geometry.compare(self.source, self.target)
        self.assertTrue(report["ok"], report["errors"])

    def test_trs_matches_column_major_matrix(self):
        node = {"translation": [1, 2, 3], "rotation": [0, 0, np.sqrt(0.5), np.sqrt(0.5)], "scale": [2, 3, 4]}
        expected = np.array([[0, -3, 0, 1], [2, 0, 0, 2], [0, 0, 4, 3], [0, 0, 0, 1]])
        np.testing.assert_allclose(geometry.node_matrix(node), expected, atol=1e-14)
        np.testing.assert_allclose(geometry.node_matrix({"matrix": expected.T.ravel().tolist()}), expected)


if __name__ == "__main__":
    unittest.main()
