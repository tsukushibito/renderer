import copy
import json
from pathlib import Path
import sys
import tempfile
import textwrap
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from compare_runtime import digest
import measure_runtime as measure


def report(api="vulkan", gpu=False):
    return {"schema_version": 1, "runtime": {"api": api, "validation_mode": "disabled"},
        "identity": {"view": "street", "width": 4, "height": 2, "inflight": 2},
        "measurement": {"frames": 10, "warmup_frames": 2, "inflight": 2,
            "readback_outside_measurement": True, "wall_ns": 1_000_000_000,
            "cpu": {"qpc_start": 1, "qpc_end": 10_000_001, "qpc_frequency": 10_000_000,
                "qpc_elapsed_ticks": 10_000_000, "process_kernel_100ns": 1_000_000,
                "process_user_100ns": 5_000_000, "main_thread_kernel_100ns": 500_000,
                "main_thread_user_100ns": 3_500_000},
            "phase_totals_ns": {name: 1000 for name in measure.PHASES},
            "gpu": {"enabled": gpu, "samples": 10 if gpu else 0,
                    "total_ns": 1_000_000 if gpu else None,
                    "min_ns": 100_000 if gpu else None, "max_ns": 100_000 if gpu else None},
            "frame_wall_ns": [100_000_000] * 10,
            "frame_wall_scope": "test input: per-frame submit-through-retire latency"}}


def input_plan():
    return {"frames": 10, "warmup": 2, "inflight": 2, "view": "street", "width": 4, "height": 2}


