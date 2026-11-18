"""Verify the full-scene Bloom on/off/exchange cohort."""

import argparse
from collections import defaultdict
from pathlib import Path

import numpy as np

from compare_runtime import compare_pixels, digest, load_capture, load_json
from resource_acceptance import require, validate_report, write_json


APIS = ("vulkan", "d3d12")
MODES = ("strict", "disabled")
EFFECTS = ("off", "tent", "box")
ROUTE = (0, 300, 600, 900, 1200, 1500, 1799)


def reports(root, report):
    """Resolve exactly the fixed views and route sample reports in this run."""
    views = {f"view/{item['view']}": root / "views" / item["view"] / "report.json"
             for item in report["views"]}
    require(len(views) == len(report["views"]) == 7, "Seven distinct fixed views required")
    require([item["frame"] for item in report["route_samples"]] == list(ROUTE),
            "Expected all seven route samples in order")
    route = {f"route/{frame}": root / "route" / str(frame) / "report.json" for frame in ROUTE}
    return views | route


def plane(path, report, name):
    capture_type = "hdr" if name == "bloom_hdr" else name
    capture = report["captures"][name]
    actual, meta = load_capture(path, {"captures": {capture_type: capture}}, capture_type)
    require((meta["width"], meta["height"]) ==
            (report["identity"]["width"], report["identity"]["height"]),
            f"{path}: {name} extent differs from the frame")
    return actual, meta


def verify(root):
    captures = {}
    run_identities = {}
    for mode in MODES:
        for api in APIS:
            for effect in EFFECTS:
                tag = f"{mode}-{api}-{effect}"
                path = root / tag / "report.json"
                report = load_json(path)
                identity = report["identity"]
                validation = report["validation"]
                require(identity["bloom"] == effect and identity["view"] == "route"
                        and (identity["width"], identity["height"]) == (1920, 1080), tag)
                require(report["runtime"]["api"] == api and validation["mode"] == mode
                        and validation["clean"] is True and
                        all(validation[key] == 0 for key in
                            ("warnings", "errors", "callback_failures")), tag)
                if effect == "tent":
                    validate_report(report, api=api, mode=mode, full_scene=True)
                require(report["memory"]["normal_allocation_free"] is True, tag)
                captures[mode, api, effect] = reports(path.parent, report)
                run_identities[tag] = {"report_sha256": digest(path), "runtime": report["runtime"],
                                       "identity": identity, "memory": report["memory"]["normal"]}

    sample_names = next(iter(captures.values())).keys()
    require(all(paths.keys() == sample_names for paths in captures.values()),
            "Mismatched fixed or route samples")
    changed = defaultdict(int)
    cross = defaultdict(int)
    for name in sample_names:
        for effect in EFFECTS:
            for key in ("hdr", "depth", "sdr") + (("bloom_hdr",) if effect != "off" else ()):
                inputs = {}
                for mode in MODES:
                    for api in APIS:
                        path = captures[mode, api, effect][name]
                        report = load_json(path)
                        identity = report["identity"]
                        expected = (identity.get("route_frame") if name.startswith("route/")
                                    else identity["view"])
                        require(str(expected) == name.split("/", 1)[1] and
                                identity["bloom"] == effect, f"Wrong capture identity {path}")
                        values, meta = plane(path, report, key)
                        inputs[mode, api] = values, meta["sha256"], identity
                require(all(item[2] == inputs["strict", "vulkan"][2] for item in inputs.values()),
                        f"Mismatched rendering identity {name}/{effect}")
                for api in APIS:
                    require(inputs["strict", api][1] == inputs["disabled", api][1],
                            f"Strict/shipping differed: {api} {name}/{effect}/{key}")
                fmt = "hdr" if key == "bloom_hdr" else key
                a, b = (inputs["disabled", api][0] for api in APIS)
                summary = compare_pixels(a, b, fmt, profile="scene")
                require(summary["passed"], f"API difference {name}/{effect}/{key}: {summary}")
                cross[key] += 1
        for api in APIS:
            paths = {effect: captures["disabled", api, effect][name] for effect in EFFECTS}
            selected = {effect: load_json(path)["captures"] for effect, path in paths.items()}
            for key in ("hdr", "depth"):
                require(len({selected[effect][key]["sha256"] for effect in EFFECTS}) == 1,
                        f"Bloom changed pre-effect {key}: {api}/{name}")
            hdr, _ = plane(paths["off"], load_json(paths["off"]), "hdr")
            for effect in ("tent", "box"):
                post, _ = plane(paths[effect], load_json(paths[effect]), "bloom_hdr")
                require(np.array_equal(hdr[:, :, 3], post[:, :, 3]),
                        f"Bloom changed alpha: {api}/{name}/{effect}")
            for first, second in (("off", "tent"), ("tent", "box")):
                changed[api, name.split("/", 1)[0], first, second] += (
                    selected[first]["sdr"]["sha256"] != selected[second]["sdr"]["sha256"])
    for api in APIS:
        for sample in ("view", "route"):
            for first, second in (("off", "tent"), ("tent", "box")):
                require(changed[api, sample, first, second] > 0,
                        f"Missing output change: {api}/{sample}/{first}/{second}")
    return {"schema_version": 1, "passed": True, "runs": run_identities,
            "cross_api_plane_comparisons": dict(cross),
            "changed_sdr_samples": {"/".join(key): value for key, value in changed.items()}}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    write_json(args.directory / "bloom-summary.json", verify(args.directory))
