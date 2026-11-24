"""Cook static glTF/DDS Bistro inputs into consumer-owned immutable GPU streams.

The format is intentionally outside Renderer. All semantic scans happen here and
at the consumer's load/update boundary, never inside its ordinary draw loop.
"""

from __future__ import annotations

import argparse
from contextlib import nullcontext
import hashlib
from io import BytesIO
import json
import tempfile
from pathlib import Path
import struct
from urllib.parse import unquote
import zipfile

import numpy as np
from PIL import Image, __version__ as pillow_version

import texture_codec as codec
from lightmap_uv import parameterize, TEXELS_PER_METRE
from importlib.metadata import version
from compare_geometry import Gltf
from mikk import Generator, UPSTREAM_COMMIT, source_hashes


VERTEX_STRIDE = 48
TOOL_HASHES = {
    name: hashlib.sha256((Path(__file__).parent / name).read_text(encoding="utf-8").encode("utf-8")).hexdigest()
    for name in ("cook.py", "lightmap_uv.py", "mikk.py", "compare_geometry.py", "texture_codec.py", "reflection_codec.py")
}
MATERIAL_TEXTURES = (
    ("base_color_texture", "base_color", "pbrMetallicRoughness", "baseColorTexture"),
    ("normal_texture", "normal", None, "normalTexture"),
    ("orm_texture", "orm", "pbrMetallicRoughness", "metallicRoughnessTexture"),
    ("occlusion_texture", "orm", None, "occlusionTexture"),
    ("emissive_texture", "emissive", None, "emissiveTexture"),
)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def relative_file(root, uri):
    require(isinstance(uri, str) and uri and ":" not in uri and not uri.startswith(("/", "\\")),
            "Only local relative asset URIs are supported")
    path = (root / unquote(uri)).resolve()
    require(path.is_relative_to(root.resolve()), f"Asset URI escapes glTF directory: {uri}")
    return path


def checked_gltf(path):
    path = Path(path).resolve()
    data = json.loads(path.read_text(encoding="utf-8"))
    require(not data.get("animations") and not data.get("skins"), "Only static unskinned inputs are supported")
    for buffer in data.get("buffers", []):
        relative_file(path.parent, buffer.get("uri"))
    for image in data.get("images", []):
        relative_file(path.parent, image.get("uri"))
    # The existing audited accessor reader handles interleaving, sparse entries,
    # normalized components, finite values and buffer-view bounds.
    return Gltf(path)


def normalized(values, label):
    values = np.asarray(values, dtype=np.float64)
    lengths = np.linalg.norm(values, axis=1)
    require(np.isfinite(values).all() and np.all(lengths > 1e-12), f"Invalid {label}")
    return values / lengths[:, None]


def fallback_tangents(normals):
    """Stable orthogonal axis for a corner with no usable UV tangent direction."""
    axes = np.eye(3)[np.argmin(np.abs(normals), axis=1)]
    axes -= normals * np.sum(normals * axes, axis=1)[:, None]
    return normalized(axes, "fallback tangent")


