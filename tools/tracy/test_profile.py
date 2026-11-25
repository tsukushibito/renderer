"""Build/mode/protocol identity failures without a GPU or profiler process."""
import importlib.util
from pathlib import Path
import queue
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("tracy_profile", Path(__file__).with_name("profile.py"))
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)


def manifest():
    return {"build": {"instrumentation": "tracy", "artifact_class": "tracy-instrumented", "profile": "release",
                      "example": "bistro", "effective_features": ["tracy", "vulkan", "d3d12", "presentation"],
                      "instrumentation_dependencies": [{"target": name, "package_id": "registry#package@" + version,
                          "features": ["enable", "ondemand", "only-localhost"]}
                          for name, version in (("tracy_client", "0.19.0"), ("tracy_client_sys", "0.30.1"))]},
            "git": {"dirty_status": "", "commit": "a" * 40, "source_hashes": {"Cargo.lock": "fixture"}}}


class ProfileTests(unittest.TestCase):
    def test_archive_accepts_both_bistro_binaries(self):
        for name in ("renderer-scenes", "renderer-scenes-graph"):
            value = manifest()
            value["build"].pop("example")
            value["build"].update(package="renderer-scenes", bin=name)
            with self.subTest(name=name):
                profile.check_instrumented(value)
                value["build"]["bin"] = "other"
                with self.assertRaisesRegex(ValueError, "explicitly archived"):
                    profile.check_instrumented(value)

    def test_normal_artifacts_cannot_be_relabelled_and_forbidden_client_features_fail(self):
        value = manifest()
        profile.check_instrumented(value)
        value["build"]["instrumentation"] = "none"
        with self.assertRaisesRegex(ValueError, "explicitly archived"):
            profile.check_instrumented(value)
        value = manifest()
        value["build"]["instrumentation_dependencies"][1]["features"].append("system-tracing")
        with self.assertRaisesRegex(ValueError, "unsupported collection"):
            profile.check_instrumented(value)

    def test_wrong_protocol_pid_or_out_of_order_event_fails(self):
        process = SimpleNamespace(pid=12, poll=lambda: None)
        for event in (("BISTRO_READY", {"pid": 13}), ("BISTRO_DONE", {"pid": 12}), ("ERROR", "malformed JSON")):
            events = queue.Queue()
            events.put(event)
            with self.subTest(event=event), self.assertRaisesRegex(ValueError, "Expected owned"):
                profile.wait_event(process, events, "BISTRO_READY", lambda: None)

    def test_handshake_rejects_wrong_nonce_input_hash_or_connection(self):
        plan = {"api": "vulkan", "frames": 2, "warmup": 3, "fixture": False,
                "target_environment": {"RENDERER_TRACY_CAPTURE_ID": "a" * 32},
                "inputs": {"conditions": {"sha256": "conditions"}, "cook_manifest": {"sha256": "cook"}}}
        value = {"api": "vulkan", "measured_frames": 2, "warmup_done": True, "warmup_frames": 3,
                 "tracy_connected": False, "capture_id": "a" * 32, "conditions_sha256": "conditions", "cook_manifest_sha256": "cook"}
        profile.checked_handshake(value, plan, ready=True)
        for field, changed in (("capture_id", "b" * 32), ("tracy_connected", True), ("cook_manifest_sha256", "other")):
            with self.subTest(field=field), self.assertRaises(ValueError):
                profile.checked_handshake({**value, field: changed}, plan, ready=True)

    def test_tool_or_input_modification_after_planning_is_detected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "tool.exe"
            path.write_bytes(b"fixture")
            record = profile.file_identity(path)
            profile.verify_file(record)
            path.write_bytes(b"replacement")
            with self.assertRaisesRegex(ValueError, "identity changed"):
                profile.verify_file(record)

    def test_failed_export_retains_stage_error_and_prior_success(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            result = {"stages": {"help": {"status": "passed"}}}
            with patch.object(profile, "run_logged", side_effect=subprocess.TimeoutExpired(["fixture"], 1)):
                with self.assertRaises(subprocess.TimeoutExpired):
                    profile.export_stage(result, ["fixture"], output, "inclusive", 1)
            self.assertEqual(result["stages"]["help"]["status"], "passed")
            self.assertEqual(result["stages"]["inclusive"]["status"], "failed")
            self.assertIn("TimeoutExpired", result["stages"]["inclusive"]["error"])
            self.assertIn("finished_utc", result["stages"]["inclusive"])


if __name__ == "__main__":
    unittest.main()
