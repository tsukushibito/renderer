import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const cli = fileURLToPath(new URL("./validate.mjs", import.meta.url));

async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), "renderer-gltf-validator-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const assets = path.join(root, "assets");
  await mkdir(assets);
  const values = new Float32Array([0, 0, 0, 1, 0, 0, 0, 1, 0]);
  await writeFile(path.join(assets, "geometry.bin"), new Uint8Array(values.buffer));
  await writeFile(path.join(root, "outside.bin"), new Uint8Array(values.buffer));
  const model = {
    asset: { version: "2.0" },
    scene: 0,
    scenes: [{ nodes: [0] }],
    nodes: [{ mesh: 0 }],
    meshes: [{ primitives: [{ attributes: { POSITION: 0 } }] }],
    buffers: [{ uri: "geometry.bin", byteLength: 36 }],
    bufferViews: [{ buffer: 0, byteLength: 36, target: 34962 }],
    accessors: [{ bufferView: 0, componentType: 5126, count: 3, type: "VEC3", min: [0, 0, 0], max: [1, 1, 0] }],
  };
  const input = path.join(assets, "model.gltf");
  const report = path.join(root, "report.json");
  return {
    root, assets, model, input, report,
    async run() {
      await writeFile(input, JSON.stringify(model));
      const result = spawnSync(process.execPath, [cli, input, report], { encoding: "utf8" });
      assert.equal(result.error, undefined);
      return { ...result, report: JSON.parse(await readFile(report, "utf8")) };
    },
  };
}

test("validates external geometry and preserves the official report", async (t) => {
  const data = await fixture(t);
  const result = await data.run();
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.report.issues.numErrors, 0);
  assert.equal(result.report.info.totalTriangleCount, 1);
  assert.ok(result.report.info.resources.some((resource) => resource.uri === "geometry.bin"));
  assert.equal(JSON.parse(result.stdout).validatorVersion, "2.0.0-dev.3.10");
});

test("rejects missing external buffers and validates image bytes", async (t) => {
  const data = await fixture(t);
  data.model.buffers[0].uri = "missing.bin";
  data.model.images = [{ uri: "broken.png" }];
  await writeFile(path.join(data.assets, "broken.png"), "not an image");
  const result = await data.run();
  assert.equal(result.status, 1);
  const messages = result.report.issues.messages;
  assert.ok(messages.some((issue) => issue.code === "IO_ERROR"));
  assert.ok(messages.some((issue) => issue.code === "IMAGE_UNRECOGNIZED_FORMAT"));
});

test("rejects local traversal and network resource URIs", async (t) => {
  for (const uri of ["../outside.bin", "%2e%2e/outside.bin", "https://example.com/geometry.bin"]) {
    const data = await fixture(t);
    data.model.buffers[0].uri = uri;
    const result = await data.run();
    assert.equal(result.status, 1);
    assert.ok(result.report.issues.numErrors > 0);
    const expected = uri.startsWith("https:") ? "relative local URIs" : "escapes the model directory";
    assert.ok(result.report.issues.messages.some((issue) => issue.message.includes(expected)));
  }
});

test("rejects a symlink that resolves outside the model directory", async (t) => {
  const data = await fixture(t);
  const outside = path.join(data.root, "outside");
  await mkdir(outside);
  await writeFile(path.join(outside, "geometry.bin"), await readFile(path.join(data.assets, "geometry.bin")));
  await symlink(outside, path.join(data.assets, "linked"), "junction");
  data.model.buffers[0].uri = "linked/geometry.bin";
  const result = await data.run();
  assert.equal(result.status, 1);
  assert.ok(result.report.issues.messages.some((issue) => issue.message.includes("escapes the model directory")));
});

test("fails malformed input without claiming success", async (t) => {
  const data = await fixture(t);
  await writeFile(data.input, "{ invalid json");
  const result = spawnSync(process.execPath, [cli, data.input, data.report], { encoding: "utf8" });
  assert.notEqual(result.status, 0);
  if (result.status === 1) {
    const report = JSON.parse(await readFile(data.report, "utf8"));
    assert.ok(report.issues.numErrors > 0);
  } else {
    assert.match(result.stderr, /validation failed/);
  }
});
