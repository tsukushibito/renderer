"""CPU contracts for the B3 comparison scheduler and raw timing acceptance."""
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import json
import os
import struct

import b3_benchmark as b3
from test_measure_runtime import report, input_plan


def measured(timing=False, api="vulkan"):
    value = report(gpu=timing, api=api)
    value["measurement"]["frame_samples"] = [
        {"frame": index, "phase_ns": [100] * 5,
         "poll_calls_elapsed_ns": 25, "yield_elapsed_ns": 50,
         "active_host_elapsed_ns": 425, "iteration_wall_ns": 1000,
         "gpu_ns": 100_000 if timing else None} for index in range(10)]
    return value


class B3ComparisonTests(unittest.TestCase):
    def test_full_inventory_and_reference_ao_policy_are_required(self):
        value = {"scene": {"meshes": 1296, "primitives": 1591, "materials": 132,
            "source": {"image_count": 405, "triangle_count": 2832120,
                "applied_material_policy": {"ambient_occlusion_enabled": False,
                    "source_pixels_and_references_preserved": True}}},
            "identity": {"alpha_only": False, "alpha_override": False}}
        b3.validate_renderer_scene(value)
        value["scene"]["source"]["applied_material_policy"]["ambient_occlusion_enabled"] = True
        with self.assertRaisesRegex(ValueError, "AO profile"):
            b3.validate_renderer_scene(value)
        value["scene"]["source"]["applied_material_policy"]["ambient_occlusion_enabled"] = False
        value["scene"]["primitives"] = 1590
        with self.assertRaisesRegex(ValueError, "inventory"):
            b3.validate_renderer_scene(value)
    def test_schedule_rotates_engines_and_uses_exactly_three_processes_per_condition(self):
        scheduled = b3.schedule()
        self.assertEqual(len(scheduled), 24)
        self.assertEqual(len({(run["engine"], run["run_id"]) for run in scheduled}), 24)
        self.assertEqual([scheduled[index]["variant"] for index in (0, 8, 16)],
                         ["vulkan", "all", "d3d12"])
        for engine, variants in (("renderer", ("vulkan", "d3d12")), ("godot", ("all", "frustum"))):
            for variant in variants:
                for timing in (False, True):
                    subset = [run for run in scheduled if (run["engine"], run["variant"], run["timing"])
                              == (engine, variant, timing)]
                    self.assertEqual({run["repetition"] for run in subset}, {1, 2, 3})

    def test_active_host_wait_charged_cpu_and_latency_have_distinct_metrics(self):
        metric = b3.renderer_metrics(measured(), {"variant": "vulkan", "timing": False}, input_plan())
        self.assertEqual(metric["distributions"]["active_host_ms"]["p95"], .000425)
        self.assertEqual(metric["distributions"]["poll_and_retire_ms"]["p95"], .0001)
        self.assertEqual(metric["distributions"]["yield_ms"]["p95"], .00005)
        self.assertEqual(metric["scalars"]["main_thread_charged_cpu_ms_per_frame"], 40)
        self.assertEqual(metric["scalars"]["wall_ms_per_frame"], 100)
        self.assertEqual(metric["scalars"]["offscreen_throughput_fps"], 10)
        self.assertNotIn("gpu_ms", metric["distributions"])
        self.assertNotIn("fps", metric["build_to_observed_completion_latency"])
        timed = b3.renderer_metrics(measured(True), {"variant": "vulkan", "timing": True}, input_plan())
        self.assertEqual(timed["distributions"]["gpu_ms"]["p99"], .1)

    def test_rejects_missing_frames_wrong_sum_overlap_and_unrequested_gpu(self):
        mutations = [
            lambda r: r["measurement"]["frame_samples"].pop(),
            lambda r: r["measurement"]["frame_samples"][0].update(frame=1),
            lambda r: r["measurement"]["frame_samples"][0].update(phase_ns=[100] * 4),
            lambda r: r["measurement"]["frame_samples"][0].update(active_host_elapsed_ns=525),
            lambda r: r["measurement"]["frame_samples"][0].update(yield_elapsed_ns=100),
            lambda r: r["measurement"]["frame_samples"][0].update(gpu_ns=1),
            lambda r: r["measurement"]["frame_samples"][0].pop("gpu_ns"),
            lambda r: r["measurement"]["phase_totals_ns"].update(build=1001),
        ]
        for mutate in mutations:
            value = measured()
            mutate(value)
            with self.subTest(mutate=mutate), self.assertRaises(ValueError):
                b3.renderer_metrics(value, {"variant": "vulkan", "timing": False}, input_plan())
        for gpu in (None, 0, -1, True, float("nan")):
            value = measured(True)
            value["measurement"]["frame_samples"][0]["gpu_ns"] = gpu
            with self.subTest(gpu=gpu), self.assertRaises(ValueError):
                b3.renderer_metrics(value, {"variant": "vulkan", "timing": True}, input_plan())

    def test_aggregate_preserves_frame_percentiles_and_independent_run_range(self):
        runs = []
        for condition in b3.schedule():
            if condition["engine"] != "renderer":
                continue
            metrics = b3.renderer_metrics(measured(condition["timing"], condition["variant"]), condition, input_plan())
            metrics["distributions"]["active_host_ms"]["p95"] *= condition["repetition"]
            runs.append({**condition, "metrics": metrics})
        groups = b3.aggregate_renderer(runs)
        active = groups["vulkan/timing-0"]["distributions"]["active_host_ms"]["p95"]
        for observed, expected in zip(active["run_values"], (.000425, .00085, .001275)):
            self.assertAlmostEqual(observed, expected)
        self.assertAlmostEqual(active["run_max"], .001275)
        with self.assertRaises(ValueError):
            b3.aggregate_renderer(runs[:-1])

    def test_input_hash_and_cook_path_containment_are_checked_without_gpu(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            image = directory / "image.bin"
            image.write_bytes(b"b3-payload")
            entry = b3.describe(image)
            b3.verify_inputs([entry])
            manifest = directory / "manifest.json"
            record = {"path": image.name, "byte_length": image.stat().st_size, "sha256": b3.digest(image)}
            self.assertEqual(b3.cook_inputs(manifest, [record]), [entry])
            with self.assertRaises(ValueError):
                b3.cook_inputs(manifest, [{**record, "path": "../image.bin"}])
            image.write_bytes(b"changed")
            with self.assertRaises(ValueError):
                b3.verify_inputs([entry])

    def test_plan_refuses_stale_output_before_build_or_launch(self):
        with tempfile.TemporaryDirectory() as temporary:
            args = SimpleNamespace(output=Path(temporary), timeout=600)
            with patch.object(b3.renderer_measure, "verified_release") as verify:
                with self.assertRaisesRegex(ValueError, "new directory"):
                    b3.make_plan(args)
                verify.assert_not_called()

    def test_formal_run_refuses_wrong_host_before_creating_output_or_launching(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "not-created"
            with patch.object(b3, "ordinary_host_token", side_effect=ValueError("Require ordinary host")), \
                    patch.object(b3.subprocess, "Popen") as launch:
                with self.assertRaisesRegex(ValueError, "ordinary host"):
                    b3.run_plan({"output": str(output)})
                launch.assert_not_called()
                self.assertFalse(output.exists())

    def test_collected_renderer_report_and_capture_bytes_are_rechecked(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            path = directory / "report.json"
            report_value = {"measurement": {"wall_ns": 1}, "captures": {}}
            for plane, payload, format in (("hdr", struct.pack("<4e", 0, 0, 0, 1), "rgba16f-le"),
                                           ("sdr", bytes([0, 0, 0, 255]), "rgba8-unorm"),
                                           ("depth", struct.pack("<f", 1), "r32f-le")):
                raw = directory / (plane + ".bin")
                raw.write_bytes(payload)
                report_value["captures"][plane] = {"path": raw.name, "format": format, "width": 1, "height": 1}
            path.write_text(json.dumps(report_value), encoding="utf-8")
            planes = {plane: b3.load_capture(path, report_value, plane)[1] for plane in ("hdr", "sdr", "depth")}
            record = {"report": str(path), "report_sha256": b3.digest(path),
                      "captures": {"route0": {"report": str(path), "report_sha256": b3.digest(path), "planes": planes}}}
            b3.verify_renderer_outputs(record)
            saved = path.read_bytes()
            report_value["measurement"]["wall_ns"] = 2
            path.write_text(json.dumps(report_value), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "report changed"):
                b3.verify_renderer_outputs(record)
            path.write_bytes(saved)
            (directory / "sdr.bin").write_bytes(bytes([1, 0, 0, 255]))
            with self.assertRaisesRegex(ValueError, "raw capture changed"):
                b3.verify_renderer_outputs(record)

    def test_fixed_view_paths_accept_canonical_forms_and_reject_escapes(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary).resolve() / "run"
            directory.mkdir()
            root = directory / "report.json"
            report_value = {"identity": {"view": "route"}, "views": [], "captures": {}}
            root.write_text(json.dumps(report_value), encoding="utf-8")
            paths = []
            for view in sorted(b3.VIEWS):
                path = directory / "views" / view / "report.json"
                path.parent.mkdir(parents=True)
                path.write_text(json.dumps({"identity": {"view": view}, "captures": {}}), encoding="utf-8")
                paths.append((view, path))
            styles = ("relative", "absolute", "extended") if os.name == "nt" else ("relative", "absolute")
            # Plane decoding has independent real-byte tests. This regression
            # exercises actual report I/O and directory containment only.
            with patch.object(b3, "load_capture", return_value=(None, {"width": 1920, "height": 1080})):
                for style in styles:
                    report_value["views"] = [{"view": view, "report":
                        str(path.relative_to(directory)) if style == "relative" else
                        ("\\\\?\\" if style == "extended" else "") + str(path)} for view, path in paths]
                    with self.subTest(style=style):
                        self.assertEqual(set(b3.captures(root, report_value)), {"route0", *b3.VIEWS})
                outside = directory.parent / "b3-outside-report.json"
                outside.write_text(json.dumps({"identity": {"view": paths[0][0]}, "captures": {}}), encoding="utf-8")
                try:
                    for prefix in (("", "\\\\?\\") if os.name == "nt" else ("",)):
                        report_value["views"][0]["report"] = prefix + str(outside)
                        with self.subTest(outside_prefix=prefix), self.assertRaisesRegex(ValueError, "escapes"):
                            b3.captures(root, report_value)
                finally:
                    outside.unlink()


if __name__ == "__main__":
    unittest.main()
