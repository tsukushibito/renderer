"""Deterministic, consumer-owned Bistro IBL cook; no runtime GPU preprocessing.

Normal Python invokes the repository's pinned Blender only to decode Radiance.
All convolution and DFG integration uses explicit float64 NumPy arithmetic.
Images are top-origin, tightly packed little-endian RGBA16F, mip zero first.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
import subprocess
import sys
import tempfile

import numpy as np


SOURCE_SHA256 = "5eb82f21bced3f7e901774083cfee1a05fd2bcf22f56ff699f3171c1eec65a50"
BLENDER_VERSION = (4, 5, 16)
BLENDER_BUILD = "3f6797307da5"
MAPPING = "u=fract(.5+atan2(z,x)/(2*pi));v=acos(y)/pi;top-origin;U-repeat/V-clamp"
DIFFUSE_SIZE = (128, 64)
SPECULAR_SIZE = (1024, 512)
BRDF_SIZE = (256, 256)
SAMPLES = 1024
CAP = 4.0
MAX_PREFILTER_LOD = 4


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def tool_hash():
    return hashlib.sha256(Path(__file__).read_text(encoding="utf-8").encode("utf-8")).hexdigest()


def finite_rgb(pixels):
    value = np.asarray(pixels, dtype=np.float64)
    require(value.ndim == 3 and value.shape[2] >= 3 and min(value.shape[:2]) > 0,
            "Expected nonempty HxWxRGB pixels")
    value = value[:, :, :3]
    require(np.isfinite(value).all() and (value >= 0).all(), "Radiance must be finite and nonnegative")
    return value


def capped_source(pixels):
    value = finite_rgb(pixels)
    # Same float32 source-texel cap as the fixed Blender composition recipe.
    value = value.astype(np.float32)
    luminance = value @ np.array([.2126, .7152, .0722], dtype=np.float32)
    value *= np.minimum(1.0, CAP / np.maximum(luminance, 1e-20))[:, :, None]
    return value.astype(np.float64)


def hammersley(count):
    require(type(count) is int and count > 0, "Positive sample count required")
    bits = np.arange(count, dtype=np.uint32)
    bits = (bits << 16) | (bits >> 16)
    bits = ((bits & 0x55555555) << 1) | ((bits & 0xAAAAAAAA) >> 1)
    bits = ((bits & 0x33333333) << 2) | ((bits & 0xCCCCCCCC) >> 2)
    bits = ((bits & 0x0F0F0F0F) << 4) | ((bits & 0xF0F0F0F0) >> 4)
    bits = ((bits & 0x00FF00FF) << 8) | ((bits & 0xFF00FF00) >> 8)
    return np.column_stack((np.arange(count, dtype=np.float64) / count,
                            bits.astype(np.float64) * 2.0 ** -32))


def directions(width, height):
    require(width > 0 and height > 0, "Positive image dimensions required")
    longitude = ((np.arange(width, dtype=np.float64) + .5) / width - .5) * (2 * math.pi)
    theta = (np.arange(height, dtype=np.float64) + .5) / height * math.pi
    sine = np.sin(theta)[:, None]
    return np.stack(np.broadcast_arrays(sine * np.cos(longitude)[None, :],
                                       np.cos(theta)[:, None],
                                       sine * np.sin(longitude)[None, :]), axis=-1)


def sample_environment(pixels, direction):
    """Bilinear source sampling with exactly repeat-U/clamp-V semantics."""
    height, width = pixels.shape[:2]
    direction = np.asarray(direction, dtype=np.float64)
    u = np.mod(.5 + np.arctan2(direction[..., 2], direction[..., 0]) / (2 * math.pi), 1.)
    v = np.arccos(np.clip(direction[..., 1], -1., 1.)) / math.pi
    x = u * width - .5
    y = np.clip(v * height - .5, 0., height - 1.)
    left = np.floor(x).astype(np.int64)
    top = np.floor(y).astype(np.int64)
    fx, fy = (x - left)[..., None], (y - top)[..., None]
    upper = pixels[top, left % width] * (1 - fx) + pixels[top, (left + 1) % width] * fx
    lower = pixels[np.minimum(top + 1, height - 1), left % width] * (1 - fx)
    lower += pixels[np.minimum(top + 1, height - 1), (left + 1) % width] * fx
    return upper * (1 - fy) + lower * fy


def tangent_basis(normals):
    axis = np.zeros_like(normals)
    axis[:, 1] = 1.
    axis[np.abs(normals[:, 1]) > .99] = [1., 0., 0.]
    tangent = np.cross(axis, normals)
    tangent /= np.linalg.norm(tangent, axis=-1)[:, None]
    return tangent, np.cross(normals, tangent)


def transform_samples(normals, local):
    tangent, bitangent = tangent_basis(normals)
    return (tangent[:, None, :] * local[None, :, 0, None]
            + bitangent[:, None, :] * local[None, :, 1, None]
            + normals[:, None, :] * local[None, :, 2, None])


def importance_ggx(xi, roughness):
    alpha = max(float(roughness) ** 2, 1e-3)
    phi = 2 * math.pi * xi[:, 0]
    cos_theta = np.sqrt((1 - xi[:, 1]) / (1 + (alpha * alpha - 1) * xi[:, 1]))
    sin_theta = np.sqrt(np.maximum(0., 1 - cos_theta * cos_theta))
    return np.column_stack((np.cos(phi) * sin_theta, np.sin(phi) * sin_theta, cos_theta))


def irradiance(pixels, width=128, height=64, samples=1024, chunk=64):
    """Store E, not E/pi: constant environment C produces pi*C."""
    require(chunk > 0, "Positive chunk size required")
    xi = hammersley(samples)
    phi = 2 * math.pi * xi[:, 0]
    radial = np.sqrt(xi[:, 1])
    local = np.column_stack((np.cos(phi) * radial, np.sin(phi) * radial,
                             np.sqrt(1 - xi[:, 1])))
    normals = directions(width, height).reshape(-1, 3)
    result = np.empty_like(normals)
    for start in range(0, len(normals), chunk):
        world = transform_samples(normals[start:start + chunk], local)
        result[start:start + chunk] = math.pi * sample_environment(pixels, world).mean(axis=1)
    return result.reshape(height, width, 3)


def prefilter_directions(pixels, normals, roughness, xi, chunk=64):
    """Cook the actual specular integrand at arbitrary target texel directions."""
    normals = np.asarray(normals, dtype=np.float64).reshape(-1, 3)
    if roughness == 0:
        return sample_environment(pixels, normals)
    half = importance_ggx(xi, roughness)
    local_light = 2 * half[:, 2, None] * half - np.array([0., 0., 1.])
    weight = np.maximum(local_light[:, 2], 0.)
    values = np.empty_like(normals)
    for start in range(0, len(normals), chunk):
        world = transform_samples(normals[start:start + chunk], local_light)
        radiance = sample_environment(pixels, world)
        values[start:start + chunk] = (radiance * weight[None, :, None]).sum(axis=1) / weight.sum()
    return values


def prefilter(pixels, width=1024, height=512, samples=1024, chunk=64, max_lod=MAX_PREFILTER_LOD):
    require(chunk > 0, "Positive chunk size required")
    levels = max(width, height).bit_length()
    require(type(max_lod) is int and max_lod > 0 and levels > 1, "Positive prefilter LOD required")
    max_lod = min(max_lod, levels - 1)
    xi = hammersley(samples)
    result = []
    for level in range(levels):
        w, h = max(width >> level, 1), max(height >> level, 1)
        normals = directions(w, h).reshape(-1, 3)
        # A lat-long 1x1 map cannot represent the directional irradiance still
        # present at roughness1. Keep at least64x32 at production maximum LOD4.
        # Lower resolution tail mips are initialized but never selected by PBR.
        roughness = min(level, max_lod) / max_lod
        result.append(prefilter_directions(pixels, normals, roughness, xi, chunk).reshape(h, w, 3))
    return result


def brdf_values(nov, roughness, xi):
    """Split-sum A,B using the same correlated Smith visibility as Forward."""
    nov = np.asarray(nov, dtype=np.float64).reshape(-1)
    require(np.isfinite(nov).all() and ((nov > 0) & (nov <= 1)).all(), "Require 0 < NoV <= 1")
    half = importance_ggx(xi, roughness)
    view = np.column_stack((np.sqrt(np.maximum(0., 1 - nov * nov)), np.zeros_like(nov), nov))
    voh = np.maximum(view @ half.T, 0.)
    light = 2 * voh[:, :, None] * half[None, :, :] - view[:, None, :]
    nol = np.maximum(light[:, :, 2], 0.)
    noh = half[:, 2][None, :]
    alpha2 = max(float(roughness) ** 2, 1e-3) ** 2
    denominator = nov[:, None] * np.sqrt(alpha2 + (1 - alpha2) * nol * nol)
    denominator += nol * np.sqrt(alpha2 + (1 - alpha2) * nov[:, None] ** 2)
    gvis = np.divide(2 * nol * voh, noh * denominator,
                     out=np.zeros_like(nol), where=(nol > 0) & (denominator > 0))
    fresnel = (1 - voh) ** 5
    return np.column_stack(((gvis * (1 - fresnel)).mean(axis=1), (gvis * fresnel).mean(axis=1)))


def brdf_lut(width=256, height=256, samples=1024):
    xi = hammersley(samples)
    nov = (np.arange(width, dtype=np.float64) + .5) / width
    output = np.zeros((height, width, 3), dtype=np.float64)
    for row in range(height):
        output[row, :, :2] = brdf_values(nov, (row + .5) / height, xi)
    return output


def rgba16_bytes(levels):
    parts = []
    for rgb in levels:
        rgb = finite_rgb(rgb)
        rgba = np.ones((*rgb.shape[:2], 4), dtype=np.float64)
        rgba[:, :, :3] = rgb
        require((rgba <= 65504).all(), "Cook result exceeds finite binary16 range")
        parts.append(rgba.astype("<f2").tobytes(order="C"))
    return b"".join(parts)


def radiance_bytes(pixels):
    """Uncompressed canonical RGBE; Radiance permits flat scanlines.

    Use each channel's shared exponent without display conversion. RGBE adds its
    own quantization to the Godot comparison, recorded separately from raw IBL.
    """
    pixels = finite_rgb(pixels)
    height, width = pixels.shape[:2]
    maximum = pixels.max(axis=-1)
    mantissa, exponent = np.frexp(maximum)
    scale = np.divide(mantissa * 256., maximum, out=np.zeros_like(maximum), where=maximum > 1e-32)
    rgbe = np.empty((height, width, 4), dtype=np.uint8)
    rgbe[:, :, :3] = np.clip(np.floor(pixels * scale[:, :, None]), 0, 255).astype(np.uint8)
    rgbe[:, :, 3] = np.where(maximum > 1e-32, exponent + 128, 0).astype(np.uint8)
    # Uncompressed first pixels must not accidentally look like modern RLE's
    # 2,2,width marker; fully RLE encode each channel to avoid that ambiguity.
    header = f"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {height} +X {width}\n".encode("ascii")
    if not 8 <= width <= 32767:
        return header + rgbe.tobytes()
    output = bytearray(header)
    for row in rgbe:
        output.extend((2, 2, width >> 8, width & 255))
        for channel in range(4):
            values = row[:, channel].tobytes()
            for start in range(0, width, 128):
                run = values[start:start + 128]
                output.append(len(run))
                output.extend(run)
    return bytes(output)


def image_record(output, name, levels):
    data = rgba16_bytes(levels)
    path = output / f"{name}.rgba16f"
    path.write_bytes(data)
    return dict(path=path.name, format="rgba16f-le", width=levels[0].shape[1], height=levels[0].shape[0],
                mip_levels=len(levels), byte_length=len(data), sha256=hashlib.sha256(data).hexdigest())


def decode_blender(source, blender):
    source = Path(source).resolve(strict=True)
    require(digest(source) == SOURCE_SHA256, "Pinned Bistro environment SHA-256 mismatch")
    with tempfile.TemporaryDirectory(prefix="renderer-ibl-decode-") as temporary:
        decoded = Path(temporary) / "decoded.npy"
        command = [str(Path(blender).resolve(strict=True)), "--background", "--factory-startup", "--threads", "1",
                   "--python-exit-code", "1", "--python", str(Path(__file__).resolve()), "--",
                   "--decode-blender", str(source), str(decoded)]
        completed = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", errors="replace")
        require(completed.returncode == 0, f"Pinned Blender decode failed: {completed.stdout}\n{completed.stderr}")
        metadata = json.loads(decoded.with_suffix(".json").read_text(encoding="utf-8"))
        pixels = np.load(decoded, allow_pickle=False)
    require(tuple(metadata["version"]) == BLENDER_VERSION and metadata["build"] == BLENDER_BUILD,
            "Pinned Blender build is required")
    require(metadata["numpy"] == "1.26.4", "Pinned Blender NumPy 1.26.4 is required")
    require(pixels.shape == (2048, 4096, 4), "Pinned environment dimensions disagree")
    return finite_rgb(pixels), metadata


def cook(source, output, blender, *, samples=SAMPLES, chunk=64):
    require(samples in (1024, 4096, 16384), "Samples must follow the fixed pilot ladder")
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    pixels, decoder = decode_blender(source, blender)
    source_hash = digest(source)
    pixels = capped_source(pixels)
    print("IBL cook: diffuse", flush=True)
    diffuse = irradiance(pixels, *DIFFUSE_SIZE, samples=samples, chunk=chunk)
    print("IBL cook: specular", flush=True)
    specular = prefilter(pixels, *SPECULAR_SIZE, samples=samples, chunk=chunk)
    print("IBL cook: BRDF", flush=True)
    brdf = brdf_lut(*BRDF_SIZE, samples=samples)
    images = {"diffuse": image_record(output, "diffuse", [diffuse]),
              "specular": image_record(output, "specular", specular),
              "brdf": image_record(output, "brdf", [brdf])}
    # Godot Sky's longitude convention differs by a quarter turn. Keep the
    # canonical Renderer maps unchanged, and publish this explicit derivative.
    rotated = np.roll(pixels, -pixels.shape[1] // 4, axis=1)
    panorama = output / "godot-panorama.hdr"
    panorama.write_bytes(radiance_bytes(rotated))
    record = dict(schema_version=1, max_prefilter_lod=MAX_PREFILTER_LOD,
                  source=dict(sha256=source_hash, byte_length=Path(source).stat().st_size,
                                               width=4096, height=2048),
                  lighting=dict(strength=3, yaw_degrees=0), images=images,
                  recipe=dict(version=1, mapping=MAPPING, cap=4, strength_applied=False, yaw_applied=False,
                              diffuse_storage="irradiance E; shader divides by pi", brdf_storage="A,B,0,1; f0*A+B",
                              distribution="isotropic GGX; alpha=max(roughness^2,1e-3)",
                              visibility="height-correlated Smith", samples=samples, sequence="Hammersley base2",
                              diffuse_size=list(DIFFUSE_SIZE), specular_size=list(SPECULAR_SIZE),
                              brdf_size=list(BRDF_SIZE), max_prefilter_lod=MAX_PREFILTER_LOD,
                              specular_roughness="min(mip,max_prefilter_lod)/max_prefilter_lod",
                              source_cap_arithmetic="float32 before resampling", integration_arithmetic="float64",
                              numpy=np.__version__, decoder=decoder, tool_sha256_utf8_lf=tool_hash()),
                  godot_panorama=dict(path=panorama.name, sha256=digest(panorama), byte_length=panorama.stat().st_size,
                                      width=4096, height=2048, format="radiance-rgbe", rotation_columns=1024,
                                      mapping="new[x]=source[(x+W/4)%W]", strength_applied=False, yaw_applied=False,
                                      quantization="RGBE shared-exponent, floor channels; separate from raw RGBA16F IBL"))
    (output / "manifest.json").write_text(json.dumps(record, indent=2, sort_keys=True, allow_nan=False) + "\n",
                                         encoding="utf-8")
    return record


def main():
    if "--decode-blender" in sys.argv:
        import bpy
        index = sys.argv.index("--decode-blender")
        source, output = map(Path, sys.argv[index + 1:index + 3])
        require(bpy.app.version == BLENDER_VERSION and bpy.app.build_hash.decode() == BLENDER_BUILD,
                "Pinned Blender 4.5.16 build required")
        image = bpy.data.images.load(str(source.resolve(strict=True)), check_existing=False)
        pixels = np.empty(len(image.pixels), dtype=np.float32)
        image.pixels.foreach_get(pixels)
        pixels = pixels.reshape(image.size[1], image.size[0], 4)[::-1].copy()
        np.save(output, pixels, allow_pickle=False)
        output.with_suffix(".json").write_text(json.dumps(dict(version=list(bpy.app.version),
                                                              build=bpy.app.build_hash.decode(), numpy=np.__version__)),
                                                encoding="utf-8")
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--blender", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=SAMPLES)
    parser.add_argument("--chunk", type=int, default=64)
    args = parser.parse_args()
    result = cook(args.source, args.output, args.blender, samples=args.samples, chunk=args.chunk)
    print(json.dumps(dict(manifest=str(args.output / "manifest.json"), images=result["images"]), sort_keys=True))


if __name__ == "__main__":
    main()
