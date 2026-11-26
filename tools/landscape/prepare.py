"""Extract the fixed Laubwerk Landscape scene to glTF, keeping repeated mesh references."""

import argparse
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
import json
from pathlib import Path
import re
from shutil import copyfileobj
from tempfile import NamedTemporaryFile, TemporaryDirectory
from urllib.request import urlopen

import numpy as np
from PIL import Image

import grass_cards

REV = "30cf4a0346ae5a80a2d7a530a3ef7d0fa4f70572"
BASE = f"https://raw.githubusercontent.com/mmp/pbrt-v4-scenes/{REV}/landscape/"
CREDIT = "Landscape by Jan-Walter Schliep, Burak Kahraman and Timm Dapper (Laubwerk), CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/)"
# Source coordinates in centimetres, +Y up.
CENTER = np.array([-2138.9744, 28.89301, -1707.7404])


def fetch(cache, name):
    if not re.fullmatch(r"(geometry|textures)/[\w. -]+", name) or ".." in name:
        raise ValueError(f"invalid source path {name}")
    path = cache / REV / name
    if not path.is_file():
        path.parent.mkdir(parents=True, exist_ok=True)
        with NamedTemporaryFile(dir=path.parent, delete=False) as tmp:
            try:
                with urlopen(BASE + name.replace(" ", "%20"), timeout=90) as response:
                    copyfileobj(response, tmp)
                tmp.close()
                Path(tmp.name).replace(path)
            finally:
                tmp.close()
                Path(tmp.name).unlink(missing_ok=True)
    return path


@dataclass
class Shape:
    file: str
    matrix: np.ndarray
    color: str | None
    alpha: str | None
    factor: tuple


def parse(source):
    prototypes = defaultdict(list)
    placements = []
    textures = {}
    stack = []
    group, matrix, color, factor = None, np.eye(4), None, (1.0, 1.0, 1.0)

    def param(body, name):
        match = re.search(r'"' + name + r'"\s*\[?\s*"([^"]+)"', body)
        return match[1] if match else None

    for match in re.finditer(r"^\s*([A-Z]\w*)\b(.*?)(?=^\s*[A-Z]\w*\b|\Z)", source, re.M | re.S):
        cmd, body = match.groups()
        if cmd == "AttributeBegin":
            stack.append((matrix, color, factor))
        elif cmd == "AttributeEnd":
            matrix, color, factor = stack.pop()
        elif cmd == "ConcatTransform":
            local = np.fromstring(body.split("[")[1].split("]")[0], sep=" ").reshape(4, 4).T
            matrix = matrix @ local
        elif cmd == "ObjectBegin":
            group = body.split('"')[1]
        elif cmd == "ObjectEnd":
            group = None
        elif cmd == "ObjectInstance":
            placements.append((body.split('"')[1], matrix))
        elif cmd == "Texture":
            textures[body.split('"')[1]] = param(body, "string filename")
        elif cmd == "Material":
            color = textures.get(param(body, "texture reflectance"))
            rgb = re.search(r'"rgb reflectance"\s*\[([^\]]+)\]', body)
            factor = tuple(map(float, rgb[1].split())) if rgb else (1.0, 1.0, 1.0)
            if '"dielectric"' in body:
                factor = (0.09, 0.18, 0.22)
        elif cmd == "Shape":
            prototypes[group].append(
                Shape(
                    param(body, "string filename"),
                    matrix,
                    color,
                    textures.get(param(body, "texture alpha")),
                    factor,
                )
            )
    return prototypes, placements


def read_ply(path):
    with path.open("rb") as f:
        header = []
        while (line := f.readline()) and line.strip() != b"end_header":
            header.append(line.decode("ascii").strip())
        fields = [s.split()[-1] for s in header if s.startswith("property float ")]
        if (
            fields not in (["x", "y", "z"], "x y z nx ny nz u v".split())
            or "format binary_little_endian 1.0" not in header
        ):
            raise ValueError(f"unsupported PLY header: {path}")
        counts = {s.split()[1]: int(s.split()[2]) for s in header if s.startswith("element ")}
        vertices = np.fromfile(f, "<f4", counts["vertex"] * len(fields)).reshape(-1, len(fields))
        faces = np.fromfile(f, np.dtype([("size", "u1"), ("indices", "<i4", 3)]), counts["face"])
        if len(faces) != counts["face"] or (faces["size"] != 3).any() or f.read(1):
            raise ValueError(f"invalid triangle list: {path}")
        indices = faces["indices"].astype("<u4")
        if len(fields) == 3:
            # One height field has positions only. Use area-weighted normals
            # and a planar UV spanning its authored 100 x 100 unit tile.
            a, b, c = vertices[indices].transpose(1, 0, 2)
            face_normals = np.cross(b - a, c - a)
            normals = np.zeros_like(vertices)
            for corner in indices.T:
                np.add.at(normals, corner, face_normals)
            normals /= np.maximum(1e-20, np.linalg.norm(normals, axis=1, keepdims=True))
            vertices = np.column_stack((vertices, normals, vertices[:, [0, 2]] / 100))
        return vertices, indices


