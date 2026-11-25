import hashlib
import json
from pathlib import Path
import struct
import tempfile
from types import SimpleNamespace
import unittest

from package_web_scene import publish


class WebScenePackageTests(unittest.TestCase):
    def test_pins_source_bytes_and_preserves_last_generation_on_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            assets = root / "cook"
            assets.mkdir()
            objects = root / "lighting-objects"
            objects.mkdir()

            def fixture(directory, data, name=None):
                digest = hashlib.sha256(data).hexdigest()
                name = name or digest
                (directory / name).write_bytes(data)
                return dict(path=name, sha256=digest, byte_length=len(data))

            texels = fixture(assets, bytes([12, 80, 200, 4, 33, 11, 220, 199]), "texels")
            cook = assets / "cook.json"
            cook.write_text(json.dumps({"images": [dict(texels, role="base_color")]}))
            env = assets / "environment.json"
            env.write_text(json.dumps({"images": {}}))
            probe = fixture(objects, b"probe")
            page = fixture(objects, bytes(16))
            index = (b"RSURF003" + struct.pack("<II", 1, 1) + bytes.fromhex(probe["sha256"])
                     + struct.pack("<III", 1, 1, 1) + bytes.fromhex(page["sha256"])
                     + struct.pack("<I", 0))
            surface = fixture(objects, index)
            generation = root / "lights.gi"
            generation.write_text(json.dumps({"schema": "bistro-surface-lighting-v1",
                                              "index_sha256": surface["sha256"]}))
            refs = root / "refs"
            refs.write_bytes(b"reflection")
            args = SimpleNamespace(site=root / "site", scene="sponza", manifest=cook,
                                   ibl_manifest=env, baked_lighting=generation, reflection_probes=refs)
            publish(args)
            bundle = args.site / "scenes/sponza"
            pointer = bundle / "scene.json"
            first = pointer.read_bytes()
            scene = json.loads(first)
            self.assertEqual(scene["maximumAlpha"], {texels["sha256"]: 199})
            self.assertEqual((bundle / "objects" / scene["geometry"]["sha256"]).read_bytes(),
                             cook.read_bytes())
            self.assertEqual((bundle / "objects" / page["sha256"]).read_bytes(), bytes(16))
            (objects / page["sha256"]).write_bytes(b"bad")
            refs.write_bytes(b"next generation")
            with self.assertRaisesRegex(ValueError, "invalid lighting object"):
                publish(args)
            self.assertEqual(pointer.read_bytes(), first)
            cook.write_text(json.dumps({"images": [dict(texels, role="base_color", path="../refs")]}))
            with self.assertRaisesRegex(ValueError, "invalid asset path"):
                publish(args)
            self.assertEqual(pointer.read_bytes(), first)


if __name__ == "__main__":
    unittest.main()
