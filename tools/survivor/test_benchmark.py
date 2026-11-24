import csv
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

import benchmark


class ReportTests(unittest.TestCase):
    def setUp(self):
        self.temp = TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "frames.csv"

    def write(self, active=500, graph="1.25", frames=(121, 122, 123)):
        fields = ("frame", *benchmark.CPU, *benchmark.COUNTS, *benchmark.GPU,
                  *benchmark.NATIVE_CPU, "tick")
        with self.path.open("w", newline="", encoding="utf-8") as stream:
            writer = csv.writer(stream)
            writer.writerow(fields)
            for index, frame in enumerate(frames):
                writer.writerow([frame, 8 + index, .2, .4, active, 120, 120, 32, 128,
                                 graph, ".5" if graph else "", *([graph] * 4), 121 + index])

    def test_measured_segment_and_gpu_intervals(self):
        self.write()
        report = benchmark.summarize(self.path, 3, True, "endgame")
        self.assertEqual(report["counts"]["active"]["min"], 500)
        self.assertAlmostEqual(report["timings"]["interval_ms"]["p99"], 9.98)
        self.assertEqual(report["timings"]["graph_gpu_ms"]["median"], 1.25)
        with self.assertRaisesRegex(ValueError, "unprofiled"):
            benchmark.summarize(self.path, 3, False, "endgame")

    def test_missing_or_underloaded_samples(self):
        self.write(active=499, graph="")
        with self.assertRaisesRegex(ValueError, "500 live"):
            benchmark.summarize(self.path, 3, False, "endgame")
        self.write(graph="", frames=(121, 122, 124))
        with self.assertRaisesRegex(ValueError, "nonconsecutive"):
            benchmark.summarize(self.path, 3, False, "endgame")


if __name__ == "__main__":
    unittest.main()
