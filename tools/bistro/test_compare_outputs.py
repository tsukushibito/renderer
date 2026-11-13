import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import compare_outputs
from compare_outputs import compare


class ReproductionTests(unittest.TestCase):
    def test_changed_linked_buffer_fails_even_when_json_is_identical(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("first", "second"):
                target = root / name
                target.mkdir()
                (target / "scene.gltf").write_text(json.dumps({"asset": {"version": "2.0"},
                    "buffers": [{"uri": "scene.bin", "byteLength": 4}]}))
                (target / "scene.bin").write_bytes(b"1234")
            self.assertTrue(compare(root / "first/scene.gltf", root / "second/scene.gltf")["passed"])
            (root / "second/scene.bin").write_bytes(b"1235")
            self.assertFalse(compare(root / "first/scene.gltf", root / "second/scene.gltf")["passed"])

    def test_unlinked_host_files_are_excluded_and_external_paths_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("first", "second"):
                (root / name).mkdir()
                (root / name / "scene.gltf").write_text('{"asset":{"version":"2.0"}}')
            (root / "second/source.blend").write_bytes(b"host-only")
            self.assertEqual(len(compare(root / "first/scene.gltf", root / "second/scene.gltf")["files"]), 1)
            (root / "first/scene.gltf").write_text('{"buffers":[{"uri":"../private.bin"}]}')
            with self.assertRaisesRegex(ValueError, "escapes"):
                compare(root / "first/scene.gltf", root / "second/scene.gltf")

    def test_each_export_file_is_hashed_once_and_recorded_hash_matches(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            inputs = []
            for name in ("first", "second"):
                target = root / name
                target.mkdir()
                model = target / "scene.gltf"
                model.write_text('{"buffers":[{"uri":"scene.bin"}]}')
                buffer = target / "scene.bin"
                buffer.write_bytes(b"1234")
                inputs.extend((model, buffer))
            expected = compare_outputs.digest(root / "second/scene.bin")
            with patch("compare_outputs.digest", wraps=compare_outputs.digest) as digest:
                report = compare(root / "first/scene.gltf", root / "second/scene.gltf")
            self.assertCountEqual([call.args[0] for call in digest.call_args_list], inputs)
            self.assertEqual(report["files"][1]["sha256"], expected)
            self.assertTrue(report["passed"])


if __name__ == "__main__":
    unittest.main()
