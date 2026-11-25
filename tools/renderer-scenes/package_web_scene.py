"""Publish one cooked reference scene alongside the static browser viewer."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import struct
import tempfile

ROOT = Path(__file__).resolve().parents[2]
SPONZA_UPSTREAM = "https://github.com/KhronosGroup/glTF-Sample-Assets/blob/main/"
SCENES = {
    "bistro": {
        "look": "lumberyard-day", "defaultView": "bistro-corner",
        "views": ["bistro-corner", "street", "overview"],
        "credits": "Amazon Lumberyard Bistro (2017), Amazon Lumberyard / NVIDIA ORCA. CC BY 4.0. Geometry, textures and lighting prepared for Sashiko.",
        "source": "https://developer.nvidia.com/orca/amazon-lumberyard-bistro",
        "license": "https://creativecommons.org/licenses/by/4.0/",
    },
    "sponza": {
        "look": "o3de-atrium", "defaultView": "gallery", "views": ["gallery", "renderer"],
        "credits": "Sponza, © 2016 Crytek. Frank Meinl, based on Marko Dabrovic's atrium. PBR textures by Alexandre Pestana. glTF conversion from Khronos glTF Sample Assets. Cooked geometry and baked lighting for Sashiko.",
        "source": SPONZA_UPSTREAM + "Models/Sponza/README.md",
        "license": SPONZA_UPSTREAM + "LICENSES/LicenseRef-CRYENGINE-Agreement.txt",
    },
}


def object_hash(value):
    if not isinstance(value, str) or len(value) != 64 or any(c not in "0123456789abcdef" for c in value):
        raise ValueError("invalid object SHA-256")
    return value


def asset_path(manifest, record):
    root = manifest.parent.resolve()
    path = (root / record["path"]).resolve(strict=True)
    if not path.is_relative_to(root) or not path.is_file():
        raise ValueError(f"invalid asset path in {manifest}: {path}")
    return path


class Objects:
    def __init__(self, directory):
        self.directory = directory
        directory.mkdir(parents=True, exist_ok=True)
        self.verified = set()

    def put(self, source, expected=None, *, alpha=False):
        """Publish verified immutable objects before replacing the scene descriptor."""
        h = hashlib.sha256()
        size = 0
        maximum = 0
        temp = None
        try:
            with tempfile.NamedTemporaryFile(dir=self.directory, delete=False) as stream:
                temp = Path(stream.name)
                with source.open("rb") as file:
                    while chunk := file.read(4 * 1024 * 1024):
                        stream.write(chunk)
                        h.update(chunk)
                        size += len(chunk)
                        if alpha and maximum < 255:
                            maximum = max(maximum, max(chunk[3::4], default=0))
            record = {"byte_length": size, "sha256": h.hexdigest()}
            if expected is not None and any(record[k] != expected[k] for k in record):
                raise ValueError(f"object size or digest mismatch: {source}")
            path = self.directory / record["sha256"]
            # A reader already holding the previous scene can still finish loading.
            if record["sha256"] not in self.verified:
                temp.replace(path)
                self.verified.add(record["sha256"])
            return record, maximum
        finally:
            if temp is not None:
                temp.unlink(missing_ok=True)


def lighting_dependencies(data):
    if len(data) < 48 or data[:8] != b"RSURF003":
        raise ValueError("invalid baked lighting index")
    _, count = struct.unpack_from("<II", data, 8)
    yield data[16:48].hex(), None
    offset = 48
    for _ in range(count):
        if len(data) - offset < 44:
            raise ValueError("truncated baked lighting page record")
        width, height, placements = struct.unpack_from("<III", data, offset)
        yield data[offset + 12:offset + 44].hex(), width * height * 16
        offset += 44 + placements * 4
    if offset != len(data):
        raise ValueError("invalid baked lighting index length")


def publish(args):
    destination = args.site / "scenes" / args.scene
    store = Objects(destination / "objects")
    descriptor = dict(schema="sashiko-web-scene-v1", id=args.scene, **SCENES[args.scene])
    descriptor["geometry"], _ = store.put(args.manifest)
    geometry = json.loads((store.directory / descriptor["geometry"]["sha256"]).read_bytes())
    for name in ("vertices", "indices", "lightmap_uv"):
        if (record := geometry.get(name)) is not None:
            store.put(asset_path(args.manifest, record), record)
    maximum_alpha = {}
    for record in geometry["images"]:
        alpha = record.get("format", "rgba8") == "rgba8" and record["role"] == "base_color"
        image, maximum = store.put(asset_path(args.manifest, record), record, alpha=alpha)
        if alpha:
            maximum_alpha[image["sha256"]] = maximum
    descriptor["maximumAlpha"] = maximum_alpha
    descriptor["environment"], _ = store.put(args.ibl_manifest)
    env = json.loads((store.directory / descriptor["environment"]["sha256"]).read_bytes())
    for record in env["images"].values():
        store.put(asset_path(args.ibl_manifest, record), record)

    descriptor["reflections"], _ = store.put(args.reflection_probes)
    descriptor["lighting"], _ = store.put(args.baked_lighting)
    generation = json.loads((store.directory / descriptor["lighting"]["sha256"]).read_bytes())
    if generation["schema"] != "bistro-surface-lighting-v1":
        raise ValueError("unsupported lighting generation")
    objects = args.baked_lighting.parent / "lighting-objects"
    index = object_hash(generation["index_sha256"])
    stored, _ = store.put(objects / index)
    if stored["sha256"] != index:
        raise ValueError("corrupt lighting index")
    for key, length in lighting_dependencies((store.directory / index).read_bytes()):
        stored, _ = store.put(objects / key)
        if key != stored["sha256"] or length not in (None, stored["byte_length"]):
            raise ValueError(f"invalid lighting object {key}")

    if args.scene == "bistro":
        for name in ("LICENSE.txt", "README.txt"):
            shutil.copyfile(ROOT / "assets/bistro/source" / name, destination / name)
    text = json.dumps(descriptor, indent=2, ensure_ascii=False) + "\n"
    with tempfile.NamedTemporaryFile(dir=destination, delete=False) as stream:
        temp = Path(stream.name)
        stream.write(text.encode("utf-8"))
    try:
        temp.replace(destination / "scene.json")
    finally:
        temp.unlink(missing_ok=True)
    print(f"{args.scene}: {len(store.verified)} objects, {destination / 'scene.json'}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scene", choices=SCENES, required=True)
    for name in ("manifest", "ibl-manifest", "baked-lighting", "reflection-probes", "site"):
        parser.add_argument("--" + name, required=True, type=Path)
    publish(parser.parse_args())


if __name__ == "__main__":
    main()
