#!/usr/bin/env python3
"""Compare a Blender source inventory with an uncompressed glTF 2.0 export.

This is an offline conversion check, not a Renderer or GPU acceptance test.
It preserves triangle multiplicity and winding while allowing vertex, primitive,
triangle, and cyclic corner reorder. Material assignment participates in matching.
The export must preserve mesh-local coordinates (apart from Blender Z-up -> glTF
Y-up) and node names/transforms; baking arbitrary transforms is not accepted.

Correspondence is deliberately conservative: triangles are ordered by their
unrounded mesh-local corner values, including normals and UVs to distinguish
coincident faces. Attribute comparisons then use explicit float32 tolerances.
Very close values whose relative order changes can therefore cause a false
failure; this must be investigated, never resolved by silently dropping faces
or rounding away a material/attribute discrepancy.
"""

from __future__ import annotations

import argparse
import base64
import json
from pathlib import Path
from urllib.parse import unquote

import numpy as np


AXIS = np.array([[1, 0, 0, 0], [0, 0, 1, 0], [0, -1, 0, 0], [0, 0, 0, 1]], dtype=np.float64)
COMPONENTS = {5120: "i1", 5121: "u1", 5122: "<i2", 5123: "<u2", 5125: "<u4", 5126: "<f4"}
WIDTHS = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4, "MAT4": 16}
DEFAULT_MATERIAL = "<default>"


class ComparisonError(ValueError):
    """The inventory or glTF cannot establish the requested correspondence."""


def _require(condition, message):
    if not condition:
        raise ComparisonError(message)


