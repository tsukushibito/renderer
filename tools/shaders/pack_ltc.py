"""Pack the pinned GGX fit into shader constants for bilinear lookup, without a new binding."""
import argparse
import re
import struct
from pathlib import Path


def generate(source):
    # fit/results/ltc.js at selfshadow/ltc_code 31e5e96b54f98f33098f8503003119ba2231a1c6.
    tables = [
        [float(v) for v in data.split(",") if v.strip()]
        for data in re.findall(r"g_ltc_[12] = \[([^\]]+)\]", source)
    ]
    if len(tables) != 2 or any(len(table) != 64 * 64 * 4 for table in tables):
        raise ValueError("expected the two 64x64 RGBA fit tables")
    lines = ["// GGX LTC fit, Heitz et al. (2016). See licenses/LTC.txt and tools/shaders/pack_ltc.py.",
             "static const uint3 LTC_FIT[1024] = {"]
    for y in range(32):
        for x in range(32):
            # Resample at uniform endpoints to preserve the grazing and smooth limits.
            u, v = x * 63 / 31, y * 63 / 31
            coeffs = []
            for table, channels in zip(tables, (4, 2)):
                for k in range(channels):
                    value = 0.0
                    for j, wy in enumerate((1 - v % 1, v % 1)):
                        for i, wx in enumerate((1 - u % 1, u % 1)):
                            at = min(int(v) + j, 63) * 64 + min(int(u) + i, 63)
                            value += wx * wy * table[at * 4 + k]
                    coeffs.append(value)
            packed = struct.unpack("<3I", struct.pack("<6e", *coeffs))
            lines.append("    uint3(" + ", ".join(f"0x{word:08x}u" for word in packed) + "),")
    return "\n".join([*lines, "};", ""])


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.write_text(generate(args.source.read_text(encoding="utf-8-sig")), encoding="ascii", newline="\n")
