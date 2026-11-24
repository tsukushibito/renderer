"""Measure checkout-to-interactive and saved-image-to-preview on a Windows GPU."""
import argparse
from contextlib import contextmanager
import ctypes
from ctypes import wintypes
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import queue
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zlib


ROOT = Path(__file__).resolve().parents[2]
SOURCE = Path("crates/training-yard/assets/equipment/source")


def git(repo, *args, env=None):
    return subprocess.check_output(["git", *args], cwd=repo, env=env, text=True).strip()


def objects(directory):
    """Also catch payload replacement at the same path with the same content."""
    result = {}
    for path in directory.rglob("*"):
        if path.is_file():
            stat = path.stat()
            result[path.relative_to(directory)] = (stat.st_ino, stat.st_size, stat.st_mtime_ns)
    return result


def edit_image(number):
    # One new image per sample, including the scale run. Keep the mesh and
    # source-to-image fanout constant. No external image library is needed.
    def chunk(kind, data):
        return (struct.pack(">I", len(data)) + kind + data
                + struct.pack(">I", zlib.crc32(kind + data)))

    # Retain the yellow silhouette expected by the headless replay, while
    # changing its rendered pixel values relative to the original (255,255,0).
    rgb = bytes((176 + number % 80, 176 + number // 80 % 80, 8 + number // 6400))
    header = struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(b"\0" + rgb + b"\xff")) + chunk(b"IEND", b""))


class Events:
    def __init__(self, process):
        self.received = queue.Queue()
        self.lines = []
        self.process = process
        self.thread = threading.Thread(target=self.read, daemon=True)
        self.thread.start()

    def read(self):
        for line in self.process.stdout:
            self.received.put((time.perf_counter(), line.rstrip()))
        self.received.put((time.perf_counter(), None))

    def until(self, predicate):
        deadline = time.perf_counter() + 40
        while True:
            try:
                at, line = self.received.get(timeout=max(0, deadline - time.perf_counter()))
            except queue.Empty:
                break
            if line is None:
                break
            self.lines.append(line)
            if predicate(line):
                return at, line
        raise RuntimeError(f"Preview event missing (exit={self.process.poll()}): {self.lines[-15:]}")

    def generation(self, previous=None):
        at, line = self.until(lambda s: s.startswith("asset-preview presented ")
                              and s.rsplit(" ", 1)[-1] != previous)
        value = line.rsplit(" ", 1)[-1]
        cooks = [json.loads(s.removeprefix("asset-preview cook ")) for s in self.lines
                 if s.startswith("asset-preview cook ")]
        if not cooks or cooks[-1]["generation"] != value or cooks[-1].get("diagnostic"):
            raise ValueError(f"Displayed generation has no successful matching cook: {self.lines}")
        return at, cooks[-1]


def close_windows(pid):
    user = ctypes.windll.user32
    user.GetWindowThreadProcessId.argtypes = (wintypes.HWND, ctypes.POINTER(wintypes.DWORD))
    user.PostMessageW.argtypes = (wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM)
    callback = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

    @callback
    def visit(hwnd, _):
        owner = wintypes.DWORD()
        user.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if owner.value == pid:
            user.PostMessageW(hwnd, 0x0010, 0, 0)  # WM_CLOSE
        return True

    user.EnumWindows(visit, 0)


@contextmanager
def preview(repo, app, api):
    with subprocess.Popen([app, "--api", api, "--mode", "interactive"], cwd=repo,
                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                          encoding="utf-8", errors="replace") as process:
        events = Events(process)
        try:
            yield events
        finally:
            close_windows(process.pid)
            try:
                code = process.wait(timeout=12)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
                raise RuntimeError("Preview failed to close")
            events.thread.join()
            if code:
                raise RuntimeError(f"Preview exited with {code}: {events.lines[-15:]}")


