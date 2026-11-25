// Supply a game with button, pad, focus, advance, step, draw and close methods.
// The host may call frame from its own scheduler or use start() for standalone play.
export function attach(canvas, game, { gamepadSlot = 0 } = {}) {
    const listeners = new AbortController();
    const options = { signal: listeners.signal };
    const tabIndex = canvas.getAttribute("tabindex");
    canvas.tabIndex = 0;
    let active, previous, request, paused = false, disposed = false, inputEnabled = true;

    function focus() {
        const enabled = inputEnabled && !document.hidden && document.hasFocus() && document.activeElement === canvas;
        if (enabled !== active) game.focus(enabled);
        active = enabled;
    }
    function key(event, down) {
        const usage = keyUsage(event.code);
        if (!active || !usage || (down && (event.ctrlKey || event.metaKey || event.altKey))) return;
        if (event.code === "Tab") return;
        event.preventDefault();
        if (!event.repeat) game.button(false, usage, down);
    }
    canvas.addEventListener("focus", focus, options);
    canvas.addEventListener("blur", focus, options);
    window.addEventListener("focus", focus, options);
    window.addEventListener("blur", () => { active = false; game.focus(false); }, options);
    document.addEventListener("visibilitychange", () => { previous = undefined; focus(); }, options);
    canvas.addEventListener("keydown", e => key(e, true), options);
    canvas.addEventListener("keyup", e => key(e, false), options);
    canvas.addEventListener("pointerdown", e => {
        canvas.focus();
        canvas.setPointerCapture(e.pointerId);
        game.button(true, e.button, true);
    }, options);
    canvas.addEventListener("pointerup", e => game.button(true, e.button, false), options);
    canvas.addEventListener("pointercancel", () => { game.focus(false); active = undefined; focus(); }, options);
    focus();

    function frame(now) {
        if (disposed) return;
        if (document.hidden || !canvas.clientWidth || !canvas.clientHeight) { previous = undefined; return; }
        if (active) {
            let pad;
            try { pad = navigator.getGamepads?.()[gamepadSlot]; }
            catch (error) { if (error.name !== "SecurityError") throw error; }
            // A non-standard mapping needs a device-specific adapter from the host.
            if (pad?.mapping !== "standard") pad = null;
            let mask = 0;
            if (pad) for (let i = 0; i < Math.min(pad.buttons.length, 32); ++i)
                if (pad.buttons[i].pressed) mask |= 1 << i;
            game.pad(mask, pad?.axes[0] ?? 0, pad?.axes[1] ?? 0, pad?.axes[2] ?? 0,
                pad?.axes[3] ?? 0, pad?.buttons[6]?.value ?? 0, pad?.buttons[7]?.value ?? 0);
        }
        if (!paused && previous !== undefined) game.advance(Math.round((now - previous) * 1000));
        previous = now;
        const ratio = window.devicePixelRatio || 1;
        const width = Math.max(1, Math.round(canvas.clientWidth * ratio));
        const height = Math.max(1, Math.round(canvas.clientHeight * ratio));
        if (width !== canvas.width || height !== canvas.height) {
            canvas.width = width;
            canvas.height = height;
        }
        game.draw(width, height);
    }
    function loop(now) {
        frame(now);
        if (!disposed) request = requestAnimationFrame(loop);
    }
    return {
        frame,
        start() {
            if (!disposed && request === undefined) request = requestAnimationFrame(loop);
        },
        set inputEnabled(value) { inputEnabled = value; if (!disposed) focus(); },
        get inputEnabled() { return inputEnabled; },
        set paused(value) { paused = value; previous = undefined; },
        get paused() { return paused; },
        step() { if (!disposed && paused) game.step(); },
        dispose() {
            if (disposed) return;
            disposed = true;
            cancelAnimationFrame(request);
            listeners.abort();
            if (tabIndex === null) canvas.removeAttribute("tabindex");
            else canvas.setAttribute("tabindex", tabIndex);
            game.close();
        }
    };
}

function keyUsage(code) {
    if (/^Key[A-Z]$/.test(code)) return code.charCodeAt(3) - 65 + 4;
    if (/^Digit[0-9]$/.test(code)) return 30 + (Number(code[5]) + 9) % 10;
    return specialKeys[code];
}
const specialKeys = { Enter: 40, Escape: 41, Backspace: 42, Tab: 43, Space: 44,
    ArrowRight: 79, ArrowLeft: 80, ArrowDown: 81, ArrowUp: 82 };
