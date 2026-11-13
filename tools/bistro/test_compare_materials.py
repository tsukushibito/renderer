import json
from pathlib import Path
import tempfile
import unittest

from PIL import Image

from compare_materials import compare


class MaterialContractTests(unittest.TestCase):
    def test_ao_cannot_silently_use_a_different_uv_or_transform(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            Image.new("RGB", (1, 1), (13, 71, 209)).save(root / "orm.png")
            baseline = {"materials": [{"name": "M", "alpha_mode": "OPAQUE", "double_sided": True,
                                        "textures": {"SpecularColor": "orm.png"}}]}
            textures = {"textures": [{"source": "orm.png", "output": "orm.png"}]}
            material = {"name": "M", "doubleSided": True, "occlusionTexture": {"index": 0},
                        "pbrMetallicRoughness": {"metallicRoughnessTexture": {"index": 0}}}
            model = {"asset": {"version": "2.0"}, "materials": [material],
                     "textures": [{"source": 0}], "images": [{"uri": "orm.png"}]}
            path = root / "scene.gltf"
            def result():
                path.write_text(json.dumps(model))
                return compare(baseline, textures, root, path)
            self.assertTrue(result()["passed"])
            material["occlusionTexture"]["texCoord"] = 1
            self.assertFalse(result()["passed"])
            material["occlusionTexture"].pop("texCoord")
            material["occlusionTexture"]["extensions"] = {"KHR_texture_transform": {"offset": [0.5, 0]}}
            self.assertFalse(result()["passed"])
            material["occlusionTexture"].pop("extensions")
            material["emissiveFactor"] = [1, 1, 1]
            self.assertFalse(result()["passed"])
            material.pop("emissiveFactor")
            material["emissiveTexture"] = {"index": 0}
            self.assertFalse(result()["passed"])


if __name__ == "__main__":
    unittest.main()
