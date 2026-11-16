"""CPU-only orchestration/CSV contracts; these tests never invoke WPR or a GPU."""

import csv
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from analyze import AnalysisError, analyze_precise, analyze_sampled
from capture import export_trace, loss_fields, record_owned, sha256
from symbolize import resolve_addresses
from wpa import PRECISE, PRECISE_KEYS, SAMPLED, SAMPLE_KEYS, prepare_profile


class CaptureContracts(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.commands = []

    def plan(self, program="pass", handshake=False):
        instance = "Owned_Test_Instance"
        return {"schema": 1, "output": str(self.directory / "capture"), "instance": instance,
                "cwd": str(self.directory), "command": [sys.executable, "-u", "-c", program],
                "timeout_seconds": 5, "max_output_bytes": 1024 * 1024,
                "handshake": handshake,
                "commands": {name: ["fake-wpr", name, "-instancename", instance] for name in
                             ("start", "status", "stop", "cancel", "marker_begin", "marker_end")}}

    def runner(self, command, output, name, **kwargs):
        self.assertEqual(command[-2:], ["-instancename", "Owned_Test_Instance"])
        self.commands.append(name)
        if name == "wpr-stop":
            (output / "capture.etl").write_bytes(b"fixture ETL is never processed as a real trace")
        return 0

    def record(self, plan, runner=None, **kwargs):
        return record_owned(plan, runner=runner or self.runner, validator=lambda _: None, **kwargs)

    def metadata(self):
        return json.loads((self.directory / "capture/capture.json").read_text(encoding="utf-8"))

    def test_failed_start_never_stops_or_cancels(self):
        def denied(command, output, name, **kwargs):
            self.commands.append(name)
            return 5
        with self.assertRaisesRegex(RuntimeError, "No cancellation"):
            self.record(self.plan(), denied)
        self.assertEqual(self.commands, ["wpr-start"])
        self.assertFalse(self.metadata()["own_start_succeeded"])

    def test_start_timeout_preserves_unknown_outcome_without_claiming_ownership(self):
        def timeout(command, output, name, **kwargs):
            self.commands.append(name)
            raise subprocess.TimeoutExpired(command, kwargs["timeout"])
        with self.assertRaises(subprocess.TimeoutExpired):
            self.record(self.plan(), timeout)
        state = self.metadata()
        self.assertEqual(self.commands, ["wpr-start"])
        self.assertFalse(state["own_start_succeeded"])
        self.assertEqual(state["session_state"], "start_outcome_unknown_no_cleanup_attempted")
        self.assertEqual(state["inspection_required"]["instance"], "Owned_Test_Instance")
        self.assertEqual(state["inspection_required"]["status_command"][-1], "Owned_Test_Instance")
        self.assertIn("finished_utc", state)

    def test_successful_capture_retains_pid_lifetime_and_etl_hash(self):
        result = self.record(self.plan())
        self.assertTrue(result["capture_valid"])
        self.assertGreater(result["target"]["pid"], 0)
        self.assertIn("ended_utc", result["target"])
        self.assertEqual(len(result["etl"]["sha256"]), 64)
        self.assertNotIn("wpr-cancel-owned", self.commands)

    def test_owned_failure_cancels_only_after_start_success(self):
        plan = self.plan("import time; time.sleep(10)")
        plan["timeout_seconds"] = 0.05
        with self.assertRaisesRegex(RuntimeError, "timeout"):
            self.record(plan)
        self.assertIn("wpr-cancel-owned", self.commands)
        self.assertEqual(self.metadata()["session_state"], "cancelled")
        self.assertFalse(self.metadata()["capture_valid"])

    def test_quota_failure_is_invalid_and_cleans_owned_instance(self):
        with self.assertRaisesRegex(RuntimeError, "quota"):
            self.record(self.plan("import time; time.sleep(10)"),
                        size_reader=lambda _: 1024 * 1024 if "wpr-start" in self.commands else 0)
        self.assertIn("wpr-cancel-owned", self.commands)

    def test_target_termination_failure_cannot_skip_owned_cancel(self):
        plan = self.plan()
        plan["timeout_seconds"] = 0.1
        elapsed = [0.0]
        process = SimpleNamespace(pid=123, returncode=None, stdin=None, stdout=None, poll=lambda: None)
        def sleep(duration):
            elapsed[0] += duration
        with patch("capture.terminate_target", side_effect=OSError("fixture termination error")):
            with self.assertRaisesRegex(RuntimeError, "timeout"):
                self.record(plan, launcher=lambda *args, **kwargs: process,
                            monotonic=lambda: elapsed[0], sleeper=sleep)
        self.assertIn("wpr-cancel-owned", self.commands)
        self.assertIn("target_cleanup_error", self.metadata())

    def test_launch_failure_cleans_owned_instance(self):
        def failed_launcher(*args, **kwargs):
            raise OSError("fixture missing program")
        with self.assertRaisesRegex(OSError, "missing program"):
            self.record(self.plan(), launcher=failed_launcher)
        self.assertIn("wpr-cancel-owned", self.commands)

    def test_handshake_records_markers_before_allowing_target_shutdown(self):
        program = "\n".join([
            "import os,json,sys",
            "print('PROFILE_WORKLOAD_READY '+json.dumps({'schema':1,'pid':os.getpid()}),flush=True)",
            "assert input()=='GO'",
            "print('PROFILE_WORKLOAD_DONE '+json.dumps({'schema':1,'pid':os.getpid(),'qpc_start':11,'qpc_end':22}),flush=True)",
            "assert input()=='END'",
        ])
        result = self.record(self.plan(program, True))
        self.assertTrue(result["capture_valid"])
        self.assertEqual(result["handshake_done"]["qpc_end"], 22)
        self.assertLess(self.commands.index("wpr-marker-begin"), self.commands.index("wpr-marker-end"))
        self.assertLess(self.commands.index("wpr-marker-end"), self.commands.index("wpr-stop"))

    def test_marker_failure_does_not_become_successful_capture(self):
        program = "\n".join([
            "import os,json",
            "print('PROFILE_WORKLOAD_READY '+json.dumps({'pid':os.getpid()}),flush=True)",
            "assert input()=='GO'",
            "print('PROFILE_WORKLOAD_DONE '+json.dumps({'pid':os.getpid()}),flush=True)",
            "assert input()=='END'",
        ])
        def fail_marker(command, output, name, **kwargs):
            result = self.runner(command, output, name)
            return 5 if name == "wpr-marker-begin" else result
        with self.assertRaisesRegex(RuntimeError, "retained as invalid"):
            self.record(self.plan(program, True), fail_marker)
        self.assertEqual(self.metadata()["session_state"], "stopped")
        self.assertFalse(self.metadata()["quality"]["marker_commands_succeeded"])
        self.assertNotIn("wpr-cancel-owned", self.commands)

    def test_stdin_failure_preserves_original_error_and_final_owned_cleanup_metadata(self):
        program = "import os,json; print('PROFILE_WORKLOAD_READY '+json.dumps({'pid':os.getpid()}),flush=True); input()"
        launched = []
        class FailingClose:
            def __init__(self, stream):
                self.stream = stream
            def write(self, value):
                return self.stream.write(value)
            def flush(self):
                return self.stream.flush()
            def close(self):
                try:
                    self.stream.close()
                finally:
                    raise OSError("fixture stdin close failure")
        def launcher(*args, **kwargs):
            process = subprocess.Popen(*args, **kwargs)
            process.stdin = FailingClose(process.stdin)
            launched.append(process)
            return process
        def runner(command, output, name, **kwargs):
            if name == "wpr-marker-begin":
                launched[0].terminate()
                launched[0].wait(timeout=5)
            return self.runner(command, output, name, **kwargs)
        with self.assertRaises(OSError) as caught:
            self.record(self.plan(program, True), runner, launcher=launcher)
        self.assertNotIn("fixture stdin close failure", str(caught.exception))
        state = self.metadata()
        self.assertEqual(state["session_state"], "cancelled")
        self.assertEqual(state["cancel_exit_code"], 0)
        self.assertIn("error", state)
        self.assertIn("finished_utc", state)
        self.assertEqual(state["stream_cleanup_errors"][0]["resource"], "target_stdin")
        self.assertIn("fixture stdin close failure", state["stream_cleanup_errors"][0]["error"])

    def test_loss_fields_never_fill_missing_evidence_with_zero(self):
        missing = loss_fields(self.directory / "missing.txt")
        self.assertEqual(missing["events_lost_fields"], [])
        path = self.directory / "status.txt"
        path.write_text("Events Lost : 0\nEvents Lost : 2\nBuffers Lost : 3\n", encoding="utf-16")
        fields = loss_fields(path)
        self.assertEqual(fields["events_lost_fields"], [0, 2])
        self.assertEqual(fields["buffers_lost_fields"], [3])


class CsvContracts(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)

    def csv(self, filename, rows):
        path = self.directory / filename
        with path.open("w", encoding="utf-8-sig", newline="") as stream:
            writer = csv.DictWriter(stream, fieldnames=list(rows[0]))
            writer.writeheader()
            writer.writerows(rows)
        return path

    def sampled(self, **changes):
        row = {"pid": "123", "tid": "1", "cpu": "0", "time_s": "0.011", "count": "1",
               "weight_ms": "2", "module": "renderer", "function": "B", "ip": "0x1011", "stack_json": '["A", "B"]'}
        row.update(changes)
        return row

    def precise(self, **changes):
        row = {"pid": "123", "tid": "1", "cpu": "0", "count": "1", "last_switch_out_s": "0.008",
               "ready_time_s": "0.011", "switch_in_s": "0.013", "next_switch_out_s": "0.016",
               "previous_state": "Waiting", "wait_reason": "ExecutionDelay", "wait_stack": "Sleep",
               "ready_stack": "Timer"}
        row.update(changes)
        return row

    def test_sample_self_inclusive_recursion_and_pid_lifetime_filter(self):
        path = self.csv("sampled.csv", [self.sampled(), self.sampled(time_s="0.014", weight_ms="3", function="A",
                        stack_json='["A", "C", "A"]'), self.sampled(pid="124"), self.sampled(time_s="0.020")])
        result = analyze_sampled(path, 123, (0.010, 0.020))
        self.assertEqual(result["sample_count"], 2)
        self.assertEqual(result["weight_ms"], 5)
        self.assertEqual({entry["name"]: entry["weight_ms"] for entry in result["self"]}, {"renderer!A": 3, "renderer!B": 2})
        self.assertEqual({entry["name"]: entry["weight_ms"] for entry in result["inclusive"]}, {"A": 5, "B": 2, "C": 3})

    def test_aggregated_and_duplicate_samples_are_rejected(self):
        for rows, expected in (([self.sampled(count="2")], "leaf"), ([self.sampled(), self.sampled()], "Duplicate")):
            with self.subTest(expected=expected):
                with self.assertRaisesRegex(AnalysisError, expected):
                    analyze_sampled(self.csv("sampled.csv", rows), 123, (0, 1))

    def test_quoted_multiline_stack_and_missing_symbol_keep_their_cost(self):
        path = self.csv("sampled.csv", [self.sampled(function="", stack_json='["root, comma", "line\\nnewline"]')])
        result = analyze_sampled(path, 123, (0, 1))
        self.assertEqual(result["self"][0]["name"], "renderer!<unresolved@0x1011>")
        self.assertEqual(result["weight_ms"], 2)

    def test_wpa_na_stack_sentinel_preserves_cpu_cost_without_inventing_slash_frames(self):
        rows = [self.sampled(), self.sampled(time_s="0.012", weight_ms="3")]
        for row, value in zip(rows, ("[Root]/renderer!B", "n/a")):
            del row["stack_json"]
            row["Stack"] = value
        result = analyze_sampled(self.csv("wpa-stacks.csv", rows), 123, (0, 1), stack_separator="/")
        self.assertEqual(result["sample_count"], 2)
        self.assertEqual(result["weight_ms"], 5)
        self.assertEqual(result["self"], [{"name": "renderer!B", "weight_ms": 5}])
        self.assertEqual({entry["name"]: entry["weight_ms"] for entry in result["inclusive"]}, {"[Root]": 2, "renderer!B": 2})
        self.assertEqual(result["stack_coverage"], {"available_samples": 1, "available_weight_ms": 2,
                                                  "unavailable_samples": 1, "unavailable_weight_ms": 3})
        with self.assertRaisesRegex(AnalysisError, "stack_json"):
            analyze_sampled(self.csv("invalid-json-stack.csv", [self.sampled(stack_json="n/a")]), 123, (0, 1))

    def test_same_function_in_distinct_modules_remains_distinct(self):
        path = self.csv("sampled.csv", [self.sampled(), self.sampled(module="driver", time_s="0.012")])
        result = analyze_sampled(path, 123, (0, 1))
        self.assertEqual({entry["name"] for entry in result["self"]}, {"renderer!B", "driver!B"})

    def test_duplicate_headers_and_nonfinite_weights_are_rejected(self):
        path = self.directory / "bad.csv"
        path.write_text("Ready,Ready\n1,2\n", encoding="utf-8")
        with self.assertRaisesRegex(AnalysisError, "Duplicate CSV"):
            analyze_sampled(path, 123, (0, 1))
        with self.assertRaisesRegex(AnalysisError, "non-finite"):
            analyze_sampled(self.csv("sampled.csv", [self.sampled(weight_ms="NaN")]), 123, (0, 1))

    def test_precise_roi_clips_both_boundaries_and_distinguishes_ready_blocked(self):
        path = self.csv("precise.csv", [self.precise(), self.precise(last_switch_out_s="0.016", ready_time_s="0.018",
                        switch_in_s="0.019", next_switch_out_s="0.023")])
        result = analyze_precise(path, 123, (0.010, 0.020))
        for name, expected in (("running_ms", 4), ("ready_ms", 3), ("blocked_ms", 3)):
            self.assertAlmostEqual(result["totals"][name], expected)
        self.assertEqual(result["readying_stacks"][0]["name"], "Timer")
        self.assertIn("Sleep", result["wait_reason_and_stack"][0]["name"])

    def test_precise_requires_scheduler_evidence_inside_roi(self):
        path = self.csv("precise.csv", [self.precise()])
        with self.assertRaisesRegex(AnalysisError, "inside.*ROI"):
            analyze_precise(path, 123, (1, 2))
        path = self.csv("mixed.csv", [self.precise(tid="99", last_switch_out_s="2.008",
                        ready_time_s="2.011", switch_in_s="2.013", next_switch_out_s="2.016"), self.precise()])
        result = analyze_precise(path, 123, (0.010, 0.020))
        self.assertEqual(result["event_count"], 1)
        self.assertEqual(set(result["threads"]), {"1"})

    def test_precise_keeps_off_cpu_intersection_before_a_later_switch_in(self):
        result = analyze_precise(self.csv("precise.csv", [self.precise()]), 123, (0.009, 0.010))
        self.assertEqual(result["event_count"], 1)
        self.assertAlmostEqual(result["totals"]["blocked_ms"], 1)
        self.assertEqual(result["totals"]["running_ms"], 0)
        result = analyze_precise(self.csv("incomplete.csv", [self.precise(ready_time_s="")]), 123, (0.009, 0.010))
        self.assertEqual(result["event_count"], 1)
        self.assertEqual(result["incomplete_off_cpu_events"], 1)
        self.assertEqual(result["totals"]["blocked_ms"], 0)

    def test_preempted_ready_state_is_never_called_blocking(self):
        result = analyze_precise(self.csv("precise.csv", [self.precise(previous_state="Ready")]), 123, (0, 1))
        self.assertAlmostEqual(result["totals"]["ready_ms"], 5)
        self.assertEqual(result["totals"]["blocked_ms"], 0)

    def test_actual_wpa_schema_and_ambiguous_previous_states_keep_coverage(self):
        # Observed target rows, reduced to their numeric/state/stack contract;
        # no ETL, captured stacks, addresses, or machine identity is embedded.
        def row(state, thread, last, ready, start, end):
            return {"New Process": "fixture.exe (123)", "New Thread Id": thread, "CPU": "19",
                "Switch-In Time (s)": start, "Last Switch-Out Time (s)": last, "Ready Time (s)": ready,
                "Next Switch-Out Time (s)": end, "New Prev State": state, "New Prev Wait Reason": "fixture",
                "New Thread Stack": "fixture wait", "Ready Thread Stack": "fixture ready", "Count": "1"}
        rows = [
            row("Standby", "1", "7.381018100", "7.381018100", "7.381024100", "7.381044900"),
            row("Waiting", "1", "3.603011200", "3.603107300", "3.603133800", "3.603290300"),
            row("Ready", "1", "1.465509600", "1.465509600", "1.465590800", "1.466055700"),
            row("DeferredReady", "2", "0.949462900", "0.949462100", "0.949471000", "0.949858300"),
            row("Running", "1", "7.168171400", "7.168162800", "7.168173200", "7.168280700"),
            row("(Unknown)", "3", "1.117915200", "1.117918900", "1.117925400", "1.117951800"),
        ]
        result = analyze_precise(self.csv("actual-shape.csv", rows), 123, (0, 10))
        self.assertEqual(result["event_count"], 6)
        self.assertAlmostEqual(result["totals"]["running_ms"], 1.1634)
        self.assertAlmostEqual(result["totals"]["ready_ms"], 0.1218)
        self.assertAlmostEqual(result["totals"]["blocked_ms"], 0.0961)
        self.assertAlmostEqual(result["totals"]["unclassified_off_cpu_ms"], 0.012)
        self.assertEqual(result["incomplete_off_cpu_events"], 2)
        self.assertFalse(result["observed_interval_classification_complete"])
        self.assertEqual(result["runnable_ready_precedes_last_out_events"], 1)
        self.assertEqual(set(result["previous_states"]), {row["New Prev State"] for row in rows})
        self.assertAlmostEqual(result["readying_stacks"][0]["ready_ms"], 0.0265)
        self.assertAlmostEqual(result["runnable_new_thread_stacks"][0]["ready_ms"], 0.0953)

    def test_etw_runnable_state_names_and_ids_ignore_stale_or_absent_wake_time(self):
        for state in ("Ready", "1", "Standby", "3", "DeferredReady", "7"):
            for ready in ("0.001", ""):
                with self.subTest(state=state, ready=ready):
                    result = analyze_precise(self.csv("precise.csv", [self.precise(previous_state=state,
                                              ready_time_s=ready)]), 123, (0, 1))
                    self.assertAlmostEqual(result["totals"]["ready_ms"], 5)
                    self.assertEqual(result["totals"]["blocked_ms"], 0)
                    self.assertEqual(result["totals"]["unclassified_off_cpu_ms"], 0)
                    self.assertTrue(result["observed_interval_classification_complete"])
        with self.assertRaisesRegex(AnalysisError, "Waiting.*timestamps"):
            analyze_precise(self.csv("waiting.csv", [self.precise(ready_time_s="0.001")]), 123, (0, 1))

    def test_missing_start_retains_only_observed_unclassified_lower_bound(self):
        result = analyze_precise(self.csv("precise.csv", [self.precise(last_switch_out_s="")]), 123, (0.010, 0.014))
        self.assertAlmostEqual(result["totals"]["unclassified_off_cpu_ms"], 2)
        self.assertEqual(result["missing_off_cpu_start_events"], 1)
        self.assertFalse(result["observed_interval_classification_complete"])
        result = analyze_precise(self.csv("runnable.csv", [self.precise(previous_state="Ready",
                                 last_switch_out_s="", ready_time_s="0.001")]), 123, (0.010, 0.014))
        self.assertEqual(result["totals"]["unclassified_off_cpu_ms"], 0)
        self.assertEqual(result["missing_off_cpu_start_events"], 1)
        self.assertEqual(result["missing_off_cpu_starts"][0]["ready_time_s"], 0.001)
        self.assertFalse(result["observed_interval_classification_complete"])
        with self.assertRaisesRegex(AnalysisError, "unlocalized"):
            analyze_precise(self.csv("localized.csv", [self.precise(count="1,000")]), 123, (0, 1))

    def test_overlapping_scheduler_intervals_are_rejected(self):
        path = self.csv("precise.csv", [self.precise(), self.precise(last_switch_out_s="0.015", ready_time_s="0.016",
                        switch_in_s="0.017", next_switch_out_s="0.018")])
        with self.assertRaisesRegex(AnalysisError, "Overlapping") as caught:
            analyze_precise(path, 123, (0, 1))
        self.assertIn('"record_index"', str(caught.exception))
        self.assertIn('"next_switch_out_s"', str(caught.exception))

    def test_reused_last_out_cannot_count_the_preceding_running_interval_as_waiting(self):
        rows = [self.precise(last_switch_out_s="0.8827198", ready_time_s="0.8827342",
                            switch_in_s="0.8827408", next_switch_out_s="0.8827917"),
                self.precise(last_switch_out_s="0.8827198", ready_time_s="0.8827814",
                            switch_in_s="0.8827917", next_switch_out_s="0.8828038")]
        with self.assertRaisesRegex(AnalysisError, "Overlapping"):
            analyze_precise(self.csv("reused-last-out.csv", rows), 123, (0, 1))
        # The explicit CSwitch identities, not coincident adjacent timestamps,
        # establish that the second event never switched away from this thread.
        rows[0].update(old_pid="0", old_tid="0")
        rows[1].update(old_pid="123", old_tid="1")
        result = analyze_precise(self.csv("self-switch.csv", rows), 123, (0, 1))
        self.assertAlmostEqual(result["totals"]["running_ms"], 0.063)
        self.assertAlmostEqual(result["totals"]["ready_ms"], 0.0066)
        self.assertAlmostEqual(result["totals"]["blocked_ms"], 0.0144)
        self.assertEqual(result["self_switch_events"], 1)
        self.assertEqual(result["self_switches"][0]["last_switch_out_s"], 0.8827198)
        self.assertTrue(result["observed_interval_classification_complete"])
        rows[1]["old_pid"] = "124"
        with self.assertRaisesRegex(AnalysisError, "Overlapping"):
            analyze_precise(self.csv("different-old-process.csv", rows), 123, (0, 1))

    def test_missing_endpoint_remains_unclassified_and_reversed_time_fails(self):
        result = analyze_precise(self.csv("precise.csv", [self.precise(ready_time_s="")]), 123, (0, 1))
        self.assertEqual(result["incomplete_off_cpu_events"], 1)
        self.assertEqual(result["totals"]["blocked_ms"], 0)
        with self.assertRaisesRegex(AnalysisError, "timestamps"):
            analyze_precise(self.csv("precise.csv", [self.precise(ready_time_s="0.014")]), 123, (0, 1))


class SymbolContracts(unittest.TestCase):
    def test_actual_sample_rva_is_used_instead_of_function_entry(self):
        manifest = {"binary": {"path": "fixture.exe", "size_of_image": 0x1000, "sha256": "exe"},
                    "pdb": {"sha256": "pdb"}}
        calls = []
        def runner(command, **kwargs):
            calls.append(command)
            return SimpleNamespace(returncode=0, stderr="", stdout=json.dumps([{"Address": "0x123",
                "ModuleName": command[1].removeprefix("--obj="), "Symbol": [
                {"FunctionName": "compile", "FileName": "graph.rs", "Line": 123}]}]))
        with patch.dict(sys.modules, {"identity": SimpleNamespace(verify_manifest=lambda _: manifest)}):
            result = resolve_addresses("manifest.json", 0x5000, ["0x5123", "0x9000"], "llvm-symbolizer", runner)
        self.assertEqual(calls[0][-1], "0x123")
        self.assertEqual(result["addresses"][0]["status"], "line_available")
        self.assertEqual(result["addresses"][1]["status"], "outside_verified_executable")
        self.assertEqual(len(calls), 1)


class ExportContracts(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        manifest = self.directory / "manifest.json"
        manifest.write_text("{}", encoding="utf-8")
        etl = self.directory / "capture.etl"
        etl.write_bytes(b"fixture trace is never processed")
        capture = self.directory / "capture.json"
        capture.write_text(json.dumps({"session_state": "stopped", "capture_valid": True,
            "target": {"pid": 123},
            "plan": {"manifest": str(manifest), "manifest_sha256": sha256(manifest)},
            "etl": {"path": str(etl), "sha256": sha256(etl)}}), encoding="utf-8")
        profile = self.directory / "fixture.wpaProfile"
        profile.write_text("fixture profile is never processed", encoding="utf-8")
        self.args = SimpleNamespace(capture=capture, timeout_seconds=1, output=self.directory / "export",
            microsoft_symbols=False, wpa_profile=profile, wpt=self.directory / "wpt", ready_stacks_fallback=False)
        self.addCleanup(patch.stopall)
        patch("capture.verified_build", return_value=({"binary": {"sha256": "fixture"}}, self.directory / "fixture.exe")).start()
        patch("capture.prepare_profile", return_value=profile).start()

    def report(self):
        return json.loads((self.args.output / "export.json").read_text(encoding="utf-8"))

    def test_first_export_timeout_retains_failed_stage_metadata(self):
        with patch("capture.logged", side_effect=subprocess.TimeoutExpired(["fixture exporter"], 1)):
            with self.assertRaises(subprocess.TimeoutExpired):
                export_trace(self.args)
        report = self.report()
        self.assertEqual(report["stages"]["wpa"]["status"], "timeout")
        self.assertIn("TimeoutExpired", report["stages"]["wpa"]["error"])
        self.assertIn("finished_utc", report["stages"]["wpa"])
        self.assertNotIn("wpa", report["exit_codes"])
        self.assertFalse(report["export_valid"])

    def test_later_export_launch_failure_retains_prior_success_and_error(self):
        with patch("capture.logged", side_effect=[0, OSError("fixture exporter missing")]):
            with self.assertRaisesRegex(OSError, "fixture exporter missing"):
                export_trace(self.args)
        report = self.report()
        self.assertEqual(report["stages"]["wpa"]["status"], "succeeded")
        self.assertEqual(report["exit_codes"]["wpa"], 0)
        self.assertEqual(report["stages"]["trace-statistics"]["status"], "failed")
        self.assertIn("fixture exporter missing", report["stages"]["trace-statistics"]["error"])
        self.assertFalse(report["export_valid"])

    def test_ready_stacks_fallback_is_opt_in_and_its_native_failure_invalidates_export(self):
        self.args.ready_stacks_fallback = True
        def logged(command, output, name, **kwargs):
            return 0xc0000005 if name == "ready-stacks" else 0
        with patch("capture.logged", side_effect=logged):
            with self.assertRaisesRegex(RuntimeError, "Export failed"):
                export_trace(self.args)
        report = self.report()
        self.assertFalse(report["export_valid"])
        self.assertEqual(report["stages"]["ready-stacks"]["status"], "failed")
        self.assertEqual(report["exit_codes"]["ready-stacks"], 0xc0000005)

    def test_default_export_uses_precise_wpa_stacks_and_target_identity_profile(self):
        with patch("capture.logged", return_value=0), patch("capture.prepare_profile", return_value=self.args.wpa_profile) as prepare:
            report = export_trace(self.args)
        self.assertTrue(report["export_valid"])
        self.assertNotIn("ready-stacks", report["commands"])
        self.assertNotIn("ready-stacks", report["stages"])
        self.assertEqual(report["commands"]["markers"][-1], "-nanoseconds")
        self.assertIn("{68fdd900-4a3e-11d1-84f4-0000f80464e3}", report["commands"]["raw-header"])
        self.assertEqual(report["stages"]["raw-header"]["max_output_bytes"], 8 * 1024 * 1024)
        self.assertEqual(prepare.call_args.kwargs["process"], "fixture.exe (123)")
        self.assertEqual(prepare.call_args.kwargs["identity_source"], self.args.wpt / "Catalog/WpaRuleMatchExporter.wpaProfile")


class ProfileContracts(unittest.TestCase):
    def test_cpu_profile_keeps_metadata_and_unique_flat_event_columns(self):
        import xml.etree.ElementTree as ET
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            graphs = []
            for guid, keys, metrics in ((SAMPLED, SAMPLE_KEYS, ["Count", "Weight"]),
                                        (PRECISE, PRECISE_KEYS, ["Count"])):
                columns = ''.join(f'<Column Name="{name}" Guid="{index}" />' for index, name in enumerate(keys + metrics))
                graphs.append(f'<Graph Guid="{guid}"><Preset Name="vendor" InitialFilterQuery="Idle">'
                              f'<MetadataEntries><MetadataEntry Name="preserved" /></MetadataEntries>'
                              f'<Columns>{columns}</Columns></Preset></Graph>')
            source, target = directory / "vendor.wpaProfile", directory / "cpu.wpaProfile"
            source.write_text('<WpaProfileContainer><Content><Sessions><FileReference FileName="regions.xml" />'
                              '</Sessions><Views><View><Graphs>' + ''.join(graphs) +
                              '<Graph Guid="unrelated" /></Graphs></View></Views></Content></WpaProfileContainer>', encoding="utf-8")
            prepare_profile(source, target)
            tree = ET.parse(target)
            self.assertEqual([node.get("Guid") for node in tree.iter("Graph")], [SAMPLED, PRECISE])
            for preset in tree.iter("Preset"):
                self.assertNotIn("InitialFilterQuery", preset.attrib)
                self.assertIsNotNone(preset.find("MetadataEntries/MetadataEntry"))
                names = [node.get("Name") for node in preset.find("Columns")]
                self.assertEqual(len(names), len(set(names)))
            self.assertEqual(Path(tree.find(".//FileReference").get("FileName")), (directory / "regions.xml").resolve())


if __name__ == "__main__":
    unittest.main()
