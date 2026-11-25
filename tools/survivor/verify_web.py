"""Exercise the packaged Canvas host and the real Wasm game in Chromium."""
import argparse
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import Thread

from playwright.sync_api import sync_playwright


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("site", type=Path)
    parser.add_argument("--browser", help="Chromium executable")
    args = parser.parse_args()
    handler = partial(SimpleHTTPRequestHandler, directory=args.site.resolve())
    with ThreadingHTTPServer(("127.0.0.1", 0), handler) as server, sync_playwright() as pw:
        Thread(target=server.serve_forever, daemon=True).start()
        browser = pw.chromium.launch(executable_path=args.browser)
        try:
            page = browser.new_page()
            errors = []
            page.on("pageerror", lambda error: errors.append(str(error)))
            page.goto(f"http://127.0.0.1:{server.server_port}/")
            page.get_by_role("button", name="Step").click()
            page.get_by_role("button", name="Play", exact=True).click()
            page.get_by_role("button", name="Pause").click()
            result = page.evaluate("""async () => {
                const canvas = document.createElement("canvas");
                document.body.append(canvas);
                const {attachSurvivor} = await import("./game.js");
                const mount = () => attachSurvivor(canvas, "./survivor.wasm");
                const { view, tick, position, place } = await mount();
                const pad = {mapping: "standard", buttons: [], axes: [0.59, 0]};
                Object.defineProperty(navigator, "getGamepads", {value: () => [pad]});
                canvas.focus();
                view.paused = true;
                const [x,z] = position();
                if (!place(x+0.125,z) || place(10000,z)) throw Error("placement");
                view.frame(0);
                const pixels = canvas.getContext("2d").getImageData(0,0,canvas.width,canvas.height);
                if (new Set(new Uint32Array(pixels.data.buffer)).size < 4) throw Error("scene missing");
                if (tick() !== 0) throw Error("redraw advanced simulation");
                view.step();
                const dx = position()[0] - x - 0.125;
                if (Math.abs(dx-0.0375) > 0.001) throw Error("analog speed " + dx);
                document.querySelector("input").focus();
                view.frame(100);
                const before = position()[0];
                view.step();
                if (position()[0] !== before) throw Error("motion while editing");
                const ticks = tick();
                view.dispose();
                const reopened = await mount();
                if (reopened.tick() !== 0) throw Error("old session");
                reopened.view.dispose();
                return {ticks, dx};
            }""")
            assert result["ticks"] == 2 and not errors, (result, errors)
            print(result)
        finally:
            browser.close()
            server.shutdown()


if __name__ == "__main__":
    main()