class Gltf:
    def __init__(self, path):
        self.path = Path(path)
        self.data = json.loads(self.path.read_text(encoding="utf-8"))
        _require(self.data.get("asset", {}).get("version") == "2.0", "expected glTF 2.0")
        self.buffers = []
        for item in self.data.get("buffers", []):
            uri = item.get("uri", "")
            _require(bool(uri), "JSON glTF buffer must have a URI")
            if uri.startswith("data:"):
                header, payload = uri.split(",", 1)
                _require(header.endswith(";base64"), "only base64 buffer data URIs are supported")
                raw = base64.b64decode(payload, validate=True)
            else:
                _require(":" not in uri and not uri.startswith("//"), "remote buffer URI is not supported")
                raw = (self.path.parent / unquote(uri)).read_bytes()
            _require(len(raw) >= item["byteLength"], "buffer is shorter than declared byteLength")
            self.buffers.append(raw)

    def _view(self, view_index, offset, count, width, component_type, *, allow_stride=True):
        view = self.data["bufferViews"][view_index]
        _require("EXT_meshopt_compression" not in view.get("extensions", {}), "compressed bufferView is unsupported")
        dtype = np.dtype(COMPONENTS[component_type])
        stride = view.get("byteStride", width * dtype.itemsize) if allow_stride else width * dtype.itemsize
        _require(stride >= width * dtype.itemsize, "accessor stride is smaller than an element")
        length = 0 if count == 0 else (count - 1) * stride + width * dtype.itemsize
        _require(offset >= 0 and offset + length <= view["byteLength"], "accessor exceeds its bufferView")
        raw = self.buffers[view["buffer"]]
        start = view.get("byteOffset", 0) + offset
        _require(start + length <= len(raw), "bufferView exceeds its buffer")
        return np.ndarray((count, width), dtype=dtype, buffer=raw, offset=start,
                          strides=(stride, dtype.itemsize)).copy()

    def accessor(self, index):
        item = self.data["accessors"][index]
        component = item["componentType"]
        _require(component in COMPONENTS and item["type"] in WIDTHS, "unsupported accessor component/type")
        count, width = item["count"], WIDTHS[item["type"]]
        _require(isinstance(count, int) and count >= 0, "invalid accessor count")
        if "bufferView" in item:
            values = self._view(item["bufferView"], item.get("byteOffset", 0), count, width, component)
        else:
            values = np.zeros((count, width), dtype=COMPONENTS[component])
        if "sparse" in item:
            sparse = item["sparse"]
            indices = sparse["indices"]
            _require(indices["componentType"] in (5121, 5123, 5125), "invalid sparse index component")
            ids = self._view(indices["bufferView"], indices.get("byteOffset", 0), sparse["count"], 1,
                             indices["componentType"], allow_stride=False).ravel()
            _require(np.all(ids < count) and np.all(ids[1:] > ids[:-1]), "invalid sparse accessor indices")
            replacement = sparse["values"]
            values[ids] = self._view(replacement["bufferView"], replacement.get("byteOffset", 0),
                                     sparse["count"], width, component, allow_stride=False)
        if item.get("normalized", False):
            _require(component in (5120, 5121, 5122, 5123), "invalid normalized component type")
            limit = np.iinfo(values.dtype).max
            values = values.astype(np.float64) / limit
            if component in (5120, 5122):
                values = np.maximum(values, -1)
        _require(np.all(np.isfinite(values)), "accessor contains non-finite values")
        return values

    def mesh_instances(self):
        nodes = self.data.get("nodes", [])
        scenes = self.data.get("scenes", [])
        _require(bool(scenes), "glTF has no scene")
        scene_index = self.data.get("scene", 0)
        _require(0 <= scene_index < len(scenes), "invalid default scene index")
        found, visited = {}, set()

        def visit(index, parent):
            _require(isinstance(index, int) and 0 <= index < len(nodes), "invalid node index")
            _require(index not in visited, "node is repeated or graph is cyclic")
            visited.add(index)
            node = nodes[index]
            world = parent @ node_matrix(node)
            if "mesh" in node:
                name = node.get("name")
                _require(bool(name), "mesh node has no stable name")
                _require(name not in found, f"duplicate mesh node name: {name}")
                found[name] = (node["mesh"], world)
            for child in node.get("children", []):
                visit(child, world)

        for root in scenes[scene_index].get("nodes", []):
            visit(root, np.eye(4))
        return found

    def triangles(self, mesh_index, uv_sets):
        groups = {}
        for primitive in self.data["meshes"][mesh_index]["primitives"]:
            _require(primitive.get("mode", 4) == 4, "only triangle primitives are supported")
            _require(not primitive.get("targets"), "morph targets require a separate conversion contract")
            _require("KHR_draco_mesh_compression" not in primitive.get("extensions", {}), "Draco is unsupported")
            attributes = primitive["attributes"]
            required = ["POSITION", "NORMAL"] + [f"TEXCOORD_{index}" for index in range(uv_sets)]
            _require(all(key in attributes for key in required),
                     f"mesh primitive is missing required attributes: {required}")
            positions = self.accessor(attributes["POSITION"])
            normals = self.accessor(attributes["NORMAL"])
            uv = [self.accessor(attributes[f"TEXCOORD_{index}"]) for index in range(uv_sets)]
            _require(positions.shape[1] == normals.shape[1] == 3 and all(values.shape[1] == 2 for values in uv),
                     "invalid position/normal/UV width")
            _require(len(positions) == len(normals) and all(len(values) == len(positions) for values in uv),
                     "attribute counts differ")
            if "indices" in primitive:
                index_accessor = self.data["accessors"][primitive["indices"]]
                _require(index_accessor["type"] == "SCALAR" and index_accessor["componentType"] in (5121, 5123, 5125)
                         and not index_accessor.get("normalized", False), "invalid index accessor")
                indices = self.accessor(primitive["indices"]).ravel()
            else:
                indices = np.arange(len(positions))
            _require(len(indices) % 3 == 0 and np.all(indices < len(positions)), "invalid triangle indices")
            material = DEFAULT_MATERIAL
            if "material" in primitive:
                item = self.data["materials"][primitive["material"]]
                material = item.get("name")
                _require(bool(material), "material has no stable name")
            width = 6 + 2 * uv_sets
            values = np.concatenate([positions, normals] + uv, axis=1)[indices].reshape(-1, 3, width)
            groups.setdefault(material, []).append(values)
        return {name: np.concatenate(parts, axis=0).astype(np.float64) for name, parts in groups.items()}


def node_matrix(node):
    if "matrix" in node:
        _require(not any(key in node for key in ("translation", "rotation", "scale")), "node mixes matrix and TRS")
        matrix = np.asarray(node["matrix"], dtype=np.float64).reshape(4, 4).T
    else:
        x, y, z, w = node.get("rotation", [0, 0, 0, 1])
        _require(abs(x*x + y*y + z*z + w*w - 1) <= 1e-5, "non-unit rotation quaternion")
        rotation = np.array([[1-2*(y*y+z*z), 2*(x*y-z*w), 2*(x*z+y*w)],
                             [2*(x*y+z*w), 1-2*(x*x+z*z), 2*(y*z-x*w)],
                             [2*(x*z-y*w), 2*(y*z+x*w), 1-2*(x*x+y*y)]])
        matrix = np.eye(4)
        matrix[:3, :3] = rotation @ np.diag(node.get("scale", [1, 1, 1]))
        matrix[:3, 3] = node.get("translation", [0, 0, 0])
    _require(np.all(np.isfinite(matrix)), "node matrix is non-finite")
    _require(np.allclose(matrix[3], [0, 0, 0, 1], atol=1e-12, rtol=0), "node matrix is not affine")
    return matrix


