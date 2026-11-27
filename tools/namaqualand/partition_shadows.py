"""Partition a cooked mesh into exact spatial pieces for virtual shadow drawing."""

import argparse
import hashlib
import json
from pathlib import Path
import struct
import tempfile

import numpy as np


def pieces(raw, cell_size):
    if raw[:8] != b"RMESH001" or not np.isfinite(cell_size) or cell_size <= 0:
        raise ValueError("Expected RMESH001 geometry and a finite positive cell size")
    vertex_count, index_count = struct.unpack_from("<II", raw, 8)
    index_offset = 16 + 48 * vertex_count
    if len(raw) != index_offset + 4 * index_count or not index_count or index_count % 3:
        raise ValueError("Invalid triangle mesh length")
    vertices = np.frombuffer(raw, np.uint8, 48 * vertex_count, 16).reshape(-1, 48)
    triangles = np.frombuffer(raw, "<u4", index_count, index_offset).reshape(-1, 3)
    positions = vertices.view("<f4")[:, :3]
    centers = positions[triangles].mean(axis=1)[:, [0, 2]]
    grid = np.floor((centers - centers.min(axis=0)) / cell_size)
    if not np.isfinite(grid).all() or grid.max() > 2**31 - 1:
        raise ValueError("Shadow partition grid exceeds supported extent")
    x, z = grid.astype(np.int64).T
    keys = z * (x.max() + 1) + x
    # Stable ordering preserves source winding and triangle order in each draw.
    order = np.argsort(keys, kind="stable")
    boundaries = np.flatnonzero(np.diff(keys[order])) + 1
    for group in np.split(order, boundaries):
        used, remap = np.unique(triangles[group].ravel(), return_inverse=True)
        yield (b"RMESH001" + struct.pack("<II", len(used), remap.size)
               + vertices[used].tobytes() + remap.astype("<u4").tobytes())


def partition(directory, primitive, cell_size):
    manifest = directory / "scene.json"
    store = directory / "objects"
    scene = json.loads(manifest.read_bytes())
    if not 0 <= primitive < len(scene["primitives"]):
        raise ValueError("Primitive index outside the scene")
    source = scene["primitives"][primitive]["mesh"]
    digest = source["hash"]
    if len(digest) != 64 or bytes.fromhex(digest).hex() != digest:
        raise ValueError("Invalid source mesh digest")
    raw = (store / digest).read_bytes()
    if len(raw) != source["size"] or hashlib.sha256(raw).hexdigest() != digest:
        raise ValueError("Source mesh differs from its manifest")
    refs = []
    for data in pieces(raw, cell_size):
        key = hashlib.sha256(data).hexdigest()
        blob = store / key
        if not blob.exists():
            with tempfile.NamedTemporaryFile(dir=store, delete=False) as tmp:
                tmp.write(data)
            Path(tmp.name).replace(blob)
        elif blob.read_bytes() != data:
            raise ValueError(f"Corrupt existing object {key}")
        refs.append({"hash": key, "size": len(data)})
    scene.setdefault("virtual_shadow_meshes", {})[digest] = refs
    with tempfile.NamedTemporaryFile(dir=directory, delete=False) as tmp:
        tmp.write(json.dumps(scene, separators=(",", ":")).encode())
    Path(tmp.name).replace(manifest)
    return len(refs)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path, help="cooked package to update in place")
    parser.add_argument("--primitive", type=int, required=True)
    parser.add_argument("--cell-size", type=float, required=True, help="in source mesh units")
    args = parser.parse_args()
    print(f"Prepared {partition(args.package, args.primitive, args.cell_size)} shadow pieces")