def clustered(vertices, faces, resolution):
    """One representative vertex per spatial cell for the overview mesh."""
    xyz = vertices[:, :3]
    step = np.ptp(xyz, axis=0).max() / resolution
    if not step or len(faces) <= 1500:
        return vertices, faces
    cell = np.floor((xyz - xyz.min(axis=0)) / step).astype(np.int32)
    key = cell[:, 0] + (resolution + 2) * (cell[:, 1] + (resolution + 2) * cell[:, 2])
    _, first, remap = np.unique(key, return_index=True, return_inverse=True)
    a, b, c = reduced = remap[faces].T
    keep = (a != b) & (a != c) & (b != c)
    return vertices[first], reduced.T[keep].astype("<u4")


def export(cache, output, radius, full, overview):
    prototypes, placements = parse(fetch(cache, "geometry/geometry.pbrt").read_text())
    selected = [
        (g, m)
        for g, m in placements
        if full or overview or np.linalg.norm((m[:3, 3] - CENTER)[[0, 2]]) <= radius * 100
    ]

    def is_grass(name):
        return name and name.startswith(("meadow", "Brennnessel", "Short_mowed", "Corndog", "wicken"))

    tiles = defaultdict(lambda: defaultdict(list))
    if overview:
        for name, matrix in selected:
            cell = tuple(np.floor(matrix[[0, 2], 3] / 2400).astype(int))
            tiles[name][cell].append(matrix)
    representative = dict(selected)
    groups = {g for g, _ in selected}
    shapes = [s for g, ss in prototypes.items() if g is None or g in groups for s in ss]
    files = {f for s in shapes for f in (s.file, s.color, s.alpha) if f}
    with ThreadPoolExecutor(max_workers=8) as pool:
        list(pool.map(lambda name: fetch(cache, name), sorted(files)))
    data = {
        key: []
        for key in ("meshes", "nodes", "accessors", "bufferViews", "materials", "textures", "images")
    }
    data["asset"] = {"version": "2.0", "copyright": CREDIT}
    material_ids = {}

    def source_color(s):
        if s.color:
            with Image.open(fetch(cache, s.color)) as image:
                rgba = image.convert("RGB").convert("RGBA")
        else:
            rgb = tuple(round(255 * v ** (1 / 2.2)) for v in s.factor)
            rgba = Image.new("RGBA", (1, 1), (*rgb, 255))
        if s.alpha:
            with Image.open(fetch(cache, s.alpha)) as mask:
                alpha = (
                    mask.getchannel("A")
                    if "A" in mask.getbands() and s.color == s.alpha
                    else mask.convert("L")
                )
                rgba.putalpha(alpha.resize(rgba.size))
        return rgba

    def material(s, card=None):
        key = (s.color, s.alpha, s.factor) if card is None else s.file
        if key in material_ids:
            return material_ids[key]
        pbr = {"metallicFactor": 0, "roughnessFactor": 0.85, "baseColorFactor": [*s.factor, 1.0]}
        mat = {"doubleSided": card is None, "pbrMetallicRoughness": pbr}
        if s.color or s.alpha or card is not None:
            rgba = source_color(s) if card is None else card
            if s.alpha or card is not None:
                mat["alphaMode"] = "MASK"
            rgba.thumbnail((512, 512), Image.Resampling.LANCZOS)
            i = len(data["images"])
            uri = f"color-{i}.png"
            rgba.save(output / uri)
            data["images"].append({"uri": uri})
            data["textures"].append({"source": i})
            pbr["baseColorTexture"] = {"index": i}
        i = len(data["materials"])
        material_ids[key] = i
        data["materials"].append(mat)
        return i

    with (output / "mesh.bin").open("wb") as stream:

        def accessor(array, kind, component):
            i = len(data["accessors"])
            view = {"buffer": 0, "byteOffset": stream.tell(), "byteLength": array.nbytes}
            stream.write(array.tobytes())
            data["bufferViews"].append(view)
            item = {"bufferView": i, "componentType": component, "count": len(array), "type": kind}
            if kind == "VEC3":
                item.update(min=array.min(axis=0).tolist(), max=array.max(axis=0).tolist())
            data["accessors"].append(item)
            return i

        def write_mesh(v, faces, name, material_id):
            uv = v[:, 6:8].copy()
            uv[:, 1] = 1 - uv[:, 1]
            attrs = {
                "POSITION": accessor(v[:, :3], "VEC3", 5126),
                "NORMAL": accessor(v[:, 3:6], "VEC3", 5126),
                "TEXCOORD_0": accessor(uv, "VEC2", 5126),
            }
            prim = {
                "attributes": attrs,
                "indices": accessor(faces.ravel(), "SCALAR", 5125),
                "material": material_id,
            }
            data["meshes"].append({"name": name, "primitives": [prim]})
            return len(data["meshes"]) - 1

        mesh_ids = defaultdict(list)
        for group in [None, *sorted(groups)]:
            shapes = prototypes[group]
            if overview and shapes and group is not None:
                card_v, card_f, card = grass_cards.bake(
                    shapes,
                    representative[group],
                    lambda file: read_ply(fetch(cache, file)),
                    source_color,
                    size=96 if is_grass(group) else 256,
                )
                material_id = material(Shape(group, np.eye(4), None, None, (1.0, 1.0, 1.0)), card)
                for cell, matrices in tiles[group].items():
                    # Eight vertices per plant. Bake these cheap cards by spatial tile
                    # to bound draw count while preserving the authored transforms.
                    transforms = np.stack(matrices)
                    xyz = (
                        card_v[:, :3] @ transforms[:, :3, :3].transpose(0, 2, 1)
                        + transforms[:, None, :3, 3]
                    )
                    normals = card_v[:, 3:6] @ np.linalg.inv(transforms[:, :3, :3])
                    normals /= np.linalg.norm(normals, axis=2, keepdims=True)
                    uv = np.broadcast_to(card_v[:, 6:8], (*xyz.shape[:2], 2))
                    vertices = np.concatenate((xyz, normals, uv), axis=2).reshape(-1, 8).astype("<f4")
                    faces = card_f + len(card_v) * np.arange(len(matrices), dtype="<u4")[:, None, None]
                    mesh_ids[None].append(
                        (
                            write_mesh(vertices, faces.reshape(-1, 3), f"{group}-{cell}", material_id),
                            np.eye(4),
                        )
                    )
                continue
            for shape in shapes:
                v, faces = read_ply(fetch(cache, shape.file))
                if overview:
                    v, faces = clustered(v, faces, 180)
                    if not len(faces):
                        continue
                elif group is None and not (full or overview):
                    world = v[:, :3] @ shape.matrix[:3, :3].T + shape.matrix[:3, 3]
                    xz = world[:, [0, 2]][faces] - CENTER[[0, 2]]
                    extent = max(radius + 5, 12) * 100
                    faces = faces[((xz.min(axis=1) <= extent) & (xz.max(axis=1) >= -extent)).all(axis=1)]
                    if not len(faces):
                        continue
                    used, remap = np.unique(faces, return_inverse=True)
                    v, faces = v[used], remap.reshape(-1, 3).astype("<u4")
                mesh_ids[group].append((write_mesh(v, faces, shape.file, material(shape)), shape.matrix))
        length = stream.tell()
    root = np.diag([0.01, 0.01, 0.01, 1.0])
    root[:3, 3] = -CENTER / 100
    for group, placement in [(None, np.eye(4)), *selected]:
        for mesh, local in mesh_ids[group]:
            transform = root @ placement @ local
            data["nodes"].append(
                {
                    "name": f"placement-{len(data['nodes'])}",
                    "mesh": mesh,
                    "matrix": transform.T.ravel().tolist(),
                }
            )
    data.update(
        scene=0,
        scenes=[{"nodes": list(range(len(data["nodes"])))}],
        buffers=[{"uri": "mesh.bin", "byteLength": length}],
    )
    triangles = [
        data["accessors"][mesh["primitives"][0]["indices"]]["count"] // 3 for mesh in data["meshes"]
    ]
    data["asset"]["extras"] = {
        "source": BASE,
        "credit": CREDIT,
        "mode": "overview" if overview else "full" if full else "meadow",
        "source_placements": len(placements),
        "selected_placements": len(selected),
        "unique_triangles": sum(triangles),
        "placed_triangles": sum(triangles[n["mesh"]] for n in data["nodes"]),
    }
    (output / "landscape.gltf").write_text(json.dumps(data, separators=(",", ":")), encoding="utf-8")
    print(f"{len(selected)} plant placements, {len(data['meshes'])} meshes, {length:,} geometry bytes")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--cache", type=Path, default=Path("build/landscape-cache"))
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument(
        "--radius", type=float, help="cut out a meadow patch of this radius in metres at source detail"
    )
    modes.add_argument("--full", action="store_true", help="all placed geometry at source detail")
    modes.add_argument(
        "--overview",
        action="store_true",
        help="all placements with baked vegetation cards and coarsened terrain (default)",
    )
    args = parser.parse_args()
    if args.radius is not None and (not np.isfinite(args.radius) or args.radius <= 0):
        parser.error("radius must be positive and finite")
    if args.output.exists():
        parser.error("choose a new output directory")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with TemporaryDirectory(dir=args.output.parent) as tmp:
        staged = Path(tmp) / "asset"
        staged.mkdir()
        export(
            args.cache,
            staged,
            args.radius,
            args.full,
            args.overview or (not args.full and args.radius is None),
        )
        staged.rename(args.output)


if __name__ == "__main__":
    main()
