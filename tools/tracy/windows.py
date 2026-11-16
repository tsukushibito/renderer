"""Own-process token evidence and a private, hidden capture console.

Tracy 0.14.1 capture handles SIGINT, not SIGBREAK. Its broker owns a separate
console so CTRL_C_EVENT can flush only that capture, without signalling the
workload, Codex, or another capture. No elevation or OS configuration changes.
"""
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import sys


def token_elevation(pid=None, *, handle=None):
    if sys.platform != "win32":
        return {"pid": pid or os.getpid(), "supported": False}
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    advapi = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    advapi.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(wintypes.HANDLE)]
    advapi.GetTokenInformation.argtypes = [wintypes.HANDLE, ctypes.c_int, wintypes.LPVOID,
                                         wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
    process = handle if handle is not None else kernel.OpenProcess(0x1000, False, pid or os.getpid())
    if not process:
        raise ctypes.WinError(ctypes.get_last_error())
    token = wintypes.HANDLE()
    try:
        if not advapi.OpenProcessToken(process, 0x0008, ctypes.byref(token)):
            raise ctypes.WinError(ctypes.get_last_error())
        elevation, returned = wintypes.DWORD(), wintypes.DWORD()
        if not advapi.GetTokenInformation(token, 20, ctypes.byref(elevation), ctypes.sizeof(elevation),
                                         ctypes.byref(returned)):
            raise ctypes.WinError(ctypes.get_last_error())
        return {"pid": pid or os.getpid(), "supported": True, "token_elevation": elevation.value}
    finally:
        if token:
            kernel.CloseHandle(token)
        if handle is None:
            kernel.CloseHandle(process)


def require_nonadmin(pid=None, *, handle=None):
    evidence = token_elevation(pid, handle=handle)
    if evidence.get("token_elevation") != 0:
        raise ValueError("Non-admin capture requires verified Windows TokenElevation=0")
    return evidence


def owned_listener(pid, port=8086):
    """Require the exact loopback listener to belong to our launched workload."""
    if sys.platform != "win32":
        raise ValueError("TCP owner evidence requires Windows")
    iphlp = ctypes.WinDLL("iphlpapi", use_last_error=True)
    iphlp.GetExtendedTcpTable.argtypes = [wintypes.LPVOID, ctypes.POINTER(wintypes.DWORD),
                                        wintypes.BOOL, wintypes.ULONG, ctypes.c_int, wintypes.ULONG]
    size = wintypes.DWORD()
    code = iphlp.GetExtendedTcpTable(None, ctypes.byref(size), False, 2, 3, 0)
    if code not in (0, 122):
        raise OSError(code, "GetExtendedTcpTable sizing failed")
    # The system-wide table can grow between calls. Bounded retries are setup
    # evidence only; they never touch another process or open another listener.
    for _attempt in range(5):
        data = ctypes.create_string_buffer(size.value)
        code = iphlp.GetExtendedTcpTable(data, ctypes.byref(size), False, 2, 3, 0)
        if code == 122:
            continue
        if code != 0:
            raise OSError(code, "GetExtendedTcpTable failed")
        count, = struct.unpack_from("<I", data.raw)
        if 4 + count * 24 > len(data):
            raise ValueError("Truncated Windows owner TCP table")
        found = []
        for index in range(count):
            state, address, raw_port, _remote, _remote_port, owner = struct.unpack_from("<6I", data.raw, 4 + index * 24)
            if socket.ntohs(raw_port & 0xffff) == port:
                found.append({"pid": owner, "address": socket.inet_ntoa(struct.pack("<I", address)),
                              "port": port, "state": state})
        if found != [{"pid": pid, "address": "127.0.0.1", "port": port, "state": 2}]:
            raise ValueError(f"Tracy localhost listener is not exclusively owned by target {pid}: {found}")
        return found[0]
    raise ValueError("Windows TCP listener table changed repeatedly during ownership verification")


def hidden_console():
    startup = subprocess.STARTUPINFO()
    startup.dwFlags |= subprocess.STARTF_USESHOWWINDOW
    startup.wShowWindow = 0
    return {"creationflags": subprocess.CREATE_NEW_CONSOLE, "startupinfo": startup}


def own_process_job():
    """Put this broker and descendants in a private kill-on-close job.

    The only job handle is non-inheritable and retained until broker process
    exit. Thus even TerminateProcess on the broker kills its capture, including
    during a slow flush. Assign before launching any child to avoid an orphan
    window between child creation and assignment. Modern Windows permits this
    nested within a host job; failure is fatal before launching the collector.
    """
    class Basic(ctypes.Structure):
        _fields_ = [("process_time", ctypes.c_longlong), ("job_time", ctypes.c_longlong),
                    ("flags", wintypes.DWORD), ("min_working", ctypes.c_size_t),
                    ("max_working", ctypes.c_size_t), ("active_processes", wintypes.DWORD),
                    ("affinity", ctypes.c_size_t), ("priority", wintypes.DWORD), ("scheduling", wintypes.DWORD)]

    class Extended(ctypes.Structure):
        _fields_ = [("basic", Basic), ("io", ctypes.c_ulonglong * 6),
                    ("process_memory", ctypes.c_size_t), ("job_memory", ctypes.c_size_t),
                    ("peak_process_memory", ctypes.c_size_t), ("peak_job_memory", ctypes.c_size_t)]

    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.CreateJobObjectW.argtypes = [wintypes.LPVOID, wintypes.LPCWSTR]
    kernel.CreateJobObjectW.restype = wintypes.HANDLE
    kernel.GetCurrentProcess.restype = wintypes.HANDLE
    kernel.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, wintypes.LPVOID, wintypes.DWORD]
    kernel.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    job = kernel.CreateJobObjectW(None, None)
    if not job:
        raise ctypes.WinError(ctypes.get_last_error())
    limits = Extended()
    limits.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    if (not kernel.SetInformationJobObject(job, 9, ctypes.byref(limits), ctypes.sizeof(limits))
            or not kernel.AssignProcessToJobObject(job, kernel.GetCurrentProcess())):
        error = ctypes.get_last_error()
        kernel.CloseHandle(job)
        raise ctypes.WinError(error)
    return job  # Intentionally not closed by Python: the OS closes it at exit.