def canonical(values):
    """Choose a cyclic corner rotation, then sort complete triangle records."""
    rows = values.reshape(len(values), -1).copy()
    width = values.shape[2]
    # Geometry determines correspondence before any attribute. A rounded normal
    # on a shared first vertex must not reorder geometrically distinct faces.
    columns = [corner * width + field for fields in (range(3), range(3, width))
               for corner in range(3) for field in fields]
    for shift in (1, 2):
        other = np.roll(values, -shift, axis=1).reshape(len(values), -1)
        equal = np.ones(len(values), dtype=bool)
        less = np.zeros(len(values), dtype=bool)
        for column in columns:
            less |= equal & (other[:, column] < rows[:, column])
            equal &= other[:, column] == rows[:, column]
        rows[less] = other[less]
    names = [f"v{i}" for i in range(rows.shape[1])]
    records = np.ascontiguousarray(rows).view(np.dtype([(name, "<f8") for name in names])).ravel()
    return rows[np.argsort(records, order=[names[column] for column in columns], kind="stable")].reshape(values.shape)


def transformed(values, world):
    result = values.copy()
    result[:, :, :3] = values[:, :, :3] @ world[:3, :3].T + world[:3, 3]
    normal_matrix = np.linalg.inv(world[:3, :3]).T
    normals = values[:, :, 3:6] @ normal_matrix.T
    lengths = np.linalg.norm(normals, axis=2)
    _require(np.all(lengths > 0) and np.all(np.isfinite(lengths)), "zero or invalid transformed normal")
    result[:, :, 3:6] = normals / lengths[:, :, None]
    return result


def source_triangles(path, entry):
    with np.load(path, allow_pickle=False) as raw:
        positions = np.asarray(raw["positions"], dtype=np.float64)
        triangles = raw["triangles"]
        loops = raw["triangle_loops"]
        material_indices = raw["material_indices"]
        normals = np.asarray(raw["loop_normals"], dtype=np.float64)
        uv = [np.asarray(raw["loop_uv"], dtype=np.float64)]
        if "loop_uv_1" in raw:
            uv.append(np.asarray(raw["loop_uv_1"], dtype=np.float64))
    _require(positions.ndim == 2 and positions.shape[1] == 3, "invalid source positions")
    _require(triangles.ndim == 2 and triangles.shape[1] == 3 and loops.shape == triangles.shape,
             "invalid source triangle/loop shape")
    _require(normals.ndim == 2 and normals.shape[1] == 3 and all(values.shape == (len(normals), 2) for values in uv),
             "invalid source loop attributes")
    _require(len(entry.get("uv_layers", uv)) == len(uv), "source UV layer metadata and arrays differ")
    _require(material_indices.shape == (len(triangles),), "invalid source material indices")
    _require(np.issubdtype(triangles.dtype, np.integer) and np.issubdtype(loops.dtype, np.integer),
             "source triangle/loop indices are not integers")
    _require(np.all(triangles >= 0) and np.all(triangles < len(positions)) and
             np.all(loops >= 0) and np.all(loops < len(normals)), "source index is out of range")
    points = positions[triangles] @ AXIS[:3, :3].T
    ns = normals[loops] @ AXIS[:3, :3].T
    texcoords = []
    for layer in uv:
        tex = layer[loops].copy()
        tex[:, :, 1] = 1 - tex[:, :, 1]
        texcoords.append(tex)
    values = np.concatenate([points, ns] + texcoords, axis=2)
    _require(np.all(np.isfinite(values)), "source geometry has non-finite values")
    materials = entry.get("materials", [])
    groups = {}
    for index in np.unique(material_indices):
        _require(index >= 0 and (index < len(materials) or not materials and index == 0),
                 "source material index is out of range")
        name = (materials[index] if materials else None) or DEFAULT_MATERIAL
        groups.setdefault(name, []).append(values[material_indices == index])
    return {name: np.concatenate(parts, axis=0) for name, parts in groups.items()}


