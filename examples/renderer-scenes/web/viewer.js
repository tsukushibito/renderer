import init, { openScene } from "./pkg/renderer_scenes.js";

const mounted = new WeakSet();
const movementKeys = new Set(["KeyW", "KeyA", "KeyS", "KeyD", "KeyQ", "KeyE"]);
let initialization;

function initialize() {
    initialization ??= init().catch(error => {
        initialization = undefined;
        throw error;
    });
    return initialization;
}

function fitCanvas(canvas) {
    const ratio = window.devicePixelRatio || 1;
    const width = Math.round(canvas.clientWidth * ratio);
    const height = Math.round(canvas.clientHeight * ratio);
    if (canvas.width !== width) canvas.width = width;
    if (canvas.height !== height) canvas.height = height;
}

// The host can drive frame(timeMs) or use start(). sceneUrl is relative to the host page.
export async function attachScene(canvas, sceneUrl, {
    autoResize = true, onProgress = () => {}, onError = console.error,
} = {}) {
    if (!navigator.gpu) throw new Error("WebGPU is unavailable. Open this page using HTTPS or localhost in a WebGPU browser.");
    if (mounted.has(canvas)) throw new Error("Dispose the current scene before attaching another to this canvas.");
    const url = sceneUrl === "fixture" ? sceneUrl : new URL(sceneUrl, document.baseURI).href;
    mounted.add(canvas);
    let session;
    try {
        await initialize();
        if (autoResize) fitCanvas(canvas);
        session = await openScene(canvas, url, onProgress);
    } catch (error) {
        mounted.delete(canvas);
        throw error;
    }

    const events = new AbortController();
    const options = { signal: events.signal };
    const originalTabIndex = canvas.getAttribute("tabindex");
    if (canvas.tabIndex < 0) canvas.tabIndex = 0;
    const keys = new Set();
    let dragPointer, previous, yaw = 0, pitch = 0;
    let scheduled, running = false, disposed = false;
    let tail = Promise.resolve(), closing;

    function clearInput() {
        keys.clear();
        if (dragPointer !== undefined && canvas.hasPointerCapture(dragPointer)) {
            canvas.releasePointerCapture(dragPointer);
        }
        dragPointer = undefined;
        yaw = pitch = 0;
        previous = undefined;
    }

    function key(event, pressed) {
        if (!movementKeys.has(event.code) || (pressed && (event.ctrlKey || event.metaKey || event.altKey))) return;
        event.preventDefault();
        if (pressed) keys.add(event.code);
        else keys.delete(event.code);
    }
    canvas.addEventListener("keydown", event => key(event, true), options);
    canvas.addEventListener("keyup", event => key(event, false), options);
    canvas.addEventListener("blur", clearInput, options);
    window.addEventListener("blur", clearInput, options);
    document.addEventListener("visibilitychange", clearInput, options);
    canvas.addEventListener("contextmenu", event => event.preventDefault(), options);
    canvas.addEventListener("pointerdown", event => {
        if (!event.isPrimary || (event.button !== 0 && event.button !== 2)) return;
        canvas.focus({ preventScroll: true });
        canvas.setPointerCapture(event.pointerId);
        dragPointer = event.pointerId;
    }, options);
    canvas.addEventListener("pointermove", event => {
        if (dragPointer !== event.pointerId) return;
        yaw += Math.max(-100, Math.min(100, event.movementX)) * 0.0025;
        pitch -= Math.max(-100, Math.min(100, event.movementY)) * 0.0025;
    }, options);
    function endDrag(event) {
        if (event.pointerId !== dragPointer) return;
        dragPointer = undefined;
        if (canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
    }
    for (const name of ["pointerup", "pointercancel", "lostpointercapture"]) {
        canvas.addEventListener(name, endDrag, options);
    }

    // Async Rust methods hold an exclusive borrow of the session across an await.
    // Serialize captures, camera changes and frames, and drain them before freeing it.
    function invoke(operation) {
        if (disposed) return Promise.reject(new Error("Scene disposed"));
        const result = tail.then(() => operation(session));
        tail = result.catch(() => {});
        return result;
    }
    function frame(now = performance.now()) {
        return invoke(scene => {
            if (autoResize) fitCanvas(canvas);
            if (!canvas.width || !canvas.height || document.hidden) {
                previous = undefined;
                return false;
            }
            const delta = previous === undefined ? 0 : Math.max(0, Math.min((now - previous) / 1000, 0.1));
            previous = now;
            const axis = (positive, negative) => Number(keys.has(positive)) - Number(keys.has(negative));
            const dx = yaw, dy = pitch;
            yaw = pitch = 0;
            return scene.frame(delta, axis("KeyW", "KeyS"), axis("KeyD", "KeyA"), axis("KeyE", "KeyQ"), dx, dy);
        });
    }
    async function loop(now) {
        scheduled = undefined;
        try {
            await frame(now);
        } catch (error) {
            running = false;
            if (!disposed) onError(error);
            return;
        }
        if (running && scheduled === undefined) scheduled = requestAnimationFrame(loop);
    }
    function stop() {
        running = false;
        if (scheduled !== undefined) cancelAnimationFrame(scheduled);
        scheduled = undefined;
        clearInput();
    }
    return {
        frame,
        start() {
            if (disposed) throw new Error("Scene disposed");
            if (!running) {
                running = true;
                scheduled = requestAnimationFrame(loop);
            }
        },
        stop,
        preset(name) {
            clearInput();
            return invoke(scene => scene.preset(name));
        },
        capture: () => invoke(scene => scene.capture()),
        setRectLights: mode => invoke(scene => scene.setRectLights(mode)),
        dispose() {
            if (closing) return closing;
            disposed = true;
            stop();
            events.abort();
            if (originalTabIndex === null) canvas.removeAttribute("tabindex");
            else canvas.setAttribute("tabindex", originalTabIndex);
            closing = tail.then(() => session.free()).finally(() => mounted.delete(canvas));
            return closing;
        },
    };
}
