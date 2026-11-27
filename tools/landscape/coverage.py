"""Measure primary visibility of the view-0 canopy, including filtered alpha cutouts."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

from calibrate import CAMERA_SHA256, cv2, np


# Leaf edges and sky in the top-left birch canopy. Full-film bounds, end exclusive.
BOX = (1420, 1484, 52, 116)
LIMITS = {"mean_abs_pixel_coverage": .02, "abs_covered_area_fraction": .01}


def prepare_matte(source, output):
    """Keep source shapes, opacity textures, instances, camera and Halton sampling.

    Black diffuse surfaces absorb every path; the constant sky measures misses.
    Write derived files outside the source tree and resolve asset paths at cook time.
    """
    geometry_path = source / "geometry/geometry.pbrt"
    camera_path = source / "view-0.pbrt"
    for path, digest in [(geometry_path, "38e43c2ffa51203556b8aa9c9152b93ca42690703f225e6661ca7610feaa6b9a"),
                         (camera_path, CAMERA_SHA256)]:
        if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
            raise ValueError(f"unexpected source revision at {path}")
    world = geometry_path.read_text(encoding="utf-8")
    command = re.compile(r"(?ms)^([ \t]*)([A-Z]\w*)\b(.*?)(?=^[ \t]*[A-Z]\w*\b|\Z)")

    def rewrite(match):
        indent, name, body = match.groups()
        if name == "Material":
            return f'{indent}Material "diffuse" "rgb reflectance" [0 0 0]\n'
        if name == "LightSource":
            return f'{indent}LightSource "infinite" "rgb L" [1 1 1]\n'
        return match.group()

    world = command.sub(rewrite, world)
    world = re.sub(r'("string filename"\s*\[?\s*")((?:geometry|textures)/[^"]+)"',
                   lambda m: m[1] + (source / m[2]).resolve().as_posix() + '"', world)
    geometry = (output / "matte-geometry.pbrt").resolve()
    geometry.write_text(world, encoding="utf-8")
    camera = camera_path.read_text(encoding="utf-8")
    entry = output / "matte-view-0.pbrt"
    entry.write_text(camera.replace('"geometry/geometry.pbrt"', f'"{geometry.as_posix()}"'), encoding="utf-8")
    return entry


def read_exr(path, shape):
    image = cv2.imread(str(path), cv2.IMREAD_UNCHANGED)
    if image is None or image.shape != (*shape, 3) or not np.isfinite(image).all():
        raise ValueError(f"expected {shape} RGB EXR at {path}")
    return image.mean(axis=2)


def difference(a, b):
    return {"mean_abs_pixel_coverage": float(np.abs(a-b).mean()),
            "abs_covered_area_fraction": float(abs(a.mean()-b.mean()))}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--render-with", type=Path)
    parser.add_argument("--threads", type=int, default=6)
    parser.add_argument("--candidate-mask", type=Path, help="linear grayscale opacity, native film or 64x64 probe")
    args = parser.parse_args()
    if args.candidate_mask:
        if args.render_with:
            parser.error("choose either a PBRT reference render or a candidate mask")
        oracle = Path(__file__).with_name("view-0-canopy-coverage.png")
        if hashlib.sha256(oracle.read_bytes()).hexdigest() != "80262133d2562ba726397820d908d73aa09a4f65a922607f1e1b6dc2cf230418":
            parser.error("unexpected canopy reference")
        ref = cv2.imread(str(oracle), cv2.IMREAD_UNCHANGED).astype(np.float32) / 65535
        candidate = cv2.imread(str(args.candidate_mask), cv2.IMREAD_UNCHANGED)
        if candidate is None or candidate.dtype not in (np.uint8, np.uint16):
            parser.error("expected 8-bit or 16-bit grayscale coverage")
        if candidate.shape == (2160,3840):
            x0,x1,y0,y1 = BOX
            candidate = candidate[y0:y1,x0:x1]
        if candidate.shape != ref.shape:
            parser.error("expected native film size or 64x64 grayscale coverage")
        metrics = difference(ref, candidate.astype(np.float32) / np.iinfo(candidate.dtype).max)
        failed = [key for key in LIMITS if metrics[key] > LIMITS[key]]
        print(json.dumps({"metrics": metrics, "limits": LIMITS, "failed": failed}, indent=2))
        return int(bool(failed))
    args.output.mkdir(parents=True, exist_ok=True)
    if args.render_with:
        scene = prepare_matte(args.source, args.output)
        for label, seed, box in [("sky", 0, (1980, 2044, 450, 514)), ("0", 0, BOX), ("13", 13, BOX)]:
            subprocess.run([str(args.render_with.resolve()), "--quiet", "--nthreads", str(args.threads),
                            "--spp", "512", "--seed", str(seed), "--pixelbounds", ",".join(map(str,box)),
                            "--outfile", str((args.output / f"coverage-{label}.exr").resolve()),
                            str(scene.resolve())], check=True)
    sky = read_exr(args.output / "coverage-sky.exr", (64, 64)).mean()
    if sky <= 0:
        raise ValueError("expected positive unobstructed sky radiance")
    def coverage(seed):
        return (1-read_exr(args.output / f"coverage-{seed}.exr", (64,64))/sky).clip(0,1)
    a, b = coverage(0), coverage(13)
    kernel = np.ones((3,3), np.uint8)
    result = {"reference_covered_fraction": float(a.mean()), "independent": difference(a,b),
              "dilate_1px": difference(a,cv2.dilate(a,kernel)),
              "erode_1px": difference(a,cv2.erode(a,kernel))}
    if not 0 < a.mean() < 1:
        raise ValueError("coverage probe must contain both sky and vegetation")
    for key, limit in LIMITS.items():
        if not result["independent"][key] <= limit < min(result[c][key] for c in ("dilate_1px", "erode_1px")):
            raise ValueError(f"invalid calibration for {key}")
    cv2.imwrite(str(args.output / "coverage-reference.png"), np.rint(65535*a).astype(np.uint16))
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