def console_broker(specification):
    """Internal subprocess entry point; STOP means flush, EOF means abort."""
    if sys.platform != "win32":
        raise ValueError("The private console broker requires Windows")
    spec = json.loads(Path(specification).read_text(encoding="utf-8"))
    job = own_process_job()
    # This is a handler, not SIG_IGN: Windows' inheritable ignore-CTRL-C bit
    # would otherwise prevent capture's C-runtime SIGINT handler from running.
    signal.signal(signal.SIGINT, lambda _signal, _frame: None)
    process = None
    try:
        with Path(spec["stdout"]).open("wb") as stdout, Path(spec["stderr"]).open("wb") as stderr:
            process = subprocess.Popen(spec["command"], stdin=subprocess.DEVNULL,
                                       stdout=stdout, stderr=stderr, cwd=spec["cwd"])
            evidence = require_nonadmin(process.pid, handle=process._handle)
            print("BROKER_READY " + json.dumps({"pid": os.getpid(), "capture": evidence,
                                               "broker": require_nonadmin(), "kill_on_job_close": bool(job)}), flush=True)
            if sys.stdin.readline().strip() != "STOP":
                raise ValueError("Capture broker requires STOP; controller closed or aborted")
            if process.poll() is not None:
                raise RuntimeError("Capture exited before its owned stop request")
            kernel = ctypes.WinDLL("kernel32", use_last_error=True)
            # group 0 is precisely this broker's dedicated console. Neither the
            # controller nor workload was launched into this console.
            if not kernel.GenerateConsoleCtrlEvent(0, 0):
                raise ctypes.WinError(ctypes.get_last_error())
            code = process.wait(timeout=spec["flush_timeout_seconds"])
            print("BROKER_DONE " + json.dumps({"pid": os.getpid(), "returncode": code}), flush=True)
            return code
    finally:
        if process is not None and process.poll() is None:
            process.kill()
            process.wait(timeout=10)


if __name__ == "__main__":
    raise SystemExit(console_broker(sys.argv[1]))
