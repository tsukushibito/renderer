"""CPU policy selection and real owned-child setup, without GPU work."""
import os
import struct
import subprocess
import sys
import unittest

import host_policy as host


def record(logical, efficiency, *, group=0, flags=0, size=32):
    value = bytearray(size)
    struct.pack_into("<IIIHBBBBBB", value, 0, size, 0, 100 + logical, group,
                     logical, logical // 2, 0, 0, efficiency, flags)
    return bytes(value)


class CpuPolicyTests(unittest.TestCase):
    def test_variable_records_and_highest_available_class_are_respected(self):
        parsed = host.cpu_sets(record(0, 1, size=40) + record(1, 1) + record(2, 0) + record(3, 0))
        policy = host.select_performance_cores(parsed, 0b1111)
        self.assertEqual(policy["affinity_mask"], 0b0011)
        self.assertEqual(policy["selected_logical_processors"], [0, 1])
        self.assertEqual(host.select_performance_cores(parsed, 0b1100)["affinity_mask"], 0b1100)
        self.assertEqual(host.plan("system")["mode"], "system")

    def test_unavailable_allocations_invalid_records_and_multiple_groups_fail(self):
        parsed = host.cpu_sets(record(0, 1, flags=2) + record(1, 1, flags=6) + record(2, 0))
        self.assertEqual(host.select_performance_cores(parsed, 7)["affinity_mask"], 2)
        for value in (b"", b"123", struct.pack("<II", 0, 0), record(0, 1)[:-1]):
            with self.subTest(value=value), self.assertRaises(ValueError):
                host.cpu_sets(value)
        with self.assertRaises(ValueError):
            host.select_performance_cores(parsed, 0)
        with self.assertRaisesRegex(ValueError, "allocated"):
            host.select_performance_cores(host.cpu_sets(record(0, 1, flags=2) + record(1, 0)), 3)
        with self.assertRaises(ValueError):
            host.select_performance_cores(host.cpu_sets(record(0, 1, group=1)), 1)

    @unittest.skipUnless(os.name == "nt", "Windows child-process CPU policy")
    def test_native_policy_is_applied_and_observed_only_on_owned_child(self):
        policy = host.plan("performance-cores")
        with subprocess.Popen([sys.executable, "-c", "import time; time.sleep(5)"],
                              creationflags=subprocess.CREATE_NO_WINDOW | 4) as process:
            try:
                result = host.apply(process.pid, policy)
                self.assertEqual(result["observed_affinity_mask"], policy["affinity_mask"])
                self.assertEqual(result["power_throttling"][1] & 1, 1)
                self.assertEqual(result["power_throttling"][2] & 1, 0)
                self.assertEqual(result["resume_previous_suspend_count"], 1)
                self.assertIsNone(process.poll())
            finally:
                process.terminate()
                process.wait()


if __name__ == "__main__":
    unittest.main()
