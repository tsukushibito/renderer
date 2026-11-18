"""Identity and checkpoint helpers shared by the Bistro B5 gates."""

from pathlib import Path
from uuid import uuid4

from compare_runtime import _inside_directory, digest
from resource_acceptance import require, write_json


SAMPLES = (1, 4)
PIPELINES = ("forward", "forward-plus", "forward-reference", "deferred")
SUBMISSIONS = ("direct", "packed", "indirect")
INSTANCES = ("off", "individual", "instanced")


def render_modes(report, *, windowed):
    require(type(report.get("schema_version")) is int and report["schema_version"] == 1,
            "Expected a current Bistro report")
    identity = report if windowed else report["identity"]
    modes = {}
    for field, choices in (("samples", SAMPLES), ("pipeline", PIPELINES),
                           ("submission", SUBMISSIONS), ("instances", INSTANCES)):
        value = identity.get(field)
        require(type(value) is type(choices[0]) and value in choices,
                f"Missing or invalid Bistro report field: {field}")
        modes[field] = value
    return modes


def loaded_inputs(report, *, windowed):
    if windowed:
        identity = report["inputs"]
        ibl = identity["ibl_manifest_sha256"]
    else:
        identity = report["identity"]
        ibl = identity["environment"]["manifest_sha256"]
    return {"cook_manifest_sha256": identity["cook_manifest_sha256"],
            "ibl_manifest_sha256": ibl,
            "conditions_sha256": identity["conditions_sha256"],
            "shaders_sha256": identity["shaders_sha256"]}


def validate_loaded_manifests(report, cook, ibl, *, windowed):
    identity = loaded_inputs(report, windowed=windowed)
    require(identity["cook_manifest_sha256"] == cook and identity["ibl_manifest_sha256"] == ibl,
            "Bistro loaded a different cook or IBL manifest")


def write_checkpoint(path, value):
    temporary = path.with_name(f".{path.name}-{uuid4().hex}.tmp")
    try:
        write_json(temporary, value)
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def attempt_directory(root, label):
    directory = root / label
    suffix = 1
    while directory.exists() or directory.is_symlink():
        suffix += 1
        directory = root / f"{label}-attempt{suffix}"
    return directory


def report_record(root, path):
    resolved = path.resolve(strict=True)
    require(_inside_directory(resolved, root), f"Report outside the evidence directory: {path}")
    return {"path": path.relative_to(root).as_posix(), "sha256": digest(resolved)}


def verified_report(root, record):
    relative = Path(record["path"])
    require(not relative.anchor and ".." not in relative.parts, f"Invalid report path: {relative}")
    path = root / relative
    require(report_record(root, path) == record, f"Stored report differs: {path}")
    return path
