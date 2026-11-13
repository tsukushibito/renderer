"""Audit the original Bistro FBX independently of Blender's scene importer.

Uses the pinned Blender distribution's pure-Python binary FBX reader, never bpy.
The JSON records source facts rather than inferring PBR/alpha from FBX Phong data.
This offline asset tool does not participate in Renderer draw/submit paths.
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import importlib
import json
import math
from pathlib import Path, PureWindowsPath
import sys
import types


def sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def text(value: bytes) -> str:
    return value.decode("utf-8")


def json_value(value):
    if isinstance(value, bytes):
        return text(value)
    if isinstance(value, (list, tuple)):
        return [json_value(part) for part in value]
    return value


def children(element, name: bytes):
    return [child for child in element.elems if child.id == name]


def child(element, name: bytes):
    found = children(element, name)
    if len(found) != 1:
        raise ValueError(f"expected one {name!r}, found {len(found)}")
    return found[0]


def scalar_children(element) -> dict:
    """Preserve non-array direct fields, including Culling and texture paths."""
    return {
        text(item.id): json_value(item.props)
        for item in element.elems
        if item.props and all(isinstance(p, (int, float, bytes)) for p in item.props)
    }


def properties(element) -> dict:
    result = {}
    for block in children(element, b"Properties70"):
        for item in block.elems:
            if item.id != b"P" or len(item.props) < 4:
                raise ValueError("unsupported FBX property declaration")
            name, kind, label, flags, *values = item.props
            key = text(name)
            if key in result:
                raise ValueError(f"duplicate property {key}")
            result[key] = {
                "type": text(kind),
                "label": text(label),
                "flags": text(flags),
                "value": json_value(values[0] if len(values) == 1 else values),
            }
    if children(element, b"Properties60"):
        raise ValueError("legacy Properties60 is outside this Bistro audit")
    return result


def identity(element) -> dict:
    raw_name = text(element.props[1])
    return {
        "id": str(element.props[0]),
        "name": raw_name.split("\x00\x01", 1)[0],
        "type": text(element.id),
        "subtype": text(element.props[2]),
    }


def parse_source(source: Path, parser_dir: Path):
    # Importing the actual add-on package would execute bpy imports. A synthetic
    # package loads only the standalone reader and its two pure-Python helpers.
    # Never create __pycache__ in the caller's pinned Blender installation.
    sys.dont_write_bytecode = True
    package_name = "_renderer_bistro_fbx_reader"
    if package_name in sys.modules:
        raise ValueError("FBX reader package already loaded in this process")
    package = types.ModuleType(package_name)
    package.__path__ = [str(parser_dir.resolve())]
    sys.modules[package_name] = package
    threading = importlib.import_module(package_name + ".fbx_utils_threading")
    threading._MULTITHREADING_ENABLED = False
    parser = importlib.import_module(package_name + ".parse_fbx")
    root, version = parser.parse(str(source), use_namedtuple=True)
    parser_files = {
        name: sha256(parser_dir / name)
        for name in ("parse_fbx.py", "data_types.py", "fbx_utils_threading.py")
    }
    return root, version, parser_files


def polygon_audit(indices, vertices) -> dict:
    """Count original polygons and classify exact, explicitly defined defects.

    A duplicated face is NOT declared safe to remove: alpha, winding, material
    slots and depth behavior may matter. No geometric epsilon is invented.
    """
    if len(vertices) % 3:
        raise ValueError("vertex coordinate array is not XYZ triples")
    if not all(math.isfinite(component) for component in vertices):
        raise ValueError("non-finite source vertex coordinate")
    vertex_count = len(vertices) // 3
    counts = collections.Counter()
    repeated_indices = []
    repeated_positions = []
    zero_area = []
    duplicates = []
    seen = {}
    polygon = []
    polygon_index = 0
    fan_triangles = 0
    for encoded in indices:
        end = encoded < 0
        index = -encoded - 1 if end else encoded
        if not 0 <= index < vertex_count:
            raise ValueError(f"polygon vertex {index} exceeds {vertex_count}")
        polygon.append(index)
        if not end:
            continue
        size = len(polygon)
        if size < 3:
            raise ValueError(f"polygon {polygon_index} has fewer than 3 vertices")
        counts[size] += 1
        fan_triangles += size - 2
        if size == 3:
            ids = tuple(polygon)
            if len(set(ids)) != 3:
                repeated_indices.append(polygon_index)
            points = [tuple(vertices[i * 3:i * 3 + 3]) for i in ids]
            if len(set(points)) != 3:
                repeated_positions.append(polygon_index)
            a, b, c = points
            ux, uy, uz = (b[i] - a[i] for i in range(3))
            vx, vy, vz = (c[i] - a[i] for i in range(3))
            cross = (uy * vz - uz * vy, uz * vx - ux * vz, ux * vy - uy * vx)
            if cross == (0.0, 0.0, 0.0):
                zero_area.append(polygon_index)
            key = tuple(sorted(ids))
            if key in seen:
                first_index, first_ids = seen[key]
                same_winding = ids in (
                    first_ids,
                    first_ids[1:] + first_ids[:1],
                    first_ids[2:] + first_ids[:2],
                )
                duplicates.append({
                    "polygon": polygon_index,
                    "matches_polygon": first_index,
                    "same_winding": same_winding,
                })
            else:
                seen[key] = (polygon_index, ids)
        polygon.clear()
        polygon_index += 1
    if polygon:
        raise ValueError("unterminated PolygonVertexIndex polygon")
    return {
        "vertices": vertex_count,
        "polygons": polygon_index,
        "polygon_sizes": dict(sorted(counts.items())),
        "fan_triangles": fan_triangles,
        "repeated_vertex_index_polygons": repeated_indices,
        "repeated_position_polygons": repeated_positions,
        "exact_zero_area_polygons": zero_area,
        "duplicate_index_set_polygons": duplicates,
    }


def layer_description(layer) -> dict:
    result = {"type": text(layer.id), "index": json_value(layer.props)}
    result.update(scalar_children(layer))
    for field in layer.elems:
        if field.props and any(hasattr(value, "typecode") for value in field.props):
            result[text(field.id)] = [
                {"elements": len(value), "array_type": value.typecode}
                if hasattr(value, "typecode") else json_value(value)
                for value in field.props
            ]
    return result


def corner_attribute(layer, polygon_index, indices, field, index_field, width):
    """Return source corner values keyed by vertex, so winding is independent."""
    if child(layer, b"MappingInformationType").props != [b"ByPolygonVertex"]:
        raise ValueError("duplicate attribute audit requires ByPolygonVertex mapping")
    reference = child(layer, b"ReferenceInformationType").props[0]
    data = child(layer, field).props[0]
    index_data = child(layer, index_field).props[0] if reference == b"IndexToDirect" else None
    if reference not in (b"Direct", b"IndexToDirect"):
        raise ValueError("unsupported source attribute reference mode")
    values = {}
    for corner in range(polygon_index * 3, polygon_index * 3 + 3):
        vertex = indices[corner]
        vertex = -vertex - 1 if vertex < 0 else vertex
        direct = index_data[corner] if index_data is not None else corner
        if direct < 0 or (direct + 1) * width > len(data):
            raise ValueError("source corner attribute index exceeds data")
        values[vertex] = tuple(data[direct * width:direct * width + width])
    return values


def classify_duplicate_surfaces(element, audit):
    """Preserve why topologically duplicate faces may have different shading."""
    if not audit["duplicate_index_set_polygons"]:
        return
    if audit["polygon_sizes"] != {3: audit["polygons"]}:
        raise ValueError("duplicate surface audit currently requires triangle-only geometry")
    indices = child(element, b"PolygonVertexIndex").props[0]
    normals = children(element, b"LayerElementNormal")
    uvs = children(element, b"LayerElementUV")
    material_layers = children(element, b"LayerElementMaterial")

    def material_slot(layer, polygon):
        mapping = child(layer, b"MappingInformationType").props[0]
        slots = child(layer, b"Materials").props[0]
        if mapping == b"AllSame":
            return slots[0]
        if mapping == b"ByPolygon":
            return slots[polygon]
        raise ValueError("unsupported source material mapping mode")

    for duplicate in audit["duplicate_index_set_polygons"]:
        first, second = duplicate["matches_polygon"], duplicate["polygon"]
        duplicate["normal_equal_per_vertex"] = [
            corner_attribute(layer, first, indices, b"Normals", b"NormalsIndex", 3)
            == corner_attribute(layer, second, indices, b"Normals", b"NormalsIndex", 3)
            for layer in normals
        ]
        duplicate["uv_equal_per_vertex"] = {
            child(layer, b"Name").props[0].decode("utf-8") or str(layer.props[0]):
            corner_attribute(layer, first, indices, b"UV", b"UVIndex", 2)
            == corner_attribute(layer, second, indices, b"UV", b"UVIndex", 2)
            for layer in uvs
        }
        duplicate["material_slots"] = [
            {"first": material_slot(layer, first), "duplicate": material_slot(layer, second)}
            for layer in material_layers
        ]


def validate_geometry_layers(element, audit, material_slot_counts):
    """Check the actual Bistro corner arrays before skipping importer cleanup."""
    corner_count = len(child(element, b"PolygonVertexIndex").props[0])
    for layer_type, data_field, index_field, width in (
        (b"LayerElementNormal", b"Normals", b"NormalsIndex", 3),
        (b"LayerElementUV", b"UV", b"UVIndex", 2),
    ):
        for layer in children(element, layer_type):
            mapping = child(layer, b"MappingInformationType").props[0]
            if mapping != b"ByPolygonVertex":
                raise ValueError(f"unsupported {layer_type!r} mapping {mapping!r}")
            data = child(layer, data_field).props[0]
            if len(data) % width or not all(math.isfinite(value) for value in data):
                raise ValueError("non-finite or incomplete source corner attribute")
            reference = child(layer, b"ReferenceInformationType").props[0]
            if reference == b"Direct":
                if len(data) != corner_count * width:
                    raise ValueError("source direct corner attribute length mismatch")
            elif reference == b"IndexToDirect":
                indices = child(layer, index_field).props[0]
                if len(indices) != corner_count:
                    raise ValueError("source corner attribute index length mismatch")
                if any(index < 0 or (index + 1) * width > len(data) for index in indices):
                    raise ValueError("source corner attribute index exceeds data")
            else:
                raise ValueError("unsupported source corner attribute reference")
    for layer in children(element, b"LayerElementMaterial"):
        mapping = child(layer, b"MappingInformationType").props[0]
        slots = child(layer, b"Materials").props[0]
        expected = 1 if mapping == b"AllSame" else audit["polygons"]
        if mapping not in (b"ByPolygon", b"AllSame") or len(slots) != expected:
            raise ValueError("unsupported or incomplete source material mapping")
        if any(slot < 0 or slot >= count for count in material_slot_counts for slot in slots):
            raise ValueError("source polygon material slot exceeds Model bindings")


def build_inventory(source: Path, parser_dir: Path, asset_root: Path) -> dict:
    source = source.resolve()
    asset_root = asset_root.resolve()
    root, version, parser_files = parse_source(source, parser_dir)
    objects = child(root, b"Objects").elems
    by_id = {}
    for element in objects:
        ident = element.props[0]
        if ident in by_id:
            raise ValueError(f"duplicate FBX object id {ident}")
        by_id[ident] = element
    connections = []
    incoming = collections.defaultdict(list)
    outgoing = collections.defaultdict(list)
    for element in child(root, b"Connections").elems:
        if element.id != b"C" or len(element.props) not in (3, 4):
            raise ValueError("unsupported FBX connection")
        kind, src, dst, *prop = element.props
        connection = {"kind": text(kind), "source": str(src), "target": str(dst)}
        if prop:
            connection["property"] = text(prop[0])
        if src not in by_id or (dst != 0 and dst not in by_id):
            raise ValueError(f"connection references missing object {src}->{dst}")
        connections.append(connection)
        incoming[dst].append((kind, src, prop))
        outgoing[src].append((kind, dst, prop))

    files = [path for path in sorted(asset_root.rglob("*")) if path.is_file()]
    by_relative = collections.defaultdict(list)
    by_basename = collections.defaultdict(list)
    for path in files:
        relative = path.relative_to(asset_root).as_posix()
        by_relative[relative.casefold()].append(relative)
        by_basename[path.name.casefold()].append(relative)

    models = []
    geometries = []
    materials = []
    textures = []
    attributes = []
    missing = []
    channel_counts = collections.Counter()
    totals = collections.Counter()
    for element in objects:
        ident = element.props[0]
        info = identity(element)
        if element.id == b"Model":
            info.update({
                "properties": properties(element),
                "fields": scalar_children(element),
                "parent_model_ids": [str(dst) for kind, dst, _ in outgoing[ident]
                                     if kind == b"OO" and (dst == 0 or by_id[dst].id == b"Model")],
                "geometry_ids": [str(src) for kind, src, _ in incoming[ident]
                                 if kind == b"OO" and by_id[src].id == b"Geometry"],
                "material_ids": [str(src) for kind, src, _ in incoming[ident]
                                 if kind == b"OO" and by_id[src].id == b"Material"],
                "attribute_ids": [str(src) for kind, src, _ in incoming[ident]
                                  if kind == b"OO" and by_id[src].id == b"NodeAttribute"],
            })
            models.append(info)
        elif element.id == b"Geometry":
            if element.props[2] != b"Mesh":
                raise ValueError("non-mesh geometry is outside this Bistro audit")
            audit = polygon_audit(child(element, b"PolygonVertexIndex").props[0],
                                  child(element, b"Vertices").props[0])
            connected = [str(dst) for kind, dst, _ in outgoing[ident]
                         if kind == b"OO" and by_id[dst].id == b"Model"]
            slot_counts = [
                sum(kind == b"OO" and by_id[src].id == b"Material"
                    for kind, src, _ in incoming[int(model_id)])
                for model_id in connected
            ]
            validate_geometry_layers(element, audit, slot_counts)
            classify_duplicate_surfaces(element, audit)
            info.update(audit)
            info["model_ids"] = connected
            info["layers"] = [layer_description(layer) for layer in element.elems
                              if layer.id.startswith(b"LayerElement")]
            # Material indices index the connected Model's ordered material list.
            info["material_layers"] = [
                {"index": json_value(layer.props),
                 "fields": scalar_children(layer),
                 "material_index_counts": dict(collections.Counter(
                     child(layer, b"Materials").props[0]))}
                for layer in children(element, b"LayerElementMaterial")
            ]
            geometries.append(info)
            totals["source_geometry_polygons"] += audit["polygons"]
            totals["source_geometry_triangles"] += audit["fan_triangles"]
            totals["source_instanced_triangles"] += audit["fan_triangles"] * len(connected)
            for field in ("repeated_vertex_index_polygons", "repeated_position_polygons",
                          "exact_zero_area_polygons", "duplicate_index_set_polygons"):
                totals[field] += len(audit[field])
        elif element.id == b"Material":
            slots = []
            role_paths = {}
            for kind, src, prop in incoming[ident]:
                if by_id[src].id != b"Texture":
                    continue
                slot = {"connection_kind": text(kind), "texture_id": str(src),
                        "property": text(prop[0]) if prop else None}
                reference = scalar_children(by_id[src]).get("RelativeFilename", [""])[0]
                slot["relative_filename"] = reference.replace("\\", "/")
                if slot["property"] in role_paths:
                    raise ValueError("multiple source textures attached to one material property")
                role_paths[slot["property"]] = slot["relative_filename"]
                slots.append(slot)
                channel_counts[slot["property"]] += 1
            info.update({"properties": properties(element), "fields": scalar_children(element),
                         "texture_slots": slots, "textures": role_paths})
            materials.append(info)
        elif element.id in (b"Texture", b"Video"):
            fields = scalar_children(element)
            relative = fields.get("RelativeFilename", [""])[0].replace("\\", "/")
            normalized = str(PureWindowsPath(relative)).replace("\\", "/")
            candidates = by_relative.get(normalized.casefold(), [])
            if len(candidates) > 1:
                raise ValueError(f"case-colliding source reference {relative}")
            if not candidates:
                missing.append({"id": str(ident), "type": text(element.id),
                                "relative_reference": relative,
                                "basename_candidates": by_basename.get(PureWindowsPath(relative).name.casefold(), [])})
            info.update({"properties": properties(element), "fields": fields,
                         "resolved_asset_path": candidates[0] if candidates else None,
                         "material_connections": [
                             {"material_id": str(dst), "kind": text(kind),
                              "property": text(prop[0]) if prop else None}
                             for kind, dst, prop in outgoing[ident]
                             if by_id[dst].id == b"Material"],
                         "video_ids": [str(src) for kind, src, _ in incoming[ident]
                                       if kind == b"OO" and by_id[src].id == b"Video"]})
            textures.append(info)
        elif element.id == b"NodeAttribute":
            info.update({"properties": properties(element), "fields": scalar_children(element),
                         "model_ids": [str(dst) for kind, dst, _ in outgoing[ident]
                                       if kind == b"OO" and by_id[dst].id == b"Model"]})
            attributes.append(info)

    definitions = []
    for object_type in children(child(root, b"Definitions"), b"ObjectType"):
        for template in children(object_type, b"PropertyTemplate"):
            definitions.append({"object_type": json_value(object_type.props),
                                "template": json_value(template.props),
                                "properties": properties(template)})
    return {
        "schema_version": 1,
        "source": {"path": source.relative_to(asset_root).as_posix(),
                   "bytes": source.stat().st_size, "sha256": sha256(source),
                   "fbx_version": version},
        "reader": {"kind": "Blender standalone binary FBX parser; bpy importer not used",
                   "files_sha256": parser_files},
        "global_settings": properties(child(root, b"GlobalSettings")),
        "property_templates": definitions,
        "summary": {**dict(totals),
                    "object_types": dict(sorted(collections.Counter(text(e.id) for e in objects).items())),
                    "model_types": dict(sorted(collections.Counter(m["subtype"] for m in models).items())),
                    "texture_material_channels": dict(sorted(channel_counts.items())),
                    "missing_texture_references": len(missing)},
        "validated_scope": [
            "Finite XYZ positions and valid terminated source polygon vertex indices",
            "Finite, complete ByPolygonVertex normal/UV arrays and valid corner attribute indices",
            "Complete polygon material mappings within each connected Model's ordered material bindings",
            "Existing object ids in every connection and normalized source texture relative references",
        ],
        "models": models,
        "geometries": geometries,
        "materials": materials,
        "textures_and_videos": textures,
        "node_attributes": attributes,
        "connections": connections,
        "missing_texture_references": missing,
        "basename_collisions": {name: paths for name, paths in sorted(by_basename.items()) if len(paths) > 1},
        "case_colliding_asset_paths": {name: paths for name, paths in sorted(by_relative.items()) if len(paths) > 1},
        "interpretation": {
            "material_values": "Explicit source properties only; property templates are preserved separately.",
            "pbr": "Use this distribution's README channel semantics; FBX Phong slot names are not PBR meaning.",
            "alpha": "Opacity/TransparencyFactor/Texture_Alpha_Source do not establish pixel alpha coverage.",
            "duplicate_faces": "Reported, not removed or presumed visually redundant.",
            "zero_area": "Exact cross product zero for source triangles; no geometric tolerance.",
            "paths": "Only exact normalized asset-root relative references resolve; basename guesses are diagnostic.",
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--blender-fbx-module", type=Path, required=True,
                        help="Pinned Blender scripts/addons_core/io_scene_fbx directory")
    parser.add_argument("--asset-root", type=Path,
                        help="Defaults to the FBX directory")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    report = build_inventory(args.source, args.blender_fbx_module,
                             args.asset_root or args.source.parent)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2, allow_nan=False) + "\n",
                           encoding="utf-8")
    print(json.dumps(report["summary"], indent=2))
    print(f"Source inventory: {args.output}")
    return 1 if report["missing_texture_references"] or report["case_colliding_asset_paths"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
