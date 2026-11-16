"""CPU-only Windows process tests: private SIGINT flush and abrupt broker death."""
import ctypes
from ctypes import wintypes
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

from windows import hidden_console, token_elevation


@unittest.skipUnless(sys.platform == "win32", "Windows console/job integration contract")
class WindowsBrokerTests(unittest.TestCase):
    def setUp(self):
        self.assertEqual(token_elevation()["token_elevation"], 0, "Run non-elevated; do not change UAC")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)

    def broker(self, flush_code):
        code = "import signal,time,sys\nsignal.signal(signal.SIGINT," + flush_code + ")\nprint('FAKE_READY',flush=True)\nwhile True: time.sleep(.1)"
        specification = {"command": [sys.executable, "-c", code], "cwd": str(self.directory),
                         "stdout": str(self.directory / "collector.stdout.txt"), "stderr": str(self.directory / "collector.stderr.txt"),
                         "flush_timeout_seconds": 30}
        path = self.directory / "specification.json"
        path.write_text(json.dumps(specification), encoding="utf-8")
        stderr = (self.directory / "broker.stderr.txt").open("wb")
        self.addCleanup(stderr.close)
        process = subprocess.Popen([sys.executable, str(Path(__file__).with_name("windows.py")), str(path)],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, text=True, **hidden_console())
        def cleanup():
            if process.poll() is None:
                process.kill()
            process.wait(timeout=10)
            process.stdin.close()
            process.stdout.close()
        self.addCleanup(cleanup)
        line = process.stdout.readline()
        self.assertTrue(line.startswith("BROKER_READY "), line + (self.directory / "broker.stderr.txt").read_text())
        ready = json.loads(line.removeprefix("BROKER_READY "))
        self.assertTrue(ready["kill_on_job_close"])
        deadline = time.monotonic() + 10
        while "FAKE_READY" not in (self.directory / "collector.stdout.txt").read_text(encoding="utf-8"):
            self.assertLess(time.monotonic(), deadline, "Fake collector did not establish its signal handler")
            time.sleep(.02)
        return process, ready

    def test_stop_flushes_only_private_console_and_exits_normally(self):
        process, ready = self.broker("lambda *args: sys.exit(0)")
        process.stdin.write("STOP\n")
        process.stdin.flush()
        done = json.loads(process.stdout.readline().removeprefix("BROKER_DONE "))
        self.assertEqual(done["returncode"], 0)
        self.assertEqual(process.wait(timeout=10), 0)
        self.assertEqual(ready["capture"]["token_elevation"], 0)
        self.assertEqual(ready["broker"]["token_elevation"], 0)

    def test_abrupt_broker_kill_during_flush_reaps_owned_collector(self):
        process, ready = self.broker("lambda *args: print('FLUSHING',flush=True)")
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        capture = kernel.OpenProcess(0x00100000, False, ready["capture"]["pid"])
        self.assertTrue(capture)
        try:
            process.stdin.write("STOP\n")
            process.stdin.flush()
            deadline = time.monotonic() + 10
            while "FLUSHING" not in (self.directory / "collector.stdout.txt").read_text(encoding="utf-8"):
                self.assertLess(time.monotonic(), deadline, "Owned collector did not enter slow flush")
                time.sleep(.02)
            process.kill()
            process.wait(timeout=10)
            self.assertEqual(kernel.WaitForSingleObject(capture, 5000), 0, "Owned collector survived abrupt broker death")
        finally:
            kernel.CloseHandle(capture)


if __name__ == "__main__":
    unittest.main()
