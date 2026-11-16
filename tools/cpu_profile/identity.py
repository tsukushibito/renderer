"""Verify a modern MSVC PE/PDB pair without loading or executing the image.

This checks CodeView RSDS against MSF7 PDB Info stream GUID/age. It does not
prove that sampled stacks or source lines can be resolved; those need actual
symbol/trace analysis. Formats: Microsoft PE specification and LLVM PDB docs.
"""

import argparse
import hashlib
import json
import mmap
from pathlib import Path
import struct
import uuid


MSF7_MAGIC = b"Microsoft C/C++ MSF 7.00\r\n\x1aDS\0\0\0"


class IdentityError(ValueError):
    """The image, PDB or recorded manifest cannot establish a matching pair."""


def _take(data, offset, size):
    if offset < 0 or size < 0 or offset > len(data) or size > len(data) - offset:
        raise IdentityError("Truncated or out-of-range binary record")
    return data[offset:offset + size]


def _unpack(fmt, data, offset):
    return struct.unpack(fmt, _take(data, offset, struct.calcsize(fmt)))


def _guid(data):
    return str(uuid.UUID(bytes_le=bytes(data))).upper()


def parse_pe_identity(data):
    """Read PE32/PE32+ debug identities from a bytes-like image."""
    if _take(data, 0, 2) != b"MZ":
        raise IdentityError("Not a PE image: missing DOS signature")
    pe, = _unpack("<I", data, 0x3C)
    if _take(data, pe, 4) != b"PE\0\0":
        raise IdentityError("Not a PE image: missing PE signature")
    machine, sections, timestamp = _unpack("<HHI", data, pe + 4)
    optional_size, = _unpack("<H", data, pe + 20)
    optional = pe + 24
    _take(data, optional, optional_size)
    magic, = _unpack("<H", data, optional)
    if magic == 0x20B:
        image_base, = _unpack("<Q", data, optional + 24)
        directory_count_offset, directory_offset, kind = 108, 112, "PE32+"
    elif magic == 0x10B:
        image_base, = _unpack("<I", data, optional + 28)
        directory_count_offset, directory_offset, kind = 92, 96, "PE32"
    else:
        raise IdentityError("Unsupported PE optional-header format")
    if optional_size < directory_count_offset + 4:
        raise IdentityError("Optional header does not contain directory count")
    directories, = _unpack("<I", data, optional + directory_count_offset)
    if directories <= 6 or optional_size < directory_offset + 7 * 8:
        raise IdentityError("PE image has no complete debug-directory entry")
    image_size, header_size = _unpack("<II", data, optional + 56)
    debug_rva, debug_size = _unpack("<II", data, optional + directory_offset + 6 * 8)
    if debug_size == 0 or debug_size % 28:
        raise IdentityError("PE image has no complete debug directory")
    if not 1 <= sections <= 96:
        raise IdentityError("Unsupported PE section count")
    section_table = optional + optional_size
    section_map = []
    for index in range(sections):
        section = section_table + index * 40
        _take(data, section, 40)
        virtual_size, address, raw_size, raw_pointer = _unpack("<IIII", data, section + 8)
        section_map.append((address, virtual_size, raw_pointer, raw_size))

    def offset_for(rva, size):
        if rva < header_size and size <= header_size - rva:
            _take(data, rva, size)
            return rva
        for address, _, raw_pointer, raw_size in section_map:
            if rva >= address and rva - address < raw_size:
                delta = rva - address
                if size <= raw_size - delta:
                    _take(data, raw_pointer + delta, size)
                    return raw_pointer + delta
        raise IdentityError("Debug RVA has no complete file-backed PE section")

    directory = offset_for(debug_rva, debug_size)
    identities = []
    for index in range(debug_size // 28):
        entry = directory + index * 28
        debug_type, size, rva, pointer = _unpack("<IIII", data, entry + 12)
        if debug_type != 2:
            continue
        payload = _take(data, pointer or offset_for(rva, size), size)
        if payload[:4] != b"RSDS":
            continue
        _take(payload, 0, 24)
        age, = _unpack("<I", payload, 20)
        name = payload[24:]
        terminator = name.find(b"\0")
        if terminator < 0:
            raise IdentityError("CodeView PDB path is not terminated within its record")
        identities.append({"guid": _guid(payload[4:20]), "age": age,
                           "pdb_path": name[:terminator].decode("utf-8", errors="replace")})
    if not identities:
        raise IdentityError("Modern RSDS CodeView identity is missing")
    if any((item["guid"], item["age"]) != (identities[0]["guid"], identities[0]["age"])
           for item in identities[1:]):
        raise IdentityError("PE has conflicting RSDS identities")
    return {"machine": f"0x{machine:04X}", "format": kind,
            "image_base": f"0x{image_base:X}", "size_of_image": image_size,
            "coff_timestamp_or_repro_hash": timestamp, "rsds": identities[0]}


def parse_pdb_identity(data):
    """Read only MSF7 PDB Info stream #1, not the entire debug database."""
    if _take(data, 0, 32) != MSF7_MAGIC:
        raise IdentityError("Unsupported PDB format: MSF7 signature required")
    block_size, free_map, block_count, directory_size, _, map_block = _unpack("<IIIIII", data, 32)
    if block_size not in (512, 1024, 2048, 4096) or free_map not in (1, 2):
        raise IdentityError("Unsupported or invalid MSF7 superblock")
    if block_count * block_size != len(data) or directory_size < 12:
        raise IdentityError("MSF7 declared size does not match the PDB file")
    directory_blocks = (directory_size + block_size - 1) // block_size
    if directory_blocks * 4 > block_size:
        raise IdentityError("Unsupported multi-block MSF7 directory block map")
    block_map = _take(data, map_block * block_size, directory_blocks * 4)

    def block(number):
        if number >= block_count:
            raise IdentityError("MSF7 block index exceeds the file")
        return _take(data, number * block_size, block_size)

    directory = b"".join(block(number) for number, in struct.iter_unpack("<I", block_map))[:directory_size]
    streams, = _unpack("<I", directory, 0)
    if streams < 2 or streams > (len(directory) - 4) // 4:
        raise IdentityError("PDB Info stream directory is missing or truncated")
    sizes = _unpack(f"<{streams}I", directory, 4)
    cursor = 4 + streams * 4
    for index, size in enumerate(sizes[:2]):
        count = 0 if size == 0xFFFFFFFF else (size + block_size - 1) // block_size
        pages = _take(directory, cursor, count * 4)
        cursor += count * 4
        if index == 1:
            if size == 0xFFFFFFFF or size < 28:
                raise IdentityError("PDB Info stream is absent or too short")
            first, = _unpack("<I", pages, 0)
            version, signature, age = _unpack("<III", block(first), 0)
            return {"guid": _guid(block(first)[12:28]), "age": age,
                    "version": version, "signature_or_timestamp": signature}
    raise IdentityError("PDB Info stream is missing")


def _parse(path, parser):
    with Path(path).open("rb") as source:
        if source.seek(0, 2) == 0:
            raise IdentityError("Empty binary file")
        source.seek(0)
        with mmap.mmap(source.fileno(), 0, access=mmap.ACCESS_READ) as data:
            return parser(data)


def read_pe_identity(path):
    return _parse(path, parse_pe_identity)


def read_pdb_identity(path):
    return _parse(path, parse_pdb_identity)


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def inspect_pair(binary, pdb):
    pe, info = read_pe_identity(binary), read_pdb_identity(pdb)
    if (pe["rsds"]["guid"], pe["rsds"]["age"]) != (info["guid"], info["age"]):
        raise IdentityError("PE RSDS GUID/age does not match this PDB Info stream")
    return {"binary": {"path": str(binary), "sha256": sha256(binary),
                        "size_bytes": Path(binary).stat().st_size, **pe},
            "pdb": {"path": str(pdb), "sha256": sha256(pdb),
                     "size_bytes": Path(pdb).stat().st_size, **info},
            "identity_matches": True}


def verify_manifest(path):
    """Recheck archived bytes and identities before capture or symbol analysis."""
    path = Path(path).resolve()
    manifest = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(manifest, dict) or manifest.get("schema") != 1 or manifest.get("identity_matches") is not True:
        raise IdentityError("Unsupported or unverified build manifest")
    try:
        binary, pdb = manifest["binary"], manifest["pdb"]
        archives = []
        for item in (binary, pdb):
            relative = Path(item["path"])
            archived = (path.parent / relative).resolve()
            if relative.is_absolute() or not archived.is_relative_to(path.parent):
                raise IdentityError("Archived paths must remain relative to the manifest directory")
            archives.append(archived)
        actual = inspect_pair(*archives)
        for role in ("binary", "pdb"):
            if actual[role]["sha256"] != manifest[role]["sha256"]:
                raise IdentityError(f"Archived {role} hash no longer matches the manifest")
        if actual["binary"]["rsds"] != binary["rsds"]:
            raise IdentityError("Manifest PE debug identity changed")
        if any(actual["pdb"][key] != pdb[key] for key in ("guid", "age")):
            raise IdentityError("Manifest PDB debug identity changed")
    except (KeyError, TypeError) as error:
        raise IdentityError("Incomplete build manifest") from error
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("pdb", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    try:
        result = inspect_pair(args.binary, args.pdb)
    except (IdentityError, OSError) as error:
        parser.exit(1, f"identity: {error}\n")
    text = json.dumps(result, indent=2)
    if args.output:
        args.output.write_text(text + "\n", encoding="utf-8")
    else:
        print(text)


if __name__ == "__main__":
    main()
