import json
import os
from pathlib import Path
import tempfile
import unittest

import numpy as np

import compare_runtime as compare


class RuntimeComparisonTests(unittest.TestCase):
    def test_frozen_fixture_thresholds_apply_to_all_components_and_pixels(self):
        a = np.ones((2, 3, 4), dtype="<f2")
        b = a.copy()
        b[0, 0, 0] = np.float16(1 + 2 / 1024)
        self.assertTrue(compare.compare_pixels(a, b, "hdr")["passed"])
        b[1, 2, 3] = np.float16(1 + 3 / 1024)
        result = compare.compare_pixels(a, b, "hdr")
        self.assertFalse(result["passed"])
        self.assertEqual(result["exceeding_pixels"], 1)
        self.assertEqual(result["pixels"], 6)
        self.assertGreater(result["root_mean_square_component_error"], 0)

    def test_repeat_is_bit_exact_and_rejects_nonfinite_before_equality(self):
        a = np.zeros((1, 1, 4), dtype="<f2")
        b = a.copy()
        b[0, 0, 0] = -0.0
        self.assertFalse(compare.compare_pixels(a, b, "hdr", "repeat")["passed"])
        for value in (np.nan, np.inf, -np.inf):
            b[:] = value
            with self.assertRaisesRegex(ValueError, "NaN or infinity"):
                compare.compare_pixels(b, b, "hdr", "repeat")

    def test_scene_budget_counts_pixels_with_floor_and_roi_has_no_budget(self):
        for pixels, allowed in ((999, 0), (1000, 1)):
            a = np.zeros((1, pixels, 4), dtype="u1")
            b = a.copy()
            b[0, 0] = 255
            result = compare.compare_pixels(a, b, "sdr", "scene")
            self.assertEqual(result["maximum_exceeding_pixels"], allowed)
            self.assertEqual(result["exceeding_pixels"], 1)
            self.assertEqual(result["passed"], bool(allowed))
            self.assertFalse(compare.compare_pixels(a, b, "sdr", "scene", local=True)["passed"])

    def test_sdr_uses_code_values_and_normal_depth_range_is_required(self):
        a = np.zeros((1, 1, 4), dtype="u1")
        b = a.copy()
        b[0, 0, 1] = 1
        self.assertTrue(compare.compare_pixels(a, b, "sdr")["passed"])
        b[0, 0, 1] = 2
        self.assertFalse(compare.compare_pixels(a, b, "sdr")["passed"])
        for value in (-0.1, 1.1):
            depth = np.full((1, 1, 1), value, dtype="<f4")
            with self.assertRaisesRegex(ValueError, "normal-Z"):
                compare.compare_pixels(depth, depth, "depth")

    def make_report(self, directory, *, api="vulkan", pixels=1000):
        directory.mkdir()
        report = {"schema_version": 1, "identity": {"cook": "unchanged", "view": "fixture",
                  "alpha_override": "source", "width": pixels, "height": 1},
                  "runtime": {"api": api}, "captures": {}}
        values = {"hdr": np.zeros((1, pixels, 4), dtype="<f2"),
                  "sdr": np.zeros((1, pixels, 4), dtype="u1"),
                  "depth": np.ones((1, pixels, 1), dtype="<f4")}
        for plane, pixels_array in values.items():
            name = plane + ".bin"
            pixels_array.tofile(directory / name)
            report["captures"][plane] = {"path": name, "format": compare.FORMATS[plane][0],
                                         "width": pixels, "height": 1}
        path = directory / "report.json"
        path.write_text(json.dumps(report), encoding="utf-8")
        return path, report, values

    def test_small_known_wrong_material_cannot_hide_in_global_budget(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            first, _, _ = self.make_report(root / "a")
            second, _, values = self.make_report(root / "b", api="d3d12")
            values["sdr"][0, 7] = 200
            values["sdr"].tofile(second.parent / "sdr.bin")
            result = compare.compare_reports(first, second, profile="scene")
            self.assertTrue(result["passed"])
            region = [{"name": "small-material", "rectangle": [7, 0, 1, 1], "planes": ["sdr"]}]
            result = compare.compare_reports(first, second, profile="scene", regions=region)
            self.assertFalse(result["passed"])
            self.assertEqual(result["regions"][0]["planes"]["sdr"]["exceeding_pixels"], 1)
            self.assertEqual(result["apis"], ["vulkan", "d3d12"])
            self.assertEqual(result["thresholds_sha256"], compare.canonical_hash(compare.PROFILES["scene"]))

    def test_opacity_sensitivity_requires_exact_identity_delta_and_actual_change(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            first, _, _ = self.make_report(root / "a")
            second, report, values = self.make_report(root / "b")
            report["identity"]["alpha_override"] = "opaque"
            second.write_text(json.dumps(report), encoding="utf-8")
            regions = [{"name": "zero-alpha", "rectangle": [0, 0, 10, 1], "planes": ["hdr", "depth"]}]
            changes = {"alpha_override": ["source", "opaque"]}
            with self.assertRaisesRegex(ValueError, "declared changes"):
                compare.compare_reports(first, second, profile="fixture", regions=regions,
                                         expect_different=True)
            result = compare.compare_reports(first, second, profile="fixture", regions=regions,
                                              expect_different=True, identity_changes=changes)
            self.assertFalse(result["passed"], "an invisible override cannot establish sensitivity")
            values["hdr"][0, 0] = 1
            values["depth"][0, 0] = 0.25
            for plane in ("hdr", "depth"):
                values[plane].tofile(second.parent / (plane + ".bin"))
            result = compare.compare_reports(first, second, profile="fixture", regions=regions,
                                              expect_different=True, identity_changes=changes)
            self.assertTrue(result["passed"])
            report["identity"]["cook"] = "other scene"
            second.write_text(json.dumps(report), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "declared changes"):
                compare.compare_reports(first, second, profile="fixture", regions=regions,
                                         expect_different=True, identity_changes=changes)

    def test_forward_plus_reference_requires_the_same_lights_and_scene(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            first, lhs, _ = self.make_report(root / "a", pixels=2)
            second, rhs, _ = self.make_report(root / "b", pixels=2)
            for (path, report, pipeline) in (
                    (first, lhs, "forward-plus"), (second, rhs, "forward-reference")):
                report["identity"].update(pipeline=pipeline, local_lights={"profile": "test", "count": 2})
                path.write_text(json.dumps(report), encoding="utf-8")
            result = compare.compare_reports(first, second, profile="repeat", forward_plus_reference=True)
            self.assertTrue(result["passed"])
            self.assertEqual(result["comparison"], "forward-plus-reference")
            rhs["identity"]["local_lights"]["count"] = 3
            second.write_text(json.dumps(rhs), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "same local-light"):
                compare.compare_reports(first, second, profile="repeat", forward_plus_reference=True)

    def test_input_mismatch_and_incomplete_or_unexpected_raw_artifacts_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path, report, _ = self.make_report(root / "a", pixels=2)
            second, other, _ = self.make_report(root / "b", pixels=2)
            other["identity"]["view"] = "wrong-camera"
            second.write_text(json.dumps(other), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "identities differ"):
                compare.compare_reports(path, second, profile="fixture")
            for replacement in (b"", b"x" * 17):
                (path.parent / "hdr.bin").write_bytes(replacement)
                with self.assertRaisesRegex(ValueError, "byte length"):
                    compare.load_capture(path, report, "hdr")
            report["captures"]["hdr"]["path"] = "../private.bin"
            (root / "private.bin").write_bytes(b"private")
            with self.assertRaisesRegex(ValueError, "escapes"):
                compare.load_capture(path, report, "hdr")
            report["captures"]["hdr"]["path"] = "hdr.bin"
            report["captures"]["hdr"]["format"] = "rgba32f-le"
            with self.assertRaisesRegex(ValueError, "format must"):
                compare.load_capture(path, report, "hdr")

    def test_artifact_hash_and_declared_dimensions_are_checked(self):
        with tempfile.TemporaryDirectory() as temporary:
            path, report, _ = self.make_report(Path(temporary) / "a", pixels=2)
            report["captures"]["hdr"]["sha256"] = "0" * 64
            with self.assertRaisesRegex(ValueError, "SHA-256"):
                compare.load_capture(path, report, "hdr")

    def test_capture_must_match_declared_identity_and_schema_is_explicit(self):
        with tempfile.TemporaryDirectory() as temporary:
            path, report, _ = self.make_report(Path(temporary) / "a", pixels=2)
            report["identity"]["width"] = 1920
            path.write_text(json.dumps(report), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "rendering identity"):
                compare.compare_reports(path, path, profile="repeat")
            report["schema_version"] = True
            path.write_text(json.dumps(report), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "schema_version"):
                compare.compare_reports(path, path, profile="repeat")
            report["captures"]["hdr"]["width"] = True
            with self.assertRaisesRegex(ValueError, "positive integer"):
                compare.load_capture(path, report, "hdr")

    def test_roi_contract_rejects_empty_duplicate_outside_and_unknown_plane(self):
        valid = {"name": "material", "rectangle": [0, 0, 1, 1], "planes": ["hdr"]}
        cases = [[valid, valid], [{**valid, "rectangle": [0, 0, 0, 1]}],
                 [{**valid, "rectangle": [1, 0, 1, 1]}], [{**valid, "planes": ["depth"]}],
                 [{**valid, "rectangle": [False, 0, 1, 1]}], [{**valid, "exclude_edges": True}]]
        for case in cases:
            with self.subTest(case=case), self.assertRaises(ValueError):
                compare._regions(case, 1, 1, ["hdr"])

    def test_json_nonfinite_duplicate_and_exponent_overflow_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "input.json"
            for text in ('{"x":NaN}', '{"x":Infinity}', '{"x":1e999}', '{"x":1,"x":2}'):
                path.write_text(text, encoding="utf-8")
                with self.assertRaises(ValueError):
                    compare.load_json(path)

    def test_windows_local_and_unc_extended_names_have_equivalent_keys(self):
        normal = compare._windows_containment_key(r"C:\Users\example\captures\hdr.bin")
        extended = compare._windows_containment_key(r"\\?\C:\Users\example\captures\hdr.bin")
        self.assertEqual(normal, extended)
        normal_unc = compare._windows_containment_key(r"\\server\share\captures\hdr.bin")
        extended_unc = compare._windows_containment_key(r"\\?\UNC\server\share\captures\hdr.bin")
        self.assertEqual(normal_unc, extended_unc)
        for device in (r"\\.\PhysicalDrive0", r"\\?\GLOBALROOT\Device\HarddiskVolume1\data",
                       r"\\?\Volume{guid}\data", r"\??\C:\data", r"\\?\UNC\.\pipe\data"):
            with self.subTest(device=device), self.assertRaises(ValueError):
                compare._windows_containment_key(device)

    @unittest.skipUnless(os.name == "nt", "Windows extended capture paths")
    def test_actual_windows_extended_capture_path_and_outside_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path, report, _ = self.make_report(root / "capture", pixels=2)
            raw = (path.parent / "hdr.bin").resolve(strict=True)
            report["captures"]["hdr"]["path"] = "\\\\?\\" + str(raw)
            pixels, info = compare.load_capture(path, report, "hdr")
            self.assertEqual(pixels.shape, (1, 2, 4))
            self.assertEqual(info["sha256"], compare.digest(raw))
            outside = root / "outside.bin"
            outside.write_bytes(raw.read_bytes())
            report["captures"]["hdr"]["path"] = "\\\\?\\" + str(outside.resolve(strict=True))
            with self.assertRaisesRegex(ValueError, "escapes"):
                compare.load_capture(path, report, "hdr")

    def test_resolved_symlink_cannot_escape_the_report_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path, report, _ = self.make_report(root / "capture", pixels=2)
            outside = root / "outside.bin"
            outside.write_bytes((path.parent / "hdr.bin").read_bytes())
            link = path.parent / "hdr-link.bin"
            try:
                link.symlink_to(outside)
            except OSError as error:
                if os.name == "nt":
                    self.skipTest(f"Windows symlink privilege unavailable: {error}")
                raise
            report["captures"]["hdr"]["path"] = link.name
            with self.assertRaisesRegex(ValueError, "escapes"):
                compare.load_capture(path, report, "hdr")


if __name__ == "__main__":
    unittest.main()
