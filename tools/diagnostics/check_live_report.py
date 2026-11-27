"""Exercise the real Survivor window, NDJSON client and offline report reader."""
import argparse
import json
from pathlib import Path
import queue
import struct
import subprocess
import threading
import time


def window_action(process_id, close=False):
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
    if close:
        assert user.PostMessageW(wintypes.HWND(found[0]), 0x0010, 0, 0)
    else:
        assert user.SetWindowPos(wintypes.HWND(found[0]), 0, 0, 0, 480, 340, 0x0016)


def check_layers(request, original, video):
    from PIL import Image
    import numpy as np

    def capture(action, terminal=("complete",), **options):
        accepted = request("capture", request={"action": action, **options})
        deadline = time.monotonic() + 90
        while True:
            status = request("job", job=accepted["job"])
            if status.get("phase") == "recording" and action == "start_video":
                return
            if status["state"] in terminal:
                return Path(status["artifact"]) if "artifact" in status else None
            assert status["state"] in ("running", "queued") and time.monotonic() < deadline, status
            time.sleep(0.03)

    def image(directory):
        return np.asarray(Image.open(directory / "frame.png").convert("RGB"))

    layers = dict(hud=False, velocity=True, ai=True, collision=True)
    capture("set_layers", ("applied",), layers=layers)
    overlay = capture("report")
    decorated = image(overlay)
    assert not np.array_equal(image(original)[:30, :70], decorated[:30, :70]), "HUD still visible"
    assert np.any(np.all(decorated == [0, 255, 0], axis=2)), "collision outline missing"
    stamp, = json.loads((overlay / "manifest.json").read_bytes())["frames"]
    assert stamp["layers"] == layers and stamp["stage"] == "final_srgb"
    depth = capture("report", stage="scene_depth_d32")
    manifest = json.loads((depth / "manifest.json").read_bytes())
    sample, = manifest["frames"]
    assert (sample["tick"], sample["stage"]) == (stamp["tick"], "scene_depth_d32")
    raw = np.fromfile(depth / manifest["files"]["depth"], dtype="<f4").reshape(decorated.shape[:2])
    assert 0 <= raw.min() < raw.max() <= 1 and np.isfinite(raw).all()
    assert np.abs(image(depth)[:,:,0].astype(float) - raw * 255).max() <= 0.501
    if video:
        import av
        capture("start_video", extent=stamp["extent"], fps=10)
        time.sleep(1.0)
        clip = capture("stop_video", ("partial", "complete"))
        with av.open(str(clip / "video.mkv")) as movie:
            frame = next(movie.decode(video=0)).to_ndarray(format="rgb24")
        assert np.array_equal(frame, decorated), "recorded view differs from the same paused PNG"
    capture("set_layers", ("applied",), layers={"hud":True})
    restored = capture("report")
    assert np.array_equal(image(restored), image(original)), "paused view did not restore"


def check_history(request, executable, directory, session):
    import av
    def wait(job, states):
        deadline = time.monotonic() + 30
        while True:
            status = request("job", job=job)
            if status["state"] in states:
                return status
            assert status["state"] in ("running", "queued") and time.monotonic() < deadline, status
            time.sleep(.025)

    # Normal drawing continues at a fixed simulation tick until a manual step.
    time.sleep(5.25)
    primary = request("capture", request={"action":"clip"})
    request("step")
    other = request("capture", request={"action":"clip", "trigger":"test_failure"})
    assert wait(other["job"], ["linked"])["job"] == primary["job"]
    next_id = int(other["job"].rsplit("/j",1)[1]) + 1
    (directory / f"{session}-{next_id}").write_text("existing", encoding="utf8")
    broken = request("capture", request={"action":"report"})
    assert wait(broken["job"], ["failed"])["reason"]
    request("step")
    done = wait(primary["job"], ["partial", "complete"])
    clip = Path(done["artifact"])
    m = json.loads((clip / "manifest.json").read_bytes())
    marks = {t["cause"]:t for t in m["settings"]["clip"]["triggers"]}
    assert set(marks) >= {"manual", "test_failure", "warning"}, marks
    assert (m["settings"]["clip"]["pre_ms"],m["settings"]["clip"]["post_ms"]) == (5000,2000)
    rows = [json.loads(line) for line in (clip / m["files"]["frame_map"]).read_bytes().splitlines()]
    ticks = {v["snapshot"]["tick"] for v in map(json.loads, (clip / m["files"]["ticks"]).read_bytes().splitlines())}
    covered = {int(r["pts"]) for r in rows}
    assert len(covered) == len(rows) and all(int(r["frame"]["tick"]) in ticks for r in rows)
    assert min(covered) <= 2 and max(covered) >= 68, covered
    assert int(rows[0]["frame"]["tick"]) < int(rows[-1]["frame"]["tick"])
    for g in m["gaps"]:
        if g["unit"] == "clip_sample":
            missing = set(range(int(g["first"]),int(g["last"])+1))
            assert missing.isdisjoint(covered)
            covered |= missing
    assert covered == set(range(71))
    with av.open(str(clip / m["files"]["video"])) as movie:
        frames = list(movie.decode(video=0))
        assert len(frames) == len(rows)
        for f,r in zip(frames,rows):
            assert abs(float(f.time) - int(r["pts"])/10) < .0011
    subprocess.run([str(executable),"--headless","--verify-report",str(clip)], check=True)
    print(clip, flush=True)
    closing = request("capture", request={"action":"clip","trigger":"test_failure"})
    assert wait(closing["job"], ["running"])["phase"] == "post_trigger"
    return directory / (session + "-" + closing["job"].rsplit("/j",1)[1])


def check(executable, backend, directory, video=False, layers=False, history=False):
    directory.mkdir(parents=True, exist_ok=False)
    config = directory / "settings.json"
    config.write_text('{"jobs":{"parallelism":2}}', encoding="utf-8")
    command = [str(executable), "--backend", backend, "--debug-dir", str(directory),
               "--inspect-stdio", "--width", "320", "--height", "180",
               "--config", str(config)]
    if history:
        command.append("--clip-history")
    else:
        command.extend(["--frames", "500" if layers else "200"])
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
        closing_clip = None

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
            if history:
                assert "clip" in hello["diagnostics"]["actions"]
                closing_clip = check_history(request, executable, directory, hello["session"])
                return
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
            if layers:
                assert "scene_depth_d32" in hello["diagnostics"]["stages"]
                check_layers(request, report, video)
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
                window_action(game.pid)
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
                next_id = int(stop["job"].rsplit("/j", 1)[1]) + 1
                collision = directory / (hello["session"] + "-" + str(next_id))
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
            if history and game.poll() is None:
                window_action(game.pid, close=True)
            game.stdin.close()
            try:
                game.wait(timeout=120)
            except subprocess.TimeoutExpired:
                game.kill()
                game.wait()
                raise
            if history and game.returncode:
                raise subprocess.CalledProcessError(game.returncode, command)
            if closing_clip is not None:
                m = json.loads((closing_clip / "manifest.json").read_bytes())
                assert m["outcome"]["state"] == "partial" and any(g["reason"]=="shutdown" for g in m["gaps"]), m
        if game.returncode:
            raise subprocess.CalledProcessError(game.returncode, command)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("survivor", type=Path)
    parser.add_argument("backend", choices=["vulkan", "d3d12"])
    parser.add_argument("output", type=Path)
    parser.add_argument("--video", action="store_true")
    parser.add_argument("--layers", action="store_true")
    parser.add_argument("--history", action="store_true")
    args = parser.parse_args()
    check(args.survivor.resolve(), args.backend, args.output.resolve(), args.video, args.layers, args.history)
