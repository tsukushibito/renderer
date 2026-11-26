"""Run alternating isolated trials. Browser artifacts are served from a temporary tree."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from threading import Thread

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / "examples/jobs-web"))
from serve import server_for


def matrix(modes=("current", "shared")):
    for repeat in range(2):
        for n in (1, 2, 4, 8):
            for mode in (modes if repeat == 0 else reversed(modes)):
                yield mode, n


def native():
    subprocess.run(["cargo", "build", "--locked", "--release", "-p", "sashiko-worker-budget",
                    "--bin", "compare-worker-budget"], cwd=ROOT, check=True)
    for mode, n in matrix(("current", "partitioned", "shared")):
        env = dict(os.environ, RAYON_NUM_THREADS=str(n))
        subprocess.run([ROOT / "target/release/compare-worker-budget", mode, str(n)],
                       env=env, check=True, timeout=60)


def browser(cli):
    from playwright.sync_api import sync_playwright
    flags = ["-Ctarget-feature=+atomics,+bulk-memory", "-Clink-arg=--shared-memory",
             "-Clink-arg=--import-memory", "-Clink-arg=--max-memory=1073741824"]
    flags.extend("-Clink-arg=--export=" + symbol for symbol in
                 ("__wasm_init_tls", "__tls_size", "__tls_align", "__tls_base"))
    target = ROOT / "target/jobs-web/parallel"
    subprocess.run(["cargo", "+nightly-2026-11-15", "build", "--locked", "--release", "--lib",
                    "-p", "sashiko-worker-budget", "--features", "web-threads",
                    "--target", "wasm32-unknown-unknown", "--target-dir", target,
                    "-Zbuild-std=std,panic_abort"], cwd=ROOT, check=True,
                   env=dict(os.environ, CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS=" ".join(flags)))
    with tempfile.TemporaryDirectory() as tmp, sync_playwright() as pw:
        site = Path(tmp)
        subprocess.run([cli, "--target", "web", "--out-name", "trial", "--out-dir", site / "pkg",
                        target / "wasm32-unknown-unknown/release/sashiko_worker_budget.wasm"], check=True)
        shutil.copy(HERE / "host.js", site)
        (site / "index.html").write_text("<title>Worker budget measurement</title>")
        with server_for(site, True) as server:
            thread = Thread(target=server.serve_forever)
            thread.start()
            try:
                chrome = pw.chromium.launch(channel="chrome")
                page = chrome.new_page()
                page.goto(f"http://127.0.0.1:{server.server_port}/")
                for mode, n in matrix():
                    page.evaluate("""args => {
                        window.results = []; window.finished = false;
                        const w = window.host = new Worker('./host.js', {type:'module'});
                        w.onmessage = ({data}) => {
                            if (data.phase) { window.phase = data.phase; return }
                            if (data === 'done' || data.error) window.finished = true;
                            if (data !== 'done') window.results.push(data);
                        };
                        w.onerror = e => {window.results.push({error:e.message}); window.finished=true};
                        w.postMessage(args);
                    }""", {"shared": mode == "shared", "n": n})
                    try:
                        page.wait_for_function("window.finished", timeout=60000)
                    except Exception as exc:
                        phase = page.evaluate("window.phase")
                        page.evaluate("window.host.terminate()")
                        raise RuntimeError(f"{mode} n={n} stopped in {phase}") from exc
                    for row in page.evaluate("window.results"):
                        if "error" in row:
                            raise RuntimeError(row)
                        print(mode, n, row["name"], json.dumps(row["result"]), flush=True)
                chrome.close()
            finally:
                server.shutdown()
                thread.join()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("platform", choices=("native", "browser"))
    parser.add_argument("--wasm-bindgen", default="wasm-bindgen")
    args = parser.parse_args()
    if args.platform == "browser":
        browser(args.wasm_bindgen)
    else:
        native()
