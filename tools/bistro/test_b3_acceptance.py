import argparse
from collections import Counter
import copy
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import numpy as np

import b3_acceptance as acceptance
from compare_runtime import FORMATS, digest
from resource_acceptance import write_json
from test_resource_acceptance import report as resource_report


def memory():
    normal = {"allocation_free": True, "overflowed": False,
              "event_order": acceptance.EVENT_ORDER,
              "rust": [{"calls": 0, "bytes": 0} for _ in range(4)],
              "vma_host": [{"calls": 0, "bytes": 0} for _ in range(3)],
              "native": [0] * 8 + [48]}
    setup, shutdown = copy.deepcopy(normal), copy.deepcopy(normal)
    setup["allocation_free"] = shutdown["allocation_free"] = False
    setup["rust"][0] = {"calls": 2, "bytes": 128}
    shutdown["rust"][3] = {"calls": 2, "bytes": 128}
    setup["native"] = [1, 0, 1, 0, 1, 0, 1, 0, 0]
    shutdown["native"] = [0, 1, 0, 1, 0, 1, 0, 1, 0]
    return {"normal_allocation_free": True, "normal": normal,
            "setup": setup, "shutdown": shutdown}


def identity_report():
    report = resource_report()
    report["identity"] = {"conditions_sha256": "c" * 64, "shaders_sha256": "d" * 64,
        "aa": True, "shadows": True, "alpha_only": False, "alpha_override": False,
        "inflight": 2, "width": 128, "height": 96, "view": "fixture",
        "environment": {"kind": "fixture-constant-diffuse"},
        "shadow_profile": {"cascades": 4, "resolution": 2048, "distance_m": 120,
                           "split_lambda": 0.6, "blend_fraction": 0.1},
        "aa_profile": {"edge_threshold_min": 0.0312, "edge_threshold_max": 0.125,
                       "iterations": 12, "subpixel_quality": 0.75,
                       "position": "after Neutral, before sRGB"}}
    report["measurement"] = {"frames": 24, "warmup_frames": 8, "inflight": 2,
                             "native_profiling_enabled": False, "presentation": False,
                             "readback_outside_measurement": True}
    report["readback_outside_measurement"] = True
    report["resource_checks"]["views"]["capacity"].update(
        resource="ExecutionSlots", recovery="ReleaseOwners", limit=2, required=None)
    expected = {key: report["identity"][key] for key in ("conditions_sha256", "shaders_sha256")}
    return report, expected


def capture(directory, name, plane, values):
    directory.mkdir(parents=True, exist_ok=True)
    fmt, dtype, channels = FORMATS[plane]
    pixels = np.asarray(values, dtype=dtype)
    path = directory / f"{name}.bin"
    path.write_bytes(pixels.tobytes())
    return {"path": str(path.resolve()), "format": fmt, "width": pixels.shape[1],
            "height": pixels.shape[0], "byte_length": pixels.size * dtype.itemsize,
            "sha256": digest(path)}


def image_report(directory, api="vulkan"):
    on = {"hdr": capture(directory, "hdr", "hdr", np.full((2, 2, 4), 0.25)),
          "sdr": capture(directory, "sdr", "sdr", np.full((2, 2, 4), 100)),
          "depth": capture(directory, "depth", "depth", np.full((2, 2, 1), 0.5))}
    off = {plane: capture(directory / "aa-off", plane, plane,
                         np.full((2, 2, 1 if plane == "depth" else 4),
                                 0.5 if plane == "depth" else (101 if plane == "sdr" else 0.25)))
           for plane in on}
    # A real 2048-square float depth payload exercises the production file size,
    # finite-value, hash and containment checks without a fake image decoder.
    shadow_pixels = np.full((2048, 2048, 1), 0.75, dtype="<f4")
    shadows = {f"shadow{i}": capture(directory, f"shadow{i}", "depth", shadow_pixels)
               for i in range(4)}
    report = {"schema_version": 1, "runtime": {"api": api},
              "identity": {"view": "fixture", "width": 2, "height": 2, "aa": True},
              "captures": on, "shadow_captures": shadows, "views": [],
              "aa_off": {"captures": off, "shadow_captures": {}, "readback_outside_measurement": True}}
    path = directory / "report.json"
    write_json(path, report)
    return path, report


