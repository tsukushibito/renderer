"""Capture the complete IBL-lit Landscape film in bounded native raster tiles."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

os.environ.setdefault("OPENCV_IO_ENABLE_OPENEXR", "1")
import cv2
import numpy as np
from PIL import Image

from measure_capture import run_viewer, submitted, trace_viewer


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("viewer", type=Path)
    parser.add_argument("package", type=Path)
    parser.add_argument("view")
    parser.add_argument("output", type=Path)
    parser.add_argument("--samples", type=int, default=1)
    parser.add_argument("--tile", type=int, nargs=2, default=(384, 216), metavar=("WIDTH", "HEIGHT"))
    parser.add_argument("--measure", action="store_true", help="count actual native draws and heap peaks on Windows")
    args = parser.parse_args()
    source = args.package / "scene.json"
    scene = json.loads(source.read_bytes())
    width, height = scene["physical"]["cameras"][args.view]["film_extent"]
    if scene["settings"]["sun_lux"] != 0:
        parser.error("independently cropped cascaded shadow maps cannot reproduce a whole-film shadow fit")
    del scene
    if min(*args.tile, args.samples) < 1:
        parser.error("tile extent and sample count must be positive")
    args.output.mkdir(parents=True, exist_ok=True)
    manifest = args.output / "capture-settings.json"
    settings = dict(scene=digest(source), viewer=digest(args.viewer), view=args.view,
                    tile=list(args.tile), samples=args.samples, measured=args.measure)
    if manifest.exists():
        if json.loads(manifest.read_bytes()) != settings:
            parser.error("choose a fresh output directory for different settings")
    else:
        if any(args.output.iterdir()):
            parser.error("choose an empty output directory")
        manifest.write_text(json.dumps(settings, indent=2), encoding="utf-8")
    linear = np.empty((height, width, 3), dtype=np.float32)
    display = Image.new("RGB", (width, height))
    records = []
    for y in range(0, height, args.tile[1]):
        for x in range(0, width, args.tile[0]):
            w, h = min(args.tile[0], width-x), min(args.tile[1], height-y)
            prefix = args.output / f"x{x}-y{y}"
            exr, png, receipt = [prefix.with_suffix(ext) for ext in (".exr", ".png", ".json")]
            if not receipt.exists():
                argv = [str(args.viewer.resolve()), str(args.package.resolve()), args.view, "--film-crop",
                        str(args.samples), f"{x},{y},{w},{h}", str(exr), str(png)]
                stats = {}
                if args.measure:
                    stats.update(run_viewer(argv, os.environ.copy()))
                    with tempfile.TemporaryDirectory(dir=args.output) as scratch:
                        trace = Path(scratch).resolve() / "api.txt"
                        trace_viewer(argv, trace)
                        stats.update(submitted(trace))
                else:
                    subprocess.run(argv, check=True)
                stats.update(exr=digest(exr), png=digest(png), rect=[x,y,w,h])
                tmp = receipt.with_suffix(".tmp")
                tmp.write_text(json.dumps(stats), encoding="utf-8")
                tmp.replace(receipt)
                print(f"completed {x},{y} ({w} x {h})", flush=True)
            record = json.loads(receipt.read_bytes())
            if record["rect"] != [x, y, w, h]:
                raise ValueError(f"tile rectangle differs from receipt: {prefix}")
            if record["exr"] != digest(exr) or record["png"] != digest(png):
                raise ValueError(f"tile contents differ from receipt: {prefix}")
            tile = cv2.imread(str(exr), cv2.IMREAD_UNCHANGED)
            if tile is None or tile.shape != (h,w,3) or not np.isfinite(tile).all():
                raise ValueError(f"bad linear tile at {x},{y}")
            linear[y:y+h,x:x+w] = tile
            with Image.open(png) as image:
                if image.size != (w,h) or image.mode != "RGB":
                    raise ValueError(f"bad display tile at {x},{y}")
                display.paste(image, (x,y))
            records.append(record)
    if not cv2.imwrite(str(args.output / "film.exr"), linear):
        raise OSError("failed to write full linear film")
    display.save(args.output / "film.png")
    if args.measure:
        summed = ("native_draws","indexed_draws","indexed_triangles","queue_submits","wall_seconds","observed_cpu_seconds")
        totals = {key: sum(r[key] for r in records) for key in summed}
        totals["observed_peak_working_set_bytes"] = max(r["observed_peak_working_set_bytes"] for r in records)
        totals["native_heap_peak_bytes"] = {heap: max(r["native_heap_peak_bytes"].get(heap, 0) for r in records)
            for heap in set().union(*(r["native_heap_peak_bytes"] for r in records))}
        (args.output / "totals.json").write_text(json.dumps(totals,indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
