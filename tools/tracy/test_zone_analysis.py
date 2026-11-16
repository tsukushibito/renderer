"""Raw export identity, nested elapsed-time arithmetic and percentile contracts."""
import csv
from pathlib import Path
import tempfile
import unittest

from zone_analysis import HEADER, analyze_zones

NONCE = "a" * 32


def zone(name, start, duration, *, line=1, value="", thread=7):
    return [name, "bistro.rs", line, start, duration, thread, value]


class ZoneTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.inclusive = [zone("bistro.capture_identity", 0, 1, value=NONCE),
                          zone("bistro.measurement", 100, 1000),
                          zone("bistro.frame", 100, 100), zone("bistro.frame", 300, 200),
                          zone("phase", 110, 40, line=2), zone("phase", 320, 120, line=2),
                          zone("bistro.final_drain", 900, 100)]
        self.self_rows = [row.copy() for row in self.inclusive]
        for row, duration in zip(self.self_rows, (1, 600, 60, 80, 40, 120, 100)):
            row[4] = duration

    def write(self, name, rows):
        path = self.directory / name
        with path.open("w", encoding="utf-8", newline="") as stream:
            writer = csv.writer(stream, delimiter="\t", quoting=csv.QUOTE_NONE, escapechar="\\")
            writer.writerow(HEADER)
            writer.writerows(rows)
        return path

    def analyze(self, frames=2, nonce=NONCE):
        return analyze_zones(self.write("inclusive.tsv", self.inclusive), self.write("self.tsv", self.self_rows), frames, nonce)

    def test_nested_inclusive_is_not_summed_as_cpu_and_raw_frame_percentiles_are_explicit(self):
        result = self.analyze()
        self.assertEqual(result["roi"]["elapsed_ns"], 1000)
        self.assertEqual(result["measured_frame_zones"], 2)
        self.assertEqual(result["cpu_threads_with_zones"], 1)
        self.assertEqual(result["frame_zone_elapsed"], {"samples": 2, "mean_ns": 150, "p50_ns": 150, "p95_ns": 195})
        zones = {row["name"]: row for row in result["zones"]}
        self.assertEqual(sum(row["self_ns"] for row in result["zones"]), 1000)
        self.assertGreater(sum(row["inclusive_ns"] for row in result["zones"]), 1000)
        self.assertEqual(zones["phase"]["calls_per_frame"], 1)
        self.assertEqual(zones["phase"]["inclusive_ns_per_frame"], 80)
        self.assertEqual(zones["phase"]["per_frame_aggregation"]["inclusive_p95_ns"], 116)
        self.assertIsNone(zones["bistro.final_drain"]["per_frame_aggregation"])
        self.assertTrue(result["parent_edges"])

    def test_missing_frames_and_wrong_nonce_reject_foreign_or_partial_trace(self):
        with self.assertRaisesRegex(ValueError, "Expected 3"):
            self.analyze(frames=3)
        with self.assertRaisesRegex(ValueError, "capture nonce"):
            self.analyze(nonce="b" * 32)
        self.inclusive[0][6] = ""
        self.self_rows[0][6] = ""
        with self.assertRaisesRegex(ValueError, "capture nonce"):
            self.analyze()

    def test_mismatched_exports_and_inconsistent_self_do_not_hide_missing_work(self):
        self.self_rows[4][3] += 1
        with self.assertRaisesRegex(ValueError, "different zone instances"):
            self.analyze()
        self.self_rows[4][3] -= 1
        self.self_rows[2][4] += 1
        with self.assertRaisesRegex(ValueError, "incomplete or inconsistent"):
            self.analyze()

    def test_recursive_same_source_same_start_preserves_export_instance_order(self):
        self.inclusive[4][4] = 60
        self.self_rows[2][4] = 40
        self.self_rows[4][4] = 40
        self.inclusive.insert(5, zone("phase", 110, 20, line=2))
        self.self_rows.insert(5, zone("phase", 110, 20, line=2))
        result = self.analyze()
        phase = next(row for row in result["zones"] if row["name"] == "phase")
        self.assertEqual(phase["calls"], 3)
        self.assertEqual(phase["self_ns"], 180)

    def test_crossed_zones_and_noncanonical_negative_durations_fail(self):
        self.inclusive[4][4] = 150
        self.self_rows[4][4] = 150
        with self.assertRaisesRegex(ValueError, "cross|inconsistent"):
            self.analyze()
        self.inclusive[4][4] = -1
        with self.assertRaisesRegex(ValueError, "nonnegative"):
            self.analyze()


if __name__ == "__main__":
    unittest.main()