def cook_primitive(gltf, primitive, world, generator, lightmap=None):
    require(primitive.get("mode", 4) == 4, "Only triangle primitives are supported")
    require(not primitive.get("targets") and not primitive.get("extensions"),
            "Morph/compressed/extended primitives need an explicit cook contract")
    attributes = primitive["attributes"]
    require(all(key in attributes for key in ("POSITION", "NORMAL", "TEXCOORD_0")),
            "Position, authored normal and material UV are required")
    positions = gltf.accessor(attributes["POSITION"])
    normals = gltf.accessor(attributes["NORMAL"])
    uv = gltf.accessor(attributes["TEXCOORD_0"])
    require(positions.shape == normals.shape and positions.shape[1] == 3 and
            uv.shape == (len(positions), 2), "Inconsistent vertex attribute shapes")
    require(len(positions) > 0, "Empty primitive")
    normals = normalized(normals, "authored normal")
    if "indices" in primitive:
        accessor = gltf.data["accessors"][primitive["indices"]]
        require(accessor["type"] == "SCALAR" and accessor["componentType"] in (5121, 5123, 5125)
                and not accessor.get("normalized"), "Invalid index accessor")
        indices = gltf.accessor(primitive["indices"]).ravel()
    else:
        indices = np.arange(len(positions), dtype=np.uint32)
    require(len(indices) > 0 and len(indices) % 3 == 0 and np.all(indices < len(positions)),
            "Triangle indices are empty, incomplete or out of range")
    corners = np.concatenate((positions, normals, uv), axis=1)[indices]
    positions = corners[:, :3]
    normals = corners[:, 3:6]
    uv = corners[:, 6:8]
    triangles = positions.reshape(-1, 3, 3)
    tex = uv.reshape(-1, 3, 2)
    area = np.linalg.norm(np.cross(triangles[:, 1] - triangles[:, 0],
                                   triangles[:, 2] - triangles[:, 0]), axis=1)
    du, dv = tex[:, 1] - tex[:, 0], tex[:, 2] - tex[:, 0]
    uv_area = du[:, 0] * dv[:, 1] - du[:, 1] * dv[:, 0]
    degenerate_geometry = area <= 1e-20
    degenerate_uv = np.abs(uv_area) <= 1e-20
    tangent = generator(corners).astype(np.float64)
    require(np.all(np.isin(tangent[:, 3], (-1, 1))), "MikkTSpace returned an invalid handedness")
    projected = tangent[:, :3] - normals * np.sum(normals * tangent[:, :3], axis=1)[:, None]
    lengths = np.linalg.norm(projected, axis=1)
    require(np.isfinite(projected).all(), "MikkTSpace returned a non-finite tangent")
    invalid = lengths <= 1e-10
    allowed = np.repeat(degenerate_geometry | degenerate_uv, 3)
    if np.any(invalid):
        projected[invalid] = fallback_tangents(normals[invalid])
    tangent[:, :3] = normalized(projected, "local tangent")

    linear = world[:3, :3]
    determinant = np.linalg.det(linear)
    require(np.isfinite(determinant) and abs(determinant) > 1e-12, "Singular mesh transform")
    # Generate local Mikk first: a nonuniform world transform must not change
    # its local angle weights and hence the basis used by the baked normal map.
    world_positions = positions @ linear.T + world[:3, 3]
    world_normals = normalized(normals @ np.linalg.inv(linear), "world normal")
    world_tangents = tangent[:, :3] @ linear.T
    world_tangents -= world_normals * np.sum(world_normals * world_tangents, axis=1)[:, None]
    world_tangents = normalized(world_tangents, "world tangent")
    signs = tangent[:, 3:4] * (1 if determinant > 0 else -1)
    vertices = np.concatenate((world_positions, world_normals, world_tangents, signs, uv), axis=1).astype("<f4")
    require(np.isfinite(vertices).all(), "World-space vertex exceeds finite float32 range")
    # Mikk returns per-corner tangent frames. Reindex ALL output attributes,
    # never average/overwrite a source vertex's distinct mirrored UV frames.
    # Index in local space so repeated placements share the same optional UV
    # stream, including placements with mirrored winding.
    key = np.column_stack((corners, tangent)).astype("<f4")
    if lightmap is not None:
        key = np.column_stack((key, lightmap.reshape(-1, 2))).astype("<f4")
    _, first, remap = np.unique(key.view(f"V{key.strides[0]}").ravel(), return_index=True, return_inverse=True)
    vertices = vertices[first]
    indices = remap.astype("<u4").reshape(-1, 3)
    if determinant < 0:
        indices = indices[:, [0, 2, 1]].copy()
    indices = indices.ravel()
    require(np.max(np.abs(np.sum(vertices[:, 3:6] * vertices[:, 6:9], axis=1))) < 2e-6,
            "Cooked normal/tangent is not orthogonal")
    record = {"vertex_count": len(vertices), "index_count": len(indices),
              "max_index": int(indices.max()),
              "bounds_min": vertices[:, :3].min(axis=0).astype(float).tolist(),
              "bounds_max": vertices[:, :3].max(axis=0).astype(float).tolist(),
              "tangent_fallback_corners": int(invalid.sum()),
              "collapsed_mikk_basis_corners": int((invalid & ~allowed).sum()),
              "degenerate_uv_faces": int(degenerate_uv.sum()),
              "degenerate_geometry_faces": int(degenerate_geometry.sum()),
              "reflected_transform": bool(determinant < 0)}
    return vertices.tobytes(), indices.tobytes(), record, (None if lightmap is None else lightmap.reshape(-1, 2)[first].astype("<f4").tobytes())


