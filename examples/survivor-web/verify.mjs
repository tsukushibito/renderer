// Check the built Wasm ABI in CI without depending on a browser or GPU.
import { readFile } from "node:fs/promises";
import assert from "node:assert/strict";
const { instance } = await WebAssembly.instantiate(await readFile(process.argv[2]));
const game = instance.exports;
const id = game.survivor_open(506);
assert.ok(id);
const x = game.survivor_position(id, 0);
game.survivor_pad(id, 0, 0.59, 0, 0, 0, 0, 0);
assert.equal(game.survivor_step(id), 0);
assert.ok(Math.abs(game.survivor_position(id, 0) - x - 0.0375) < 0.0001);
for (let i = 0; i < 3; i++) assert.ok(game.survivor_scene(id) > 3);
assert.equal(game.survivor_tick(id), 1);
assert.equal(game.survivor_close(id), 0);
assert.equal(game.survivor_step(id), -1);
