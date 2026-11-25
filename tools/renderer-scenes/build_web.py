"""Build the renderer-scenes WebAssembly library and its static Canvas host."""
import argparse
import json
import shutil
import subprocess
import tempfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
VIEWER = ROOT / "examples/renderer-scenes/web"


def build_wasm(development_validation):
    features = "webgpu,development-validation" if development_validation else "webgpu"
    command = [
        "cargo", "rustc", "--crate-type", "cdylib", "--locked", "--release",
        "--package", "renderer-scenes", "--lib",
        "--target", "wasm32-unknown-unknown", "--no-default-features", "--features", features,
        "--message-format", "json-render-diagnostics",
    ]
    # Cargo reports the actual artifact path even with CARGO_TARGET_DIR or a shared worktree cache.
    build = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE, text=True, check=True)
    for line in build.stdout.splitlines():
        item = json.loads(line)
        if item.get("reason") == "compiler-artifact" and item["target"]["name"] == "renderer_scenes":
            for filename in item["filenames"]:
                if filename.endswith(".wasm"):
                    return Path(filename)
    raise RuntimeError("Cargo did not produce renderer_scenes.wasm")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=ROOT / "build/renderer-scenes-web")
    parser.add_argument("--wasm-bindgen", default="wasm-bindgen", help="wasm-bindgen CLI executable")
    parser.add_argument("--development-validation", action="store_true")
    args = parser.parse_args()

    with (ROOT / "Cargo.lock").open("rb") as lock:
        version, = (p["version"] for p in tomllib.load(lock)["package"] if p["name"] == "wasm-bindgen")
    install = f"cargo install --locked wasm-bindgen-cli --version {version}"
    try:
        tool_version = subprocess.check_output([args.wasm_bindgen, "--version"], text=True).strip()
    except FileNotFoundError:
        parser.exit(1, f"Install the matching wasm-bindgen CLI with: {install}\n")
    if tool_version != f"wasm-bindgen {version}":
        parser.exit(1, f"Found {tool_version}; this workspace needs wasm-bindgen {version}. Run: {install}\n")

    wasm = build_wasm(args.development_validation)
    output = args.out.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=output.parent) as temp:
        generated = Path(temp) / "pkg"
        subprocess.run([
            args.wasm_bindgen, "--target", "web", "--no-typescript", "--out-name", "renderer_scenes",
            "--out-dir", str(generated), str(wasm),
        ], check=True)
        # Copy only viewer files; separately packaged scenes can be much larger and are shared across builds.
        shutil.copytree(generated, output / "pkg", dirs_exist_ok=True)
        shutil.copytree(VIEWER, output, dirs_exist_ok=True)
    print(f"Serve the viewer with: python -m http.server 8000 --bind 127.0.0.1 --directory \"{output}\"")


if __name__ == "__main__":
    main()
