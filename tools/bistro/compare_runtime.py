"""Compare consumer PBR captures using thresholds frozen before acceptance.

These profiles test numerical agreement, not physical or visual ground truth.
Every pixel is counted; no edge, alpha, or geometry mask is discarded. Named
regions add stricter local checks so a small material defect cannot hide in the
whole-image exceedance budget. Raw images stay local; the report contains only
hashes, settings, thresholds and statistics.
"""
import argparse
import copy
import hashlib
import json
import math
from pathlib import Path

import numpy as np


FORMATS = {
    "hdr": ("rgba16f-le", np.dtype("<f2"), 4),
    "sdr": ("rgba8-unorm", np.dtype("u1"), 4),
    "depth": ("r32f-le", np.dtype("<f4"), 1),
}
# Frozen before full-scene output inspection. A failed comparison requires a
# cause investigation, not a tolerance fitted to the failing pair of images.
PROFILES = {
    "repeat": {
        "version": 1, "bit_exact": True, "maximum_exceeding_fraction": 0.0,
        "planes": {name: {"absolute": 0.0, "relative": 0.0} for name in FORMATS},
    },
    "fixture": {
        "version": 1, "bit_exact": False, "maximum_exceeding_fraction": 0.0,
        "planes": {"hdr": {"absolute": 1 / 1024, "relative": 1 / 1024},
                   "sdr": {"absolute": 1.0, "relative": 0.0},
                   "depth": {"absolute": 2 ** -20, "relative": 0.0}},
    },
    "scene": {
        "version": 1, "bit_exact": False, "maximum_exceeding_fraction": 0.001,
        "planes": {"hdr": {"absolute": 0.005, "relative": 0.01},
                   "sdr": {"absolute": 3.0, "relative": 0.0},
                   "depth": {"absolute": 0.000002, "relative": 0.0}},
    },
}


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def canonical_hash(value):
    payload = json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False)
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def load_json(path):
    def reject(value):
        raise ValueError(f"Non-finite JSON number: {value}")

    def unique(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError(f"Duplicate JSON key: {key}")
            result[key] = value
        return result

    result = json.loads(Path(path).read_text(encoding="utf-8"),
                        parse_constant=reject, object_pairs_hook=unique)
    # JSON exponent overflow, e.g. 1e999, does not call parse_constant.
    def finite(value):
        if isinstance(value, float) and not math.isfinite(value):
            raise ValueError("Non-finite JSON number")
        if isinstance(value, dict):
            for item in value.values():
                finite(item)
        elif isinstance(value, list):
            for item in value:
                finite(item)
    finite(result)
    return result


def _positive_integer(value, label):
    if type(value) is not int or value <= 0:
        raise ValueError(f"{label} must be a positive integer")
    return value


def load_capture(report_path, report, plane):
    """Read an exact-sized raw artifact and verify all of its finite values."""
    if plane not in FORMATS:
        raise ValueError(f"Unknown capture plane: {plane}")
    capture = report["captures"][plane]
    expected_format, dtype, channels = FORMATS[plane]
    if capture["format"] != expected_format:
        raise ValueError(f"{plane} format must be {expected_format}")
    width = _positive_integer(capture["width"], f"{plane}.width")
    height = _positive_integer(capture["height"], f"{plane}.height")
    if width * height > 2 ** 28:
        raise ValueError("Capture exceeds the offline image-size limit")
    root = Path(report_path).resolve().parent
    name = capture["path"]
    if not isinstance(name, str) or not name:
        raise ValueError("Capture path must be a nonempty string")
    path = (root / name).resolve()
    if not path.is_relative_to(root):
        raise ValueError("Capture path escapes the report directory")
    expected_size = width * height * channels * dtype.itemsize
    if path.stat().st_size != expected_size:
        raise ValueError(f"{plane} raw byte length differs from dimensions/format")
    payload = path.read_bytes()
    if len(payload) != expected_size:
        raise ValueError(f"{plane} raw byte length changed while reading")
    actual_hash = hashlib.sha256(payload).hexdigest()
    if "sha256" in capture and capture["sha256"] != actual_hash:
        raise ValueError(f"{plane} capture SHA-256 differs from report")
    values = np.frombuffer(payload, dtype=dtype).reshape(height, width, channels)
    validate_pixels(values, plane)
    return values, {"format": expected_format, "width": width, "height": height,
                    "bytes": expected_size, "sha256": actual_hash}


def validate_pixels(values, plane):
    if plane not in FORMATS:
        raise ValueError(f"Unknown capture plane: {plane}")
    _, dtype, channels = FORMATS[plane]
    if values.ndim != 3 or values.shape[2] != channels or not all(values.shape):
        raise ValueError(f"{plane} must be a nonempty height/width/{channels} array")
    if values.dtype != dtype:
        raise ValueError(f"{plane} has the wrong component representation")
    if not np.all(np.isfinite(values)):
        raise ValueError(f"{plane} capture contains NaN or infinity")
    if plane == "depth" and (np.any(values < 0) or np.any(values > 1)):
        raise ValueError("Depth capture is outside normal-Z [0, 1]")


def compare_pixels(first, second, plane, profile="fixture", *, local=False):
    """Compare all channels; SDR errors are in integer 8-bit code values."""
    validate_pixels(first, plane)
    validate_pixels(second, plane)
    if first.shape != second.shape:
        raise ValueError("Capture dimensions differ")
    if profile not in PROFILES:
        raise ValueError(f"Unknown threshold profile: {profile}")
    threshold = PROFILES[profile]
    a, b = first.astype(np.float64), second.astype(np.float64)
    absolute = np.abs(a - b)
    bound = threshold["planes"][plane]
    allowed = bound["absolute"] + bound["relative"] * np.maximum(np.abs(a), np.abs(b))
    if threshold["bit_exact"]:
        # Signed zeros are numerically equal but are not bit-identical repeats.
        channels_bytes = first.shape[2] * first.dtype.itemsize
        first_bits = np.ascontiguousarray(first).view(np.uint8).reshape(*first.shape[:2], channels_bytes)
        second_bits = np.ascontiguousarray(second).view(np.uint8).reshape(*second.shape[:2], channels_bytes)
        exceeding = np.any(first_bits != second_bits, axis=2)
    else:
        exceeding = np.any(absolute > allowed, axis=2)
    pixels = first.shape[0] * first.shape[1]
    count = int(np.count_nonzero(exceeding))
    maximum_fraction = 0.0 if local else threshold["maximum_exceeding_fraction"]
    # Integer count comparison avoids rounding a ratio just below the budget.
    budget = math.floor(maximum_fraction * pixels)
    return {"passed": count <= budget, "pixels": pixels,
            "exceeding_pixels": count, "exceeding_fraction": count / pixels,
            "maximum_exceeding_pixels": budget,
            "maximum_absolute_component_error": float(absolute.max()),
            "mean_absolute_component_error": float(absolute.mean()),
            "root_mean_square_component_error": float(np.sqrt(np.mean(absolute ** 2))),
            "p95_absolute_component_error": float(np.quantile(absolute, 0.95)),
            "threshold": {**bound, "bit_exact": threshold["bit_exact"],
                          "maximum_exceeding_fraction": maximum_fraction}}


def _regions(specification, width, height, planes):
    names = set()
    result = []
    for region in specification:
        if not isinstance(region, dict) or set(region) - {"name", "rectangle", "planes", "minimum_changed_pixels"}:
            raise ValueError("ROI supports only name/rectangle/planes/minimum_changed_pixels; no exclusion masks")
        name = region["name"]
        if not isinstance(name, str) or not name or name in names:
            raise ValueError("ROI names must be nonempty and unique")
        names.add(name)
        rectangle = region["rectangle"]
        if (not isinstance(rectangle, list) or len(rectangle) != 4
                or any(type(value) is not int for value in rectangle)):
            raise ValueError("ROI rectangle must contain four integers [x,y,width,height]")
        x, y, w, h = rectangle
        if x < 0 or y < 0 or w <= 0 or h <= 0 or x + w > width or y + h > height:
            raise ValueError("ROI rectangle is empty or outside the image")
        selected = region.get("planes", planes)
        if (not isinstance(selected, list) or not selected or len(set(selected)) != len(selected)
                or any(plane not in planes for plane in selected)):
            raise ValueError("ROI planes must be distinct selected capture planes")
        minimum = region.get("minimum_changed_pixels", 1)
        _positive_integer(minimum, "ROI minimum_changed_pixels")
        if minimum > w * h:
            raise ValueError("ROI minimum_changed_pixels exceeds its size")
        result.append({"name": name, "rectangle": rectangle, "planes": selected,
                       "minimum_changed_pixels": minimum})
    return result


def compare_reports(first_path, second_path, *, profile, planes=None, regions=None,
                    expect_different=False, identity_changes=None):
    first_path, second_path = Path(first_path), Path(second_path)
    first, second = load_json(first_path), load_json(second_path)
    if any(type(item.get("schema_version")) is not int or item["schema_version"] != 1 for item in (first, second)):
        raise ValueError("Runner report schema_version must be 1")
    selected = list(FORMATS) if planes is None else list(planes)
    if not selected or len(set(selected)) != len(selected) or any(p not in FORMATS for p in selected):
        raise ValueError("Select distinct supported capture planes")
    if profile not in PROFILES:
        raise ValueError(f"Unknown threshold profile: {profile}")
    if any(not isinstance(item.get("identity"), dict) or not item["identity"] for item in (first, second)):
        raise ValueError("Runner report needs a nonempty rendering identity")
    for item in (first, second):
        _positive_integer(item["identity"].get("width"), "identity.width")
        _positive_integer(item["identity"].get("height"), "identity.height")
    apis = [item.get("runtime", {}).get("api") for item in (first, second)]
    if any(api not in ("vulkan", "d3d12") for api in apis):
        raise ValueError("Runner reports need an explicit supported runtime.api")
    if (profile == "repeat" or expect_different) and apis[0] != apis[1]:
        raise ValueError("Repeat and controlled sensitivity require the same API")
    identity_equal = first["identity"] == second.get("identity")
    if expect_different:
        expected = dict(first["identity"])
        for key, change in (identity_changes or {}).items():
            if (not isinstance(change, list) or len(change) != 2
                    or key not in expected or expected[key] != change[0]):
                raise ValueError("Identity changes must name an exact existing [before,after] pair")
            expected[key] = change[1]
        if expected != second.get("identity"):
            raise ValueError("Sensitivity input identity differs beyond the declared changes")
    elif identity_changes:
        raise ValueError("Identity changes are only for controlled sensitivity checks")
    elif not identity_equal:
        raise ValueError("Rendering identities differ; this is not an agreement comparison")
    if expect_different and not regions:
        raise ValueError("Known-wrong sensitivity checks require explicit named ROIs")
    output = {"schema_version": 1, "comparison": "known-wrong-sensitivity" if expect_different else "agreement",
              "profile": profile, "frozen_thresholds": copy.deepcopy(PROFILES[profile]),
              "thresholds_sha256": canonical_hash(PROFILES[profile]),
              "comparator_sha256": digest(Path(__file__)),
              "scope": "Numerical agreement only; all pixels/channels counted; no edge exclusions; not physical/visual ground truth.",
              "apis": apis, "identity_equal": identity_equal,
              "declared_identity_changes": identity_changes or {},
              "identities": [first["identity"], second.get("identity")],
              "report_sha256": [digest(first_path), digest(second_path)], "planes": {}, "regions": []}
    captures = {}
    dimensions = None
    for plane in selected:
        a, a_info = load_capture(first_path, first, plane)
        b, b_info = load_capture(second_path, second, plane)
        for pixels, source in ((a, first), (b, second)):
            if pixels.shape[:2] != (source["identity"]["height"], source["identity"]["width"]):
                raise ValueError("Capture dimensions differ from rendering identity")
        if a.shape != b.shape:
            raise ValueError("Capture dimensions differ")
        current = a.shape[:2]
        if dimensions is not None and dimensions != current:
            raise ValueError("Capture planes have different image dimensions")
        dimensions = current
        captures[plane] = (a, b)
        output["planes"][plane] = {"artifacts": [a_info, b_info],
                                   **compare_pixels(a, b, plane, profile)}
    height, width = dimensions
    for region in _regions(regions or [], width, height, selected):
        x, y, w, h = region["rectangle"]
        results = {}
        for plane in region["planes"]:
            a, b = captures[plane]
            metrics = compare_pixels(a[y:y + h, x:x + w], b[y:y + h, x:x + w],
                                     plane, profile, local=True)
            if expect_different:
                metrics["passed"] = metrics["exceeding_pixels"] >= region["minimum_changed_pixels"]
            results[plane] = metrics
        output["regions"].append({**region, "planes": results,
                                  "passed": all(item["passed"] for item in results.values())})
    if expect_different:
        output["passed"] = all(item["passed"] for item in output["regions"])
    else:
        output["passed"] = (all(item["passed"] for item in output["planes"].values())
                            and all(item["passed"] for item in output["regions"]))
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("first", type=Path, help="First runner report.json")
    parser.add_argument("second", type=Path, help="Second runner report.json")
    parser.add_argument("--profile", choices=PROFILES, required=True)
    parser.add_argument("--planes", nargs="+", choices=FORMATS, default=list(FORMATS))
    parser.add_argument("--regions", type=Path, help="JSON list of named [x,y,width,height] ROIs")
    parser.add_argument("--expect-different", action="store_true", help="Each ROI/plane must detect a known wrong variant")
    parser.add_argument("--identity-changes", type=Path,
                        help="Sensitivity-only JSON {identity_key:[exact_before,exact_after]}")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = compare_reports(args.first, args.second, profile=args.profile, planes=args.planes,
                             regions=load_json(args.regions) if args.regions else None,
                             expect_different=args.expect_different,
                             identity_changes=load_json(args.identity_changes) if args.identity_changes else None)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    print(json.dumps({"passed": result["passed"], "profile": args.profile,
                      "thresholds_sha256": result["thresholds_sha256"]}))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
