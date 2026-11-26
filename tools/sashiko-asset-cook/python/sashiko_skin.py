"""Encode versioned Sashiko skins from offline model importers."""
import struct

_VERTEX = struct.Struct("<12f4H4f")
_MATRIX = struct.Struct("<12f")


def pack_vertex(position, normal, tangent, uv, joints, weights):
    return _VERTEX.pack(*position, *normal, *tangent, *uv, *joints, *weights)


def vertex_sort_key(record):
    """Spatial order with the complete packed vertex as the tie breaker."""
    return struct.unpack_from("<3f", record), record


def pack_transform(rows):
    """Three rows of an affine model-space transform, including translation."""
    return _MATRIX.pack(*(component for row in rows for component in row))


def encode_skin(vertices, indices, joint_count, poses, *, bind_joints=None):
    """Encode frame-major packed transforms and optional (parent, bind rows) pairs.

    A parent of None denotes a root. Omitting the bind skeleton encodes a
    SURSKIN1 palette stream; providing the skeleton encodes SURSKIN2.
    """
    if joint_count <= 0 or not poses or len(poses) % (joint_count * _MATRIX.size):
        raise ValueError("expected a whole number of nonempty skin palettes")
    if not vertices or any(len(record) != _VERTEX.size for record in vertices):
        raise ValueError("expected packed skinned vertices")
    if not indices or len(indices) % 3 or any(i < 0 or i >= len(vertices) for i in indices):
        raise ValueError("invalid triangle indices")
    frames = len(poses) // (joint_count * _MATRIX.size)
    version = b"SURSKIN1" if bind_joints is None else b"SURSKIN2"
    output = bytearray(struct.pack("<8s4I", version, len(vertices), len(indices), joint_count, frames))
    output.extend(b"".join(vertices))
    output.extend(struct.pack(f"<{len(indices)}I", *indices))
    output.extend(poses)
    if bind_joints is not None:
        if len(bind_joints) != joint_count:
            raise ValueError("expected one bind joint per palette entry")
        for parent, rows in bind_joints:
            if parent is not None and not 0 <= parent < joint_count:
                raise ValueError("invalid bind parent")
            output.extend(struct.pack("<I", 0xffffffff if parent is None else parent))
            output.extend(pack_transform(rows))
    return output
