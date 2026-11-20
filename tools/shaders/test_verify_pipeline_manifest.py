import hashlib
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from verify_pipeline_manifest import RENDERER_SHADERS, SHADERS, verify


class PipelineShaderManifestTest(unittest.TestCase):
    def setUp(self):
        directory = TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.shaders = self.root / SHADERS
        self.paths = {
            "source_utf8_lf": "surface.frag.hlsl",
            "spv": "surface.frag.spv",
            "dxil": "dxil/surface.frag.dxil",
            "reflection.txt": "dxil/surface.frag.reflection.txt",
            "spirv-reflection.json": "dxil/surface.frag.spirv-reflection.json",
        }
        artifact = {"source": self.paths["source_utf8_lf"], "defines": []}
        for key, name in self.paths.items():
            artifact[key] = self.write(self.shaders / name, name.encode() + b"\n")
        self.include = self.shaders / "math.hlsli"
        self.toolchain = self.root / RENDERER_SHADERS / "dxil-toolchain.json"
        self.renderer_manifest = self.root / RENDERER_SHADERS / "dxil/manifest.json"
        self.manifest = {
            "schema": 2,
            "toolchain_sha256": self.write(self.toolchain, b"{}\n"),
            "renderer_shaders_sha256": self.write(self.renderer_manifest, b"{}\n"),
            "artifacts": {"surface.frag": artifact},
            "includes": {self.include.name: self.write(self.include, b"// math\n")},
        }
        self.write_manifest()

    @staticmethod
    def write(path, data):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return hashlib.sha256(data).hexdigest()

    def write_manifest(self):
        self.write(self.shaders / "manifest.json", json.dumps(self.manifest).encode())

    def test_text_matches_with_crlf_but_binary_hashes_use_exact_bytes(self):
        for name in (self.paths["source_utf8_lf"], self.paths["reflection.txt"], self.include.name):
            path = self.shaders / name
            path.write_bytes(path.read_bytes().replace(b"\n", b"\r\n"))
        self.assertEqual(verify(self.root), (1, 1))
        binary = self.shaders / self.paths["dxil"]
        binary.write_bytes(binary.read_bytes().replace(b"\n", b"\r\n"))
        with self.assertRaisesRegex(ValueError, r"SHA-256.*surface\.frag\.dxil"):
            verify(self.root)

    def test_rejects_changes_to_sources_includes_artifacts_reflection_and_pins(self):
        paths = [self.shaders / name for name in self.paths.values()]
        for path in [*paths, self.include, self.toolchain, self.renderer_manifest]:
            with self.subTest(path=path.name):
                original = path.read_bytes()
                path.write_bytes(original + b"changed")
                with self.assertRaisesRegex(ValueError, "SHA-256 differs"):
                    verify(self.root)
                path.write_bytes(original)

    def test_reports_missing_and_unlisted_files(self):
        (self.shaders / self.paths["spv"]).unlink()
        extra = self.shaders / "other.frag.hlsl"
        self.write(extra, b"// unlisted\n")
        with self.assertRaisesRegex(ValueError, r"missing=.*surface\.frag\.spv.*unlisted=.*other\.frag\.hlsl"):
            verify(self.root)

    def test_rejects_inconsistent_hashes_for_a_shared_variant_source(self):
        first = self.manifest["artifacts"]["surface.frag"]
        self.manifest["artifacts"]["surface_variant.frag"] = dict(first, source_utf8_lf="0" * 64)
        self.write_manifest()
        with self.assertRaisesRegex(ValueError, r"Variants disagree.*surface\.frag\.hlsl"):
            verify(self.root)


if __name__ == "__main__":
    unittest.main()
