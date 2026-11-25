"""Capture the native Forward reference views and Sponza rectangular-light controls."""

import argparse
import json
from pathlib import Path
import subprocess
import sys

TOOLS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TOOLS / "sponza"))
from compare_rect_shadows import VIEWS as SPONZA_VIEWS

VIEWS = {
    "bistro": [("bistro-corner", (1920, 1061), None)],
    "sponza": SPONZA_VIEWS,
}
LOOKS = {"bistro": "lumberyard-day", "sponza": "o3de-atrium"}


def light_modes(scene):
    return ("shadowed", "off", "unshadowed") if scene == "sponza" else ("shadowed",)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scene", choices=VIEWS)
    for key in ("binary", "manifest", "ibl-manifest", "baked-lighting", "reflection-probes", "output"):
        parser.add_argument(f"--{key}", type=Path, required=True)
    args = parser.parse_args()
    for view, (width, height), _ in VIEWS[args.scene]:
        for mode in light_modes(args.scene):
            dest = args.output / f"{view}-{mode}"
            subprocess.run(
                [
                    str(args.binary.resolve()),
                    "--api",
                    "vulkan",
                    "--pipeline",
                    "forward",
                    "--manifest",
                    str(args.manifest),
                    "--ibl-manifest",
                    str(args.ibl_manifest),
                    "--baked-lighting",
                    str(args.baked_lighting),
                    "--reflection-probes",
                    str(args.reflection_probes),
                    "--look",
                    LOOKS[args.scene],
                    "--view",
                    view,
                    "--width",
                    str(width),
                    "--height",
                    str(height),
                    "--inflight",
                    "1",
                    "--warmup",
                    "2",
                    "--frames",
                    "4",
                    "--rect-lights",
                    mode,
                    "--output",
                    str(dest),
                ],
                check=True,
            )
            report = json.loads((dest / "report.json").read_text(encoding="utf-8"))
            validation = report["validation"]
            if validation["mode"] != "strict" or not validation["clean"]:
                raise ValueError(f"Native validation failed or was disabled: {validation}")


if __name__ == "__main__":
    main()
