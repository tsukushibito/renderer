"""Optional, child-process-only Windows CPU policy for controlled comparisons.

This is benchmark setup, never a change to Renderer shipping behavior or the
machine's power plan. API failures abort before the long warmup/measurement.
"""
import ctypes
from ctypes import wintypes
import os
import struct


def cpu_sets(payload):
    records = []
    offset = 0
    while offset < len(payload):
        if len(payload) - offset < 8:
            raise ValueError("Truncated CPU-set header")
        size, kind = struct.unpack_from("<II", payload, offset)
        if size < 8 or size % 4 or offset + size > len(payload):
            raise ValueError("Invalid variable CPU-set record size")
        if kind == 0:
            if size < 32:
                raise ValueError("Truncated CPU-set information")
            identifier, group, logical, core, cache, numa, efficiency, flags = struct.unpack_from(
                "<IHBBBBBB", payload, offset + 8)
            records.append({"id": identifier, "group": group, "logical": logical, "core": core,
                            "cache": cache, "numa": numa, "efficiency": efficiency, "flags": flags})
        offset += size
    if not records:
        raise ValueError("No supported CPU-set information")
    return records


def select_performance_cores(records, allowed):
    if any(record["group"] != 0 for record in records):
        raise ValueError("Controlled CPU policy currently requires one processor group")
    available = [record for record in records if allowed & (1 << record["logical"])]
    if not available:
        raise ValueError("No CPU sets available to this comparison")
    efficiency = max(record["efficiency"] for record in available)
    selected = [record for record in available if record["efficiency"] == efficiency
                and not (record["flags"] & 2 and not record["flags"] & 4)]
    if not selected:
        raise ValueError("Highest-performance CPU class is allocated to another process")
    mask = sum(1 << record["logical"] for record in selected)
    return {"mode": "performance-cores", "affinity_mask": mask,
            "selected_efficiency_class": efficiency, "selected_logical_processors": [r["logical"] for r in selected],
            "cpu_sets": records, "execution_speed_throttling": False,
            "scope": "Both target processes use the same highest available EfficiencyClass core group and explicit HighQoS. Controller and other applications are unchanged. This is a controlled CPU subset, not default-scheduler whole-machine performance."}


