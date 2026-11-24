"""Offline PC material texture policy and block encoding (logical RGBA8 input)."""
import numpy as np
from PIL import Image

ROLES = {"base_color", "emissive", "normal", "orm"}
FORMATS = {"auto", "rgba8", "bc1", "bc3", "bc5", "bc7"}
DDS_FORMAT = {"DXT1": "bc1", "DXT5": "bc3", "ATI2": "bc5"}


def validate_policy(policy, uris):
    if not isinstance(policy, dict) or policy.keys() - {"default", "roles", "images"}:
        raise ValueError("Expected texture policy with default, roles and/or images")
    for key, allowed in (("roles", ROLES), ("images", set(uris))):
        entries = policy.get(key, {})
        if not isinstance(entries, dict) or entries.keys() - allowed:
            raise ValueError(f"Unknown texture policy {key} entry")
        if any(value not in FORMATS for value in entries.values()):
            raise ValueError(f"Unknown texture format in {key}")
    if policy.get("default", "auto") not in FORMATS:
        raise ValueError("Unknown default texture format")


def choose_format(policy, uri, role, source_format):
    choice = policy.get("images", {}).get(uri, policy.get("roles", {}).get(role, policy.get("default", "auto")))
    if choice == "auto":
        choice = source_format or ("bc5" if role == "normal" else "bc7")
    if choice == "bc5" and role != "normal":
        raise ValueError(f"{uri}: BC5 stores only two channels; select an RGB format for {role}")
    return choice


def mip_bytes(fmt, w, h):
    if fmt == "rgba8":
        return w * h * 4
    return ((w + 3) // 4) * ((h + 3) // 4) * (8 if fmt == "bc1" else 16)


def encode(pixels, fmt):
    if fmt == "rgba8":
        return pixels.tobytes()
    import ispc_texcomp as it
    h, w = pixels.shape[:2]
    alpha = pixels[:, :, 3].min() < 255
    if fmt == "bc1" and alpha:
        raise ValueError("BC1 encoder requires opaque input; use BC3 or BC7 to retain alpha")
    padded = np.pad(pixels, ((0, -h % 4), (0, -w % 4), (0, 0)), mode="edge")
    ph, pw = padded.shape[:2]
    if fmt == "bc5":
        # Native BC5 expects tightly interleaved RG rather than RGBA.
        data = padded[:, :, :2].tobytes()
        surface = it.RGBASurface(data, pw, ph, pw * 2)
        encoded = it.compress_blocks_bc5(surface)
    else:
        surface = it.RGBASurface(padded.tobytes(), pw, ph)
        if fmt == "bc7":
            settings = it.BC7EncSettings.from_profile("alpha_slow" if alpha else "slow")
            encoded = it.compress_blocks_bc7(surface, settings)
        elif fmt == "bc3":
            encoded = it.compress_blocks_bc3(surface)
        else:
            # The binding reserves 16 bytes/block even for eight-byte BC1.
            encoded = it.compress_blocks_bc1(surface)[:mip_bytes(fmt, w, h)]
    if len(encoded) != mip_bytes(fmt, w, h):
        raise ValueError(f"Invalid {fmt} encoder output size")
    return encoded


def decode(data, fmt, w, h):
    if fmt == "rgba8":
        return np.frombuffer(data, dtype=np.uint8).reshape(h, w, 4)
    number = int(fmt[-1])
    mode = "RGB" if fmt == "bc5" else "RGBA"
    image = Image.frombytes(mode, (w, h), data, "bcn", (number, "BC5U" if fmt == "bc5" else fmt.upper()))
    return np.asarray(image.convert("RGBA"))

