"""Build the Namaqualand comparison package with source rock and terrain meshes."""
import argparse
import json
import os
from pathlib import Path
import shutil
import struct
import tempfile


# WalkMeshNNN in the UE reference is numbered in glTF mesh order.
CONTACT_MATERIALS = {
    26: "namaqualand_boulder_02", 27: "namaqualand_boulder_03",
    28: "namaqualand_boulder_06", 29: "namaqualand_boulder_03",
    30: "namaqualand_cliff_02", 31: "namaqualand_cliff_01", 32: "cliff_side",
    33: "landscape_main", 34: "namaqualand_boulders_01", 35: "namaqualand_boulder_06",
    36: "namaqualand_boulder_04", 37: "namaqualand_boulder_05",
    38: "namaqualand_rocks_01", 39: "namaqualand_stones_01",
    40: "namaqualand_stones_01", 41: "namaqualand_boulders_01",
    42: "namaqualand_rocks_01", 80: "landscape_background", 81: "damp_sand",
}


def prepare(source, gltf, destination):
    if destination.exists() or destination.resolve().is_relative_to(source.resolve()):
        raise ValueError("Choose a new output package outside the source")
    scene = json.loads((source / "scene.json").read_bytes())
    model = json.loads(gltf.read_bytes())
    primitives = [(i, p) for i, mesh in enumerate(model["meshes"]) for p in mesh["primitives"]]
    contact = set()
    seen = set()
    for (index, primitive), cooked in zip(primitives, scene["primitives"], strict=True):
        if index not in CONTACT_MATERIALS:
            continue
        material = model["materials"][primitive["material"]]["name"]
        mesh = cooked["mesh"]["hash"]
        with (source / "objects" / mesh).open("rb") as handle:
            magic, _, indices = struct.unpack("<8sII", handle.read(16))
        if (material != CONTACT_MATERIALS[index] or magic != b"RMESH001"
                or indices != model["accessors"][primitive["indices"]]["count"]):
            raise ValueError(f"Source and cooked geometry disagree for WalkMesh{index:03}")
        contact.add(mesh)
        seen.add(index)
    if seen != CONTACT_MATERIALS.keys():
        raise ValueError("Incomplete Namaqualand contact geometry")
    scene["mesh_lods"] = [lod for lod in scene["mesh_lods"] if lod["source"]["hash"] not in contact]
    # Only publish a complete package. Existing results are never mixed with new inputs.
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as staging:
        package = Path(staging) / "package"
        objects = package / "objects"
        objects.mkdir(parents=True)
        for obj in (source / "objects").iterdir():
            try:
                os.link(obj, objects / obj.name)
            except OSError:
                shutil.copyfile(obj, objects / obj.name)
        (package / "scene.json").write_text(json.dumps(scene), encoding="utf-8")
        package.rename(destination)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("source", "gltf", "destination"):
        parser.add_argument(arg, type=Path)
    args = parser.parse_args()
    prepare(args.source, args.gltf, args.destination)