class WindowsCpuPolicy:
    def __init__(self):
        if os.name != "nt":
            raise ValueError("Controlled CPU policy requires Windows")
        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self.kernel.GetCurrentProcess.restype = wintypes.HANDLE
        self.kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        self.kernel.OpenProcess.restype = wintypes.HANDLE
        self.kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        self.kernel.GetSystemCpuSetInformation.argtypes = [ctypes.c_void_p, wintypes.ULONG,
            ctypes.POINTER(wintypes.ULONG), wintypes.HANDLE, wintypes.ULONG]
        self.kernel.GetProcessAffinityMask.argtypes = [wintypes.HANDLE, ctypes.POINTER(ctypes.c_size_t),
                                                       ctypes.POINTER(ctypes.c_size_t)]
        self.kernel.SetProcessAffinityMask.argtypes = [wintypes.HANDLE, ctypes.c_size_t]
        self.kernel.GetProcessInformation.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
        self.kernel.SetProcessInformation.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]

    @staticmethod
    def check(ok):
        if not ok:
            raise ctypes.WinError(ctypes.get_last_error())

    def affinity(self, process):
        allowed, system = ctypes.c_size_t(), ctypes.c_size_t()
        self.check(self.kernel.GetProcessAffinityMask(process, ctypes.byref(allowed), ctypes.byref(system)))
        return allowed.value, system.value

    def describe(self):
        process = self.kernel.GetCurrentProcess()
        length = wintypes.ULONG()
        self.kernel.GetSystemCpuSetInformation(None, 0, ctypes.byref(length), process, 0)
        if not length.value:
            raise ctypes.WinError(ctypes.get_last_error())
        buffer = ctypes.create_string_buffer(length.value)
        self.check(self.kernel.GetSystemCpuSetInformation(buffer, len(buffer), ctypes.byref(length), process, 0))
        allowed, system = self.affinity(process)
        selected = select_performance_cores(cpu_sets(buffer.raw[:length.value]), allowed)
        selected.update(controller_affinity_mask=allowed, system_affinity_mask=system)
        return selected

    def apply(self, pid, policy):
        # PROCESS_QUERY_INFORMATION | PROCESS_SET_INFORMATION on our own child.
        process = self.kernel.OpenProcess(0x600, False, pid)
        self.check(process)
        try:
            before, system = self.affinity(process)
            mask = policy["affinity_mask"]
            if mask & ~before:
                raise ValueError("Target did not inherit the CPU cores selected by the controller")
            self.check(self.kernel.SetProcessAffinityMask(process, mask))
            # ProcessPowerThrottling=4, CURRENT_VERSION=1. Control only execution
            # speed (mask 0x1); state 0 explicitly requests HighQoS.
            state = (wintypes.ULONG * 3)(1, 1, 0)
            self.check(self.kernel.SetProcessInformation(process, 4, state, ctypes.sizeof(state)))
            observed = (wintypes.ULONG * 3)(1, 0, 0)
            self.check(self.kernel.GetProcessInformation(process, 4, observed, ctypes.sizeof(observed)))
            after, _ = self.affinity(process)
            if after != mask or not observed[1] & 1 or observed[2] & 1:
                raise ValueError("Target CPU policy differs from the requested affinity/HighQoS")
            resumed = self.resume_initial_thread(pid)
            return {"mode": policy["mode"], "inherited_affinity_mask": before, "system_affinity_mask": system,
                    "observed_affinity_mask": after, "power_throttling": list(observed),
                    "applied_before_warmup": True, **resumed}
        finally:
            self.check(self.kernel.CloseHandle(process))

    def resume_initial_thread(self, pid):
        """Resume the sole thread of our freshly CREATE_SUSPENDED child.

        Popen closes CreateProcess's original thread handle. The documented
        Toolhelp snapshot obtains a new handle before any child user code runs.
        Unexpected extra threads fail closed; the caller kills the child.
        """
        class ThreadEntry(ctypes.Structure):
            _fields_ = [("size", wintypes.DWORD), ("usage", wintypes.DWORD),
                        ("thread_id", wintypes.DWORD), ("process_id", wintypes.DWORD),
                        ("base_priority", wintypes.LONG), ("delta_priority", wintypes.LONG),
                        ("flags", wintypes.DWORD)]
        kernel = self.kernel
        kernel.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
        kernel.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
        kernel.Thread32First.argtypes = [wintypes.HANDLE, ctypes.POINTER(ThreadEntry)]
        kernel.Thread32Next.argtypes = [wintypes.HANDLE, ctypes.POINTER(ThreadEntry)]
        kernel.OpenThread.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenThread.restype = wintypes.HANDLE
        kernel.ResumeThread.argtypes = [wintypes.HANDLE]
        kernel.ResumeThread.restype = wintypes.DWORD
        snapshot = kernel.CreateToolhelp32Snapshot(4, 0)  # TH32CS_SNAPTHREAD
        if snapshot == ctypes.c_void_p(-1).value:
            raise ctypes.WinError(ctypes.get_last_error())
        ids = []
        try:
            entry = ThreadEntry()
            entry.size = ctypes.sizeof(entry)
            self.check(kernel.Thread32First(snapshot, ctypes.byref(entry)))
            while True:
                if entry.process_id == pid:
                    ids.append(entry.thread_id)
                entry.size = ctypes.sizeof(entry)
                if not kernel.Thread32Next(snapshot, ctypes.byref(entry)):
                    if ctypes.get_last_error() != 18:  # ERROR_NO_MORE_FILES
                        raise ctypes.WinError(ctypes.get_last_error())
                    break
        finally:
            self.check(kernel.CloseHandle(snapshot))
        if len(ids) != 1:
            raise ValueError(f"Expected one suspended initial target thread, observed {len(ids)}")
        thread = kernel.OpenThread(2, False, ids[0])  # THREAD_SUSPEND_RESUME
        self.check(thread)
        try:
            previous = kernel.ResumeThread(thread)
            if previous == 0xffffffff:
                raise ctypes.WinError(ctypes.get_last_error())
            if previous != 1:
                raise ValueError("Target did not start with exactly one explicit suspension")
            return {"initial_thread_id": ids[0], "resume_previous_suspend_count": previous}
        finally:
            self.check(kernel.CloseHandle(thread))


def plan(mode):
    if mode == "system":
        return {"mode": mode, "scope": "Inherited Windows scheduler/affinity/QoS; no process CPU policy changes."}
    if mode != "performance-cores":
        raise ValueError("Unknown CPU policy")
    return WindowsCpuPolicy().describe()


def apply(pid, policy):
    if policy["mode"] == "system":
        return {"mode": "system", "applied": False}
    return WindowsCpuPolicy().apply(pid, policy)