def capture(repo, app, api):
    with tempfile.TemporaryDirectory(prefix="yard-capture-") as directory:
        subprocess.run([app, "--api", api, "--mode", "headless", "--output", directory],
                       cwd=repo, check=True, stdout=subprocess.DEVNULL)
        # Frame 0 precedes equipment loading. Frame 16 equips Wide, which
        # shares the edited paint with the interactive preview's Narrow.
        return [(Path(directory) / f"frame-{n:02}.ppm").read_bytes() for n in (0, 16)]


def sample(repo, app, api, number, resident, reference):
    tree = repo.parent / "B"
    store = Path(resident["root"])
    base_objects = objects(store / "objects")
    start = time.perf_counter()
    git(repo, "worktree", "add", "--quiet", "--detach", tree, "HEAD")
    try:
        with preview(tree, app, api) as view:
            ready, _ = view.until(lambda s: s == "asset-preview interactive")
            _, warm = view.generation()
            if (warm["generation"] != resident["generation"] or Path(warm["root"]) != store
                    or warm["snapshot_hit"] is not True or warm["conversions"] != 0
                    or warm["object_bytes_written"] != 0):
                raise AssertionError(f"Expected identical warm generation: {warm}")
            if objects(store / "objects") != base_objects:
                raise AssertionError("Warm launch changed persisted payload files")
            if (tree / SOURCE.parent / "dist/objects").exists():
                raise AssertionError("Checkout materialized distribution objects")
            checked = json.loads(subprocess.check_output(
                [sys.executable, "-B", tree / "tools/bistro/verify_lfs_assets.py",
                 "--root", tree, "--pointers"], text=True))

            paint = tree / SOURCE / "paint.png"
            staged = tree / "new-paint.tmp"
            staged.write_bytes(edit_image(number))
            saved = time.perf_counter()
            staged.replace(paint)
            changed_at, changed = view.generation(warm["generation"])
            if changed["conversions"] != 1 or changed["object_bytes_written"] <= 0:
                raise AssertionError(f"Expected a single converted image: {changed}")
            result = {"checkout_seconds": ready - start, "edit_seconds": changed_at - saved,
                      "warm": warm, "edit": changed, "bistro_pointers": checked}
            if reference is not None:
                if capture(repo, app, api) != reference:
                    raise AssertionError("B's edit changed the replay in A")
                clean, equipped = capture(tree, app, api)
                if clean != reference[0] or equipped == reference[1]:
                    raise AssertionError("B's published edit did not change the equipped frame alone")
                result["frame16_changed_components"] = sum(a != b for a, b in zip(equipped, reference[1], strict=True))
            return result
    finally:
        # This detached worktree belongs to the private, temporary clone.
        git(repo, "worktree", "remove", "--force", tree)