class RuntimeMeasurementTests(unittest.TestCase):
    def test_schedule_rotates_conditions_and_has_three_fresh_processes_each(self):
        runs = measure.schedule(["vulkan", "d3d12"], ["off", "on"], 3)
        self.assertEqual(len(runs), 12)
        self.assertEqual(len({item["run_id"] for item in runs}), 12)
        self.assertEqual([(r["api"], r["gpu_timing"]) for r in runs[:4]],
                         [("vulkan", "off"), ("d3d12", "off"), ("vulkan", "on"), ("d3d12", "on")])
        self.assertEqual(runs[4]["api"], "d3d12")
        self.assertEqual(runs[8]["gpu_timing"], "on")
        for apis, modes, repeats in ((["vulkan"], ["off"], 2), (["vulkan", "vulkan"], ["off"], 3),
                                     (["other"], ["off"], 3), (["vulkan"], ["off", "off"], 3)):
            with self.subTest(apis=apis, modes=modes, repeats=repeats), self.assertRaises(ValueError):
                measure.schedule(apis, modes, repeats)

    def test_report_uses_inner_boundaries_and_distinct_frame_statistics(self):
        result = measure.validate_report(report(), {"api": "vulkan", "gpu_timing": "off"}, input_plan())
        self.assertEqual(result["process_cpu_ns_per_frame"], 60_000_000)
        self.assertEqual(result["main_thread_cpu_ns_per_frame"], 40_000_000)
        self.assertEqual(result["wall_ns_per_frame"], 100_000_000)
        self.assertEqual(result["throughput_frames_per_second"], 10)
        self.assertIsNone(result["gpu_mean_ns_per_frame"])
        self.assertEqual(result["individual_frame_wall_ns"]["p95"], 100_000_000)
        gpu_result = measure.validate_report(report(gpu=True), {"api": "vulkan", "gpu_timing": "on"}, input_plan())
        self.assertEqual(gpu_result["gpu_mean_ns_per_frame"], 100_000)

    def test_missing_wrong_or_partial_measurement_is_rejected(self):
        alterations = [
            lambda r: r.update(schema_version=2),
            lambda r: r["runtime"].update(validation_mode="strict"),
            lambda r: r["runtime"].update(api="d3d12"),
            lambda r: r["identity"].update(view="other-camera"),
            lambda r: r["measurement"].update(frames=9),
            lambda r: r["measurement"].update(readback_outside_measurement=False),
            lambda r: r["measurement"].update(wall_ns=2_000_000_000),
            lambda r: r["measurement"]["cpu"].update(qpc_elapsed_ticks=1),
            lambda r: r["measurement"]["cpu"].update(process_user_100ns=-1),
            lambda r: r["measurement"]["phase_totals_ns"].update(build=True),
            lambda r: r["measurement"].update(frame_wall_ns=[1]),
            lambda r: r["measurement"].update(frame_wall_scope=""),
            lambda r: r["measurement"]["gpu"].update(enabled=True),
        ]
        for alter in alterations:
            value = report()
            alter(value)
            with self.subTest(value=value), self.assertRaises(ValueError):
                measure.validate_report(value, {"api": "vulkan", "gpu_timing": "off"}, input_plan())
        for alteration in ({"samples": 9}, {"total_ns": float("nan")}, {"max_ns": 1}):
            value = report(gpu=True)
            value["measurement"]["gpu"].update(alteration)
            with self.subTest(alteration=alteration), self.assertRaises(ValueError):
                measure.validate_report(value, {"api": "vulkan", "gpu_timing": "on"}, input_plan())

    def test_missing_frame_samples_are_unknown_not_fabricated(self):
        value = report()
        del value["measurement"]["frame_wall_ns"]
        metrics = measure.validate_report(value, {"api": "vulkan", "gpu_timing": "off"}, input_plan())
        self.assertIsNone(metrics["individual_frame_wall_ns"])

    def test_fixture_uses_its_effective_view_and_does_not_weaken_scene_identity(self):
        fixture = report()
        fixture["identity"]["view"] = "fixture"
        condition = {"api": "vulkan", "gpu_timing": "off"}
        metrics = measure.validate_report(fixture, condition, {**input_plan(), "view": "fixture"})
        self.assertEqual(metrics["throughput_frames_per_second"], 10)
        with self.assertRaisesRegex(ValueError, "identity view"):
            measure.validate_report(fixture, condition, input_plan())

    def test_normal_release_manifest_rejects_diagnostics_wrong_profile_and_dirty_tree(self):
        value = {"build": {"profile": "release", "example": "bistro",
                            "effective_features": ["vulkan", "d3d12"]},
                 "git": {"dirty_status": "", "commit": "1" * 40,
                         "source_hashes": {"source.rs": {"sha256": "2" * 64}}}}
        measure.check_release_manifest(value)
        for name in ("bistro", "bistro-graph"):
            binary = copy.deepcopy(value)
            binary["build"].pop("example")
            binary["build"].update(package="renderer-bistro", bin=name)
            with self.subTest(name=name):
                measure.check_release_manifest(binary)
                with self.assertRaisesRegex(ValueError, "archived bistro"):
                    measure.check_release_manifest({**binary, "build": {**binary["build"], "package": "renderer"}})
        for change in (lambda v: v["build"].update(profile="profiling"),
                       lambda v: v["build"].update(effective_features=["vulkan", "d3d12", "tracy"]),
                       lambda v: v["build"].update(instrumentation="tracy"),
                       lambda v: v["build"].update(artifact_class="tracy-instrumented"),
                       lambda v: v["build"].update(instrumentation_dependencies=[{"target": "tracy_client"}]),
                       lambda v: v["build"].update(example="profile_workload"),
                       lambda v: v["build"].update(package="renderer-bistro", bin="bistro"),
                       lambda v: v["build"].update(effective_features=["vulkan"]),
                       lambda v: v["build"]["effective_features"].append("vulkan-diagnostics"),
                       lambda v: v["git"].update(dirty_status=" M shader.frag"),
                       lambda v: v["git"].pop("commit"),
                       lambda v: v["git"].update(source_hashes={})):
            changed = copy.deepcopy(value)
            change(changed)
            with self.assertRaises(ValueError):
                measure.check_release_manifest(changed)

    def test_aggregate_keeps_run_values_and_does_not_call_run_spread_frame_p95(self):
        runs = []
        for index in range(3):
            metrics = measure.validate_report(report(), {"api": "vulkan", "gpu_timing": "off"}, input_plan())
            metrics["wall_ns_per_frame"] += index
            runs.append({"api": "vulkan", "gpu_timing": "off", "metrics": metrics})
        summary = measure.aggregate(runs)["vulkan/gpu-off"]
        value = summary["metrics"]["wall_ns_per_frame"]
        self.assertEqual(value["median_of_runs"], 100_000_001)
        self.assertEqual(value["values"], [100_000_000, 100_000_001, 100_000_002])
        self.assertNotIn("p95", value)
        self.assertEqual(len(summary["frame_wall_by_run"]), 3)

    def test_query_cost_is_a_descriptive_ratio_and_absent_conditions_are_not_zero(self):
        records = []
        for mode, scale in (("off", 1), ("on", 1.1)):
            metrics = measure.validate_report(report(gpu=mode == "on"),
                                              {"api": "vulkan", "gpu_timing": mode}, input_plan())
            for name in ("process_cpu_ns_per_frame", "main_thread_cpu_ns_per_frame", "wall_ns_per_frame"):
                metrics[name] *= scale
            records.append({"api": "vulkan", "gpu_timing": mode, "metrics": metrics})
        observations = measure.query_cost_observations(measure.aggregate(records))
        self.assertAlmostEqual(observations["vulkan"]["metrics"]["wall_ns_per_frame"]["on_over_off_percent_change"], 10)
        self.assertNotIn("d3d12", observations)
        self.assertEqual(measure.query_cost_observations(measure.aggregate(records[:1])), {})

    def test_plan_uses_archived_executable_and_rejects_stale_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            build_path = root / "manifest.json"
            build_path.write_text("{}", encoding="utf-8")
            build = {"binary": {"sha256": digest(sys.executable)}, "git": {}}
            args = SimpleNamespace(frames=10, warmup=2, inflight=2, width=4, height=2,
                timeout_seconds=10, build_manifest=build_path, output=root / "new",
                fixture=True, manifest=None, ibl_manifest=None, apis=["vulkan", "d3d12"], gpu_modes=["off", "on"],
                repeats=3, view="street", cwd=root)
            with patch("measure_runtime.verified_release", return_value=(build, Path(sys.executable))):
                plan = measure.make_plan(args)
                self.assertEqual(len(plan["runs"]), 12)
                self.assertEqual(plan["view"], "fixture")
                self.assertEqual(plan["runs"][0]["command"][0], sys.executable)
                self.assertNotIn("--gpu-timing", plan["runs"][0]["command"])
                self.assertIn("--gpu-timing", plan["runs"][2]["command"])
                args.fixture = False
                args.manifest = root / "cook.json"
                with self.assertRaisesRegex(ValueError, "ibl-manifest"):
                    measure.make_plan(args)
                args.ibl_manifest = root / "ibl.json"
                full = measure.make_plan(args)
                command = full["runs"][0]["command"]
                self.assertEqual(command[command.index("--ibl-manifest") + 1], str(args.ibl_manifest.resolve()))
                args.output.mkdir()
                with self.assertRaisesRegex(ValueError, "new directory"):
                    measure.make_plan(args)

    def batch(self, root, mode):
        path = root / "fake_target.py"
        path.write_text(textwrap.dedent('''
            import json, pathlib, struct, sys, time
            directory, mode, api, gpu = sys.argv[1:]
            directory = pathlib.Path(directory)
            if mode == 'timeout':
                time.sleep(10)
            if mode == 'fail':
                print('native failure retained', file=sys.stderr)
                raise SystemExit(2)
            if mode == 'missing':
                raise SystemExit(0)
            if mode == 'panic':
                print("thread 'main' panicked at fixture", file=sys.stderr)
                raise SystemExit(0)
            directory.mkdir()
            value = REPORT_TEMPLATE
            value['runtime']['api'] = api
            if mode == 'different-input' and api == 'd3d12':
                value['identity']['cook_manifest_sha256'] = 'different cooked scene'
            if gpu == 'on':
                value['measurement']['gpu'] = dict(enabled=True,samples=10,total_ns=1000000,min_ns=100000,max_ns=100000)
            value['captures'] = {}
            for plane, fmt, pixel in [('hdr','rgba16f-le',struct.pack('<4e',0,0,0,1)),
                                     ('sdr','rgba8-unorm',bytes([0,0,0,255])),
                                     ('depth','r32f-le',struct.pack('<f',1))]:
                (directory / (plane+'.bin')).write_bytes(pixel*8)
                value['captures'][plane] = dict(path=plane+'.bin',format=fmt,width=4,height=2)
            (directory/'report.json').write_text(json.dumps(value),encoding='utf-8')
        ''').replace("REPORT_TEMPLATE", repr(report())), encoding="utf-8")
        build_path = root / "build-manifest.json"
        build_path.write_text("{}", encoding="utf-8")
        output = root / "batch"
        plan = {**input_plan(), "output": str(output), "cwd": str(root),
                "build_manifest": str(build_path), "build_manifest_sha256": digest(build_path),
                "binary": {"sha256": digest(sys.executable)},
                "timeout_seconds": 0.01 if mode == "timeout" else 10,
                "runs": measure.schedule(["vulkan", "d3d12"], ["off"], 3)}
        for condition in plan["runs"]:
            condition["command"] = [sys.executable, str(path), str(output / condition["run_id"]),
                                     mode, condition["api"], condition["gpu_timing"]]
        return plan

    def test_real_process_boundary_is_serial_and_preserves_target_reports(self):
        with tempfile.TemporaryDirectory() as temporary:
            plan = self.batch(Path(temporary), "pass")
            result = measure.run_batch(plan)
            self.assertEqual(result["status"], "passed")
            self.assertEqual(len(result["runs"]), 6)
            self.assertEqual(result["aggregates"]["d3d12/gpu-off"]["independent_processes"], 3)
            self.assertTrue(all(item["final_capture_consistency"]["passed"] for item in result["runs"]))
            self.assertTrue(all("controller_wall_ns" in item for item in result["runs"]))

    def test_exit_panic_missing_and_timeout_fail_with_logs_and_failure_status_retained(self):
        for mode in ("fail", "panic", "missing", "timeout", "different-input"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temporary:
                plan = self.batch(Path(temporary), mode)
                with self.assertRaises(measure.MeasurementError):
                    measure.run_batch(plan)
                summary = json.loads((Path(plan["output"]) / "summary.json").read_text())
                self.assertEqual(summary["status"], "failed")
                self.assertEqual(len(summary["runs"]), 2 if mode == "different-input" else 1)
                self.assertTrue(Path(summary["runs"][0]["stdout"]).is_file())
                self.assertTrue(Path(summary["runs"][0]["stderr"]).is_file())
                self.assertEqual(summary["runs"][-1]["status"], "timeout" if mode == "timeout" else "failed")


if __name__ == "__main__":
    unittest.main()
