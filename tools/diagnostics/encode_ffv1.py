"""Read (u64 little-endian PTS, tightly packed RGB24) frames on stdin."""
import struct
import sys
from fractions import Fraction

import av
import numpy as np


def encode(path, width, height, fps):
    with av.open(path, "w", format="matroska") as container:
        stream = container.add_stream("ffv1", rate=fps)
        stream.width, stream.height, stream.pix_fmt = width, height, "bgr0"
        stream.time_base = Fraction(1, fps)
        while header := sys.stdin.buffer.read(8):
            pts, = struct.unpack("<Q", header)
            pixels = sys.stdin.buffer.read(width * height * 3)
            frame = av.VideoFrame.from_ndarray(
                np.frombuffer(pixels, np.uint8).reshape(height, width, 3), format="rgb24")
            frame.pts, frame.time_base = pts, Fraction(1, fps)
            for packet in stream.encode(frame):
                container.mux(packet)
        for packet in stream.encode():
            container.mux(packet)


if __name__ == "__main__":
    if sys.argv[1:] == ["--probe"]:
        av.codec.Codec("ffv1", "w")
    else:
        encode(sys.argv[1], *map(int, sys.argv[2:]))
