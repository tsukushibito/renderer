"""Compare pinned Bistro FBX mesh/material/corner data with the Blender baseline.

Runs without bpy or a scene import. The original binary FBX reader and Blender's
name-shortening function are loaded from the same pinned Blender distribution.
Only identity Geometric* transforms are supported; ordinary Model transforms
belong to object matrices and are deliberately not baked into position checks.
"""

from __future__ import annotations

import argparse
import ast
import collections
import hashlib
import json
from pathlib import Path

import numpy as np

from source_inventory import child, children, identity, parse_source, properties, sha256


FBX_SHA256 = "471991629c7e35a88338c48f3565390ad19cc5d1d7c7e067fc2a60b4f3b5f195"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def load_name_rule(importer_path):
    """Execute only the self-contained pinned function, never bpy imports."""
    source = importer_path.read_text(encoding="utf-8")
    tree = ast.parse(source, filename=str(importer_path))
    functions = [node for node in tree.body
                 if isinstance(node, ast.FunctionDef) and node.name == "validate_blend_names"]
    require(len(functions) == 1, "expected one pinned Blender name function")
    namespace = {}
    function_tree = ast.Module(body=functions, type_ignores=[])
    exec(compile(function_tree, str(importer_path), "exec"), namespace)
    function_source = ast.get_source_segment(source, functions[0])
    return namespace["validate_blend_names"], hashlib.sha256(function_source.encode()).hexdigest()


def unique_names(elements, name_rule):
    result = {}
    for element in elements:
        name = name_rule(element.props[1].split(b"\x00\x01", 1)[0])
        require(name not in result, f"Blender name collision: {name}")
        result[name] = element
    return result


def expand_material_slots(mapping, slots, polygon_count, binding_count):
    slots = np.asarray(slots, dtype=np.int64)
    if mapping == b"AllSame":
        require(slots.shape == (1,), "AllSame must contain one material index")
        slots = np.full(polygon_count, slots[0], dtype=np.int64)
    elif mapping == b"ByPolygon":
        require(slots.shape == (polygon_count,), "ByPolygon material array length mismatch")
    else:
        raise ValueError(f"unsupported source material mapping: {mapping!r}")
    require(np.all((slots >= 0) & (slots < binding_count)), "source polygon material exceeds bindings")
    return slots


def exact_array(label, actual, expected):
    require(actual.shape == expected.shape, f"{label}: shape {actual.shape} != {expected.shape}")
    require(np.array_equal(actual, expected), f"{label}: source values differ")