def scalar(value, label, low=None, high=None):
    require(isinstance(value, (int, float)) and not isinstance(value, bool) and np.isfinite(value),
            f"Invalid material scalar: {label}")
    require((low is None or value >= low) and (high is None or value <= high),
            f"Material scalar out of range: {label}")
    return float(value)


def factors(values, count, label, low=None, high=None):
    require(len(values) == count, f"Invalid {label} width")
    return [scalar(value, label, low, high) for value in values]


def check_sampler(sampler, origin):
    require(isinstance(sampler, dict), f"{origin}: invalid sampler {sampler!r}")
    require(not sampler.get("extensions"), f"{origin}: unsupported sampler extensions {sampler.get('extensions')!r}")
    for key, allowed in (("wrapS", (10497, 33071, 33648)), ("wrapT", (10497, 33071, 33648)),
                         ("minFilter", (9728, 9729, 9984, 9985, 9986, 9987)), ("magFilter", (9728, 9729))):
        if key in sampler:
            value = sampler[key]
            require(type(value) is int and value in allowed, f"{origin}: invalid {key}={value!r}")


def cook_materials(data):
    roles = {}
    materials = []
    textures = data.get("textures", [])
    images = data.get("images", [])
    samplers = data.get("samplers", [])
    checked_samplers = set()
    for index, source in enumerate(data.get("materials", [])):
        require(not source.get("extensions"), "Material extensions need an explicit cook contract")
        pbr = source.get("pbrMetallicRoughness", {})
        alpha = source.get("alphaMode", "OPAQUE")
        require(alpha in ("OPAQUE", "MASK", "BLEND"), "Invalid material alpha mode")
        material = {"id": index, "name": source.get("name", f"material-{index}"),
                    "base_factor": factors(pbr.get("baseColorFactor", [1, 1, 1, 1]), 4, "base factor", 0, 1),
                    "emissive": factors(source.get("emissiveFactor", [0, 0, 0]), 3, "emissive", 0, 1),
                    "normal_scale": scalar(source.get("normalTexture", {}).get("scale", 1), "normal scale"),
                    "metallic": scalar(pbr.get("metallicFactor", 1), "metallic", 0, 1),
                    "roughness": scalar(pbr.get("roughnessFactor", 1), "roughness", 0, 1),
                    "occlusion_strength": scalar(source.get("occlusionTexture", {}).get("strength", 1), "AO", 0, 1),
                    "alpha_cutoff": scalar(source.get("alphaCutoff", 0.5), "alpha cutoff", 0),
                    "alpha_mode": alpha, "double_sided": bool(source.get("doubleSided", False))}
        for target, role, parent, field in MATERIAL_TEXTURES:
            value = (source.get(parent, {}) if parent else source).get(field)
            texture_id = None
            if value is not None:
                origin = f"Material {material['name']} slot {field}"
                require(not value.get("extensions"), f"{origin}: unsupported extensions {value.get('extensions')!r}")
                require(value.get("texCoord", 0) == 0, f"{origin}: unsupported texCoord={value.get('texCoord')!r}")
                texture_id = value["index"]
                require(type(texture_id) is int and 0 <= texture_id < len(textures),
                        f"{origin}: invalid texture index {texture_id!r}")
                texture = textures[texture_id]
                require(not texture.get("extensions"), f"{origin}: unsupported texture extensions {texture.get('extensions')!r}")
                if "sampler" in texture:
                    sampler = texture["sampler"]
                    require(type(sampler) is int and 0 <= sampler < len(samplers),
                            f"{origin}: invalid sampler index {sampler!r}")
                    if sampler not in checked_samplers:
                        check_sampler(samplers[sampler], f"{origin} sampler {sampler}")
                        checked_samplers.add(sampler)
                image_id = texture.get("source")
                require(type(image_id) is int and 0 <= image_id < len(images), f"{origin}: invalid image {image_id!r}")
                previous = roles.get(image_id, role)
                if {previous, role} == {"base_color", "emissive"}:
                    # Both use sRGB RGB; preserve the base-color alpha for both bindings.
                    roles[image_id] = "base_color"
                else:
                    require(previous == role, f"{origin}: image {image_id} already has incompatible role {previous}")
                    roles[image_id] = role
            material[target] = texture_id
        ao, orm = material["occlusion_texture"], material["orm_texture"]
        require(ao is None or ao == orm, f"Material {material['name']} slot occlusionTexture={ao}: "
                f"packed ORM requires the same texture as metallicRoughnessTexture={orm}")
        materials.append(material)
    require(set(roles) == set(range(len(images))), "Unreferenced images need an explicit preservation role")
    for texture in textures:
        require(not texture.get("extensions") and texture.get("source") in roles, "Unsupported texture source")
        sampler = texture.get("sampler")
        require(sampler is None or type(sampler) is int and 0 <= sampler < len(samplers),
                "Invalid sampler reference")
    for index, sampler in enumerate(samplers):
        if index not in checked_samplers:
            check_sampler(sampler, f"Sampler {index}")
    return materials, roles


