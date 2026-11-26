"""Verify the packaged viewer in Chromium. Full runs require Windows hardware and native references."""

import argparse
import base64
from contextlib import contextmanager
from functools import partial
import hashlib
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import sys
from threading import Event, Thread
from time import perf_counter

import numpy as np
from PIL import Image
from playwright.sync_api import sync_playwright
import psutil

from reference import LOOKS, TOOLS, VIEWS, light_modes

sys.path.insert(0, str(TOOLS / "bistro"))
from compare_runtime import FORMATS, compare_pixels, digest, load_capture, validate_pixels


@contextmanager
def serve(site):
    class Handler(SimpleHTTPRequestHandler):
        def log_request(self, code="-", size="-"):
            if isinstance(code, int) and code >= 400:
                super().log_request(code, size)

    handler = partial(Handler, directory=site.resolve())
    with ThreadingHTTPServer(("127.0.0.1", 0), handler) as server:
        thread = Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield f"http://127.0.0.1:{server.server_port}/"
        finally:
            server.shutdown()
            thread.join()


@contextmanager
def process_memory(browser):
    # CDP supplies PIDs specifically for this browser, not unrelated user processes.
    cdp = browser.new_browser_cdp_session()
    pids = cdp.send("SystemInfo.getProcessInfo")["processInfo"]
    root = psutil.Process(next(entry["id"] for entry in pids if entry["type"] == "browser"))
    result = {"peak_process_tree_rss_bytes": 0, "sample_interval_ms": 100}
    stop = Event()

    def sample():
        while not stop.is_set():
            total = 0
            for proc in [root, *root.children(recursive=True)]:
                try:
                    total += proc.memory_info().rss
                except (psutil.NoSuchProcess, psutil.AccessDenied):
                    continue
            result["peak_process_tree_rss_bytes"] = max(total, result["peak_process_tree_rss_bytes"])
            stop.wait(0.1)

    thread = Thread(target=sample, daemon=True)
    thread.start()
    try:
        yield result
    finally:
        stop.set()
        thread.join()
        cdp.detach()


DEVICE_EVENTS = """
globalThis.gpuEvidence = {adapters: [], errors: []};
globalThis.compilationChecks = [];
if (globalThis.GPUDevice) {
    const create = GPUDevice.prototype.createShaderModule;
    GPUDevice.prototype.createShaderModule = function(descriptor) {
        const module = create.call(this, descriptor);
        compilationChecks.push(module.getCompilationInfo().then(info => {
            for (const message of info.messages) {
                if (message.type !== 'info') {
                    gpuEvidence.errors.push(`${descriptor.label ?? 'WGSL'}:${message.lineNum} ${message.message}`);
                }
            }
        }));
        return module;
    };
}
if (globalThis.GPUAdapter) {
    const original = GPUAdapter.prototype.requestDevice;
    GPUAdapter.prototype.requestDevice = async function(...args) {
        const device = await original.apply(this, args);
        globalThis.lastSceneDevice = device;
        const info = this.info;
        gpuEvidence.adapters.push({vendor:info.vendor, architecture: info.architecture,
            description:info.description, fallback:info.isFallbackAdapter});
        device.addEventListener("uncapturederror", event => {
            gpuEvidence.errors.push(event.error.message);
        });
        device.lost.then(loss => {
            if (loss.reason !== "destroyed") gpuEvidence.errors.push("Device lost: " + loss.message);
        });
        return device;
    };
}
"""


def harness(page, base):
    # Load the public API with a host-owned, unscaled canvas. A same-origin
    # document also exercises relative scene and wasm URLs.
    page.route(
        "**/__verify__",
        lambda route: route.fulfill(
            content_type="text/html",
            body='<link rel="icon" href="data:,"><style>*{margin:0}canvas{display:block}</style><canvas></canvas>',
        ),
    )
    page.goto(base + "__verify__")


def attach(page, url, width, height, technique=None, layout=None):
    page.set_viewport_size({"width": width, "height": height})
    page.evaluate(
        """async ([url,w,h,technique,shadowLayout]) => {
        const canvas = document.querySelector('canvas');
        canvas.width=w; canvas.height=h;
        const {attachScene} = await import('./viewer.js');
        globalThis.view = await attachScene(canvas, url, {autoResize:false,technique,shadowLayout});
    }""",
        [url, width, height, technique, layout],
    )


