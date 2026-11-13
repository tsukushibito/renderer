"""Publish current asset evidence from ignored working files; shared payloads live in Git LFS."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def require_successful_checks(source_bindings, geometry, materials, validator, reproduction):
    for name, value in (("source bindings", source_bindings["passed"]),
                        ("geometry", geometry["ok"]), ("materials", materials["passed"]),
                        ("reproduction", reproduction["passed"])):
        if value is not True:
            raise ValueError(f"Cannot publish failed {name} evidence")
    issues = validator["issues"]
    if type(issues["numErrors"]) is not int or issues["numErrors"] != 0 or issues["truncated"] is not False:
        raise ValueError("Cannot publish failed or truncated validator evidence")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("work", type=Path)
    parser.add_argument("export", type=Path)
    parser.add_argument("output", type=Path)
    for name in ("source-bindings", "geometry", "materials", "validator", "reproduction", "preview", "views", "alpha", "conditions"):
        parser.add_argument(f"--{name}-report", type=Path, required=True)
    args = parser.parse_args()
    load = lambda path: json.loads(path.read_text(encoding="utf-8"))
    source_bindings = load(args.source_bindings_report)
    geometry = load(args.geometry_report)
    materials = load(args.materials_report)
    validator = load(args.validator_report)
    reproduction = load(args.reproduction_report)
    require_successful_checks(source_bindings, geometry, materials, validator, reproduction)
    audit = load(args.work / "logs/source-inventory.json")
    baseline = load(args.export / "baseline.json")
    alpha = load(args.alpha_report)
    conditions = load(args.conditions_report)
    root = Path(__file__).resolve().parents[2]
    config_path = (root / conditions["config"]["path"]).resolve()
    if not config_path.is_relative_to(root) or sha(config_path) != conditions["config"]["sha256"]:
        raise ValueError("Conditions configuration differs from verified report")
    conditions_config = load(config_path)
    pinned = {entry["path"]: entry["sha256"] for entry in conditions_config["inputs"]}
    rendered = conditions["comparison"]["render_report"]
    if rendered["config_sha256"] != conditions["config"]["sha256"]:
        raise ValueError("Conditions images were produced from another configuration")
    gltf_sha = sha(args.export / "BistroExterior.gltf")
    if (pinned["assets/bistro/converted/BistroExterior.gltf"] != gltf_sha
            or rendered["input_files_sha256"]["BistroExterior.gltf"] != gltf_sha):
        raise ValueError("Conditions do not cover this converted glTF")
    if alpha["decision"]["source_fbx_sha256"] != baseline["source_sha256"]:
        raise ValueError("Alpha decision does not cover this source FBX")
    textures = load(args.work / "logs/textures.json")
    archive = args.work / "downloads/Bistro_v5_2.zip"
    source = args.work / "source/Bistro_v5_2"
    for texture in textures["textures"]:
        raw = texture.get("source_mip_levels_raw", texture["source_mip_levels"])
        texture["source_mip_levels_raw"] = raw
        texture["source_mip_levels"] = max(1, raw)
    report = {
        "schema_version": 1,
        "scope": "Bistro v5.2 exterior source inspection and explicit conversion-profile verification, not Renderer/GPU/artist-intent acceptance",
        "asset": {"title": "Amazon Lumberyard Bistro", "author": "Amazon Lumberyard", "archive": "NVIDIA ORCA",
                  "page": "https://developer.nvidia.com/orca/amazon-lumberyard-bistro",
                  "download_entry": "https://developer.nvidia.com/bistro", "filename": archive.name,
                  "bytes": archive.stat().st_size, "sha256": sha(archive),
                  "download_completed_utc": datetime.fromtimestamp(archive.stat().st_mtime, timezone.utc).isoformat(),
                  "license": "CC-BY-4.0", "license_url": "https://creativecommons.org/licenses/by/4.0/",
                  "license_file_sha256": sha(source / "LICENSE.txt"), "readme_sha256": sha(source / "README.txt"),
                  "fbx_filename": "BistroExterior.fbx", "fbx_sha256": sha(source / "BistroExterior.fbx"),
                  "modifications": "DDS base mip decoded to PNG; BC5 Z reconstruction and DX-to-GL normal green inversion; explicit PBR/alpha recipe; duplicate-face vertex separation; authored normal-preserving glTF conversion"},
        "tools": {"blender": baseline["blender"], "blender_build": baseline["blender_build"],
                  "blender_zip_sha256": sha(args.work / "downloads/blender-4.5.16-windows-x64.zip"),
                  "python_numpy": textures["numpy"], "pillow": textures["pillow"],
                  "gltf_validator": validator["validatorVersion"],
                  "script_sha256": baseline["script_sha256"], "source_audit_sha256": baseline["source_audit_sha256"],
                  "texture_report_sha256": baseline["texture_report_sha256"]},
        "source": {"summary": audit["summary"], "validated_scope": audit.get("validated_scope"),
                   "global_settings": audit["global_settings"],
                   "geometry_attributes": ["POSITION", "authored corner NORMAL", "TextureUV", "LightMapUV"],
                   "absent_authored_attributes": ["TANGENT", "COLOR"],
                   "readme_unique_triangles": 2799704,
                   "identity_note": "FBX has 1296 Geometry ids each bound to one Mesh Model. Parsed triangles total is 2832120; README unique count is an author catalog value, not an applied buffer-dedup result.",
                   "missing_texture_references": audit["missing_texture_references"],
                   "basename_collisions": audit["basename_collisions"]},
        "conversion": {"importer": baseline["importer_change"], "duplicate_preservation": baseline["duplicate_preservation"],
                       "normal_export": baseline["normal_export"], "normal_rounding": baseline["export_normal_rounding_digits"],
                       "tangents": baseline["tangent_policy"], "snapshot": baseline["snapshot"],
                       "sampling": "TextureUV, repeat S/T, linear magnification, trilinear minification",
                       "mip_scope": textures["mip_policy"],
                       "materials": baseline["materials"], "textures": textures["textures"]},
        "checks": {"source_bindings": source_bindings, "geometry": geometry, "materials": {"passed": materials["passed"],
                   "materials": len(materials["materials"]), "unique_pixel_comparisons": materials["unique_pixel_comparisons"]},
                   "validator": validator["issues"], "reproduction": reproduction,
                   "preview": load(args.preview_report),
                   "preview_views": load(args.views_report)},
        "alpha_interpretation": {"issue": "renderer-7ft.1.1.2", "report": args.alpha_report.name,
                                 "report_sha256": sha(args.alpha_report), "decision": alpha["decision"]},
        "render_conditions": {"issue": "renderer-7ft.1.1", "report": args.conditions_report.name,
                              "report_sha256": sha(args.conditions_report), "config": conditions["config"]},
        "unresolved": [
            {"issue": "renderer-7ft.3", "problem": "Runtime/cook MikkTSpace generation, degenerate-input fallback and DDS/native mip policy remain required; source contains no tangents and this glTF stores base image mip only"},
            {"issue": "renderer-7ft.6", "problem": "Runtime image threshold calibration, GPU performance/VRAM budgets and integrated two-API acceptance remain unexecuted; fixed B0 camera/light conditions are inputs, not proof of runtime correctness"},
        ],
        "not_executed": ["Renderer Bistro implementation", "Vulkan/D3D12 image or window acceptance",
                         "GPU performance/VRAM capacity", "All original DDS mip pixels in glTF", "Proof of original artist alpha-mode intent"],
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2, allow_nan=False) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps({"report": str(args.output), "geometry": geometry["ok"], "materials": materials["passed"],
                      "reproduced": reproduction["passed"], "validator_errors": validator["issues"]["numErrors"]}))


if __name__ == "__main__":
    main()