class B3AcceptanceTests(unittest.TestCase):
    def test_schedule_covers_three_independent_processes_and_required_command(self):
        runs = acceptance.schedule(3)
        self.assertEqual(len(runs), 12)
        self.assertEqual(Counter((mode, api) for mode, api, _ in runs),
                         Counter({(mode, api): 3 for mode in ("strict", "disabled")
                                  for api in ("vulkan", "d3d12")}))
        self.assertNotEqual([r[:2] for r in runs[:4]], [r[:2] for r in runs[4:8]])
        with self.assertRaises(ValueError):
            acceptance.schedule(2)
        args = acceptance.command(Path("bistro.exe"), Path("output"), api="vulkan")
        for flag in ("--fixture", "--resource-checks", "--capture-shadow-maps", "--capture-all-views"):
            self.assertIn(flag, args)
        for option, value in (("--frames", "24"), ("--warmup", "8"), ("--inflight", "2")):
            self.assertEqual(args[args.index(option) + 1], value)
        self.assertEqual(args[args.index("--view") + 1], "route")
        with self.assertRaises(ValueError):
            acceptance.command(Path("exe"), Path("out"), api="vulkan", manifest=Path("cook.json"))

    def test_raw_memory_gates_reject_false_positive_flags_and_missing_controls(self):
        self.assertTrue(acceptance.validate_memory(memory())["passed"])
        changes = [
            lambda r: r["normal"]["rust"][0].update(calls=1),
            lambda r: r["normal"]["vma_host"][1].update(bytes=8),
            lambda r: r["normal"].update(rust=[]),
            lambda r: r["normal"]["rust"][0].update(calls=False),
            lambda r: r["normal"].update(native=[0] * 8),
            lambda r: r["normal"]["native"].__setitem__(2, 1),
            lambda r: r["normal"]["native"].__setitem__(8, -1),
            lambda r: r["normal"].update(overflowed=True),
            lambda r: r["setup"]["rust"][0].update(calls=0),
            lambda r: r["shutdown"]["rust"][3].update(calls=0),
            lambda r: r["setup"]["native"].__setitem__(2, 0),
            lambda r: r["shutdown"]["native"].__setitem__(3, 0),
        ]
        for change in changes:
            value = memory()
            change(value)
            with self.subTest(change=change), self.assertRaises(ValueError):
                acceptance.validate_memory(value)

    def test_identity_scope_and_f32_expansion_are_checked(self):
        report, expected = identity_report()
        acceptance.validate_identity(report, expected, full_scene=False)
        report["identity"]["shadow_profile"]["split_lambda"] = np.float32(0.6).item()
        report["identity"]["shadow_profile"]["blend_fraction"] = np.float32(0.1).item()
        acceptance.validate_identity(report, expected, full_scene=False)
        changes = [
            lambda r: r["identity"].update(aa=False),
            lambda r: r["identity"].update(shadows=False),
            lambda r: r["identity"].update(conditions_sha256="0" * 64),
            lambda r: r["identity"]["shadow_profile"].update(split_lambda=0.61),
            lambda r: r["identity"]["aa_profile"].update(iterations=8),
            lambda r: r["measurement"].update(native_profiling_enabled=True),
            lambda r: r["measurement"].update(frames=23),
            lambda r: r["resource_checks"]["replacement"]["cycles"].pop(),
            lambda r: r["resource_checks"]["views"]["capacity"].update(recovery="Poll"),
        ]
        for change in changes:
            value = copy.deepcopy(report)
            change(value)
            with self.subTest(change=change), self.assertRaises(ValueError):
                acceptance.validate_identity(value, expected, full_scene=False)
        report["identity"].update(width=1920, height=1080, cook_manifest_sha256="a" * 64,
                                  environment={"manifest_sha256": "b" * 64})
        report["scene"] = {"source": {"applied_material_policy": {
            "ambient_occlusion_enabled": False, "source_pixels_and_references_preserved": True}}}
        expected.update(cook_manifest_sha256="a" * 64, ibl_manifest_sha256="b" * 64)
        acceptance.validate_identity(report, expected, full_scene=True)
        report["scene"]["source"]["applied_material_policy"]["ambient_occlusion_enabled"] = True
        with self.assertRaisesRegex(ValueError, "AO lighting profile"):
            acceptance.validate_identity(report, expected, full_scene=True)
        report["scene"]["source"]["applied_material_policy"]["ambient_occlusion_enabled"] = False
        report["identity"]["environment"]["manifest_sha256"] = "f" * 64
        with self.assertRaises(ValueError):
            acceptance.validate_identity(report, expected, full_scene=True)

    def test_shadow_aa_files_and_invariance_are_validated_without_gpu(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            path, report = image_report(directory)
            checks = []
            with patch.object(acceptance, "resource_captures", return_value={}):
                artifacts = acceptance.collect(path, report, [], aa_checks=checks)
                self.assertEqual(len(artifacts), 6)
                self.assertEqual(checks[0]["sdr_changed_pixels"], 4)
                self.assertTrue(checks[0]["hdr_depth_bit_exact"])
                # A valid, rehashed HDR mutation must fail AA invariance even
                # though every individual file still passes format/hash checks.
                report["aa_off"]["captures"]["hdr"] = capture(
                    directory / "aa-off", "hdr", "hdr", np.full((2, 2, 4), 0.5))
                with self.assertRaisesRegex(ValueError, "AA changes the hdr"):
                    acceptance.collect(path, report, [])
            bad = copy.deepcopy(report)
            bad["shadow_captures"].pop("shadow3")
            with self.assertRaises(ValueError):
                acceptance.diagnostics(path, bad)
            bad = copy.deepcopy(report)
            bad["shadow_captures"]["shadow3"]["path"] = "./shadow0.bin"
            with self.assertRaisesRegex(ValueError, "reuse one artifact path"):
                acceptance.diagnostics(path, bad)
            bad = copy.deepcopy(report)
            bad["shadow_captures"]["shadow3"]["width"] = 1024
            with self.assertRaises(ValueError):
                acceptance.diagnostics(path, bad)
            outside = directory / "outside.json"
            outside.write_text("{}", encoding="utf8")
            owner = directory / "run"
            owner.mkdir()
            with self.assertRaises(ValueError):
                acceptance._report_path(owner, outside)

    def test_failed_bit_exact_comparison_is_retained_before_raising(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            reports = []
            for index, value in enumerate((100, 101)):
                output = directory / str(index)
                meta = capture(output, "sdr", "sdr", np.full((2, 2, 4), value))
                path = output / "report.json"
                write_json(path, {"schema_version": 1, "runtime": {"api": "vulkan"},
                                  "identity": {"width": 2, "height": 2}, "captures": {"sdr": meta}})
                reports.append({"probe": (path, ["sdr"])})
            retained = []
            with self.assertRaisesRegex(ValueError, "strict-disabled"):
                acceptance.compare_sets(*reports, "repeat", "strict-disabled", destination=retained)
            self.assertEqual(len(retained), 1)
            self.assertFalse(retained[0]["passed"])
            self.assertEqual(retained[0]["planes"]["sdr"]["exceeding_pixels"], 4)

    def test_child_process_failure_retains_logs_and_failed_summary(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            strict, shipping = directory / "strict.exe", directory / "shipping.exe"
            strict.write_bytes(b"strict")
            shipping.write_bytes(b"shipping")
            args = argparse.Namespace(strict_binary=strict, shipping_binary=shipping,
                source_commit="a" * 40, manifest=None, ibl_manifest=None,
                output=directory / "acceptance", repeats=3, timeout=1)
            with patch.object(acceptance.subprocess, "run", return_value=subprocess.CompletedProcess([], 23)):
                with self.assertRaisesRegex(ValueError, "failed; inspect retained logs"):
                    acceptance.run(args)
            summary = json.loads((args.output / "summary.json").read_text(encoding="utf8"))
            self.assertEqual(summary["status"], "failed")
            self.assertEqual(len(summary["runs"]), 1)
            self.assertEqual(summary["runs"][0]["exit_code"], 23)
            self.assertEqual(len(list((args.output / "logs").glob("*.txt"))), 2)


if __name__ == "__main__":
    unittest.main()
