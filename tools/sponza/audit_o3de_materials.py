"""Audit the named Sponza glTF against O3DE 26.10 compiled Atom products.

Run with CameraCheck as the project and the O3DE SDK used to build its cache.
The neutral swatches compare source-level texture samples without lighting,
exposure, compression or filtering; they do not measure final shaded pixels.
"""

import argparse
from collections import defaultdict
import hashlib
import json
import re
import sqlite3

import numpy as np
from PIL import Image, ImageDraw

from io import BytesIO
from pathlib import Path
from dataclasses import dataclass
import struct


@dataclass
class Field:
    name: int
    value: bytes
    children: list


def object_stream(path):
    stream = BytesIO(path.read_bytes())

    def take(n):
        b = stream.read(n)
        if len(b) != n:
            raise ValueError("Truncated ObjectStream")
        return b

    def node(flags):
        if not flags & 8:
            raise ValueError("Expected an ObjectStream field")
        name = int.from_bytes(take(4), "big") if flags & 64 else 0
        if flags & 128:
            take(1)
        take(16)
        length = flags & 7
        if flags & 32:
            length = int.from_bytes(take(length), "big")
        value = take(length) if flags & 16 else b""
        children = []
        while tag := take(1)[0]:
            children.append(node(tag))
        return Field(name, value, children)

    if take(5) != bytes.fromhex("0000000003"):
        raise ValueError("Expected O3DE ObjectStream v3")
    return node(take(1)[0])


def number(field):
    return int.from_bytes(field.value, "big")


def accessor(gltf, blob, index):
    desc = gltf["accessors"][index]
    view = gltf["bufferViews"][desc["bufferView"]]
    dtype = np.dtype({5123: "<u2", 5125: "<u4", 5126: "<f4"}[desc["componentType"]])
    cols = {"VEC2": 2, "SCALAR": 1}[desc["type"]]
    return np.ndarray(
        (desc["count"], cols),
        dtype,
        blob,
        view.get("byteOffset", 0) + desc.get("byteOffset", 0),
        (view.get("byteStride", cols * dtype.itemsize), dtype.itemsize),
    )


def compiled_triangles(products, stem, slots):
    def raw(semantic, dtype):
        asset = object_stream(products / f"{stem}_lod0_{semantic}.gltf.azbuffer")
        return np.frombuffer(asset.children[1].value, dtype)

    uvs = raw("uv0", "<f4").reshape(-1, 2)
    indices = raw("index", "<u4")
    lod = object_stream(products / f"{stem}_lod0.gltf.azlod")
    groups = {}
    for mesh in lod.children[0].children:
        slot = number(mesh.children[2])
        idx = mesh.children[3].children[1].children
        off, count = map(number, idx[:2])
        streams = mesh.children[4].children
        uv = next(v for v in streams if v.children[0].children[0].value == b"UV")
        start = number(uv.children[2].children[1].children[0])
        groups[slots[slot]] = uvs[start + indices[off : off + count]].reshape(-1, 3, 2)
    return groups


def signature(triangles):
    # Ignore import batching, triangle order and winding, retain all six UV coordinates.
    uv = np.ascontiguousarray(triangles.astype("<f4")).view("<u8").reshape(-1, 3)
    rows = np.sort(uv, axis=1)
    return hashlib.sha256(rows[np.lexsort(rows.T)].tobytes()).hexdigest()


def verify_meshes(source, blob, products, stem):
    model = object_stream(products / f"{stem}.gltf.azmodel")
    slots = {}
    for pair in model.children[3].children:
        key, value = pair.children
        ident, label, asset = value.children
        slot = number(key)
        if slot != number(ident) or slot != int.from_bytes(asset.value[16:20], "big"):
            raise ValueError("Material slot/default asset mismatch")
        slots[slot] = int(label.value.decode().removeprefix("SponzaMaterial"))
    compiled = compiled_triangles(products, stem, slots)
    grouped = defaultdict(list)
    primitives = source["meshes"][0]["primitives"]
    for prim in primitives:
        uv = accessor(source, blob, prim["attributes"]["TEXCOORD_0"])
        idx = accessor(source, blob, prim["indices"]).ravel()
        grouped[prim["material"]].append(uv[idx].reshape(-1, 3, 2))
    if set(compiled) != set(grouped):
        raise ValueError("Compiled material slots differ from glTF")
    for mat, triangles in grouped.items():
        if signature(np.concatenate(triangles)) != signature(compiled[mat]):
            raise ValueError(
                f"Triangle UV/material assignment differs at material {mat}"
            )
    return len(primitives), sum(map(len, compiled.values())), slots


