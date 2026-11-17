# Optional Godot CPU boundary clock

This standalone Windows x86_64 GDExtension is used only by the Bistro benchmark.
It is not linked into Renderer or an ordinary Godot export. It installs no
per-frame callback, thread, engine patch, polling loop, or GPU resource.

The extension accepts only the official standard Godot **4.7.2** build, commit
`ed1daf0bf001b61586d9930840f2f1394092c079`. It uses the official generated C ABI
header and the Windows APIs directly, without godot-cpp or another binding.
The vendored MIT license, source hashes, generated header, editor identity and
verified builtin storage sizes are recorded in `vendor/provenance.json`.

```gdscript
var status := GDExtensionManager.load_extension(absolute_descriptor_path)
assert(status == GDExtensionManager.LOAD_STATUS_OK)
var boundary = ClassDB.class_call_static("B3CpuClock", "snapshot")
```

The result is a seven-element `PackedInt64Array`:

| Index | Value |
|---|---|
| 0 | QueryPerformanceCounter ticks |
| 1 | QueryPerformanceFrequency ticks per second |
| 2, 3 | Process kernel and user CPU, in 100 ns units |
| 4, 5 | Calling thread kernel and user CPU, in 100 ns units |
| 6 | Calling Windows thread ID |

Capture order is QPC, frequency, process CPU, calling-thread CPU, thread ID.
These are sequential observations, not an atomic sample. Process CPU sums all
threads, including rendering workers and driver work charged to this process.
Calling-thread CPU covers only the caller. The unit of the CPU counters does
not establish their resolution. A difference divided by frame count is an
aggregate mean; it cannot provide a per-frame percentile or wait duration.

A negative index 0 indicates failure, with a Win32 error code at index 1. Stages
are `-1` QPC, `-2` frequency, `-3` invalid clock values, `-4` process CPU and
`-5` thread CPU. Godot also emits an error, so a failed capture cannot silently
be accepted as performance evidence. Array construction errors emit an error
and yield an invalid/partial array that the report validator must reject.

The caller takes two snapshots at defined ROI boundaries. Packing each return
array can allocate; the first boundary's return packaging and caller overhead
fall inside the measured interval, while packaging after the final CPU capture
falls outside. This asymmetric boundary overhead is retained and documented,
not subtracted as an assumed constant. No such call belongs inside the ordinary
frame loop. Use timing-disabled/enabled independent runs to observe other
instrumentation costs separately.

Build offline with Python and installed MSVC x64 build tools:

```powershell
python -B tools/godot_bistro/cpu_clock/build.py --output .tools/cpu-clock-build
python -B tools/godot_bistro/cpu_clock/smoke.py --editor C:/path/to/Godot_v4.7.2-stable_win64_console.exe --manifest .tools/cpu-clock-build/manifest.json --output .tools/cpu-clock-smoke
```

Both commands require the shared B3 exclusive-work lock to be held by the
caller. Build output must be new. `manifest.json` records compiler/source/ABI
identity and the SHA-256 and byte length of the DLL and descriptor. Copy those
two files together without rewriting the descriptor: its relative DLL path is
resolved against its own directory, including when loaded by an absolute path
outside the Godot project. `smoke.py` verifies this loading mode, live CPU
counter changes during work, monotonic counters, stable thread/frequency, and
a separate sleep elapsed-time control.

To regenerate the official header, run the pinned editor in a fresh output
directory with `--headless --dump-gdextension-interface` under that same lock.
Run `--headless --dump-extension-api` separately and verify the standard
`float_64` builtin sizes (single precision, 64-bit pointers): Variant 24,
StringName 8 and String 8 bytes. Record the
editor SHA-256, full engine hash, generated header hash, API JSON hash and
upstream JSON/generator hashes in `vendor/provenance.json`. No regeneration or
network access is needed for an ordinary build.

Primary contracts:

- [Pinned Godot interface definitions](https://github.com/godotengine/godot/blob/ed1daf0bf001b61586d9930840f2f1394092c079/core/extension/gdextension_interface.json)
- [Pinned dynamic library path resolution](https://github.com/godotengine/godot/blob/ed1daf0bf001b61586d9930840f2f1394092c079/core/extension/gdextension_library_loader.cpp#L90-L97)
- [GetProcessTimes](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocesstimes)
- [GetThreadTimes](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getthreadtimes)
