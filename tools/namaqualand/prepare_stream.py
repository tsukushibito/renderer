"""Cook low-resolution fallbacks and on-demand detail records for a walking package."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct

import numpy as np

from restore_near_csm import references


def object_path(store, digest):
    if len(digest) != 64 or bytes.fromhex(digest).hex() != digest:
        raise ValueError("Invalid object digest")
    return store / digest


def read_object(source, ref, magic):
    raw = object_path(source / "objects", ref["hash"]).read_bytes()
    if (
        len(raw) != ref["size"]
        or hashlib.sha256(raw).hexdigest() != ref["hash"]
        or not raw.startswith(magic)
    ):
        raise ValueError(f"Incorrect cooked object {ref['hash']}")
    return raw


def prepare(source, dest, resident, transfer, slots, mip_limit):
    if min(resident, transfer, slots, mip_limit) <= 0:
        raise ValueError("Streaming limits must be positive")
    doc = json.loads((source / "scene.json").read_bytes())
    if "streaming" in doc:
        raise ValueError("Expected an original fixed-detail package")
    store = dest / "objects"
    dest.mkdir(parents=True, exist_ok=False)
    store.mkdir()
    lods = {q["source"]["hash"]: q for q in doc.get("mesh_lods", [])}
    shadows = doc.get("virtual_shadow_meshes", {})
    coarse = {}

    def image(ref):
        if ref is None:
            return None
        key = ref["hash"]
        if key not in coarse:
            raw = read_object(source, ref, b"RIMAG001")
            w, h, levels, srgb = struct.unpack_from("<4I", raw, 8)
            offset = 24
            while levels > 1 and max(w, h) > mip_limit:
                offset += w * h * 4
                w, h, levels = max(w // 2, 1), max(h // 2, 1), levels - 1
            data = raw[:8] + struct.pack("<4I", w, h, levels, srgb) + raw[offset:]
            digest = hashlib.sha256(data).hexdigest()
            if digest != key and not (store / digest).exists():
                (store / digest).write_bytes(data)
            coarse[key] = {"hash": digest, "size": len(data)}
        return coarse[key]

    fields = []
    for idx, p in enumerate(doc["primitives"]):
        mesh = p["mesh"]
        lod = lods.get(mesh["hash"])
        images = p["images"]
        smaller = [image(x) for x in images]
        if not lod and images == smaller:
            continue
        extents = []
        for ref in [mesh, lod["reduced"]] if lod else [mesh]:
            raw = read_object(source, ref, b"RMESH001")
            count = struct.unpack_from("<I", raw, 8)[0]
            pos = np.frombuffer(raw, "<f4", 12 * count, 16).reshape(-1, 12)[:, :3]
            extents.append((pos.min(0), pos.max(0)))
        bounds = [
            np.min([b[0] for b in extents], axis=0).tolist(),
            np.max([b[1] for b in extents], axis=0).tolist(),
        ]
        fields.append(
            dict(
                primitive=idx,
                mesh=mesh,
                images=images,
                bounds=bounds,
                min_pixels=lod["min_pixels"] if lod else 128,
                lightmap_uvs=(
                    doc.get("baked", {}).get("mesh_uvs", {}).get(mesh["hash"])
                    if doc.get("baked")
                    else None
                ),
            )
        )
        p["images"] = smaller
        if lod:
            p["mesh"] = lod["reduced"]
            if mesh["hash"] in shadows:
                shadows[lod["reduced"]["hash"]] = shadows[mesh["hash"]]
    valid = {p["mesh"]["hash"] for p in doc["primitives"]}
    doc["virtual_shadow_meshes"] = {h: q for h, q in shadows.items() if h in valid}
    doc["mesh_lods"] = []
    doc["streaming"] = dict(
        resident_bytes=resident,
        transfer_bytes=transfer,
        max_resident=slots,
        details=fields,
    )
    for key, size in set(references(doc)):
        path = object_path(store, key)
        if not path.exists():
            try:
                os.link(source / "objects" / key, path)
            except OSError:
                shutil.copyfile(source / "objects" / key, path)
        with path.open("rb") as blob:
            if (
                path.stat().st_size != size
                or hashlib.file_digest(blob, "sha256").hexdigest() != key
            ):
                raise ValueError(f"Object mismatch: {key}")
    (dest / "scene.json").write_text(
        json.dumps(doc, separators=(",", ":")), encoding="utf8"
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("dest", type=Path)
    parser.add_argument("--resident-mib", type=int, default=768)
    parser.add_argument("--transfer-mib", type=int, default=32)
    parser.add_argument("--slots", type=int, default=40)
    parser.add_argument("--fallback-texture", type=int, default=512)
    a = parser.parse_args()
    prepare(
        a.source,
        a.dest,
        a.resident_mib << 20,
        a.transfer_mib << 20,
        a.slots,
        a.fallback_texture,
    )
