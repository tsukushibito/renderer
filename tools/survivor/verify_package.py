"""Exercise the installed game from an empty working directory and isolated user profile."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile


def verify(package, backend):
    manifest = json.loads((package / "sashiko-package.json").read_text(encoding="utf-8"))
    if manifest["version"] != 1:
        raise ValueError("unsupported package version")
    for name, expected in manifest["sha256"].items():
        source = (package / name).resolve()
        if not source.is_relative_to(package):
            raise ValueError(f"invalid package path {name}")
        with source.open("rb") as stream:
            actual = hashlib.file_digest(stream, "sha256").hexdigest()
        if actual != expected:
            raise ValueError(f"package file changed: {name}")
    if manifest["executable"] not in manifest["sha256"]:
        raise ValueError("executable missing from package checksums")
    exe = package / manifest["executable"]
    with tempfile.TemporaryDirectory() as tmp:
        cwd = Path(tmp)
        profile = cwd / "user"

        def run(*args):
            result = subprocess.run([exe, "--user-dir", profile, *args], cwd=cwd,
                                    capture_output=True, text=True, timeout=120)
            if result.returncode:
                raise RuntimeError(f"{args}: {result.stderr}")
            return json.loads(result.stdout)

        initial = run("--profile-status")
        if initial["alternate_suit_unlocked"]:
            raise ValueError("fresh install unexpectedly has an unlock")
        after = run("--headless", "--ticks", "600", "--width", "640", "--height", "360", "--volume", "0.3")
        profile_state = run("--profile-status")
        if not profile_state["alternate_suit_unlocked"] or profile_state["volume"] != 0.3:
            raise ValueError(f"unlock or settings did not survive restart: {profile_state}")
        if [profile_state["width"], profile_state["height"]] != [640, 360]:
            raise ValueError("display settings did not persist")
        if backend:
            picture = cwd / "frame.ppm"
            run("--window", "--frames", "2", "--backend", backend, "--capture", picture)
            if not picture.read_bytes().startswith(b"P6\n640 360\n"):
                raise ValueError("unexpected packaged capture extent")
        return {"ticks": after["tick"], "restored": profile_state, "window_backend": backend}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path)
    parser.add_argument("--backend", choices=["vulkan", "d3d12"])
    args = parser.parse_args()
    print(json.dumps(verify(args.package.resolve(), args.backend), indent=2))
