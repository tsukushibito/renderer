"""Build boundary guards; Cargo itself is not emulated."""

import hashlib
import json
from pathlib import Path
import tempfile
import unittest

from build import (BuildError, RELEASE_DEFAULTS, check_artifact, check_configuration,
                   check_environment, parse_features, profile_settings, select_artifact,
                   source_snapshot, instrumentation_dependencies, ROOT)


class BuildTests(unittest.TestCase):
    def test_bistro_embedded_conditions_are_bound_to_the_build_source(self):
        name = "tools/bistro/conditions.json"
        snapshot = source_snapshot()
        self.assertIn("tools/bistro/ibl.py", snapshot)
        self.assertEqual(snapshot[name]["raw_sha256"],
                         hashlib.sha256((ROOT / name).read_bytes()).hexdigest())

    def test_shipping_features_reject_direct_diagnostics(self):
        self.assertEqual(parse_features("vulkan,d3d12 vulkan,presentation"),
                         ["d3d12", "presentation", "vulkan"])
        with self.assertRaisesRegex(BuildError, "diagnostics"):
            parse_features("vulkan,development-validation")
        with self.assertRaisesRegex(BuildError, "explicit"):
            parse_features("vulkan,tracy")
        self.assertEqual(parse_features("vulkan", "tracy"), ["tracy", "vulkan"])

    def test_profile_and_flag_overrides_are_rejected_by_name_without_leaking_values(self):
        for key in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_PROFILE_RELEASE_LTO",
                    "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS", "RUSTC_WRAPPER",
                    "TRACY_CLIENT_LIB", "TRACY_CLIENT_SYS_CXXFLAGS", "CXXFLAGS",
                    "CXXFLAGS_x86_64_pc_windows_msvc", "CL", "HOST_CXX"):
            with self.subTest(key=key), self.assertRaises(BuildError) as caught:
                check_environment({key: "private-value"})
            self.assertNotIn("private-value", str(caught.exception))
        check_environment({"CARGO_INCREMENTAL": "0", "CARGO_TARGET_DIR": "target/prof", "RUSTFLAGS": ""})

    def test_cargo_configuration_cannot_hide_optimization_overrides(self):
        for data in ({"profile": {"release": {"lto": True}}},
                     {"target": {"cfg(windows)": {"rustflags": ["-Cforce-frame-pointers=yes"]}}},
                     {"env": {"RUSTFLAGS": {"value": "-Copt-level=0", "force": True}}},
                     {"include": "elsewhere.toml"}):
            with self.subTest(data=data), self.assertRaises(BuildError):
                check_configuration(data, "test-config.toml")
        check_configuration({"env": {"CRITERION_HOME": {"value": "target/criterion", "relative": True}}}, "ok")

    def test_profiling_changes_only_debug_data_from_release(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "Cargo.toml"
            manifest.write_text('[profile.profiling]\ninherits="release"\ndebug=2\nstrip="none"\n', encoding="utf-8")
            settings = profile_settings(root, "profiling")
            self.assertEqual({key: settings[key] for key in RELEASE_DEFAULTS if key not in ("debug", "strip")},
                             {key: value for key, value in RELEASE_DEFAULTS.items() if key not in ("debug", "strip")})
            manifest.write_text(manifest.read_text(encoding="utf-8") + 'lto="thin"\n', encoding="utf-8")
            with self.assertRaisesRegex(BuildError, "only change"):
                profile_settings(root, "profiling")

    def test_select_exact_example_and_validate_observed_cargo_contract(self):
        artifact = {"reason": "compiler-artifact", "target": {"name": "workload", "kind": ["example"]},
                    "executable": "workload.exe", "profile": {"opt_level": "3", "debug_assertions": False,
                    "overflow_checks": False, "debuginfo": 2}, "features": ["vulkan"]}
        message = json.dumps(artifact)
        self.assertEqual(select_artifact(message, "workload"), artifact)
        check_artifact(artifact, {**RELEASE_DEFAULTS, "debug": 2})
        with self.assertRaisesRegex(BuildError, "found 2"):
            select_artifact(message + "\n" + message, "workload")
        artifact["features"].append("memory-profiling")
        with self.assertRaisesRegex(BuildError, "transitively"):
            check_artifact(artifact, {**RELEASE_DEFAULTS, "debug": 2})
        artifact["profile"]["debuginfo"] = 0
        with self.assertRaisesRegex(BuildError, "debug information"):
            check_artifact(artifact, {**RELEASE_DEFAULTS, "debug": 2})

    def test_instrumented_dependency_proof_uses_actual_compiled_artifacts(self):
        messages = "\n".join(json.dumps({"reason": "compiler-artifact", "package_id": "registry#" + name,
            "target": {"name": name}, "features": ["enable", "ondemand", "only-localhost"]})
            for name in ("tracy_client", "tracy_client_sys"))
        self.assertEqual(len(instrumentation_dependencies(messages, "tracy")), 2)
        with self.assertRaisesRegex(BuildError, "unexpectedly compiled"):
            instrumentation_dependencies(messages, "none")
        with self.assertRaisesRegex(BuildError, "missing actual"):
            instrumentation_dependencies("", "tracy")


if __name__ == "__main__":
    unittest.main()