def prepare(project):
    directory = project / "Assets/Sponza"
    scene = json.loads((directory / "Sponza.gltf").read_bytes())
    for i, material in enumerate(scene["materials"]):
        material["name"] = f"SponzaMaterial{i:02}"
    (directory / "SponzaNamedCheck.gltf").write_text(json.dumps(scene), encoding="utf8")


def main(project, sdk, output, swatches):

    source_dir = project / "Assets/Sponza"
    products = project / "Cache/pc/assets/sponza"
    original = json.loads((source_dir / "Sponza.gltf").read_bytes())
    named = json.loads((source_dir / "SponzaNamedCheck.gltf").read_bytes())
    for i, mat in enumerate(named["materials"]):
        if mat.pop("name") != f"SponzaMaterial{i:02}":
            raise ValueError("Unexpected material name")
    if original != named:
        raise ValueError("Named fixture changes fields other than the material names")
    blob = (source_dir / original["buffers"][0]["uri"]).read_bytes()
    primitives, triangles, slots = verify_meshes(
        original, blob, products, "sponzanamedcheck"
    )
    inputs = sdk / "Gems/Atom/Feature/Common/Assets/Shaders/Materials/MaterialInputs"
    channels = {}
    for role in ("Metallic", "Roughness", "Normal"):
        shader = (inputs / (role + "Input.azsli")).read_text()
        selectors = set(re.findall(r"map\.Sample\(mapSampler, uv\)\.(\w+)", shader))
        if len(selectors) != 1:
            raise ValueError(f"Cannot establish {role} shader input")
        channels[role] = selectors.pop()
    if channels != {"Metallic": "r", "Roughness": "r", "Normal": "xy"}:
        raise ValueError("Shader selectors changed: review the neutral controls")
    layout = object_stream(
        project / "Cache/pc/materials/types/standardpbr_generated.azmaterialtype"
    )
    names = [
        f.children[0].value.decode()
        for f in layout.children[7].children[0].children[1].children
    ]
    stats = []
    width = 72
    panel = Image.new("RGB", (5 * width, (len(slots) + 1) * width), "white")
    labels = ImageDraw.Draw(panel)
    for col, label in enumerate(("material", "glTF rough", "Atom rough", "glTF metal", "Atom metal")):
        labels.text((col * width + 2, 24), label, fill="black")
    db = sqlite3.connect(
        (project / "Cache/assetdb.sqlite").resolve().as_uri() + "?mode=ro", uri=True
    )
    for slot, i in sorted(slots.items(), key=lambda item: item[1]):
        (p,) = products.glob(f"sponzanamedcheck_sponzamaterial{i:02}_*.gltf.azmaterial")
        (subid,) = db.execute(
            "SELECT SubID FROM Products WHERE ProductName=?",
            ("pc/assets/sponza/" + p.name,),
        ).fetchone()
        if (subid & 0xFFFFFFFF) != slot:
            raise ValueError("Material product does not match model slot")
        asset = object_stream(p)
        if b"standardpbr_generated.azmaterialtype" not in asset.children[1].value:
            raise ValueError("Unexpected material type")
        properties = dict(
            zip(
                names,
                (v.children[0].value for v in asset.children[3].children),
                strict=True,
            )
        )
        material = original["materials"][i]
        pbr = material["pbrMetallicRoughness"]

        def check_map(prop, ref):
            value = properties[prop]
            actual = (
                Path(value[56:].rstrip(b"\0").decode()).name if any(value[:16]) else ""
            )
            expected = ""
            if ref is not None:
                texture = original["textures"][ref["index"]]
                expected = original["images"][texture["source"]]["uri"]
            if actual != expected:
                raise ValueError(f"Material {i}: {prop}={actual}, expected {expected}")
            return actual

        base = check_map("baseColor.textureMap", pbr.get("baseColorTexture"))
        orm = check_map("metallic.textureMap", pbr.get("metallicRoughnessTexture"))
        check_map("roughness.textureMap", pbr.get("metallicRoughnessTexture"))
        normal = check_map("normal.textureMap", material.get("normalTexture"))
        check_map("occlusion.diffuseTextureMap", material.get("occlusionTexture"))
        factor = struct.unpack(">4f", properties["baseColor.color"])
        if not np.allclose(
            factor, pbr.get("baseColorFactor", [1] * 4), rtol=0, atol=1e-6
        ):
            raise ValueError(f"Base color factor differs for material {i}")
        if not properties["baseColor.useTexture"][0] or not np.isclose(
            struct.unpack(">f", properties["baseColor.factor"])[0], 1
        ):
            raise ValueError("Unexpected base color modulation")

        # Compare channels at every source texel, in a neutral unit-response
        # control. This isolates routing from light intensity and the tone map.
        def scalar(prop):
            return struct.unpack(">f", properties[prop])[0]

        labels.text((4, (i + 1) * width + 24), str(i), fill="black")
        errors = {}
        if orm:
            with Image.open(source_dir / orm) as image:
                pixels = np.asarray(image.convert("RGB"), dtype=np.float32) / 255
            for role, channel, prop in [
                ("roughness", 1, "roughnessFactor"),
                ("metallic", 2, "metallicFactor"),
            ]:
                expected = pixels[:, :, channel] * pbr.get(prop, 1)
                if not properties[role + ".useTexture"][0]:
                    raise ValueError(f"Disabled {role} input")
                actual = pixels[:, :, 0]
                if role == "roughness":
                    lo, hi = scalar("roughness.lowerBound"), scalar(
                        "roughness.upperBound"
                    )
                    actual = lo + actual * (hi - lo)
                col = 1 if role == "roughness" else 3
                for offset, values in enumerate((expected, actual)):
                    swatch = Image.fromarray(np.uint8(np.clip(values, 0, 1) * 255))
                    panel.paste(swatch.resize((width, width), Image.Resampling.BOX),
                                ((col + offset) * width, (i + 1) * width))
                errors[role] = round(float(np.abs(expected - actual).mean()), 6)
        stats.append(
            dict(
                index=i,
                base_color=base,
                base_factor=factor,
                normal=normal,
                packed=orm,
                metallic_factor=scalar("metallic.factor"),
                roughness_factor=scalar("roughness.factor"),
                normal_strength=scalar("normal.factor"),
                normal_flip_xy=[
                    bool(properties["normal.flipX"][0]),
                    bool(properties["normal.flipY"][0]),
                ],
                gltf_alpha=material.get("alphaMode", "OPAQUE"),
                atom_opacity_mode=int.from_bytes(properties["opacity.mode"], "big"),
                gltf_double_sided=material.get("doubleSided", False),
                atom_double_sided=bool(properties["general.doubleSided"][0]),
                channel_mean_absolute_error=errors,
            )
        )
    db.close()
    panel.save(swatches)
    report = dict(
        primitives=primitives,
        triangles=triangles,
        material_count=len(stats),
        shader_selectors=channels,
        materials=stats,
    )
    output.write_text(
        json.dumps(report, indent=2, allow_nan=False) + "\n", encoding="utf8"
    )


if __name__ == "__main__":
    cli = argparse.ArgumentParser(description=__doc__)
    commands = cli.add_subparsers(dest="action", required=True)
    commands.add_parser("prepare").add_argument("project", type=Path)
    audit = commands.add_parser("audit")
    audit.add_argument("project", type=Path)
    audit.add_argument("sdk", type=Path)
    audit.add_argument("output", type=Path)
    audit.add_argument("swatches", type=Path)
    args = cli.parse_args()
    if args.action == "prepare":
        prepare(args.project)
    else:
        main(args.project, args.sdk, args.output, args.swatches)
