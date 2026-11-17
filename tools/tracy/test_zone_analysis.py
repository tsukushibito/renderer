"""Raw export identity, nested elapsed-time arithmetic and percentile contracts."""
import csv
from pathlib import Path
import tempfile
import tracemalloc
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

    def test_independent_source_enumeration_and_absent_frame_calls_keep_exact_percentiles(self):
        del self.inclusive[5]
        del self.self_rows[5]
        self.self_rows[3][4] = 200
        self.self_rows.reverse()
        result = self.analyze()
        phase = next(row for row in result["zones"] if row["name"] == "phase")
        self.assertEqual(phase["calls_per_frame"], .5)
        self.assertEqual(phase["per_frame_aggregation"]["inclusive_p50_ns"], 20)
        self.assertEqual(phase["per_frame_aggregation"]["inclusive_p95_ns"], 38)

    def test_equal_interval_ancestors_do_not_invent_the_immediate_parent_of_shorter_children(self):
        self.inclusive = [zone("bistro.capture_identity", 0, 1, value=NONCE),
                          zone("bistro.measurement", 100, 1000), zone("bistro.frame", 100, 100),
                          zone("B", 100, 100), zone("A", 100, 100),
                          zone("C", 100, 50), zone("D", 150, 50),
                          zone("bistro.frame", 300, 200), zone("bistro.final_drain", 900, 100)]
        self.self_rows = [row.copy() for row in self.inclusive]
        for row, duration in zip(self.self_rows, (1, 600, 0, 0, 0, 50, 50, 200, 100)):
            row[4] = duration
        result = self.analyze()
        self.assertGreater(result["ambiguous_equal_interval_parent_edges"], 0)
        self.assertFalse(any(edge["child"]["name"] in ("C", "D") for edge in result["parent_edges"]))
        self.assertEqual(sum(row["self_ns"] for row in result["zones"]), 1000)

    def test_large_generated_route_keeps_event_memory_bounded_and_all_frame_costs(self):
        frames, draws, sites, frame_ns = 1800, 64, 16, 256
        paths = []
        for exclusive in (False, True):
            path = self.directory / ("large-self.tsv" if exclusive else "large-inclusive.tsv")
            paths.append(path)
            with path.open("w", encoding="utf-8", newline="") as stream:
                writer = csv.writer(stream, delimiter="\t", quoting=csv.QUOTE_NONE)
                writer.writerow(HEADER)
                writer.writerow(zone("bistro.capture_identity", 0, 1, value=NONCE))
                writer.writerow(zone("bistro.measurement", 100, frames * (1000 - frame_ns) if exclusive else frames * 1000))
                for frame in range(frames):
                    start = 100 + frame * 1000
                    writer.writerow(zone("bistro.frame", start, frame_ns - draws if exclusive else frame_ns))
                    for draw in range(draws):
                        writer.writerow(zone("draw." + str(draw % sites), start + draw * 2, 1, line=draw % sites + 2))
        tracemalloc.start()
        try:
            result = analyze_zones(*paths, frames, NONCE)
            _current, peak = tracemalloc.get_traced_memory()
        finally:
            tracemalloc.stop()
        self.assertEqual(result["measured_frame_zones"], frames)
        self.assertEqual(result["cpu_zone_events"], frames * (draws + 1) + 1)
        self.assertEqual(result["frame_zone_elapsed"]["p95_ns"], frame_ns)
        self.assertEqual(sum(row["self_ns"] for row in result["zones"]), frames * 1000)
        for row in result["zones"]:
            if row["name"].startswith("draw."):
                self.assertEqual(row["calls"], frames * draws // sites)
                self.assertEqual(row["per_frame_aggregation"]["inclusive_p95_ns"], draws // sites)
        # This checks the event-memory regression, not a wall-clock benchmark.
        # Original full dict/counter/copy storage exceeds this by many times.
        self.assertLess(peak, 16 * 1024 * 1024, "Raw event count leaked into Python retained memory")
        self.assertLessEqual(result["analysis_storage"]["peak_event_batch_rows"], 2048)
        self.assertLessEqual(result["analysis_storage"]["peak_source_cache_entries"], 512)
        self.assertEqual(list(self.directory.glob(".tracy-analysis-*")), [])

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