def compare_geometry_arrays(label, geometry, saved, slot_count):
    """Check source corners as well as original vertex rows after duplication."""
    positions = np.asarray(child(geometry, b"Vertices").props[0], dtype=np.float32).reshape(-1, 3)
    encoded = np.asarray(child(geometry, b"PolygonVertexIndex").props[0], dtype=np.int64)
    require(encoded.size % 3 == 0, f"{label}: incomplete source triangle")
    encoded = encoded.reshape(-1, 3)
    require(np.all(encoded[:, :2] >= 0) and np.all(encoded[:, 2] < 0),
            f"{label}: source is not triangle-only")
    triangles = encoded.copy()
    triangles[:, 2] = -triangles[:, 2] - 1
    require(np.all((triangles >= 0) & (triangles < len(positions))),
            f"{label}: invalid source vertex index")
    require(np.isfinite(positions).all(), f"{label}: non-finite source positions")
    actual_positions = saved["positions"]
    require(actual_positions.ndim == 2 and actual_positions.shape[1] == 3
            and len(actual_positions) >= len(positions), f"{label}: invalid baseline position array")
    exact_array(label + " original vertex rows", actual_positions[:len(positions)], positions)
    actual_triangles = saved["triangles"]
    require(actual_triangles.shape == triangles.shape, f"{label}: triangle count differs")
    require(np.all((actual_triangles >= 0) & (actual_triangles < len(actual_positions))),
            f"{label}: invalid baseline vertex index")
    exact_array(label + " ordered triangle corner positions",
                actual_positions[actual_triangles], positions[triangles])

    material_layers = children(geometry, b"LayerElementMaterial")
    require(len(material_layers) == 1, f"{label}: expected one material layer")
    layer = material_layers[0]
    material_indices = expand_material_slots(
        child(layer, b"MappingInformationType").props[0],
        child(layer, b"Materials").props[0], len(triangles), slot_count)
    exact_array(label + " ordered polygon material indices", saved["material_indices"], material_indices)
    exact_array(label + " ordered triangle loop mapping", saved["triangle_loops"],
                np.arange(len(triangles) * 3, dtype=np.int64).reshape(-1, 3))

    normal_layers = children(geometry, b"LayerElementNormal")
    require(len(normal_layers) == 1, f"{label}: expected one source normal layer")
    normal_layer = normal_layers[0]
    require(child(normal_layer, b"MappingInformationType").props == [b"ByPolygonVertex"]
            and child(normal_layer, b"ReferenceInformationType").props == [b"Direct"],
            f"{label}: unsupported normal mapping")
    normals = np.asarray(child(normal_layer, b"Normals").props[0], dtype=np.float32).reshape(-1, 3)
    exact_array(label + " raw source corner normals", saved["source_loop_normals"], normals)
    lengths = np.linalg.norm(normals, axis=1, keepdims=True)
    require(np.isfinite(lengths).all() and np.all(lengths > 0), f"{label}: invalid source normal length")
    exact_array(label + " normalized source corner normals", saved["loop_normals"], normals / lengths)
    uv_names = []
    for index, uv_layer in enumerate(children(geometry, b"LayerElementUV")):
        require(child(uv_layer, b"MappingInformationType").props == [b"ByPolygonVertex"]
                and child(uv_layer, b"ReferenceInformationType").props == [b"IndexToDirect"],
                f"{label}: unsupported UV mapping")
        uv = np.asarray(child(uv_layer, b"UV").props[0], dtype=np.float32).reshape(-1, 2)
        uv_indices = np.asarray(child(uv_layer, b"UVIndex").props[0], dtype=np.int64)
        require(np.all((uv_indices >= 0) & (uv_indices < len(uv))), f"{label}: invalid source UV index")
        field = "loop_uv" if index == 0 else f"loop_uv_{index}"
        exact_array(label + f" source UV layer {index}", saved[field], uv[uv_indices])
        uv_names.append(child(uv_layer, b"Name").props[0].decode("utf-8"))

    duplicate_count = len(triangles) - len(np.unique(np.sort(triangles, axis=1), axis=0))
    require(len(actual_positions) == len(positions) + 3 * duplicate_count,
            f"{label}: unexpected vertex count after duplicate-face separation")
    return {
        "source_vertices": len(positions), "baseline_vertices": len(actual_positions),
        "triangles": len(triangles), "duplicate_faces": duplicate_count,
        "uv_layers": uv_names,
        "material_face_counts": {str(slot): int(count) for slot, count in
                                 zip(*np.unique(material_indices, return_counts=True))},
    }