def advance(page, count):
    return page.evaluate(
        """async count => {
        const intervals = [], submissionIntervals = [];
        let submitted = 0, previous;
        for (let i=0; i<count; ++i) {
            const start = performance.now();
            const time = await new Promise(requestAnimationFrame);
            const visible = await view.frame(time);
            const end = performance.now();
            intervals.push(end-start);
            if (visible) {
                ++submitted;
                if (previous !== undefined) submissionIntervals.push(end-previous);
                previous = end;
            }
        }
        return {raf_intervals_ms:intervals, submitted_frames:submitted,
            skipped_frames:count-submitted, submission_intervals_ms:submissionIntervals};
    }""",
        count,
    )


def capture(page, directory, width, height):
    directory.mkdir(parents=True, exist_ok=True)
    result = page.evaluate(
        """async () => {
        const {sdr,hdr,depth,width,height} = await view.capture();
        return {sdr:sdr.toBase64(), hdr:hdr.toBase64(), depth:depth.toBase64(), width,height};
    }"""
    )
    if (result["width"], result["height"]) != (width, height):
        raise ValueError("Capture size differs from the host canvas")
    planes = {}
    for name, (_, dtype, channels) in FORMATS.items():
        data = base64.b64decode(result.pop(name), validate=True)
        plane = np.frombuffer(data, dtype=dtype).reshape(height, width, channels)
        validate_pixels(plane, name)
        (directory / f"{name}.bin").write_bytes(data)
        planes[name] = plane
    page.locator("canvas").screenshot(path=directory / "canvas.png")
    with Image.open(directory / "canvas.png") as img:
        displayed = np.array(img.convert("RGBA"))
    result["canvas_vs_sdr"] = compare_pixels(planes["sdr"], displayed, "sdr", "scene")
    return planes, result


def verify_inputs(site, scene, report):
    manifest = site / "scenes" / scene / "scene.json"
    data = json.loads(manifest.read_text(encoding="utf-8"))
    if data["id"] != scene or data["look"] != LOOKS[scene]:
        raise ValueError(f"Wrong scene or preset in {manifest}")
    identity = report["identity"]
    if identity["look"]["name"] != data["look"] or not identity["aa"] or identity["pipeline"] != "forward":
        raise ValueError("Native preset is inconsistent with the browser reference profile")
    environment = identity["environment"]
    lighting = manifest.parent / "objects" / data["lighting"]["sha256"]
    if digest(lighting) != data["lighting"]["sha256"]:
        raise ValueError(f"Damaged GI manifest in {manifest}")
    index = json.loads(lighting.read_text(encoding="utf-8"))["index_sha256"]
    reflections = (manifest.parent / "objects" / data["reflections"]["sha256"]).read_bytes()
    if hashlib.sha256(reflections).hexdigest() != data["reflections"]["sha256"]:
        raise ValueError(f"Damaged reflection atlas in {manifest}")
    # The first 64 bytes identify the bake inputs. The native renderer reports
    # the hash of the actual atlas that follows this cook-specific header.
    atlas_sha256 = hashlib.sha256(reflections[64:]).hexdigest()
    for native, browser in (
        (identity["cook_manifest_sha256"], data["geometry"]["sha256"]),
        (environment["manifest_sha256"], data["environment"]["sha256"]),
        (environment["reflection_probes_sha256"], atlas_sha256),
        (environment["baked_lighting_sha256"], index),
    ):
        if native != browser:
            raise ValueError(f"Native and packaged inputs differ: {native}, {browser}")
    return digest(manifest)


def compare_view(args, site, scene, name, mode, planes):
    path = args.references / f"{name}-{mode}" / "report.json"
    report = json.loads(path.read_text(encoding="utf-8"))
    if (
        report["runtime"]["api"] != "vulkan"
        or report["validation"]["mode"] != "strict"
        or not report["validation"]["clean"]
    ):
        raise ValueError(f"A validation-clean Vulkan reference is required in {path}")
    if report["identity"]["view"] != name:
        raise ValueError(f"Incorrect reference view in {path}")
    results = {
        "native_report_sha256": digest(path),
        "scene_manifest_sha256": verify_inputs(site, scene, report),
    }
    for key, plane in planes.items():
        native, _ = load_capture(path, report, key)
        results[key] = compare_pixels(native, plane, key, "scene")
    return results