def compare(source_path, gltf_path, *, position_atol=2e-5, position_rtol=2e-6,
            normal_atol=2e-5, uv_atol=2e-6, matrix_atol=2e-5, matrix_rtol=2e-6):
    """Return a JSON-serializable report; no tolerance is a pixel-quality gate."""
    source_path = Path(source_path)
    source = json.loads(source_path.read_text(encoding="utf-8"))
    gltf = Gltf(gltf_path)
    instances = gltf.mesh_instances()
    names = [entry["name"] for entry in source["objects"]]
    _require(len(names) == len(set(names)), "duplicate source object names")
    report = {"ok": True, "errors": [], "objects": [], "source_objects": len(names),
              "export_objects": len(instances), "source_triangles": 0, "export_triangles": 0,
              "tolerance": {"position_atol": position_atol, "position_rtol": position_rtol,
                            "normal_atol": normal_atol, "uv_atol": uv_atol,
                            "matrix_atol": matrix_atol, "matrix_rtol": matrix_rtol},
              "correspondence": "unrounded local-value cyclic triangle sort; multiplicity and winding preserved"}

    def error(message):
        report["ok"] = False
        report["errors"].append(message)

    for name in sorted(set(instances) - set(names)):
        error(f"unexpected mesh object: {name}")
    for entry in source["objects"]:
        name = entry["name"]
        item = {"name": name, "source_triangles": 0, "export_triangles": 0,
                "max_position_diff": 0.0, "max_normal_diff": 0.0, "max_uv_diff": 0.0}
        report["objects"].append(item)
        try:
            original = source_triangles(source_path.parent / entry["geometry"], entry)
            item["source_triangles"] = sum(map(len, original.values()))
            report["source_triangles"] += item["source_triangles"]
            if name not in instances:
                error(f"missing mesh object: {name}")
                continue
            mesh_index, actual_world = instances[name]
            expected_world = AXIS @ np.asarray(entry["matrix_world"], dtype=np.float64).reshape(4, 4) @ AXIS.T
            _require(np.all(np.isfinite(expected_world)), "source matrix is non-finite")
            item["max_matrix_diff"] = float(np.max(np.abs(expected_world - actual_world)))
            if not np.allclose(expected_world, actual_world, atol=matrix_atol, rtol=matrix_rtol):
                error(f"{name}: world transform mismatch")
            uv_sets = (next(iter(original.values())).shape[2] - 6) // 2 if original else len(entry.get("uv_layers", [0]))
            exported = gltf.triangles(mesh_index, uv_sets)
            item["export_triangles"] = sum(map(len, exported.values()))
            report["export_triangles"] += item["export_triangles"]
            if set(original) != set(exported):
                error(f"{name}: material assignment mismatch: source={sorted(original)}, export={sorted(exported)}")
            for material in sorted(set(original) & set(exported)):
                a, b = original[material], exported[material]
                if len(a) != len(b):
                    error(f"{name}/{material}: triangle count differs ({len(a)} vs {len(b)})")
                    continue
                a, b = canonical(a), canonical(b)
                local_ok = np.allclose(a[:, :, :3], b[:, :, :3], atol=position_atol, rtol=position_rtol)
                a, b = transformed(a, expected_world), transformed(b, actual_world)
                fields = [("position", slice(0, 3), position_atol, position_rtol),
                          ("normal", slice(3, 6), normal_atol, 0)]
                fields.extend((f"uv_{index}", slice(6 + 2*index, 8 + 2*index), uv_atol, 0)
                              for index in range(uv_sets))
                for label, section, atol, rtol in fields:
                    maximum = float(np.max(np.abs(a[:, :, section] - b[:, :, section]), initial=0))
                    item[f"max_{label}_diff"] = max(item.get(f"max_{label}_diff", 0), maximum)
                    if label.startswith("uv_"):
                        item["max_uv_diff"] = max(item["max_uv_diff"], maximum)
                    if not np.allclose(a[:, :, section], b[:, :, section], atol=atol, rtol=rtol) or label == "position" and not local_ok:
                        error(f"{name}/{material}: {label} mismatch (max absolute difference {maximum:.9g})")
        except (ComparisonError, IndexError, KeyError, ValueError, np.linalg.LinAlgError, OSError) as exc:
            error(f"{name}: {exc}")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="source inventory JSON; NPZ paths are relative to this file")
    parser.add_argument("gltf", type=Path, help="uncompressed glTF 2.0 JSON")
    parser.add_argument("--output", type=Path, required=True)
    for name, default in (("position-atol", 2e-5), ("position-rtol", 2e-6), ("normal-atol", 2e-5),
                          ("uv-atol", 2e-6), ("matrix-atol", 2e-5), ("matrix-rtol", 2e-6)):
        parser.add_argument(f"--{name}", type=float, default=default)
    args = parser.parse_args()
    options = {key: value for key, value in vars(args).items() if key not in ("source", "gltf", "output")}
    if any(not np.isfinite(x) or x < 0 for x in options.values()):
        parser.error("tolerances must be finite and non-negative")
    try:
        report = compare(args.source, args.gltf, **options)
    except (ComparisonError, KeyError, IndexError, ValueError, OSError) as exc:
        report = {"ok": False, "errors": [str(exc)]}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"ok": report["ok"], "errors": len(report["errors"]), "report": str(args.output)}))
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
