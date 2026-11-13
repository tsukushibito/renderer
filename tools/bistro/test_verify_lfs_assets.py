import hashlib
import json
from pathlib import Path
import tempfile
import unittest

from verify_lfs_assets import verify


class LfsAssetsTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.asset = self.root / "assets/bistro/converted/scene.bin"
        self.asset.parent.mkdir(parents=True)
        self.asset.write_bytes(b"1234")
        self.entry = {"path": self.asset.relative_to(self.root).as_posix(), "size": 4,
                      "sha256": hashlib.sha256(b"1234").hexdigest()}
        self.write_manifest()

    def write_manifest(self):
        (self.root / "assets/bistro/manifest.json").write_text(
            json.dumps({"schema_version": 1, "files": [self.entry]}), encoding="utf-8")

    def test_same_size_corruption_fails(self):
        self.assertEqual(verify(self.root)["files"], 1)
        self.asset.write_bytes(b"1235")
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            verify(self.root)

    def test_pointer_is_checked_without_treating_it_as_payload(self):
        pointer = f"version https://git-lfs.github.com/spec/v1\noid sha256:{self.entry['sha256']}\nsize 4\n"
        self.asset.write_text(pointer, encoding="utf-8", newline="\n")
        self.assertTrue(verify(self.root, pointers=True)["passed"])
        with self.assertRaisesRegex(ValueError, "run git lfs pull"):
            verify(self.root)
        self.asset.write_text(pointer.replace("size 4", "size 5"), encoding="utf-8", newline="\n")
        with self.assertRaisesRegex(ValueError, "pointer differs"):
            verify(self.root, pointers=True)

    def test_unlisted_asset_and_missing_asset_fail(self):
        extra = self.asset.with_name("forgotten.png")
        extra.write_bytes(b"image")
        with self.assertRaisesRegex(ValueError, "coverage differs"):
            verify(self.root)
        extra.unlink()
        self.asset.unlink()
        with self.assertRaises(FileNotFoundError):
            verify(self.root)

    def test_manifest_cannot_traverse_outside_repository(self):
        self.entry["path"] = "../private.bin"
        self.write_manifest()
        with self.assertRaisesRegex(ValueError, "Invalid manifest path"):
            verify(self.root)


if __name__ == "__main__":
    unittest.main()
