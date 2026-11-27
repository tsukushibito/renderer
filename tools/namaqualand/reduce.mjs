import fs from 'node:fs';
import path from 'node:path';
import {createRequire} from 'node:module';

const [directory, modulePath] = process.argv.slice(2);
const {MeshoptSimplifier} = createRequire(import.meta.url)(modulePath);
await MeshoptSimplifier.ready;
const gltf = JSON.parse(fs.readFileSync(path.join(directory, 'section.gltf')));
const source = fs.readFileSync(path.join(directory, gltf.buffers[0].uri));
const indices = [];
let length = 0;
function read(id, type, components) {
    const a = gltf.accessors[id];
    const view = gltf.bufferViews[a.bufferView];
    return new type(source.buffer, source.byteOffset + view.byteOffset, components * a.count);
}
for (const mesh of gltf.meshes) {
    for (const primitive of mesh.primitives) {
        const input = read(primitive.indices, Uint32Array, 1);
        const position = read(primitive.attributes.POSITION, Float32Array, 3);
        const uv = read(primitive.attributes.TEXCOORD_0, Float32Array, 2);
        const target = 3 * Math.max(1, Math.floor(input.length * 0.06 / 3));
        const [low] = MeshoptSimplifier.simplifyWithAttributes(input, position, 3, uv, 2,
            [0.2, 0.2], null, target, 0.015);
        if (low.length === input.length) continue;
        const view = gltf.bufferViews.length;
        gltf.bufferViews.push({buffer:1, byteOffset:length, byteLength:low.byteLength});
        primitive.indices = gltf.accessors.length;
        gltf.accessors.push({bufferView:view, componentType:5125, count:low.length, type:'SCALAR'});
        indices.push(Buffer.from(low.buffer, low.byteOffset, low.byteLength));
        length += low.byteLength;
    }
}
gltf.buffers.push({uri:'reduced.bin', byteLength:length});
fs.writeFileSync(path.join(directory, 'reduced.bin'), Buffer.concat(indices));
fs.writeFileSync(path.join(directory, 'reduced.gltf'), JSON.stringify(gltf));
