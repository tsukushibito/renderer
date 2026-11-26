import { attach } from "./sashiko-view.js";

const colors = ["#263638", "#50676a", "#e2c074", "#ea7b62", "#ffcf55", "#58da9c", "#ffdf7b", "#e3faf6", "#d19b56"];

export async function attachSurvivor(canvas, wasmUrl) {
    const response = await fetch(wasmUrl);
    if (!response.ok) throw new Error("Could not load Survivor WebAssembly: " + response.status);
    const { instance } = await WebAssembly.instantiate(await response.arrayBuffer());
    const api = instance.exports;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("Canvas 2D is unavailable");
    const id = api.survivor_open(506);
    if (!id) throw new Error("Could not open Survivor");
    const checked = result => {
        if (result < 0) {
            const bytes = new Uint8Array(api.memory.buffer, api.survivor_error());
            throw new Error(new TextDecoder().decode(bytes.subarray(0, bytes.indexOf(0))));
        }
        return result;
    };
    let rects;
    const game = {
        focus: active => checked(api.survivor_focus(id, active)),
        button: (mouse, code, down) => checked(api.survivor_button(id, mouse, code, down)),
        advance: micros => checked(api.survivor_advance(id, micros)),
        step: () => checked(api.survivor_step(id)),
        close: () => checked(api.survivor_close(id)),
        pad: (mask, lx, ly, rx, ry, lt, rt) =>
            checked(api.survivor_pad(id, mask, lx, ly, rx, ry, lt, rt)),
        draw(w, h) {
            const count = checked(api.survivor_scene(id));
            const start = api.survivor_rects(id);
            if (!rects || rects.buffer !== api.memory.buffer || rects.byteOffset !== start)
                rects = new Float32Array(api.memory.buffer, start);

            const scale = Math.min(w / (rects[2] * 2), h / (rects[3] * 2));
            ctx.fillStyle = "#121c1d";
            ctx.fillRect(0, 0, w, h);
            for (let i = 0; i < count * 5; i += 5) {
                ctx.fillStyle = colors[rects[i + 4]];
                ctx.fillRect(w / 2 + (rects[i] - rects[i + 2]) * scale,
                    h / 2 + (rects[i + 1] - rects[i + 3]) * scale,
                    2 * rects[i + 2] * scale, 2 * rects[i + 3] * scale);
            }
        }
    };
    const view = attach(canvas, game);
    return { view, tick: () => api.survivor_tick(id),
        position: () => [api.survivor_position(id, 0), api.survivor_position(id, 1)],
        place: (x, z) => checked(api.survivor_place(id, x, z)) === 0 };
}