def rectangle_ratio(output, name, width, height, roi):
    x0, y0, x1, y1 = roi
    luma = {}
    depths = set()
    for mode in light_modes("sponza"):
        folder = output / f"{name}-{mode}"
        depths.add(digest(folder / "depth.bin"))
        hdr = np.memmap(folder / "hdr.bin", dtype="<f2", mode="r", shape=(height, width, 4))
        luma[mode] = (hdr[y0:y1, x0:x1, :3].astype(np.float32) @ [0.2126, 0.7152, 0.0722]).mean()
    if len(depths) != 1 or luma["unshadowed"] <= luma["off"]:
        raise ValueError(f"Invalid direct rectangle lighting control at {name}")
    ratio = float((luma["shadowed"] - luma["off"]) / (luma["unshadowed"] - luma["off"]))
    return {
        "roi": roi,
        "ratio": ratio,
        "passed": abs(ratio) <= 0.01 if name == "gallery" else abs(ratio - 1) <= 0.05,
    }


def exercise_controls(page, width, height):
    canvas = page.locator("canvas")
    canvas.focus()
    before = canvas.screenshot()
    page.keyboard.down("w")
    try:
        advance(page, 6)
    finally:
        page.keyboard.up("w")
    if before == canvas.screenshot():
        raise ValueError("Holding W did not move the displayed camera")
    before = canvas.screenshot()
    page.mouse.move(width / 2, height / 2)
    page.mouse.down()
    page.mouse.move(width / 2 + 24, height / 2 + 12, steps=3)
    page.mouse.up()
    advance(page, 2)
    if before == canvas.screenshot():
        raise ValueError("Dragging on the canvas did not turn the displayed camera")
    size = [width // 2, height // 2]
    page.evaluate("([w,h]) => { const c=document.querySelector('canvas'); c.width=w; c.height=h; }", size)
    advance(page, 2)
    actual = page.evaluate("async () => { const c=await view.capture(); return [c.width,c.height]; }")
    if actual != size:
        raise ValueError(f"Resize did not update the rendered scene: {actual} != {size}")


def run_views(page, args, result):
    for scene in args.scenes:
        url = "fixture" if scene == "fixture" else f"./scenes/{scene}/scene.json"
        views = [("fixture", (320, 180), None)] if scene == "fixture" else VIEWS[scene]
        for name, (width, height), roi in views:
            started = perf_counter()
            attach(page, url, width, height)
            try:
                if scene != "fixture":
                    page.evaluate("name => view.preset(name)", name)
                advance(page, 2)
                result["timing"][name] = {
                    "attach_and_first_frames_ms": (perf_counter() - started) * 1000,
                    **advance(page, 24),
                }
                for mode in light_modes(scene):
                    if scene == "sponza":
                        page.evaluate("mode => view.setRectLights(mode)", mode)
                    advance(page, 2)
                    label = f"{name}-{mode}"
                    planes, details = capture(page, args.output / label, width, height)
                    if not np.any(planes["depth"] < 1) or np.all(planes["sdr"] == planes["sdr"][0, 0]):
                        raise ValueError(f"No visible scene geometry in {label}")
                    if scene != "fixture":
                        details["native"] = compare_view(args, args.site, scene, name, mode, planes)
                    result["views"][label] = details
                if roi:
                    result["rectangle_shadows"][name] = rectangle_ratio(
                        args.output, name, width, height, roi
                    )
                exercise_controls(page, width, height)
            finally:
                page.evaluate("() => view.dispose()")


def verify_indirect_shadows(page, args, result):
    captures = []
    for name in ("indirect-shadow-off", "indirect-shadow", "indirect-shadow-half"):
        attach(page, name, 320, 180)
        try:
            planes, details = capture(page, args.output / name, 320, 180)
            result["views"][name] = details
            if args.shadow_reference:
                native = np.fromfile(args.shadow_reference / name / "sdr.bin", np.uint8).reshape(180, 320, 4)
                difference = np.abs(native.astype(float) - planes["sdr"])
                details["native_sdr_mean_abs"] = float(difference.mean())
                if details["native_sdr_mean_abs"] > 0.5:
                    raise ValueError(f"{name} differs from the native reference: {details}")
            captures.append(planes)
            result["timing"][name] = advance(page, 24)
            if args.shadow_memory:
                details["steady_memory"] = measure_shadow_memory(page)
        finally:
            page.evaluate("() => view.dispose()")
    base, full, half = captures
    for shadow in (full, half):
        loss = base["hdr"][:, :, :3].astype(float) - shadow["hdr"][:, :, :3]
        if not np.array_equal(base["depth"], shadow["depth"]) or np.min(loss) < -0.001 or np.count_nonzero(loss > 0.01) < 100:
            raise ValueError("Capsules must darken the static diffuse receivers while preserving depth")
    error = np.abs(full["hdr"][:, :, :3].astype(float) - half["hdr"][:, :, :3])
    quality = {"mean": float(error.mean()), "p99": float(np.percentile(error, 99))}
    result["views"]["indirect-shadow-half"]["full_hdr_abs"] = quality
    if not 0.0 < quality["mean"] <= 0.001 or quality["p99"] > 0.01:
        raise ValueError(f"Half-resolution receiver reconstruction differs from Full: {quality}")
    for technique in ("forward-plus", "deferred"):
        attach(page, "indirect-shadow", 321, 181, technique, "single" if args.software else "dense")
        try:
            for mode in ("half", "off", "full"):
                page.evaluate("mode => view.setIndirectShadows(mode)", mode)
                label = f"{technique}-{mode}"
                _, result["views"][label] = capture(page, args.output / label, 321, 181)
        finally:
            page.evaluate("() => view.dispose()")
    if args.shadow_memory:
        off = result["views"]["indirect-shadow-off"]["steady_memory"]
        for key in ("indirect-shadow", "indirect-shadow-half"):
            on = result["views"][key]["steady_memory"]
            expected = off["heap"].copy()
            if key == "indirect-shadow-half":
                # wgpu 30.0.2 collects one color attachment Vec per additional
                # render pass on the browser backend (renderer-v8bk).
                expected[0] += on["frames"]
                expected[3] += on["frames"]
            if on["heap"] != expected or any(on["resources"].values()):
                raise ValueError(f"Steady-state indirect shadow allocations ({key}): {off} -> {on}")


def measure_shadow_memory(page):
    return page.evaluate("""async () => {
        const {heapCalls} = await import('./pkg/renderer_scenes.js');
        const snapshot = () => [0, 1, 2, 3].map(heapCalls);
        // wasm-bindgen can release completion closures through JS finalizers.
        // Drain both endpoints so releases are charged to the same 60 frames.
        async function drain() {
            await lastSceneDevice.queue.onSubmittedWorkDone();
            for (let i = 0; i < 3; ++i) {
                globalThis.gc();
                await new Promise(resolve => setTimeout(resolve, 0));
            }
        }
        const resources = {}, restore = [];
        for (const [proto, names] of [
            [GPUDevice.prototype, ['createBuffer', 'createTexture', 'createBindGroup',
                'createRenderPipeline', 'createComputePipeline', 'createSampler']],
            [GPUBuffer.prototype, ['destroy']], [GPUTexture.prototype, ['destroy']]]) {
            for (const name of names) {
                const original = proto[name], label = proto.constructor.name + '.' + name;
                resources[label] = 0;
                proto[name] = function(...args) { ++resources[label]; return original.apply(this,args); };
                restore.push(() => { proto[name] = original; });
            }
        }
        try {
            await drain();
            const before = snapshot();
            for (let i = 0; i < 60; ++i) {
                if (!await view.frame(await new Promise(requestAnimationFrame))) throw Error('Skipped measured frame');
                await lastSceneDevice.queue.onSubmittedWorkDone();
            }
            await drain();
            return {frames: 60, heap: snapshot().map((value,i) => value-before[i]), resources};
        } finally { restore.forEach(fn => fn()); }
    }""")


def verify_device_loss(page):
    attach(page, "fixture", 320, 180)
    try:
        page.evaluate("async () => { const device=lastSceneDevice; device.destroy(); await device.lost; }")
        rejected = page.evaluate(
            """async () => {
            try { await view.frame(performance.now()); return false; }
            catch (err) { return /lost|destroyed/i.test(String(err)); }
        }"""
        )
        if not rejected:
            raise ValueError("Destroyed device did not yield a device loss error to the host")
    finally:
        page.evaluate("() => view.dispose()")
    attach(page, "fixture", 320, 180)
    try:
        advance(page, 2)
        if not page.evaluate("async () => (await view.capture()).depth.length === 320*180*4"):
            raise ValueError("Canvas could not reconnect after device loss")
    finally:
        page.evaluate("() => view.dispose()")


def sample_page(page, base, scenes):
    page.set_viewport_size({"width": 640, "height": 480})
    page.goto(base + "?scene=" + scenes[0])
    selected = page.locator("#scene")
    for index, scene in enumerate(scenes):
        if index:
            selected.select_option(scene)
        page.wait_for_function(
            """() => {
            const retry = document.querySelector('#retry');
            return !retry.hidden || (document.activeElement?.tagName === 'CANVAS' &&
                !document.querySelector('#scene').disabled && !document.querySelector('#status').textContent);
        }""",
            timeout=600_000,
        )
        if page.locator("#retry").is_visible():
            raise ValueError(page.locator("#status").inner_text())
        if scene != "fixture":
            view = VIEWS[scene][0][0]
            page.locator("#preset").select_option(view)
            page.wait_for_function(
                "name => new URL(location.href).searchParams.get('view') === name", arg=view
            )
            page.locator("#reset").click()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("site", type=Path)
    parser.add_argument("--scenes", nargs="+", choices=["fixture", *VIEWS], default=["fixture"])
    parser.add_argument("--references", type=Path)
    parser.add_argument("--shadow-reference", type=Path, help="Native 320x180 frame-zero captures of the three shadow demo modes")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--channel", choices=["chrome", "msedge", "chromium"], default="chrome")
    parser.add_argument("--software", action="store_true", help="Use SwiftShader for the CI fixture")
    parser.add_argument("--shadow-memory", action="store_true", help="Compare warmed frames with a --memory-profiling viewer")
    args = parser.parse_args()
    if any(s != "fixture" for s in args.scenes) and (not args.references or args.software):
        parser.error("Full-scene acceptance requires --references and a hardware GPU")
    args.output.mkdir(parents=True, exist_ok=True)
    errors = []
    result = {"timing": {}, "views": {}, "rectangle_shadows": {}, "browser_errors": errors, "passed": False}
    flags = ["--enable-gpu", "--force-color-profile=srgb"]
    if args.shadow_memory:
        flags.append("--js-flags=--expose-gc")
    if args.software:
        flags += [
            "--enable-unsafe-webgpu",
            "--enable-unsafe-swiftshader",
            "--use-webgpu-adapter=swiftshader",
        ]
        if sys.platform == "win32":
            flags += ["--use-angle=d3d11-warp", "--ignore-gpu-blocklist"]
    try:
        with serve(args.site) as base, sync_playwright() as pw:
            browser = pw.chromium.launch(channel=args.channel, args=flags)
            try:
                result["channel"] = args.channel
                result["browser_version"] = browser.version
                with process_memory(browser) as memory:
                    result["memory"] = memory
                    page = browser.new_page(device_scale_factor=1)
                    page.add_init_script(DEVICE_EVENTS)
                    page.on("pageerror", lambda error: errors.append(str(error)))
                    page.on(
                        "console",
                        lambda msg: (
                            errors.append(msg.text)
                            if msg.type == "error"
                            or (msg.type == "warning" and "webgpu" in msg.text.lower())
                            else None
                        ),
                    )
                    harness(page, base)
                    run_views(page, args, result)
                    if "fixture" in args.scenes:
                        verify_indirect_shadows(page, args, result)
                    verify_device_loss(page)
                    gpu = result["gpu"] = page.evaluate(
                        "async () => { await Promise.all(compilationChecks); return gpuEvidence; }"
                    )
                    sample_page(page, base, args.scenes)
                    ui = page.evaluate(
                        "async () => { await Promise.all(compilationChecks); return gpuEvidence; }"
                    )
                    gpu["adapters"].extend(ui["adapters"])
                    gpu["errors"].extend(ui["errors"])
                    if (
                        not gpu["adapters"]
                        or any(a["fallback"] for a in gpu["adapters"])
                        and not args.software
                    ):
                        raise ValueError(f"Unexpected GPU adapter selection: {gpu['adapters']}")
                    if gpu["errors"] or errors:
                        raise ValueError(f"Browser errors: {gpu['errors'] + errors}")
            finally:
                browser.close()
        result["passed"] = (
            all(v["canvas_vs_sdr"]["passed"] for v in result["views"].values())
            and all(v["passed"] for v in result["rectangle_shadows"].values())
            and all(
                checks["native"][plane]["passed"]
                for checks in result["views"].values()
                if "native" in checks
                for plane in FORMATS
            )
        )
    except Exception as error:
        result["failure"] = str(error)
        raise
    finally:
        (args.output / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    if not result["passed"]:
        raise SystemExit(f"Image comparison failed; see {args.output / 'result.json'}")


if __name__ == "__main__":
    main()