def scale_up(repo):
    source = repo / SOURCE
    existing = list(source.glob("*.gltf"))
    # comparison.gltf has independent inputs; these assets are never equipped
    # and do not reference the edited paint.png.
    sidecar = json.loads((source / "comparison.gltf.meta.json").read_bytes())
    for i in range(9 * len(existing)):
        name = f"extra-{i}.gltf"
        shutil.copyfile(source / "comparison.gltf", source / name)
        sidecar["asset_id"] = f"{0x10000 + i:032x}"
        (source / (name + ".meta.json")).write_text(json.dumps(sidecar), encoding="utf-8")
    git(repo, "add", "--", SOURCE)
    git(repo, "-c", "user.name=Asset benchmark", "-c", "user.email=bench@localhost",
        "-c", "commit.gpgSign=false", "commit", "-qm", "Scale isolated equipment fixture")


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def measure(args, report, save):
    with tempfile.TemporaryDirectory(prefix="yard-worktrees-") as temporary:
        repo = Path(temporary) / "A"
        revision = git(ROOT, "rev-parse", "--verify", args.revision + "^{commit}")
        # A shared Git clone exposes the developer's LFS object directory
        # through alternates. Use a shallow local fetch for a genuinely empty
        # LFS store and for measurements independent of the author's disk.
        git(ROOT, "init", "--quiet", repo)
        git(repo, "fetch", "--quiet", "--depth=1", ROOT.as_uri(), revision)
        # Only the initial clone is bootstrapped with skip-smudge. Every B
        # checkout below uses ordinary Git with the project's one-time setup.
        git(repo, "checkout", "--quiet", "--detach", "FETCH_HEAD",
            env={**os.environ, "GIT_LFS_SKIP_SMUDGE": "1"})
        subprocess.run([args.cooker, "setup"], cwd=repo, check=True)
        report["revision"] = git(repo, "rev-parse", "HEAD")
        media = next(line.partition("=")[2] for line in git(repo, "lfs", "env").splitlines()
                     if line.startswith("LocalMediaDir="))
        lfs_directory = Path(media)
        if not lfs_directory.resolve().is_relative_to(repo.resolve()) or objects(lfs_directory):
            raise ValueError("Benchmark needs an empty LFS object store inside its private clone")
        for index, case in enumerate(("baseline", "10x")):
            if index:
                scale_up(repo)
            with preview(repo, args.app, args.api) as first:
                _, resident = first.generation()
                reference = capture(repo, args.app, args.api)
                result = {"asset_count": len(list((repo / SOURCE).glob("*.gltf"))), "samples": []}
                report[case] = result
                for n in range(args.samples):
                    result["samples"].append(sample(repo, args.app, args.api,
                                                    index * args.samples + n + 1, resident,
                                                    reference if n == 0 else None))
                    if objects(lfs_directory):
                        raise AssertionError("Benchmark fetched an LFS payload")
                    save()
            result["p95"] = {key: sorted(s[key] for s in result["samples"])[math.ceil(.95 * args.samples) - 1]
                             for key in ("checkout_seconds", "edit_seconds")}
            if case == "baseline":
                # Commit both limits to the report before constructing 10x.
                report["limits"] = {key: {"seconds": result["p95"][key] + margin, "ratio": 1.5}
                                    for key, margin in (("checkout_seconds", 1.), ("edit_seconds", .5))}
            else:
                for key, value in result["p95"].items():
                    limits = report["limits"][key]
                    if value > min(limits["seconds"], report["baseline"]["p95"][key] * limits["ratio"]):
                        raise AssertionError(f"10x {key} exceeded pre-scale p95 limits: {value}, {limits}")
            save()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", required=True, type=Path,
                        help="release training-yard with presentation, memory-profiling, and selected API")
    parser.add_argument("--cooker", required=True, type=Path, help="release renderer-asset-cook")
    parser.add_argument("--report", required=True, type=Path, help="new JSON file")
    parser.add_argument("--api", choices=["vulkan", "d3d12"], default="vulkan")
    parser.add_argument("--revision", default="HEAD")
    parser.add_argument("--samples", type=int, default=20, help="independent worktrees per scale")
    args = parser.parse_args()
    if sys.platform != "win32" or not 1 <= args.samples <= 10000:
        parser.error("requires Windows and 1–10000 samples")
    for name in ("TRAINING_YARD_ASSETS", "GIT_LFS_SKIP_SMUDGE", "GIT_INDEX_FILE", "GIT_DIR", "GIT_WORK_TREE"):
        if name in os.environ:
            parser.error(f"unset {name} to measure normal worktree operation")
    args.app, args.cooker = args.app.resolve(strict=True), args.cooker.resolve(strict=True)
    report = {"passed": False, "api": args.api, "samples_per_scale": args.samples,
              "platform": platform.platform(), "cpu": platform.processor(),
              "git": git(ROOT, "--version"), "app_sha256": digest(args.app), "cooker_sha256": digest(args.cooker)}
    args.report.parent.mkdir(exist_ok=True, parents=True)
    args.report.touch(exist_ok=False)

    def save():
        args.report.write_text(json.dumps(report, indent=2), encoding="utf-8")

    try:
        measure(args, report, save)
        report["passed"] = True
    finally:
        save()


if __name__ == "__main__":
    main()
