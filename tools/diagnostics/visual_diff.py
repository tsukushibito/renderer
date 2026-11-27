"""Compare recorded views in sRGB byte space and package failures for inspection."""

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path, PureWindowsPath
import shutil
import sys

import numpy as np
from PIL import Image


def write(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False), encoding="utf-8")


def artifact(root, relative):
    path = (root / relative).resolve()
    if not path.is_relative_to(root.resolve()):
        raise ValueError(f"artifact outside report: {relative}")
    return path


def images(source, tick):
    if source.is_file():
        return {("main", "sdr", 0, 0): (source, {"frame": "0", "tick": "0"})}
    m = json.loads((source / "manifest.json").read_bytes())
    if m["version"] != 1 or m["outcome"]["state"] not in ("partial", "complete"):
        raise ValueError(f"unavailable capture: {source}")
    result = {}
    occurrences = Counter()
    for frame in m["frames"]:
        key = (frame["view"], frame["stage"], int(frame["tick"]))
        if tick is not None and key[2] != tick:
            continue
        role = f"frame_{int(frame['frame']):06d}"
        if role not in m["files"] and len(m["frames"]) == 1:
            role = "image"
        path = artifact(source, m["files"][role])
        result[key + (occurrences[key],)] = (path, frame)
        occurrences[key] += 1
    return result


def copy_report(source, target):
    if not source.is_dir():
        return
    target.mkdir()
    manifest = source / "manifest.json"
    shutil.copyfile(manifest, target / manifest.name)
    for relative in set(json.loads(manifest.read_bytes())["files"].values()):
        src = artifact(source, relative)
        dst = artifact(target, relative)
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dst)


def compare(baseline, candidate, output, channel=0, fraction=0.0, tick=None):
    if not 0 <= channel <= 255 or not 0 <= fraction <= 1:
        raise ValueError("channel tolerance must be 0..255 and pixel fraction 0..1")
    expected, actual = images(baseline, tick), images(candidate, tick)
    if not expected and not actual:
        raise ValueError("no comparable still frames at the requested tick")
    output.mkdir(parents=True, exist_ok=False)
    report = {
        "space": "sRGB byte channels including alpha",
        "channel_tolerance": channel,
        "allowed_changed_fraction": fraction,
        "frames": [],
        "passed": True,
    }
    for index, key in enumerate(sorted(expected.keys() | actual.keys())):
        row = {"view": key[0], "stage": key[1], "tick": key[2], "occurrence": key[3]}
        report["frames"].append(row)
        first, second = expected.get(key), actual.get(key)
        diff = None
        if first is None or second is None:
            row.update(
                passed=False, reason="missing baseline" if first is None else "missing candidate"
            )
        else:
            row.update(baseline_frame=first[1]["frame"], candidate_frame=second[1]["frame"])
            with Image.open(first[0]) as a, Image.open(second[0]) as b:
                if a.size != b.size:
                    row.update(
                        passed=False,
                        reason="extent mismatch",
                        baseline_size=a.size,
                        candidate_size=b.size,
                    )
                else:
                    delta = np.abs(
                        np.asarray(a.convert("RGBA"), dtype=np.int16)
                        - np.asarray(b.convert("RGBA"), dtype=np.int16)
                    )
                    pixel = delta.max(axis=2)
                    changed = int(np.count_nonzero(pixel > channel))
                    row.update(
                        changed=changed,
                        pixels=pixel.size,
                        max_error=int(pixel.max()),
                        passed=bool(changed <= fraction * pixel.size),
                    )
                    if not row["passed"]:
                        diff = Image.fromarray(np.minimum(pixel * 8, 255).astype("uint8"))
        if not row["passed"]:
            report["passed"] = False
            directory = output / f"frame-{index:06d}"
            directory.mkdir()
            for item, name in ((first, "baseline"), (second, "candidate")):
                if item:
                    with Image.open(item[0]) as image:
                        image.save(directory / (name + ".png"))
                    row[name] = f"{directory.name}/{name}.png"
            if diff is not None:
                diff.save(directory / "diff.png")
                row["diff"] = f"{directory.name}/diff.png"
    if not report["passed"]:
        for name, src in (("baseline", baseline), ("candidate", candidate)):
            if src.is_dir():
                copy_report(src, output / name)
                report[name + "_report"] = name + "/manifest.json"
    write(output / "comparison.json", report)
    return report["passed"]


def bistro(output, reports):
    # Reuse the existing Bistro format, byte length and digest checks.
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "bistro"))
    from compare_runtime import load_capture

    output.mkdir(parents=True, exist_ok=False)
    manifest = dict(
        version=1,
        session="bistro",
        world="bistro",
        job="import",
        build="source reports",
        assets={},
        settings={
            "source": "Bistro capture reports",
            "clock": "route ticks at 60 Hz; fixed views at tick zero",
        },
        requested_tick="0",
        frames=[],
        gaps=[],
        files={},
        outcome={"state": "recording"},
    )
    write(output / "manifest.json", manifest)
    try:
        for i, source in enumerate(reports):
            r = json.loads(source.read_bytes())
            saved = PureWindowsPath(r["captures"]["sdr"]["path"])
            if (
                saved.is_absolute()
                and saved.name == "sdr.bin"
                and (source.parent / "sdr.bin").is_file()
            ):
                # Relocated native runs keep their original absolute path. Read only
                # the adjacent named plane and require its original size and digest.
                r["captures"]["sdr"]["path"] = "sdr.bin"
            data, meta = load_capture(source, r, "sdr")
            identity = r["identity"]
            view = identity["view"]
            tick = identity.get("route_frame", 0)
            role = f"frame_{i:06d}"
            path = role + ".png"
            Image.fromarray(data).save(output / path)
            manifest["files"][role] = path
            metadata = role + "-source.json"
            write(
                output / metadata,
                {
                    "identity": identity,
                    "capture": meta,
                    "report_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
                },
            )
            manifest["files"][role + "_source"] = metadata
            manifest["frames"].append(
                dict(
                    frame=str(i),
                    tick=str(tick),
                    view=view,
                    stage="sdr",
                    extent=[meta["width"], meta["height"]],
                    elapsed_ns=str(tick * 1_000_000_000 // 60),
                    interpolation=None,
                )
            )
        identity_hash = hashlib.sha256()
        for source in reports:
            identity_hash.update(hashlib.sha256(source.read_bytes()).digest())
        manifest["session"] = "bistro-" + identity_hash.hexdigest()[:16]
        manifest["outcome"] = {"state": "complete"}
    except Exception as error:
        manifest["outcome"] = {"state": "failed", "reason": str(error)}
        raise
    finally:
        write(output / "manifest.json", manifest)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    cmp = sub.add_parser("compare")
    for name in ("baseline", "candidate", "output"):
        cmp.add_argument(name, type=Path)
    cmp.add_argument("--channel-tolerance", type=int, default=0)
    cmp.add_argument("--changed-fraction", type=float, default=0.0)
    cmp.add_argument("--tick", type=int)
    imp = sub.add_parser("bistro")
    imp.add_argument("output", type=Path)
    imp.add_argument("reports", nargs="+", type=Path)
    args = parser.parse_args()
    if args.action == "bistro":
        bistro(args.output, args.reports)
    elif not compare(
        args.baseline,
        args.candidate,
        args.output,
        args.channel_tolerance,
        args.changed_fraction,
        args.tick,
    ):
        print(f"visual regression: {args.output/'comparison.json'}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
