import hashlib
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from verify_pipeline_manifest import BAKE_SHADERS, CORE_SHADERS, RENDERER_SHADERS, verify


class PipelineShaderManifestTest(unittest.TestCase):
    def setUp(self):
        directory = TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.shaders = self.root / RENDERER_SHADERS
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
        self.toolchain = self.root / CORE_SHADERS / "dxil-toolchain.json"
        self.core_manifest = self.root / CORE_SHADERS / "dxil/manifest.json"
        self.manifest = {
            "schema": 2,
            "toolchain_sha256": self.write(self.toolchain, b"{}\n"),
            "renderer_shaders_sha256": self.write(self.core_manifest, b'{"artifacts": {}, "licenses": {}}\n'),
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
        self.assertEqual(verify(self.root, RENDERER_SHADERS), (1, 1))
        binary = self.shaders / self.paths["dxil"]
        binary.write_bytes(binary.read_bytes().replace(b"\n", b"\r\n"))
        with self.assertRaisesRegex(ValueError, r"SHA-256.*surface\.frag\.dxil"):
            verify(self.root, RENDERER_SHADERS)

    def test_rejects_changes_to_sources_includes_artifacts_reflection_and_pins(self):
        paths = [self.shaders / name for name in self.paths.values()]
        for path in [*paths, self.include, self.toolchain, self.core_manifest]:
            with self.subTest(path=path.name):
                original = path.read_bytes()
                path.write_bytes(original + b"changed")
                with self.assertRaisesRegex(ValueError, "SHA-256 differs"):
                    verify(self.root, RENDERER_SHADERS)
                path.write_bytes(original)

    def test_bake_package_has_its_own_inventory_and_only_requires_the_toolchain_pin(self):
        self.manifest.pop("renderer_shaders_sha256")
        self.write_manifest()
        self.core_manifest.unlink()
        bake = self.root / BAKE_SHADERS
        bake.parent.mkdir(parents=True)
        self.shaders.rename(bake)
        self.assertEqual(verify(self.root, BAKE_SHADERS), (1, 1))
        (bake / self.paths["spv"]).unlink()
        with self.assertRaisesRegex(ValueError, r"lighting-bake.*missing=.*surface\.frag\.spv"):
            verify(self.root, BAKE_SHADERS)

    def test_reports_missing_and_unlisted_files(self):
        (self.shaders / self.paths["spv"]).unlink()
        extra = self.shaders / "other.frag.hlsl"
        self.write(extra, b"// unlisted\n")
        with self.assertRaisesRegex(ValueError, r"missing=.*surface\.frag\.spv.*unlisted=.*other\.frag\.hlsl"):
            verify(self.root, RENDERER_SHADERS)

    def test_verifies_standard_output_assets_from_the_shared_shader_manifest(self):
        source = self.shaders / "output/sdr.frag.hlsl"
        artifact = {"source_utf8_lf": self.write(source, b"shader\n")}
        for field, target in [
            ("spirv", "sdr_linear.frag.spv"),
            ("dxil", "dxil/sdr_linear.frag.dxil"),
            ("reflection", "dxil/sdr_linear.frag.reflection.txt"),
            ("spirv_reflection", "dxil/sdr_linear.frag.spirv-reflection.json"),
        ]:
            artifact[field] = self.write(self.shaders / "output" / target, b"compiled shader")
        license_path = self.shaders / "output/licenses/FXAA-notices.txt"
        shared_manifest = {
            "artifacts": {"../../sashiko-renderer/shaders/output/sdr_linear.frag": artifact},
            "licenses": {license_path.name: self.write(license_path, b"notice\n")},
        }
        self.manifest["renderer_shaders_sha256"] = self.write(
            self.core_manifest, json.dumps(shared_manifest).encode()
        )
        self.write_manifest()
        with self.assertRaisesRegex(ValueError, r"missing=.*sdr_linear\.frag\.wgsl"):
            verify(self.root, RENDERER_SHADERS)
        self.write(self.shaders / "output/sdr_linear.frag.wgsl", b"wgsl")
        self.assertEqual(verify(self.root, RENDERER_SHADERS), (2, 1))
        source.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "SHA-256 differs.*sdr.frag"):
            verify(self.root, RENDERER_SHADERS)

    def test_rejects_inconsistent_hashes_for_a_shared_variant_source(self):
        first = self.manifest["artifacts"]["surface.frag"]
        self.manifest["artifacts"]["surface_variant.frag"] = dict(first, source_utf8_lf="0" * 64)
        self.write_manifest()
        with self.assertRaisesRegex(ValueError, r"Variants disagree.*surface\.frag\.hlsl"):
            verify(self.root, RENDERER_SHADERS)


if __name__ == "__main__":
    unittest.main()
