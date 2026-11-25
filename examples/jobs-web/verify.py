"""Exercise the real threaded and inline hosts in Chromium."""
import argparse
from contextlib import contextmanager
import json
from pathlib import Path
from functools import partial
from threading import Thread
import time

from playwright.sync_api import expect, sync_playwright
from serve import server_for


@contextmanager
def serve(site, isolated, fail_second_child=False):
    with server_for(site, isolated) as server:
        if fail_second_child:
            original = server.RequestHandlerClass

            class FailSecondChild(original.func):
                def do_GET(self):
                    if self.path != "/test-host.js":
                        return super().do_GET()
                    script = b"""
                        let started = 0;
                        self.Worker = class extends Worker {
                            constructor(url, options) {
                                super(++started === 2 ? './missing-worker.js' : url, options);
                            }
                        };
                        import './host.js';
                    """
                    self.send_response(200)
                    self.send_header("Content-Type", "text/javascript")
                    self.end_headers()
                    self.wfile.write(script)

            server.RequestHandlerClass = partial(FailSecondChild, **original.keywords)
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
            assert len(children) == (2 if isolated else 1), children
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
            assert result["hostParticipated"] == 1, result
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


def verify_team_lifecycle(browser, site):
    # Keep the host alive across a partially failed startup, recovery, and drop.
    with serve(site, True, fail_second_child=True) as url:
        page = browser.new_page()
        targets = browser.new_browser_cdp_session()

        def workers():
            return {t["targetId"] for t in targets.send("Target.getTargets")["targetInfos"]
                    if t["type"] == "worker"}

        baseline = workers()
        page.goto(url)
        page.evaluate("""() => { window.testHost = new Worker('./test-host.js', {type: 'module'}); }""")

        def send(data):
            return page.evaluate("""data => new Promise((resolve, reject) => {
                const timer = setTimeout(() => reject(Error("host response timeout")), 10000);
                testHost.onerror = event => { clearTimeout(timer); reject(Error(event.message)); };
                testHost.onmessage = ({data}) => { clearTimeout(timer); resolve(data); };
                testHost.postMessage(data);
            })""", data)

        def wait_count(n):
            deadline = time.monotonic() + 10
            while len(workers() - baseline) != n and time.monotonic() < deadline:
                page.wait_for_timeout(30)
            assert len(workers() - baseline) == n, workers() - baseline

        try:
            start = {"type": "start", "parallelism": 3, "requireParallel": True}
            assert send(start)["type"] == "error"
            wait_count(1)
            for _ in range(2):
                assert send(start)["mode"] == "parallel"
                wait_count(3)
                assert send({"type": "release"})["type"] == "released"
                wait_count(1)
        finally:
            page.evaluate("testHost.terminate()")
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
            verify_team_lifecycle(browser, args.site)
            parallel = run(browser, args.site, True)
            inline = run(browser, args.site, False)
            if parallel != inline:
                raise AssertionError(f"checksums differ: {parallel} vs {inline}")
        finally:
            browser.close()


if __name__ == "__main__":
    main()
