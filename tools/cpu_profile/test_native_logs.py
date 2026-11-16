"""Only native metadata subprocess boundaries; no WPR, GPU or trace parsing."""

import io
import subprocess
import sys
from types import SimpleNamespace
import unittest

from native_logs import run_bounded


class NativeLogContracts(unittest.TestCase):
    def test_success_retains_both_streams_and_native_exit_code(self):
        stdout, stderr = io.BytesIO(), io.BytesIO()
        code = run_bounded([sys.executable, "-c", "import sys; print('out'); print('err',file=sys.stderr); sys.exit(3)"],
                           stdout, stderr, 5, 1024)
        self.assertEqual(code, 3)
        self.assertEqual(stdout.getvalue().strip(), b"out")
        self.assertEqual(stderr.getvalue().strip(), b"err")

    def test_combined_byte_budget_retains_only_bounded_prefix_and_stops_child(self):
        stdout, stderr, launched = io.BytesIO(), io.BytesIO(), []
        def launch(*args, **kwargs):
            process = subprocess.Popen(*args, **kwargs)
            launched.append(process)
            return process
        with self.assertRaisesRegex(RuntimeError, "bounded prefix"):
            run_bounded([sys.executable, "-c", "import sys; sys.stdout.buffer.write(b'x'*10000); sys.stderr.buffer.write(b'y'*10000)"],
                        stdout, stderr, 5, 1024, launcher=launch)
        self.assertEqual(len(stdout.getvalue()) + len(stderr.getvalue()), 1024)
        self.assertIsNotNone(launched[0].poll())

    def test_timeout_uses_injected_clock_and_stops_owned_process(self):
        elapsed, terminated = [0.0], []
        process = SimpleNamespace(stdout=io.BytesIO(), stderr=io.BytesIO(), code=None)
        process.poll = lambda: process.code
        def terminate():
            terminated.append(True)
            process.code = -1
        process.terminate = terminate
        process.wait = lambda timeout: process.code
        def sleep(duration):
            elapsed[0] += duration
        with self.assertRaises(subprocess.TimeoutExpired):
            run_bounded(["fixture"], io.BytesIO(), io.BytesIO(), 0.1, 1024,
                        launcher=lambda *args, **kwargs: process, monotonic=lambda: elapsed[0], sleeper=sleep)
        self.assertEqual(terminated, [True])


if __name__ == "__main__":
    unittest.main()
