"""Build the inline and shared-memory versions of the browser host."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tomllib

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
TARGET = "wasm32-unknown-unknown"
NIGHTLY = "nightly-2026-11-15"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wasm-bindgen", default="wasm-bindgen")
    parser.add_argument("--out", type=Path, default=ROOT / "build/jobs-web")
    parser.add_argument("--experimental-rayon", action="store_true")
    args = parser.parse_args()
    with (ROOT / "Cargo.lock").open("rb") as lock:
        version = next(p["version"] for p in tomllib.load(lock)["package"] if p["name"] == "wasm-bindgen")
    if subprocess.check_output([args.wasm_bindgen, "--version"], text=True).strip() != f"wasm-bindgen {version}":
        parser.error(f"expected wasm-bindgen CLI {version}")
    for mode in (["inline", "parallel", "rayon"] if args.experimental_rayon else ["inline", "parallel"]):
        target_dir = ROOT / "target/jobs-web" / ("inline" if mode == "inline" else "parallel")
        env = os.environ.copy()
        cargo = ["cargo"]
        if mode != "inline":
            cargo.append("+" + NIGHTLY)
        cmd = cargo + ["build", "--release", "--locked", "--target-dir", str(target_dir),
                       "--target", TARGET, "-p", "sashiko-jobs-web-example"]
        if mode != "inline":
            cmd += ["--features", "experimental-rayon" if mode == "rayon" else "web-threads", "-Zbuild-std=std,panic_abort"]
            flags = ["-Ctarget-feature=+atomics,+bulk-memory", "-Clink-arg=--shared-memory",
                     "-Clink-arg=--import-memory", "-Clink-arg=--max-memory=1073741824"]
            for symbol in ("__wasm_init_tls", "__tls_size", "__tls_align", "__tls_base"):
                flags.append(f"-Clink-arg=--export={symbol}")
            env["CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS"] = " ".join(flags)
        subprocess.run(cmd, env=env, cwd=ROOT, check=True)
        wasm = target_dir / TARGET / "release/sashiko_jobs_web_example.wasm"
        subprocess.run([args.wasm_bindgen, "--target", "web", "--no-typescript", "--out-name", "jobs",
                        "--out-dir", args.out / mode, wasm], check=True)
    shutil.copytree(HERE / "web", args.out, dirs_exist_ok=True)


if __name__ == "__main__":
    main()
