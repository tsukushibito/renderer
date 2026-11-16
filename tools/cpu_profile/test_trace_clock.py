"""Portable clock/ownership contracts without native tools or an actual ETL."""

from copy import deepcopy
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

from trace_clock import ClockError, derive_clock, parse_header, parse_marks, parse_statistics


SCHEMA = "# Cpu # RawTimeStamp # TimeStamp[ns] # EventKey\n"


def header(trace, second_qpc=1_000_000_030):
    return (SCHEMA + "# 0 # 1000000000 # 0 # 0x0\n"
            + f"OS Version: test, Events Lost: 0, Buffers lost: 0, Trace Name: {trace}\n"
            + f"# 1 # {second_qpc:#x} # 10000000000 # 0x0\n")


def statistics(trace):
    return ("Clock type : PerfCounter\nNumber of Traces : 1\n"
            + f"Trace name: {trace}\nTotal # Lost Events : 0\n# Lost Events : 0\n")


class TraceClockTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.etl = self.directory / "capture.etl"
        self.etl.write_bytes(b"not a real trace; no native tool is invoked")
        self.capture_path = self.directory / "capture.json"
        self.header_path = self.directory / "header.txt"
        self.statistics_path = self.directory / "statistics.txt"
        self.capture = {
            "schema": 1, "capture_valid": True, "own_start_succeeded": True, "session_state": "stopped",
            "plan": {"handshake": True, "instance": "Owned_123"},
            "target": {"pid": 42, "exit_code": 0}, "handshake_ready": {"pid": 42, "qpc_frequency": 3},
            "handshake_done": {"pid": 42, "window": {"qpc_frequency": 3,
                               "qpc_start": 1_000_000_001, "qpc_end": 1_000_000_007, "qpc_elapsed_ticks": 6}},
            "marker_begin": {"text": "Owned_123_BEGIN", "exit_code": 0},
            "marker_end": {"text": "Owned_123_END", "exit_code": 0},
            "etl": {"path": self.etl.name, "sha256": hashlib.sha256(self.etl.read_bytes()).hexdigest()},
            "quality": {"lost_events": None, "lost_buffers": None}}
        self.header_path.write_text(header(self.etl), encoding="utf-8")
        self.statistics_path.write_text(statistics(self.etl), encoding="utf-8")

    def derive(self, capture=None, marks=None):
        self.capture_path.write_text(json.dumps(capture or self.capture), encoding="utf-8")
        return derive_clock(self.capture_path, self.header_path, self.statistics_path, marks)

    def test_exact_nonterminating_seconds_preserve_target_and_unknown_losses(self):
        result = self.derive()
        self.assertEqual(result["origin_qpc_exact"], {"numerator": 1_000_000_000, "denominator": 1})
        self.assertEqual(result["roi"]["start_seconds_exact"], {"numerator": 1, "denominator": 3})
        self.assertEqual(result["roi"]["end_seconds_exact"], {"numerator": 7, "denominator": 3})
        self.assertEqual((result["pid"], result["session"]), (42, "Owned_123"))
        self.assertEqual(result["header"]["pair_rows"], 2)
        self.assertIsNone(result["marks"]["actual_presence_verified"])
        self.assertIsNone(result["loss"]["capture_lost_events"])
        self.assertEqual(result["loss"]["statistics_lost_buffers_fields"], [])
        self.assertEqual(result["evidence"]["etl"]["sha256"], self.capture["etl"]["sha256"])

    def test_fractional_origin_and_every_pair_agree_exactly(self):
        text = SCHEMA + "# 0 # 10 # 100000000 # 0x0\n# 1 # 11 # 600000000 # 0x0\nTrace Name: fixture.etl\n"
        self.assertEqual(parse_header(text, 2)["origin"], Fraction(49, 5))
        with self.assertRaisesRegex(ClockError, "contradictory"):
            parse_header(text.replace("# 11 #", "# 12 #"), 2)
        with self.assertRaisesRegex(ClockError, "distinct"):
            parse_header(header("fixture.etl").split("# 1 #")[0], 3)
        with self.assertRaisesRegex(ClockError, "schema"):
            parse_header(header("fixture.etl").replace("TimeStamp[ns]", "TimeStamp[us]"), 3)

    def test_clock_and_trace_file_identity_must_match(self):
        with self.assertRaisesRegex(ClockError, "PerfCounter"):
            parse_statistics(statistics(self.etl).replace("PerfCounter", "SystemTime"))
        self.statistics_path.write_text(statistics(self.directory / "other.etl"), encoding="utf-8")
        with self.assertRaisesRegex(ClockError, "different ETL"):
            self.derive()
        self.statistics_path.write_text(statistics(self.etl), encoding="utf-8")
        self.etl.write_bytes(b"replaced trace")
        with self.assertRaisesRegex(ClockError, "hash"):
            self.derive()

    def test_capture_pid_session_and_window_are_not_inferred_from_exit_codes(self):
        mutations = (
            lambda data: data.update(capture_valid=False),
            lambda data: data.update(own_start_succeeded=False),
            lambda data: data.update(session_state="cancelled"),
            lambda data: data["handshake_done"].update(pid=43),
            lambda data: data["handshake_ready"].update(qpc_frequency=4),
            lambda data: data["marker_begin"].update(text="Other_BEGIN"),
            lambda data: data["handshake_done"]["window"].update(qpc_elapsed_ticks=7),
            lambda data: data["handshake_done"]["window"].update(qpc_elapsed_ticks=6.0),
            lambda data: data["handshake_done"]["window"].update(qpc_frequency=True),
        )
        for mutation in mutations:
            data = deepcopy(self.capture)
            mutation(data)
            with self.subTest(data=data), self.assertRaises(ClockError):
                self.derive(data)
        data = deepcopy(self.capture)
        data["handshake_done"]["window"].update(qpc_start=1_000_000_030, qpc_end=1_000_000_036)
        with self.assertRaisesRegex(ClockError, "outside"):
            self.derive(data)

    def test_actual_unique_owned_nanosecond_marks_bracket_qpc_roi(self):
        text = ("Mark Type, TimeStamp, Label\nMark, 100000000, Owned_123_BEGIN\n"
                "Mark, 3000000000, Owned_123_END\nMark, 4000000000, Other_END\n")
        marks = self.directory / "marks.txt"
        marks.write_text(text, encoding="utf-16")
        result = self.derive(marks=marks)
        self.assertTrue(result["marks"]["actual_presence_verified"])
        self.assertEqual(result["marks"]["own_mark_rows"], 2)
        self.assertNotIn("Other_END", json.dumps(result))
        for invalid in (text.replace("Owned_123_END", "Other_END"),
                        text + "Mark, 200000000, Owned_123_BEGIN\n",
                        text.replace("3000000000", "3000000"),
                        "Mark Type, TimeStamp, Label\nMark, 3000000000, Owned_123_END\nMark, 100000000, Owned_123_BEGIN\n"):
            with self.subTest(text=invalid), self.assertRaises(ClockError):
                parse_marks(invalid, "Owned_123", Fraction(1, 3), Fraction(7, 3))


if __name__ == "__main__":
    unittest.main()
