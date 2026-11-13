"""Decode audited Bistro texture references without silently changing PBR channels."""
import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
from PIL import Image, __version__ as pillow_version


def file_hash(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def prepare(source, references, output):
    source = source.resolve()
    output.mkdir(parents=True, exist_ok=True)
    records = []
    destinations = set()
    for ref in sorted(references, key=lambda item: item["path"]):
        path = (source / ref["path"]).resolve()
        if not path.is_relative_to(source):
            raise ValueError(f"Texture escapes source directory: {ref['path']}")
        role = ref["role"]
        if role not in ("base_color", "normal", "orm", "emissive"):
            raise ValueError(f"Unknown texture role: {role}")
        raw = path.read_bytes()
        fourcc = raw[84:88].decode("ascii") if raw[:4] == b"DDS " else None
        with Image.open(path) as image:
            pixels = np.array(image.convert("RGBA"))
        decoded_hash = hashlib.sha256(pixels.tobytes()).hexdigest()
        if role == "normal":
            # README: DirectX normals. BC5 stores X/Y; reconstruct positive Z.
            if fourcc != "ATI2":
                raise ValueError(f"Unaudited normal encoding {fourcc}: {path.name}")
            xy = pixels[:, :, :2].astype(np.float64) / 127.5 - 1.0
            z = np.sqrt(np.maximum(0.0, 1.0 - np.sum(xy * xy, axis=2)))
            result = pixels[:, :, :3].copy()
            result[:, :, 1] = 255 - pixels[:, :, 1]
            result[:, :, 2] = np.rint((z + 1.0) * 127.5).astype(np.uint8)
        elif role in ("orm", "emissive"):
            result = pixels[:, :, :3]
        else:
            result = pixels
        destination = output / f"{path.stem}.png"
        if destination.name.casefold() in destinations:
            raise ValueError(f"Colliding output texture name: {destination.name}")
        destinations.add(destination.name.casefold())
        Image.fromarray(result).save(destination, compress_level=6)
        with Image.open(destination) as check:
            if not np.array_equal(np.asarray(check), result):
                raise ValueError(f"PNG pixel roundtrip failed: {destination}")
        record = {
            "source": path.relative_to(source).as_posix(), "role": role,
            "source_sha256": file_hash(path), "source_size": len(raw),
            "dds_fourcc": fourcc,
            "source_mip_levels_raw": int.from_bytes(raw[28:32], "little") if fourcc else 1,
            "source_mip_levels": max(1, int.from_bytes(raw[28:32], "little")) if fourcc else 1,
            "width": pixels.shape[1], "height": pixels.shape[0],
            "decoded_rgba_sha256": decoded_hash,
            "output": destination.name, "output_sha256": file_hash(destination),
            "output_pixel_sha256": hashlib.sha256(result.tobytes()).hexdigest(),
            "colorspace": "sRGB" if role in ("base_color", "emissive") else "linear-data",
            "pixel_roundtrip": True,
        }
        if role == "base_color":
            hist = np.bincount(pixels[:, :, 3].ravel(), minlength=256)
            record["alpha"] = {"min": int(pixels[:, :, 3].min()), "max": int(pixels[:, :, 3].max()),
                               "zero": int(hist[0]), "opaque": int(hist[255]),
                               "fractional": int(hist[1:255].sum())}
        if role == "normal":
            record["normal_conversion"] = "BC5 positive-Z reconstruction; DirectX-to-OpenGL green inversion"
        records.append(record)
    return {"schema_version": 1, "numpy": np.__version__, "pillow": pillow_version,
            "mip_policy": "PNG contains base mip only; preserve DDS source/mip count; runtime/cook mip generation is separate",
            "textures": records}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("references", type=Path, help="Source inventory JSON, or a JSON list of {path, role}")
    parser.add_argument("output", type=Path)
    parser.add_argument("report", type=Path)
    args = parser.parse_args()
    references = json.loads(args.references.read_text(encoding="utf-8"))
    if isinstance(references, dict):
        roles = {"DiffuseColor": "base_color", "NormalMap": "normal", "SpecularColor": "orm", "EmissiveColor": "emissive"}
        paths = {}
        for material in references["materials"]:
            for channel, path in material["textures"].items():
                role = roles[channel]
                if path in paths and paths[path] != role:
                    raise ValueError(f"Conflicting texture interpretation: {path}")
                paths[path] = role
        references = [{"path": path, "role": role} for path, role in paths.items()]
    report = prepare(args.source, references, args.output)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"textures": len(report["textures"]), "pixel_roundtrip": True}))


if __name__ == "__main__":
    main()
