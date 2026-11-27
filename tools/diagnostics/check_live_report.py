"""Exercise the real Survivor window, NDJSON client and offline report reader."""
import argparse
import json
from pathlib import Path
import queue
import struct
import subprocess
import threading
import time


def resize_window(process_id):
    import ctypes
    from ctypes import wintypes
    user = ctypes.windll.user32
    callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    found = []

    @callback_type
    def find(hwnd, _):
        pid = wintypes.DWORD()
        user.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        if pid.value == process_id and user.IsWindowVisible(hwnd):
            found.append(hwnd)
            return False
        return True

    user.EnumWindows(find, 0)
    assert len(found) == 1
    assert user.SetWindowPos(wintypes.HWND(found[0]), 0, 0, 0, 480, 340, 0x0016)


def check(executable, backend, directory, video=False):
    directory.mkdir(parents=True, exist_ok=False)
    config = directory / "settings.json"
    config.write_text('{"jobs":{"parallelism":2}}', encoding="utf-8")
    command = [str(executable), "--backend", backend, "--debug-dir", str(directory),
               "--inspect-stdio", "--frames", "200", "--width", "320", "--height", "180",
               "--config", str(config)]
    with subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          encoding="utf-8", cwd=directory) as game:
        messages = queue.Queue()

        def receive():
            try:
                for line in game.stdout:
                    messages.put(json.loads(line))
            finally:
                messages.put(None)

        threading.Thread(target=receive, daemon=True).start()
        sequence = 0

        def request(op, **params):
            nonlocal sequence
            sequence += 1
            game.stdin.write(json.dumps(dict(id=str(sequence), op=op, **params)) + "\n")
            game.stdin.flush()
            reply = messages.get(timeout=120)
            assert reply and reply["id"] == str(sequence) and reply["ok"], reply
            return reply["result"]

        try:
            hello = request("hello")
            assert "report" in hello["diagnostics"]["actions"], hello
            request("pause")
            request("step")
            player = request("find", kind="Player")["objects"][0]
            subject = player["id"]
            center = player["display_center"]
            pointer = {"view": "main", "viewport": [15, 20, 640, 360],
                       "point": [15 + center[0] * 640, 20 + center[1] * 360]}
            assert request("pick", pointer=pointer)["object"] == subject
            assert request("selected")["object"] == subject
            pointer["point"] = [15, 20]
            assert request("pick", pointer=pointer)["state"] == "empty"
            requested = request("capture", request={"action": "report"})
            request("read", object=subject, fields=["hp"])
            deadline = time.monotonic() + 90
            while True:
                job = request("job", job=requested["job"])
                if job["state"] == "complete":
                    break
                assert job["state"] in ("running", "queued"), job
                assert time.monotonic() < deadline, job
                time.sleep(0.02)
            report = Path(job["artifact"])
            manifest = json.loads((report / "manifest.json").read_bytes())
            assert (manifest["session"], manifest["world"], manifest["job"]) == (
                hello["session"], hello["world"], requested["job"])
            assert manifest["requested_tick"] == requested["requested_tick"]
            frame, = manifest["frames"]
            assert frame["tick"] == requested["requested_tick"] and frame["extent"] == [320, 180]
            png = (report / manifest["files"]["image"]).read_bytes()
            assert png[:8] == b"\x89PNG\r\n\x1a\n" and struct.unpack(">II", png[16:24]) == (320, 180)
            subprocess.run([str(executable), "--headless", "--verify-report", str(report)],
                           check=True, timeout=120)
            # Occupy the next report name. A failed writer must leave it intact,
            # publish the job error, and keep the simulation responsive.
            collision = directory / (hello["session"] + "-2")
            collision.write_bytes(b"existing file")
            failed = request("capture", request={"action": "report"})
            deadline = time.monotonic() + 90
            while True:
                status = request("job", job=failed["job"])
                if status["state"] == "failed":
                    assert status["reason"]
                    break
                assert status["state"] in ("queued", "running") and time.monotonic() < deadline, status
            assert collision.read_bytes() == b"existing file"
            request("step")
            request("resume")
            if video:
                import av
                assert "start_video" in hello["diagnostics"]["actions"], hello
                start = request("capture", request={"action": "start_video", "extent": [160, 90], "fps": 15})
                deadline = time.monotonic() + 20
                while request("job", job=start["job"]).get("phase") != "recording":
                    assert time.monotonic() < deadline
                time.sleep(0.6)
                resize_window(game.pid)
                resumed_at = int(request("hello")["state"]["tick"])
                deadline = time.monotonic() + 45
                while int(request("hello")["state"]["tick"]) < resumed_at + 12:
                    assert time.monotonic() < deadline, "simulation did not advance after resize"
                    time.sleep(0.05)
                stop = request("capture", request={"action": "stop_video"})
                deadline = time.monotonic() + 90
                while True:
                    done = request("job", job=stop["job"])
                    if done["state"] in ("complete", "partial"):
                        break
                    assert done["state"] in ("queued", "running") and time.monotonic() < deadline, done
                clip = Path(done["artifact"])
                index = json.loads((clip / "manifest.json").read_bytes())
                rows = [json.loads(line) for line in (clip / index["files"]["frame_map"]).read_bytes().splitlines()]
                assert len({tuple(row["frame"]["extent"]) for row in rows}) >= 2, rows
                with av.open(str(clip / index["files"]["video"])) as movie:
                    assert not movie.streams.audio
                    decoded = list(movie.decode(video=0))
                    assert len(decoded) == len(rows) > 1
                    for frame, row in zip(decoded, rows):
                        assert (frame.width, frame.height) == (160, 90)
                        assert abs(float(frame.time) - int(row["pts"]) / 15) < 0.0011
                print(clip, flush=True)
                collision = directory / (hello["session"] + "-5")
                collision.write_bytes(b"retain")
                failed = request("capture", request={"action": "start_video", "extent": [160, 90], "fps": 15})
                deadline = time.monotonic() + 15
                while True:
                    status = request("job", job=failed["job"])
                    if status["state"] == "failed":
                        assert status["reason"] and collision.read_bytes() == b"retain"
                        break
                    assert time.monotonic() < deadline, status
                request("read", object=subject, fields=["hp"])
            print(report, flush=True)
        finally:
            game.stdin.close()
            try:
                game.wait(timeout=120)
            except subprocess.TimeoutExpired:
                game.kill()
                game.wait()
                raise
        if game.returncode:
            raise subprocess.CalledProcessError(game.returncode, command)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("survivor", type=Path)
    parser.add_argument("backend", choices=["vulkan", "d3d12"])
    parser.add_argument("output", type=Path)
    parser.add_argument("--video", action="store_true")
    args = parser.parse_args()
    check(args.survivor.resolve(), args.backend, args.output.resolve(), args.video)
