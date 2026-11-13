import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from summarize import require_successful_checks


class SummaryPublicationTests(unittest.TestCase):
    def test_successful_checks_accept_complete_validator_evidence(self):
        require_successful_checks({"passed": True}, {"ok": True}, {"passed": True},
                                  {"issues": {"numErrors": 0, "truncated": False}}, {"passed": True})

    def test_failed_checks_exit_nonzero_without_replacing_published_evidence(self):
        reports = {"source-bindings": {"passed": True}, "geometry": {"ok": True},
                   "materials": {"passed": True}, "reproduction": {"passed": True},
                   "validator": {"issues": {"numErrors": 0, "truncated": False}}}
        failures = (("source-bindings", {"passed": False}), ("geometry", {"ok": False}),
                    ("materials", {"passed": False}), ("reproduction", {"passed": False}),
                    ("validator", {"issues": {"numErrors": 1, "truncated": False}}),
                    ("validator", {"issues": {"numErrors": 0, "truncated": True}}))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "published.json"
            original = b'{"previous":"accepted evidence"}\n'
            for name, failure in failures:
                with self.subTest(report=name, failure=failure):
                    data = copy.deepcopy(reports)
                    data[name] = failure
                    output.write_bytes(original)
                    command = [sys.executable, "-B", str(Path(__file__).with_name("summarize.py")),
                               str(root / "work"), str(root / "export"), str(output)]
                    for report in ("source-bindings", "geometry", "materials", "validator", "reproduction",
                                   "preview", "views", "alpha", "conditions"):
                        path = root / f"{report}.json"
                        path.write_text(json.dumps(data.get(report, {})), encoding="utf-8")
                        command.extend((f"--{report}-report", str(path)))
                    result = subprocess.run(command, capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("Cannot publish failed", result.stderr)
                    self.assertEqual(output.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