def converted_pixels(image, role, fourcc=None):
    pixels = np.asarray(image.convert("RGBA")).copy()
    if role == "normal":
        require(fourcc == "ATI2", f"Unaudited normal DDS encoding: {fourcc}")
        xy = pixels[:, :, :2].astype(np.float64) / 127.5 - 1
        z = np.sqrt(np.maximum(0, 1 - np.sum(xy * xy, axis=2)))
        pixels[:, :, 1] = 255 - pixels[:, :, 1]
        pixels[:, :, 2] = np.rint((z + 1) * 127.5).astype(np.uint8)
    if role != "base_color":
        pixels[:, :, 3] = 255
    return pixels


def dds_levels(raw, role):
    require(len(raw) >= 128 and raw[:4] == b"DDS " and struct.unpack_from("<I", raw, 4)[0] == 124,
            "Invalid legacy DDS header")
    height, width = struct.unpack_from("<II", raw, 12)
    require(width > 0 and height > 0 and width & (width - 1) == 0 and height & (height - 1) == 0,
            "Bistro DDS dimensions must be positive powers of two")
    raw_count = struct.unpack_from("<I", raw, 28)[0]
    count = max(1, raw_count)
    require(count == max(width, height).bit_length(), "Source DDS is not a complete mip chain")
    fourcc = raw[84:88].decode("ascii")
    require(fourcc in ("DXT1", "DXT5", "ATI2"), f"Unsupported DDS compression: {fourcc}")
    block = 8 if fourcc == "DXT1" else 16
    offset = 128
    expected = sum(max(1, (max(1, width >> level) + 3) // 4) *
                   max(1, (max(1, height >> level) + 3) // 4) * block for level in range(count))
    require(len(raw) == 128 + expected, "DDS mip payload length differs from declared chain")
    metadata = {"fourcc": fourcc, "mip_levels_raw": raw_count, "mip_levels": count,
                "byte_length": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}

    def levels():
        nonlocal offset
        for level in range(count):
            w, h = max(1, width >> level), max(1, height >> level)
            size = max(1, (w + 3) // 4) * max(1, (h + 3) // 4) * block
            header = bytearray(raw[:128])
            struct.pack_into("<II", header, 12, h, w)
            struct.pack_into("<I", header, 20, size)
            struct.pack_into("<I", header, 28, 1)
            with Image.open(BytesIO(header + raw[offset:offset + size])) as image:
                require(image.size == (w, h), "DDS decoder dimensions differ")
                pixels = converted_pixels(image, role, fourcc)
            offset += size
            yield pixels
    return metadata, levels()


def generated_levels(pixels, role):
    """Filter glTF sources without authored mips in linear color or normal space."""
    while True:
        yield pixels
        h, w = pixels.shape[:2]
        if (w, h) == (1, 1):
            return
        values = pixels.astype(np.float32) / 255
        rgb = values[:, :, :3]
        if role in ("base_color", "emissive"):
            values[:, :, :3] = np.where(rgb <= .04045, rgb / 12.92, ((rgb + .055) / 1.055) ** 2.4)
        size = max(1, w // 2), max(1, h // 2)
        values = np.stack([np.asarray(Image.fromarray(band).resize(size, Image.Resampling.BOX))
                           for band in values.transpose(2, 0, 1)], axis=-1)
        rgb = values[:, :, :3]
        if role in ("base_color", "emissive"):
            values[:, :, :3] = np.where(rgb <= .0031308, 12.92 * rgb, 1.055 * rgb ** (1 / 2.4) - .055)
        elif role == "normal":
            normal = 2 * rgb - 1
            values[:, :, :3] = .5 + .5 * normal / np.maximum(np.linalg.norm(normal, axis=2, keepdims=True), 1e-7)
        pixels = np.rint(np.clip(values, 0, 1) * 255).astype(np.uint8)


def cook_images(gltf, roles, output, archive, policy):
    codec.validate_policy(policy, [i["uri"] for i in gltf.data.get("images", [])])
    entries = {}
    if archive:
        for entry in archive.infolist():
            if entry.filename.lower().endswith(".dds"):
                key = Path(entry.filename).stem.casefold()
                require(key not in entries, f"Ambiguous source DDS stem: {key}")
                entries[key] = entry
    records = []
    (output / "images").mkdir()
    for index, source in enumerate(gltf.data.get("images", [])):
        role = roles[index]
        path = relative_file(gltf.path.parent, source["uri"])
        with Image.open(path) as image:
            baseline = np.asarray(image.convert("RGBA"))
        record = {"id": index, "source_uri": source["uri"], "source_sha256": digest(path),
                  "role": role, "colorspace": "srgb" if role in ("base_color", "emissive") else "linear",
                  "width": baseline.shape[1], "height": baseline.shape[0],
                  "path": f"images/{index:04}.bin", "mips": []}
        if archive:
            entry = entries.get(path.stem.casefold())
            require(entry is not None, f"No original DDS for {source['uri']}")
            raw = archive.read(entry)
            metadata, levels = dds_levels(raw, role)
            record["source_dds"] = {"archive_path": entry.filename, **metadata}
        else:
            levels = generated_levels(baseline, role)
        source_format = codec.DDS_FORMAT[metadata["fourcc"]] if archive else None
        fmt = codec.choose_format(policy, source["uri"], role, source_format)
        passthrough = source_format == fmt
        record["format"] = fmt
        record["normal_encoding"] = ("xy_directx" if passthrough else "xy") if fmt == "bc5" else "xyz"
        source_at = 128
        offset = 0
        with (output / record["path"]).open("wb") as stream:
            for level, pixels in enumerate(levels):
                if level == 0:
                    require(np.array_equal(pixels, baseline), f"DDS base conversion differs from fixed PNG: {path.name}")
                h, w = pixels.shape[:2]
                if passthrough:
                    size = codec.mip_bytes(fmt, w, h)
                    raw_pixels = raw[source_at:source_at + size]
                    source_at += size
                else:
                    raw_pixels = codec.encode(pixels, fmt)
                if fmt != "rgba8" and (role == "base_color" or (max(w, h) <= 128 and "bake_mip" not in record)):
                    decoded = codec.decode(raw_pixels, fmt, w, h)
                    if max(w, h) <= 128 and "bake_mip" not in record:
                        relative = f"images/{index:04}.bake-rgba8"
                        data = decoded.tobytes()
                        (output / relative).write_bytes(data)
                        record["bake_mip"] = {"level": level, "path": relative, "byte_length": len(data),
                                              "sha256": hashlib.sha256(data).hexdigest()}
                else:
                    decoded = pixels
                stream.write(raw_pixels)
                mip = {"level": level, "width": pixels.shape[1], "height": pixels.shape[0],
                       "offset": offset, "byte_length": len(raw_pixels)}
                if role == "base_color":
                    mip["alpha_min"] = int(decoded[:, :, 3].min())
                    mip["alpha_max"] = int(decoded[:, :, 3].max())
                record["mips"].append(mip)
                offset += len(raw_pixels)
        record.update(mip_levels=len(record["mips"]), byte_length=offset,
                      sha256=digest(output / record["path"]))
        records.append(record)
    return records


def _cook(gltf_path, output, generator, source_zip, texels_per_metre, texture_policy):
    output = Path(output).resolve()
    require(not output.exists() or not any(output.iterdir()), "Cook output must be a new or empty directory")
    gltf = checked_gltf(gltf_path)
    materials, roles = cook_materials(gltf.data)
    # A glTF node index is stable within its source asset even without a name.
    for index, node in enumerate(gltf.data.get("nodes", [])):
        if "mesh" in node and not node.get("name"):
            node["name"] = f"node[{index}]"
    instances = gltf.mesh_instances()
    source_meshes = gltf.data.get("meshes", [])
    require({mesh for mesh, _ in instances.values()} == set(range(len(source_meshes))),
            "The default scene does not preserve every source mesh")
    output.mkdir(parents=True, exist_ok=True)
    mesh_records, primitive_records = [], []
    node_ids = {node["name"]: index for index, node in enumerate(gltf.data.get("nodes", [])) if node.get("name")}
    layouts = {}
    uv_offsets = {}
    with ((output / "vertices.bin").open("wb") as vertex_stream,
          (output / "indices.bin").open("wb") as index_stream,
          (output / "lightmap_uv.bin").open("wb") as uv_stream):
        for name, (source_mesh, world) in sorted(instances.items(), key=lambda item: (item[1][0], node_ids[item[0]])):
            mesh_id = len(mesh_records)
            mesh = {"id": mesh_id, "source_mesh": source_mesh, "node": node_ids[name], "name": name,
                    "world_transform": world.T.ravel().tolist(), "primitives": []}
            for source_primitive, primitive in enumerate(source_meshes[source_mesh]["primitives"]):
                material = primitive.get("material")
                require(isinstance(material, int) and 0 <= material < len(materials), "Missing or invalid primitive material")
                try:
                    layout = None
                    if materials[material]["alpha_mode"] != "BLEND":
                        key = source_mesh, source_primitive
                        if key not in layouts:
                            layouts[key] = (*parameterize(gltf, primitive, world, texels_per_metre), np.linalg.inv(world[:3, :3]))
                        layout = layouts[key]
                    vertices, indices, record, lightmap = cook_primitive(
                        gltf, primitive, world, generator, None if layout is None else layout[0])
                    if layout is not None:
                        _, extent, origin, inverse = layout
                        scale = np.linalg.norm(world[:3, :3] @ inverse, ord=2)
                        digest_uv = hashlib.sha256(lightmap).digest()
                        if digest_uv not in uv_offsets:
                            uv_offsets[digest_uv] = uv_stream.tell()
                            uv_stream.write(lightmap)
                        record["lightmap"] = {"uv_offset": uv_offsets[digest_uv],
                                              "extent": [max(1, int(np.ceil(v * scale - 1e-8))) for v in extent],
                                              "source": origin}
                except ValueError as error:
                    raise ValueError(f"Mesh {name}, primitive {source_primitive}: {error}") from error
                record.update(id=len(primitive_records), mesh=mesh_id, source_primitive=source_primitive,
                              material=material, vertex_offset=vertex_stream.tell(), index_offset=index_stream.tell())
                vertex_stream.write(vertices)
                index_stream.write(indices)
                mesh["primitives"].append(record["id"])
                primitive_records.append(record)
            mesh_records.append(mesh)
    del layouts
    archive_context = zipfile.ZipFile(source_zip) if source_zip else nullcontext(None)
    with archive_context as archive:
        images = cook_images(gltf, roles, output, archive, texture_policy)
    textures = [{"id": index, "source": item["source"], "sampler": item.get("sampler")}
                for index, item in enumerate(gltf.data.get("textures", []))]
    summary = {"mesh_count": len(mesh_records), "primitive_count": len(primitive_records),
               "material_count": len(materials), "image_count": len(images),
               "triangles": sum(item["index_count"] // 3 for item in primitive_records),
               "vertices": sum(item["vertex_count"] for item in primitive_records),
               "image_bytes": sum(item["byte_length"] for item in images),
               "mip_levels": sum(item["mip_levels"] for item in images),
               "tangent_fallback_corners": sum(item["tangent_fallback_corners"] for item in primitive_records),
               "collapsed_mikk_basis_corners": sum(item["collapsed_mikk_basis_corners"] for item in primitive_records),
               "degenerate_uv_faces": sum(item["degenerate_uv_faces"] for item in primitive_records),
               "degenerate_geometry_faces": sum(item["degenerate_geometry_faces"] for item in primitive_records)}
    source = {"gltf_sha256": digest(gltf.path), "buffers": [
        {"uri": item["uri"], "sha256": digest(relative_file(gltf.path.parent, item["uri"]))}
        for item in gltf.data.get("buffers", [])]}
    if source_zip:
        source["dds_archive_sha256"] = digest(source_zip)
    manifest = {"schema_version": 1, "vertex_stride": VERTEX_STRIDE, "source": source,
                "vertices": {"path": "vertices.bin", "byte_length": (output / "vertices.bin").stat().st_size,
                             "count": summary["vertices"], "sha256": digest(output / "vertices.bin")},
                "lightmap_uv": {"path": "lightmap_uv.bin", "byte_length": (output / "lightmap_uv.bin").stat().st_size,
                                "sha256": digest(output / "lightmap_uv.bin"), "texels_per_metre": texels_per_metre},
                "indices": {"path": "indices.bin", "byte_length": (output / "indices.bin").stat().st_size,
                            "count": summary["triangles"] * 3, "sha256": digest(output / "indices.bin")},
                "meshes": mesh_records, "primitives": primitive_records, "materials": materials,
                "images": images, "textures": textures, "samplers": gltf.data.get("samplers", []),
                "summary": summary,
                "recipe": {"bake_sampling": "first source mip with max dimension <= 128, sampled as bake mip 0 with material wrap and mag filter",
                           "texture_policy": texture_policy, "ispc_texcomp": version("ispc-texcomp"), "mikk_commit": UPSTREAM_COMMIT, "mikk_sources": source_hashes(),
                           "tool_sha256_utf8_lf": TOOL_HASHES,
                           "mikk_compiler": generator.build_record["compiler"],
                           "mikk_flags": generator.build_record["flags"],
                           "xatlas": version("xatlas"), "numpy": np.__version__, "pillow": pillow_version,
                           "vertex_layout": "little-endian float32 position3 normal3 tangent4 uv2; local uint32 indices",
                           "source_attributes": "POSITION/NORMAL/TEXCOORD_0 plus separate TEXCOORD_1 or generated lightmap UVs for static opaque/masked placements",
                           "transform": "local Mikk first; world position A*p+t, normal inverse-transpose, tangent A*T projected/normalized, sign*det; reflect winding",
                           "tangent_fallback": "Finite Mikk tangent with collapsed projected length (including degenerate UV/geometry or collapsed projected-angle basis): least-aligned Cartesian axis projected onto normal plane; preserve Mikk handedness. Non-finite output/generation failure is rejected",
                           "mips": "complete authored DDS levels" if archive else "linear-color and normalized tangent-space glTF mip chain",
                           "color": "base/emissive RGB sRGB, base alpha linear; normal/ORM linear; use native sRGB sampled formats for color",
                           "normal": "BC5 reconstructs positive Z at sampling; xy_directx negates authored DDS Y, xy and xyz use glTF Y"}}
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    return manifest


def cook(gltf_path, output, library, source_zip=None, texels_per_metre=TEXELS_PER_METRE, texture_policy=None):
    output = Path(output).resolve()
    require(not output.exists() or not any(output.iterdir()), "Cook output must be a new or empty directory")
    output.parent.mkdir(parents=True, exist_ok=True)
    # Publish complete assets together on the same filesystem. Failed cooks
    # leave the requested destination untouched and can be retried.
    with tempfile.TemporaryDirectory(prefix=".lightmap-cook-", dir=output.parent) as work, Generator(library) as generator:
        staged = Path(work) / "asset"
        result = _cook(gltf_path, staged, generator, source_zip, texels_per_metre, {} if texture_policy is None else texture_policy)
        if output.exists():
            output.rmdir()  # succeeds only if it is still empty
        staged.rename(output)
        return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gltf", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--mikk-library", type=Path, required=True)
    parser.add_argument("--source-zip", type=Path)
    parser.add_argument("--texels-per-metre", type=float, default=TEXELS_PER_METRE)
    parser.add_argument("--texture-policy", type=Path, help="JSON default, roles and image URI overrides")
    args = parser.parse_args()
    result = cook(args.gltf, args.output, args.mikk_library, args.source_zip, args.texels_per_metre,
                  json.loads(args.texture_policy.read_text(encoding="utf-8")) if args.texture_policy else None)
    print(json.dumps(result["summary"], sort_keys=True))
