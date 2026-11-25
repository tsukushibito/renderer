"""Exercise the real threaded and inline hosts in Chromium."""
import argparse
from contextlib import contextmanager
import json
from pathlib import Path
from threading import Thread
import time

from playwright.sync_api import expect, sync_playwright
from serve import server_for


@contextmanager
def serve(site, isolated):
    with server_for(site, isolated) as server:
        thread = Thread(target=server.serve_forever)
        thread.start()
        try:
            yield f"http://127.0.0.1:{server.server_port}/"
        finally:
            server.shutdown()
            thread.join()


def run(browser, site, isolated):
    with serve(site, isolated) as url:
        page = browser.new_page()
        targets = browser.new_browser_cdp_session()

        def worker_ids():
            return {t["targetId"] for t in targets.send("Target.getTargets")["targetInfos"]
                    if t["type"] == "worker"}

        baseline = worker_ids()
        errors = []
        page.on("pageerror", lambda exc: errors.append(str(exc)))
        try:
            page.goto(url)
            status = page.locator("#status")
            expect(status).to_have_text("Stopped")
            if not isolated:
                page.check("#required")
                page.click("#start")
                expect(status).to_contain_text("require cross-origin isolation")
                page.uncheck("#required")
            page.click("#start")
            expect(status).to_have_text("parallel" if isolated else "inline", timeout=35000)
            children = worker_ids() - baseline
            assert len(children) == (3 if isolated else 1), children
            page.evaluate("""() => {
                const status = document.querySelector('#status');
                const input = document.querySelector('#input');
                input.addEventListener('input', () => window.inputStatus = status.value);
            }""")
            page.click("#run")
            page.wait_for_function("document.querySelector('#status').value === 'Running'", polling=5)
            page.locator("#input").press("a")
            expect(status).to_have_text("Done", timeout=60000)
            result = json.loads(page.locator("#result").inner_text())
            assert page.evaluate("window.inputStatus") == "Running"
            assert result["participants"] == (2 if isolated else 1), result
            assert result["heap"] == [0, 0, 0, 0], result
            assert not errors, errors
            page.click("#stop")
            expect(status).to_have_text("Stopped")
            deadline = time.monotonic() + 10
            while children & worker_ids() and time.monotonic() < deadline:
                page.wait_for_timeout(50)
            assert not children & worker_ids(), "workers survived host shutdown"
            print(("parallel" if isolated else "inline"), result, flush=True)
            return result["checksum"]
        finally:
            targets.detach()
            page.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("site", type=Path)
    parser.add_argument("--channel", default="chromium")
    args = parser.parse_args()
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(channel=args.channel)
        try:
            parallel = run(browser, args.site, True)
            inline = run(browser, args.site, False)
            if parallel != inline:
                raise AssertionError(f"checksums differ: {parallel} vs {inline}")
        finally:
            browser.close()


if __name__ == "__main__":
    main()
