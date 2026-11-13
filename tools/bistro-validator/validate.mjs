import { mkdir, readFile, realpath, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import validator from "gltf-validator";

// Official API: https://github.com/KhronosGroup/glTF-Validator/blob/main/node/README.md
// This is an offline import-time tool, separate from Renderer shipping validation.
function requireInside(root, candidate) {
  const relative = path.relative(root, candidate);
  if (path.isAbsolute(relative) || relative === ".." || relative.startsWith(`..${path.sep}`)) {
    throw new Error("External resource escapes the model directory");
  }
}

function localResource(model, uri) {
  if (typeof uri !== "string" || /^[a-z][a-z0-9+.-]*:/i.test(uri) || /^[\\/]/.test(uri) || uri.includes("\\")) {
    throw new Error("External resources must use relative local URIs");
  }
  const url = new URL(uri, pathToFileURL(model));
  if (url.protocol !== "file:" || url.host || url.search || url.hash) {
    throw new Error("External resources must be local files without query or fragment");
  }
  return fileURLToPath(url);
}

function declaredResources(bytes) {
  let json = bytes;
  if (bytes.length >= 4 && bytes.readUInt32LE(0) === 0x46546c67) {
    // glTF 2 GLB: inspect only a bounded JSON first chunk, never binary payloads.
    if (bytes.length < 20 || bytes.readUInt32LE(16) !== 0x4e4f534a) return [];
    const length = bytes.readUInt32LE(12);
    if (length > bytes.length - 20) return [];
    json = bytes.subarray(20, 20 + length);
  }
  let document;
  try {
    document = JSON.parse(json.toString("utf8").replace(/^\uFEFF/, ""));
  } catch {
    return [];
  }
  return [document?.buffers, document?.images].flatMap((items) =>
    Array.isArray(items) ? items.map((item) => item?.uri).filter((uri) => typeof uri === "string") : []);
}

async function validate(model) {
  const root = path.dirname(model);
  const inputs = new Set([model]);
  // Node Buffers already implement Uint8Array. Keep the file-backed allocation
  // instead of copying every model, external buffer, and image before validation.
  const bytes = await readFile(model);
  const unfetched = new Set();
  for (const uri of declaredResources(bytes)) {
    try {
      const resource = localResource(model, uri);
      inputs.add(resource);
      unfetched.add(resource);
    } catch {
      // The validator diagnoses unsupported URI forms; they name no local input.
    }
  }
  const report = await validator.validateBytes(bytes, {
    uri: path.basename(model),
    maxIssues: 0,
    writeTimestamp: false,
    externalResourceFunction: async (uri) => {
      const resource = localResource(model, uri);
      requireInside(root, resource);
      inputs.add(resource);
      const target = await realpath(resource);
      requireInside(root, target);
      inputs.add(target);
      unfetched.delete(resource);
      return readFile(target);
    },
  });
  // Invalid assets can stop validation before resource callbacks. Protect their
  // declared inputs too, while preserving useful reports for missing resources.
  for (const resource of unfetched) {
    try {
      inputs.add(await realpath(resource));
    } catch {
      // Its lexical name remains protected if the target cannot be resolved.
    }
  }
  return { report, inputs };
}

async function main() {
  if (process.argv.length !== 4) {
    throw new Error("Usage: node validate.mjs <model.gltf|model.glb> <report.json>");
  }
  const model = await realpath(process.argv[2]);
  const output = path.resolve(process.argv[3]);
  let outputTarget = output;
  let outputInfo;
  try {
    outputTarget = await realpath(output);
    outputInfo = await stat(outputTarget, { bigint: true });
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  if (!path.relative(model, outputTarget)) throw new Error("The report must not overwrite the model");
  const { report, inputs } = await validate(model);
  if (inputs.has(outputTarget)) throw new Error("The report must not overwrite an input resource");
  // realpath resolves symbolic links; hard links need the file identity check.
  // Ordinary report files have a single link and need no per-input stat scan.
  if (outputInfo?.nlink > 1n) {
    for (const input of inputs) {
      let info;
      try {
        info = await stat(input, { bigint: true });
      } catch (error) {
        if (error.code === "ENOENT") continue;
        throw error;
      }
      if (info.dev === outputInfo.dev && info.ino === outputInfo.ino) {
        throw new Error("The report must not overwrite an input resource");
      }
    }
  }
  await mkdir(path.dirname(output), { recursive: true });
  await writeFile(output, `${JSON.stringify(report, null, 2)}\n`);
  const { numErrors, numWarnings, numInfos, numHints, truncated } = report.issues;
  console.log(JSON.stringify({
    validatorVersion: validator.version(),
    model,
    report: output,
    numErrors,
    numWarnings,
    numInfos,
    numHints,
    truncated,
  }));
  process.exitCode = numErrors > 0 || truncated ? 1 : 0;
}

main().catch((error) => {
  console.error(`glTF validation failed: ${error instanceof Error ? error.message : error}`);
  process.exitCode = 2;
});
