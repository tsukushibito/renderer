"""Check exported Bistro PBR parameters and texture channels against its explicit recipe."""
import argparse
import hashlib
import json
from pathlib import Path
from urllib.parse import unquote

import numpy as np
from PIL import Image


def compare(baseline, textures, prepared_directory, model_path):
    model = json.loads(model_path.read_text(encoding="utf-8"))
    expected = {item["source"]: item for item in textures["textures"]}
    actual = {item["name"]: item for item in model.get("materials", [])}
    if len(actual) != len(model.get("materials", [])):
        raise ValueError("Ambiguous exported material names")
    if set(actual) != {item["name"] for item in baseline["materials"]}:
        raise ValueError("Exported material set differs from source recipe")
    records = []
    pixels_cache = {}
    for recipe in baseline["materials"]:
        material = actual[recipe["name"]]
        errors = []
        if material.get("alphaMode", "OPAQUE") != recipe["alpha_mode"]:
            errors.append("alphaMode")
        if recipe["alpha_mode"] == "MASK" and abs(material.get("alphaCutoff", 0.5) - recipe["alpha_cutoff"]) > 1e-7:
            errors.append("alphaCutoff")
        if material.get("doubleSided", False) != recipe["double_sided"]:
            errors.append("doubleSided")
        pbr = material.get("pbrMetallicRoughness", {})
        if pbr.get("baseColorFactor", [1, 1, 1, 1]) != [1, 1, 1, 1]:
            errors.append("baseColorFactor")
        if "orm_fallback" in recipe:
            fallback = recipe["orm_fallback"]
            if pbr.get("metallicFactor", 1) != fallback["metallic"] or pbr.get("roughnessFactor", 1) != fallback["roughness"]:
                errors.append("ORM fallback")
        elif pbr.get("metallicFactor", 1) != 1 or pbr.get("roughnessFactor", 1) != 1:
            errors.append("ORM factors")
        if material.get("emissiveFactor", [0, 0, 0]) != recipe.get("emissive_factor", [0, 0, 0]):
            errors.append("emissiveFactor")
        if material.get("extensions"):
            errors.append("unrecorded material extension")
        slots = {"DiffuseColor": pbr.get("baseColorTexture"), "NormalMap": material.get("normalTexture"),
                 "SpecularColor": pbr.get("metallicRoughnessTexture"), "EmissiveColor": material.get("emissiveTexture")}
        for channel, slot in slots.items():
            if channel not in recipe["textures"] and slot is not None:
                errors.append(f"unexpected {channel} texture")
        if "SpecularColor" not in recipe["textures"] and material.get("occlusionTexture") is not None:
            errors.append("unexpected occlusion texture")
        pixel_checks = []
        for channel, source_path in recipe["textures"].items():
            slot = slots[channel]
            if slot is None:
                errors.append(f"missing {channel}")
                continue
            if slot.get("texCoord", 0) != 0 or slot.get("extensions"):
                errors.append(f"unexpected UV mapping {channel}")
            texture = model["textures"][slot["index"]]
            if channel == "NormalMap" and slot.get("scale", 1) != 1:
                errors.append("normal scale")
            if channel == "SpecularColor":
                occlusion = material.get("occlusionTexture")
                if (occlusion is None or occlusion["index"] != slot["index"] or occlusion.get("strength", 1) != 1
                        or occlusion.get("texCoord", 0) != 0 or occlusion.get("extensions")):
                    errors.append("occlusion channel")
            sampler = model.get("samplers", [])[texture["sampler"]] if "sampler" in texture else {}
            if sampler.get("wrapS", 10497) != 10497 or sampler.get("wrapT", 10497) != 10497:
                errors.append(f"wrap mode {channel}")
            if sampler.get("magFilter", 9729) != 9729 or sampler.get("minFilter", 9987) != 9987:
                errors.append(f"filter mode {channel}")
            uri = model["images"][texture["source"]]["uri"]
            path = (model_path.parent / unquote(uri)).resolve()
            if not path.is_relative_to(model_path.parent.resolve()):
                raise ValueError("Exported image escapes model directory")
            cache_key = (source_path, str(path), recipe["alpha_mode"] == "OPAQUE")
            if cache_key not in pixels_cache:
                with Image.open(prepared_directory / expected[source_path]["output"]) as image:
                    reference = np.asarray(image.convert("RGBA"))
                with Image.open(path) as image:
                    converted = np.asarray(image.convert("RGBA"))
                dimensions = reference.shape == converted.shape
                channels = 4 if channel == "DiffuseColor" and recipe["alpha_mode"] != "OPAQUE" else 3
                same = dimensions and np.array_equal(reference[:, :, :channels], converted[:, :, :channels])
                pixels_cache[cache_key] = same
            same = pixels_cache[cache_key]
            if not same:
                errors.append(f"pixel/channel mismatch {channel}")
            pixel_checks.append({"role": channel, "source": source_path, "output": uri, "equal": bool(same)})
        records.append({"name": recipe["name"], "errors": errors, "textures": pixel_checks})
    return {"schema_version": 1, "passed": all(not item["errors"] for item in records),
            "materials": records, "unique_pixel_comparisons": len(pixels_cache),
            "scope": "Explicit conversion recipe, base mip pixels, PBR channels/factors/alpha/sidedness/UV; not proof of unspecified original artist intent"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("texture_report", type=Path)
    parser.add_argument("prepared_directory", type=Path)
    parser.add_argument("model", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    report = compare(json.loads(args.baseline.read_text(encoding="utf-8")),
                     json.loads(args.texture_report.read_text(encoding="utf-8")), args.prepared_directory, args.model)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"passed": report["passed"], "materials": len(report["materials"]),
                      "errors": [item for item in report["materials"] if item["errors"]]}))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