def compare(source, audit_path, baseline_path, parser_dir):
    source_hash = sha256(source)
    require(source_hash == FBX_SHA256, "unsupported original FBX hash")
    audit_hash, baseline_hash = sha256(audit_path), sha256(baseline_path)
    audit = json.loads(audit_path.read_text(encoding="utf-8"))
    baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    require(audit["source"]["sha256"] == source_hash == baseline["source_sha256"],
            "source/audit/baseline provenance mismatch")
    require(baseline["source_audit_sha256"] == audit_hash, "baseline references a different source audit")
    importer = parser_dir / "import_fbx.py"
    require(sha256(importer) == baseline["importer_change"]["file_sha256"],
            "pinned Blender importer differs from baseline")
    require(baseline["blender"] == "4.5.16 LTS", "unsupported Blender baseline version")
    name_rule, name_rule_hash = load_name_rule(importer)
    root, version, reader_hashes = parse_source(source, parser_dir)
    require(reader_hashes == audit["reader"]["files_sha256"], "source reader differs from audit")
    require(version == audit["source"]["fbx_version"] == 7500, "unexpected FBX version")
    source_objects = child(root, b"Objects").elems
    by_id = {element.props[0]: element for element in source_objects}
    require(len(by_id) == len(source_objects), "duplicate raw FBX object ids")
    incoming = collections.defaultdict(list)
    texture_connections = collections.defaultdict(list)
    for connection in child(root, b"Connections").elems:
        kind, src, dst, *prop = connection.props
        if kind == b"OO":
            incoming[dst].append(src)
        elif kind == b"OP" and by_id[src].id == b"Texture" and by_id[dst].id == b"Material":
            require(len(prop) == 1, "missing raw texture connection property")
            texture_connections[dst].append((src, prop[0].decode("utf-8")))
    mesh_models = [element for element in source_objects
                   if element.id == b"Model" and element.props[2] == b"Mesh"]
    named_models = unique_names(mesh_models, name_rule)
    named_materials = unique_names([e for e in source_objects if e.id == b"Material"], name_rule)
    material_names = {element.props[0]: name for name, element in named_materials.items()}
    saved_objects = {obj["name"]: obj for obj in baseline["objects"]}
    require(len(saved_objects) == len(baseline["objects"]), "duplicate baseline object name")
    require(saved_objects.keys() == named_models.keys(), "source/baseline mesh names differ after pinned name rule")
    audit_models = {int(obj["id"]): obj for obj in audit["models"]}
    audit_geometries = {int(obj["id"]): obj for obj in audit["geometries"]}
    audited_model_ids = {int(obj["id"]) for obj in audit["models"] if obj["subtype"] == "Mesh"}
    require(audited_model_ids == {m.props[0] for m in mesh_models}, "audit mesh identities differ from raw source")
    recipes = {int(material["source_id"]): material for material in baseline["materials"]}
    require(len(recipes) == len(baseline["materials"]), "duplicate baseline material source id")
    require({ident: recipe["name"] for ident, recipe in recipes.items()} == material_names,
            "baseline material identities differ")
    texture_roles = 0
    for material_id, recipe in recipes.items():
        source_textures = {}
        for texture_id, channel in texture_connections[material_id]:
            require(channel not in source_textures, "multiple source textures for material property")
            texture = by_id[texture_id]
            source_textures[channel] = child(texture, b"RelativeFilename").props[0].decode("utf-8").replace("\\", "/")
            require(properties(texture)["UVSet"]["value"] == recipe["uv_set"],
                    f"{recipe['name']}: texture UV-set binding differs")
            texture_roles += 1
        require(recipe["textures"] == source_textures,
                f"{recipe['name']}: raw material texture role bindings differ")

    model_template = None
    for object_type in children(child(root, b"Definitions"), b"ObjectType"):
        if object_type.props == [b"Model"]:
            templates = children(object_type, b"PropertyTemplate")
            require(len(templates) == 1, "unexpected Model property templates")
            model_template = properties(templates[0])
    require(model_template is not None, "missing Model property template")
    geometric_defaults = {"GeometricTranslation": [0.0, 0.0, 0.0],
                          "GeometricRotation": [0.0, 0.0, 0.0],
                          "GeometricScaling": [1.0, 1.0, 1.0]}
    for key, expected in geometric_defaults.items():
        require(model_template.get(key, {}).get("value") == expected,
                f"unsupported Model template {key}")

    results = []
    material_usage = collections.Counter()
    seen_geometry_ids = set()
    baseline_dir = baseline_path.parent.resolve()
    for name, model in sorted(named_models.items()):
        source_id = model.props[0]
        model_properties = properties(model)
        for key, expected in geometric_defaults.items():
            require(model_properties.get(key, model_template[key])["value"] == expected,
                    f"{name}: non-identity {key} is not supported by direct vertex comparison")
        ids = incoming[source_id]
        geometries = [by_id[ident] for ident in ids if by_id[ident].id == b"Geometry"]
        materials = [ident for ident in ids if by_id[ident].id == b"Material"]
        require(len(geometries) == 1, f"{name}: expected one raw Geometry")
        geometry = geometries[0]
        geometry_id = geometry.props[0]
        require(geometry_id not in seen_geometry_ids, "unexpected shared raw Geometry")
        seen_geometry_ids.add(geometry_id)
        audited = audit_models[source_id]
        require(audited["geometry_ids"] == [str(geometry_id)]
                and audited["material_ids"] == [str(ident) for ident in materials],
                f"{name}: audit geometry/material bindings differ from original connections")
        saved = saved_objects[name]
        expected_materials = [material_names[ident] for ident in materials]
        require(saved["materials"] == expected_materials, f"{name}: ordered material slot names differ")
        geometry_path = (baseline_dir / saved["geometry"]).resolve()
        require(geometry_path.is_relative_to(baseline_dir), "baseline geometry path escapes output directory")
        with np.load(geometry_path, allow_pickle=False) as data:
            result = compare_geometry_arrays(name, geometry, data, len(materials))
        require(result["source_vertices"] == audit_geometries[geometry_id]["vertices"],
                f"{name}: audit source vertex count differs")
        require(result["triangles"] == saved["triangles"] == audit_geometries[geometry_id]["fan_triangles"],
                f"{name}: per-Geometry triangle count differs")
        require(result["baseline_vertices"] == saved["vertices"], f"{name}: baseline vertex count differs")
        require(result["uv_layers"] == saved["uv_layers"], f"{name}: UV layer order/names differ")
        for slot, count in result["material_face_counts"].items():
            material_usage[material_names[materials[int(slot)]]] += count
        results.append({"source_model_id": str(source_id), "source_geometry_id": str(geometry_id),
                        "source_model_name": identity(model)["name"], "baseline_name": name,
                        "geometry_sha256": sha256(geometry_path), "materials": expected_materials, **result})
    require(seen_geometry_ids == set(audit_geometries), "raw/audited Geometry set differs")
    triangles = sum(item["triangles"] for item in results)
    require(triangles == baseline["triangles"] == audit["summary"]["source_instanced_triangles"] == 2832120,
            "source/baseline total triangle count differs")
    return {
        "schema_version": 1, "passed": True,
        "inputs": {"source_sha256": source_hash, "source_audit_sha256": audit_hash,
                   "baseline_sha256": baseline_hash, "importer_sha256": sha256(importer),
                   "name_rule_sha256": name_rule_hash, "reader_files_sha256": reader_hashes},
        "summary": {"models": len(results), "geometries": len(seen_geometry_ids),
                    "materials": len(material_names), "texture_role_bindings": texture_roles, "triangles": triangles,
                    "duplicate_faces_preserved": sum(item["duplicate_faces"] for item in results),
                    "additional_vertices": sum(item["baseline_vertices"] - item["source_vertices"] for item in results),
                    "renamed_models": sum(item["source_model_name"] != item["baseline_name"] for item in results),
                    "exact_float32_original_positions": True, "exact_ordered_corner_positions": True,
                    "exact_raw_corner_normals": True, "exact_both_corner_uv_sets": True,
                    "exact_normalized_corner_normals": True,
                    "exact_ordered_polygon_material_indices": True},
        "material_face_usage": dict(sorted(material_usage.items())),
        "geometric_transforms": {"all_identity": True, "audited_template_defaults": geometric_defaults},
        "objects": results,
        "limitations": ["Model hierarchy/world matrices are not independently re-derived here; the separate baseline-to-glTF comparison covers exported matrices.",
                        "No rendering or material artist-intent/alpha appearance acceptance is implied."],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--audit", type=Path, required=True)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--blender-fbx-module", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    report = compare(args.source, args.audit, args.baseline, args.blender_fbx_module)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    print(json.dumps(report["summary"], indent=2))
    print(f"Source-to-baseline binding report: {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
