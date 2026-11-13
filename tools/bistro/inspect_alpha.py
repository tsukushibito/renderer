"""Reproduce the two pinned Bistro zero-opacity materials' offline evidence.

This reads the original archive, hash-bound FBX audit, BC3 alpha at every authored
mip, and unchanged glTF/PNG payloads. It does not infer artist intent or exercise
Renderer/native GPU execution. Only the legacy 2D DXT5 layout used here is accepted.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
from urllib.parse import unquote
import zipfile

import numpy as np
from PIL import Image

from compare_geometry import Gltf
from verify_lfs_assets import verify


MATERIALS = ("MASTER_Glass_Dirty_MASKED", "Paris_LiquorBottle_01_Labels")
ARCHIVE_SHA256 = "0d50e3c724c6c5da19f8eb99ad3f53e36fec37ffa2df9621f9ccf0603f3934e1"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def file_record(path):
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"bytes": path.stat().st_size, "sha256": digest}


def alpha_stats(pixels):
    histogram = np.bincount(pixels.ravel(), minlength=256)
    return {"min": int(pixels.min()), "max": int(pixels.max()),
            "zero": int(histogram[0]), "opaque": int(histogram[255]),
            "fractional": int(histogram[1:255].sum()),
            "histogram": {str(i): int(n) for i, n in enumerate(histogram) if n}}


def decode_bc3_alpha(raw):
    """Return actual texels per mip; exclude padded block texels at image edges.

    Interpolated integer samples use floor division. The pinned observations are
    all zero, independent of interpolation rounding or normalized sampling.
    """
    require(len(raw) >= 128 and raw[:4] == b"DDS ", "truncated or missing DDS header")
    header = struct.unpack_from("<31I", raw, 4)
    size, flags, height, width, linear_size, depth, mip_count = header[:7]
    require(size == 124 and header[18] == 32, "invalid DDS header sizes")
    require(flags & 0x81007 == 0x81007 and flags & ~0xA1007 == 0,
            "unsupported DDS flags; require 2D linear-size DXT5")
    require(0 < width <= 16384 and 0 < height <= 16384 and depth == 0,
            "unsupported DDS dimensions/depth")
    require(header[19] == 4 and raw[84:88] == b"DXT5" and not any(header[21:26]),
            "require legacy DXT5 FourCC pixel format")
    require(header[26] & 0x1000 and header[26] & ~0x401008 == 0 and not any(header[27:31]),
            "unsupported DDS caps, cube, volume or reserved fields")
    require(1 <= mip_count <= max(width, height).bit_length(), "invalid DDS mip count")
    require(mip_count == 1 or flags & 0x20000 and header[26] & 0x400008 == 0x400008,
            "mip chain lacks DDS mip flags/caps")
    require(linear_size == ((width + 3) // 4) * ((height + 3) // 4) * 16,
            "DDS base linear size differs from block extent")
    extents = [(max(1, width >> level), max(1, height >> level)) for level in range(mip_count)]
    expected_bytes = 128 + sum(((w + 3) // 4) * ((h + 3) // 4) * 16 for w, h in extents)
    require(len(raw) == expected_bytes, "truncated DDS mip data or unexpected trailing bytes")
    result = []
    offset = 128
    for w, h in extents:
        block_width, block_height = (w + 3) // 4, (h + 3) // 4
        length = block_width * block_height * 16
        blocks = np.frombuffer(raw, dtype=np.uint8, count=length, offset=offset).reshape(-1, 16)
        first, second = blocks[:, 0].astype(np.uint16), blocks[:, 1].astype(np.uint16)
        palette = np.empty((len(blocks), 8), dtype=np.uint8)
        palette[:, 0], palette[:, 1] = first, second
        eight = first > second
        for i in range(2, 8):
            palette[eight, i] = ((8-i)*first[eight] + (i-1)*second[eight]) // 7
        for i in range(2, 6):
            palette[~eight, i] = ((6-i)*first[~eight] + (i-1)*second[~eight]) // 5
        palette[~eight, 6], palette[~eight, 7] = 0, 255
        packed = np.sum(blocks[:, 2:8].astype(np.uint64) << np.arange(0, 48, 8, dtype=np.uint64), axis=1)
        indices = ((packed[:, None] >> np.arange(0, 48, 3, dtype=np.uint64)) & 7).astype(np.intp)
        values = palette[np.arange(len(blocks))[:, None], indices]
        padded = values.reshape(block_height, block_width, 4, 4).transpose(0, 2, 1, 3)
        result.append(padded.reshape(block_height * 4, block_width * 4)[:h, :w].copy())
        offset += length
    return result


def image_record(path):
    with Image.open(path) as image:
        require(image.format == "PNG" and image.mode == "RGBA", "expected source-preserving RGBA PNG")
        pixels = np.array(image)
    flat = pixels.reshape(-1, 4)
    return {**file_record(path), "width": pixels.shape[1], "height": pixels.shape[0],
            "rgba_min": flat.min(axis=0).tolist(), "rgba_max": flat.max(axis=0).tolist(),
            "rgba_pixels_sha256": hashlib.sha256(pixels.tobytes()).hexdigest(),
            "alpha": alpha_stats(pixels[:, :, 3])}, pixels


def source_usage(audit, material_id):
    geometries = {g["id"]: g for g in audit["geometries"]}
    result = []
    for model in audit["models"]:
        slots = [i for i, value in enumerate(model.get("material_ids", [])) if value == material_id]
        if not slots:
            continue
        count = 0
        geometry_ids = []
        for geometry_id in model["geometry_ids"]:
            geometry = geometries[geometry_id]
            require(geometry["polygon_sizes"] == {"3": geometry["polygons"]},
                    "target material geometry must contain only source triangles")
            require(len(geometry["material_layers"]) == 1, "ambiguous source material layers")
            layer = geometry["material_layers"][0]
            mapping = layer["fields"]["MappingInformationType"]
            require(layer["fields"]["ReferenceInformationType"] == ["IndexToDirect"],
                    "unsupported source material reference mapping")
            if mapping == ["AllSame"]:
                bound = [int(slot) for slot, n in layer["material_index_counts"].items() if n]
                require(len(bound) == 1, "ambiguous AllSame material")
                triangles = geometry["polygons"] if bound[0] in slots else 0
            else:
                require(mapping == ["ByPolygon"], "unsupported source material mapping")
                triangles = sum(layer["material_index_counts"].get(str(slot), 0) for slot in slots)
            if triangles:
                geometry_ids.append(geometry_id)
                count += triangles
        if count:
            result.append({"model_id": model["id"], "name": model["name"],
                           "geometry_ids": geometry_ids, "triangles": count})
    return sorted(result, key=lambda item: item["name"])


def gltf_usage(model, material_index):
    result = []
    for name, (mesh_index, _) in model.mesh_instances().items():
        count = 0
        for primitive in model.data["meshes"][mesh_index]["primitives"]:
            if primitive.get("material") != material_index:
                continue
            require(primitive.get("mode", 4) == 4, "target glTF primitive is not triangles")
            accessor = primitive.get("indices", primitive["attributes"]["POSITION"])
            elements = model.data["accessors"][accessor]["count"]
            require(type(elements) is int and elements > 0 and elements % 3 == 0,
                    "invalid target glTF triangle count")
            count += elements // 3
        if count:
            result.append({"name": name, "mesh_index": mesh_index, "triangles": count})
    return sorted(result, key=lambda item: item["name"])


def inspect(source, audit_path, gltf_path, *, root, archive=None, prepared=None):
    source, root = source.resolve(), root.resolve()
    archive = archive or root / "assets/bistro/source/Bistro_v5_2.zip"
    archive_record = file_record(archive)
    require(archive_record["sha256"] == ARCHIVE_SHA256, "archive differs from pinned official Bistro v5.2")
    manifest_result = verify(root)
    audit = json.loads(audit_path.read_text(encoding="utf-8"))
    require(audit.get("schema_version") == 1 and audit["source"]["path"] == "BistroExterior.fbx",
            "unsupported source audit")
    require(file_record(source / "BistroExterior.fbx") ==
            {key: audit["source"][key] for key in ("bytes", "sha256")}, "FBX audit hash/size differs")
    model = Gltf(gltf_path)
    documents = []
    materials = []
    with zipfile.ZipFile(archive) as zipped:
        require(len(zipped.namelist()) == len(set(zipped.namelist())), "duplicate archive member paths")

        def archive_match(relative):
            member = "Bistro_v5_2/" + relative
            with zipped.open(member) as stream:
                digest = hashlib.file_digest(stream, "sha256").hexdigest()
            record = {"bytes": zipped.getinfo(member).file_size, "sha256": digest}
            require(file_record(source / relative) == record, f"extracted file differs from archive: {relative}")
            return record

        fbx_record = archive_match("BistroExterior.fbx")
        for name in ("README.txt", "CHANGELOG.txt", "BistroExterior.pyscene"):
            record = archive_match(name)
            documents.append({"path": name, **record,
                              "content": zipped.read("Bistro_v5_2/" + name).decode("utf-8")})
        for name in MATERIALS:
            source_material = next(m for m in audit["materials"] if m["name"] == name)
            material_index = next(i for i, m in enumerate(model.data["materials"]) if m["name"] == name)
            material = model.data["materials"][material_index]
            texture_relative = source_material["textures"]["DiffuseColor"]
            dds_record = archive_match(texture_relative)
            dds = source / texture_relative
            levels = decode_bc3_alpha(dds.read_bytes())
            with Image.open(dds) as image:
                source_pixels = np.array(image.convert("RGBA"))
            require(np.array_equal(source_pixels[:, :, 3], levels[0]), "BC3 base alpha differs from Pillow decoder")
            pbr = material["pbrMetallicRoughness"]
            slot = pbr["baseColorTexture"]
            texture = model.data["textures"][slot["index"]]
            uri = model.data["images"][texture["source"]]["uri"]
            texture_path = (gltf_path.parent / unquote(uri)).resolve()
            require(texture_path.is_relative_to(gltf_path.parent.resolve()), "base image escapes glTF directory")
            current, current_pixels = image_record(texture_path)
            require(np.array_equal(current_pixels, source_pixels), "current PNG pixels differ from original DDS base mip")
            source_meshes, current_meshes = source_usage(audit, source_material["id"]), gltf_usage(model, material_index)
            source_triangles = sum(m["triangles"] for m in source_meshes)
            require(source_triangles == sum(m["triangles"] for m in current_meshes)
                    and len(source_meshes) == len(current_meshes), "target material geometry inventory differs")
            record = {"name": name, "source_id": source_material["id"],
                      "source_properties": source_material["properties"], "source_textures": source_material["textures"],
                      "source_texture_slots": [t for t in audit["textures_and_videos"]
                                               if t.get("type") == "Texture" and any(
                                                   c["material_id"] == source_material["id"]
                                                   for c in t.get("material_connections", []))],
                      "source_dds": {"path": texture_relative, **dds_record,
                                     "format": "legacy DXT5/BC3 2D", "alpha_mips": [
                                         {"level": i, "width": pixels.shape[1], "height": pixels.shape[0],
                                          "texels": pixels.size, **alpha_stats(pixels)}
                                         for i, pixels in enumerate(levels)]},
                      "current_png": {"uri": uri, **current, "source_base_rgba_equal": True},
                      "current_material": material,
                      "source_meshes": source_meshes, "current_meshes": current_meshes,
                      "triangles": source_triangles, "mesh_models": len(source_meshes)}
            if prepared is not None:
                prepared_record, prepared_pixels = image_record(prepared / texture_path.name)
                require(np.array_equal(prepared_pixels, source_pixels), "prepared PNG pixels differ from original DDS")
                record["prepared_png"] = {**prepared_record, "source_base_rgba_equal": True}
            materials.append(record)
    return {"schema_version": 1,
            "scope": "Pinned source/current-artifact facts only; no artist-intent or Renderer/GPU acceptance claim",
            "archive": {"path": "assets/bistro/source/Bistro_v5_2.zip", **archive_record},
            "source_fbx": fbx_record, "source_audit": file_record(audit_path),
            "source_documents": documents, "materials": materials,
            "unchanged_payloads": {**manifest_result, "manifest": file_record(root / "assets/bistro/manifest.json"),
                                   "meaning": "Every shared source/converted/environment/comparison-preview payload matches the current manifest"},
            "not_executed": ["Original Falcor executable rendering", "Renderer/native GPU rendering",
                             "Artist-intent confirmation", "Final runtime camera/lighting selection"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--audit", required=True, type=Path)
    parser.add_argument("--gltf", required=True, type=Path)
    parser.add_argument("--root", type=Path, help="Repository with hydrated manifest payloads; defaults to glTF's repository")
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--prepared", type=Path, help="Optional existing prepared PNG directory to compare")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    report = inspect(args.source, args.audit, args.gltf,
                     root=args.root or args.gltf.resolve().parents[3], archive=args.archive, prepared=args.prepared)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, ensure_ascii=False, allow_nan=False) + "\n", encoding="utf-8")
    print(json.dumps({"report": str(args.output), "materials": len(report["materials"]),
                      "unchanged_payloads": report["unchanged_payloads"]["files"]}))


if __name__ == "__main__":
    main()
