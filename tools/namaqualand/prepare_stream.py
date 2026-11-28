"""Cook low-resolution fallbacks and on-demand detail records for a walking package."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import tempfile

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


def reserve_classes(doc, details, meshes, images, resident, slots, route_path):
    """Reserve storage for likely views without reading their detail at startup."""
    poses = [
        (v["position"], v["forward"], v["vertical_fov_degrees"])
        for v in doc["settings"]["views"].values()
    ]
    if route_path:
        route = json.loads(route_path.read_bytes())
        keys = np.asarray(route["keys"])
        if (
            keys.ndim != 2
            or keys.shape[1] != 6
            or len(keys) < 2
            or not np.isfinite(keys).all()
            or keys[0, 0] != 0
            or np.any(np.diff(keys[:, 0]) <= 0)
            or not 1 < route["fov"] < 179
        ):
            raise ValueError("Expected a finite, ordered camera route")
        for t in np.arange(0, keys[-1, 0], 0.25):
            x, y, z, yaw, pitch = [
                np.interp(t, keys[:, 0], keys[:, i]) for i in range(1, 6)
            ]
            a, b = np.radians([yaw, pitch])
            poses.append(
                (
                    [x, y, z],
                    [np.sin(a) * np.cos(b), np.sin(b), np.cos(a) * np.cos(b)],
                    route["fov"],
                )
            )
    weights = np.zeros(len(doc["primitives"]))
    by_id = {g["primitive"]: g for g in details}
    placements = [p for p in doc["placements"] if p["primitive"] in by_id]
    ids = np.asarray([p["primitive"] for p in placements])
    transforms = np.asarray([p["transform"] for p in placements])
    bounds = np.asarray([by_id[i]["bounds"] for i in ids])
    cutoffs = np.asarray([by_id[i]["min_pixels"] for i in ids])
    basis = transforms[:, :3, :3]
    centers = np.einsum("ni,nij->nj", bounds.mean(1), basis) + transforms[:, 3, :3]
    diameters = np.linalg.norm(
        np.einsum("ni,nij->nj", bounds[:, 1] - bounds[:, 0], abs(basis)), axis=1
    )
    for eye, forward, fov in poses:
        offset = centers - eye
        scale = 540 / np.tan(np.radians(fov / 2))
        projected = scale * diameters / np.maximum(0.02, np.linalg.norm(offset, axis=1))
        np.maximum.at(
            weights,
            ids,
            np.where(offset @ forward > -diameters, projected / cutoffs, 0),
        )
    buffers = []
    image_classes = []
    retained = set()
    for g in sorted(details, key=lambda g: -weights[g["primitive"]]):
        if slots == 0:
            break
        base = doc["primitives"][g["primitive"]]
        sizes = [64]
        if g["mesh"] != base["mesh"]:
            sizes.extend(meshes[g["mesh"]["hash"]])
            if g["lightmap_uvs"]:
                sizes.append(g["lightmap_uvs"]["size"])
        new_images = {}
        for slot, (hi, lo) in enumerate(zip(g["images"], base["images"])):
            if hi and hi != lo:
                key = (hi["hash"], lo["hash"] if lo else None, slot)
                if key not in retained and images[hi["hash"]] is not None:
                    new_images[key] = images[hi["hash"]]
        cost = sum(sizes) + sum(n for _, n in new_images.values())
        if cost <= resident:
            resident -= cost
            slots -= 1
            buffers.extend(sizes)
            retained.update(new_images)
            image_classes.extend(desc for desc, _ in new_images.values())
    return dict(buffers=buffers, images=image_classes)


def cook(source, dest, resident, transfer, slots, mip_limit, route_path):
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
    mesh_classes = {}
    image_classes = {}

    def image(ref):
        if ref is None:
            return None
        key = ref["hash"]
        if key not in coarse:
            raw = read_object(source, ref, b"RIMAG001")
            w, h, levels, srgb = struct.unpack_from("<4I", raw, 8)
            tw, th, tl, size = w, h, levels, len(raw) - 24
            while tl > 0 and size > transfer:
                size -= tw * th * 4
                tw, th, tl = max(tw // 2, 1), max(th // 2, 1), tl - 1
            image_classes[key] = (
                (dict(width=tw, height=th, levels=tl, srgb=bool(srgb)), size)
                if tl
                else None
            )
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
            count, indices = struct.unpack_from("<2I", raw, 8)
            mesh_classes[ref["hash"]] = [48 * count, 4 * indices]
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
    if not fields:
        raise ValueError("Scene has no higher-detail meshes or texture mips to stream")
    doc["streaming"] = dict(
        resident_bytes=resident,
        transfer_bytes=transfer,
        max_resident=slots,
        details=fields,
        reservation=reserve_classes(
            doc, fields, mesh_classes, image_classes, resident, slots, route_path
        ),
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


def prepare(source, dest, resident, transfer, slots, mip_limit, route_path=None):
    dest.parent.mkdir(parents=True, exist_ok=True)
    if dest.exists():
        raise FileExistsError(dest)
    with tempfile.TemporaryDirectory(dir=dest.parent) as scratch:
        cooked = Path(scratch) / "scene"
        cook(source, cooked, resident, transfer, slots, mip_limit, route_path)
        cooked.rename(dest)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("dest", type=Path)
    parser.add_argument("--resident-mib", type=int, default=768)
    parser.add_argument("--transfer-mib", type=int, default=32)
    parser.add_argument("--slots", type=int, default=40)
    parser.add_argument("--fallback-texture", type=int, default=512)
    parser.add_argument(
        "--route",
        type=Path,
        help="include known camera path in the initial GPU class reservation",
    )
    a = parser.parse_args()
    prepare(
        a.source,
        a.dest,
        a.resident_mib << 20,
        a.transfer_mib << 20,
        a.slots,
        a.fallback_texture,
        a.route,
    )
