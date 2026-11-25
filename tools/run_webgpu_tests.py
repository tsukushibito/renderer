"""Run all public WebGPU Runtime integration tests on an installed Chromium browser."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess

from playwright.sync_api import sync_playwright


def build(development_validation):
    features = "webgpu,development-validation" if development_validation else "webgpu"
    result = subprocess.run(
        [
            "cargo", "test", "--locked", "--release", "--target", "wasm32-unknown-unknown",
            "-p", "sashiko-renderer-core", "--features", features, "--test", "webgpu_runtime",
            "--no-run", "--message-format=json-render-diagnostics",
        ],
        cwd=Path(__file__).resolve().parents[1],
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    for line in result.stdout.splitlines():
        event = json.loads(line)
        if event.get("target", {}).get("name") == "webgpu_runtime" and event.get("executable"):
            return event["executable"]
    raise RuntimeError("Cargo did not produce the WebGPU test module")


def run_browser(url, channel, software):
    flags = ["--enable-gpu"]
    if software:
        flags += ["--enable-unsafe-webgpu", "--enable-unsafe-swiftshader", "--use-webgpu-adapter=swiftshader"]
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(channel=channel, args=flags)
        page = browser.new_page()
        page.on("pageerror", lambda error: print(f"Browser error: {error}", flush=True))
        page.goto(url)
        info = page.evaluate("""async () => {
            const adapter = await navigator.gpu.requestAdapter();
            if (!adapter) throw Error("WebGPU adapter unavailable");
            return [adapter.info.vendor, adapter.info.architecture, adapter.info.isFallbackAdapter];
        }""")
        print(f"{channel} {browser.version}, WebGPU adapter {info}", flush=True)
        if bool(info[2]) != software:
            raise RuntimeError("The browser did not select the requested hardware/software adapter")
        try:
            page.wait_for_function("document.querySelector('#output').textContent.includes('test result:')",
                                   timeout=120_000)
        finally:
            output = page.locator("#output").text_content()
            print(output, flush=True)
        if not re.search(r"test result: ok\. [1-9]\d* passed; 0 failed; 0 ignored; 0 filtered out;", output):
            raise RuntimeError("Browser tests failed or were skipped")
        browser.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--channel", choices=["chrome", "msedge", "chromium"], default="chrome")
    parser.add_argument("--software", action="store_true", help="Request SwiftShader for CI")
    parser.add_argument("--development-validation", action="store_true")
    args = parser.parse_args()
    wasm = build(args.development_validation)
    runner = os.environ.get("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER", "wasm-bindgen-test-runner")
    env = {**os.environ, "NO_HEADLESS": "1", "WASM_BINDGEN_TEST_ADDRESS": "127.0.0.1:0"}
    server = subprocess.Popen([runner, wasm, "--include-ignored"], env=env,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        for line in server.stdout:
            print(line, end="", flush=True)
            match = re.search(r"Interactive browsers tests are now available at (http://127\.0\.0\.1:\d+)", line)
            if match:
                run_browser(match[1], args.channel, args.software)
                return
        raise RuntimeError(f"wasm-bindgen-test-runner exited before serving tests ({server.wait()})")
    finally:
        if server.poll() is None:
            server.terminate()
        server.wait()


if __name__ == "__main__":
    main()
