"""Build a self-contained static site in build/survivor-web."""
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def main():
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).resolve()
    subprocess.run(
        ["cargo", "rustc", "--crate-type", "cdylib", "--features", "embed", "--locked", "-p", "survivor", "--lib", "--release",
         "--no-default-features", "--target", "wasm32-unknown-unknown",
         "--target-dir", str(target)],
        cwd=ROOT, check=True,
    )
    out = ROOT / "build/survivor-web"
    out.mkdir(parents=True, exist_ok=True)
    for name in ("index.html", "game.js"):
        shutil.copyfile(Path(__file__).with_name(name), out / name)
    shutil.copyfile(ROOT / "hosts/web/view.js", out / "sashiko-view.js")
    shutil.copyfile(target / "wasm32-unknown-unknown/release/survivor.wasm", out / "survivor.wasm")
    print(out)


if __name__ == "__main__":
    main()
