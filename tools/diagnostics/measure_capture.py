"""Compare four capture modes on the same fixed Survivor workload and native counters."""

import argparse
import csv
import json
from pathlib import Path
import queue
import statistics
import subprocess
import threading
import time


def measure(exe, backend, root, mode, frames):
    run = root / mode
    run.mkdir()
    config = run / "config.json"
    config.write_text('{"jobs":{"parallelism":2}}', encoding="utf8")
    reports = run / "reports"
    metrics = run / "metrics.csv"
    command = [
        str(exe),
        "--backend",
        backend,
        "--config",
        str(config),
        "--frames",
        str(frames),
        "--warmup",
        "15",
        "--width",
        "320",
        "--height",
        "180",
        "--metrics",
        str(metrics),
    ]
    if mode != "off":
        command += ["--debug-dir", str(reports), "--inspect-stdio"]
    if mode == "history":
        command.append("--clip-history")
    with subprocess.Popen(
        command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, encoding="utf8", cwd=run
    ) as game:
        try:
            if mode != "off":
                responses = queue.Queue()

                def consume():
                    for line in game.stdout:
                        responses.put(json.loads(line))
                    responses.put(None)

                reader = threading.Thread(target=consume, daemon=True)
                reader.start()
                sequence = 0

                def call(op, **params):
                    nonlocal sequence
                    sequence += 1
                    game.stdin.write(json.dumps(dict(id=str(sequence), op=op, **params)) + "\n")
                    game.stdin.flush()
                    response = responses.get(timeout=90)
                    if not response or not response["ok"] or response["id"] != str(sequence):
                        raise RuntimeError(response)
                    return response["result"]

                def reach(n):
                    deadline = time.monotonic() + 90
                    while int(call("hello")["displayed"]["frame"] or 0) < n:
                        if time.monotonic() > deadline:
                            raise TimeoutError(f"frame {n}")
                        time.sleep(0.02)

                reach(16)
                if mode == "video":
                    call(
                        "capture",
                        request={"action": "start_video", "extent": [320, 180], "fps": 30},
                    )
                reach(frames // 2)
                if mode in ("still", "history"):
                    call("capture", request={"action": "clip" if mode == "history" else "report"})
                if mode == "video":
                    reach(frames - 5)
                    call("capture", request={"action": "stop_video"})
                game.stdin.close()
            if game.wait(timeout=180):
                raise RuntimeError(f"{mode} returned {game.returncode}")
        finally:
            if game.poll() is None:
                game.kill()
                game.wait()
    with metrics.open(encoding="utf8", newline="") as file:
        rows = list(csv.DictReader(file))
    if len(rows) != frames:
        raise ValueError("incomplete measurement")
    times = sorted(float(r["interval_ms"]) for r in rows)
    total = {
        key: sum(int(r[key]) for r in rows)
        for key in (
            "alloc",
            "alloc_zeroed",
            "realloc",
            "free",
            "object_create",
            "object_destroy",
            "resource_create",
            "resource_destroy",
            "memory_allocate",
            "memory_free",
        )
    }
    found = []
    for path in sorted(reports.glob("*/manifest.json")):
        m = json.loads(path.read_bytes())
        if m["outcome"]["state"] not in ("complete", "partial"):
            raise ValueError((path, m["outcome"]))
        count = len(m["frames"])
        if "frame_map" in m["files"]:
            count = len((path.parent / m["files"]["frame_map"]).read_bytes().splitlines())
        found.append({"frames": count, "outcome": m["outcome"]["state"], "gaps": m["gaps"]})
    if mode != "off" and not found:
        raise ValueError(f"{mode} produced no reports")
    return {
        "mode": mode,
        "frames": frames,
        "median_ms": statistics.median(times),
        "p95_ms": times[(95 * frames + 99) // 100 - 1],
        "max_ms": times[-1],
        "calls": total,
        "reports": found,
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("exe", type=Path)
    parser.add_argument("backend", choices=["vulkan", "d3d12"])
    parser.add_argument("output", type=Path)
    parser.add_argument("--frames", type=int, default=180)
    a = parser.parse_args()
    if a.frames < 60:
        parser.error("at least 60 frames required for the capture window")
    output = a.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    results = []
    for mode in ("off", "still", "video", "history"):
        results.append(measure(a.exe.resolve(), a.backend, output, mode, a.frames))
        print(json.dumps(results[-1]), flush=True)
        (output / "summary.json").write_text(json.dumps(results, indent=2), encoding="utf8")
