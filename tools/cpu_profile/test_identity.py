"""Identity contracts exercised with small independent PE/MSF fixtures."""

import json
from pathlib import Path
import struct
import tempfile
import unittest
import uuid

from identity import (IdentityError, MSF7_MAGIC, inspect_pair, parse_pdb_identity,
                      parse_pe_identity, verify_manifest)


GUID = "BD169CF5-1A98-48B9-AE93-9C8F84210FA7"


def pe_fixture(age=7, pe32=False, raw_pointer=True):
    data = bytearray(2048)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HHI", data, 0x84, 0x8664, 1, 123)
    struct.pack_into("<H", data, 0x94, 240)
    optional = 0x98
    struct.pack_into("<H", data, optional, 0x10B if pe32 else 0x20B)
    struct.pack_into("<I" if pe32 else "<Q", data, optional + (28 if pe32 else 24),
                     0x400000 if pe32 else 0x140000000)
    struct.pack_into("<II", data, optional + 56, 0x3000, 0x400)
    directory_offset = 96 if pe32 else 112
    struct.pack_into("<I", data, optional + (92 if pe32 else 108), 16)
    struct.pack_into("<II", data, optional + directory_offset + 48, 0x2080, 28)
    struct.pack_into("<IIII", data, optional + 240 + 8, 0x400, 0x2000, 0x400, 0x400)
    payload = b"RSDS" + uuid.UUID(GUID).bytes_le + struct.pack("<I", age) + b"C:\\build\\sample.pdb\0"
    struct.pack_into("<IIII", data, 0x480 + 12, 2, len(payload), 0x2100, 0x500 if raw_pointer else 0)
    data[0x500:0x500 + len(payload)] = payload
    return data


def pdb_fixture(age=7):
    # Stream zero occupies two nonadjacent blocks. Info stream #1 starts in
    # block eight, requiring the parser to honor directory sizes/block lists.
    data = bytearray(512 * 10)
    data[:32] = MSF7_MAGIC
    struct.pack_into("<IIIIII", data, 32, 512, 1, 10, 24, 0, 3)
    struct.pack_into("<I", data, 512 * 3, 5)
    struct.pack_into("<6I", data, 512 * 5, 2, 520, 28, 4, 6, 8)
    struct.pack_into("<III", data, 512 * 8, 20000404, 123, age)
    data[512 * 8 + 12:512 * 8 + 28] = uuid.UUID(GUID).bytes_le
    return data


class IdentityTests(unittest.TestCase):
    def test_pe_maps_debug_directory_and_reads_both_optional_headers(self):
        for pe32 in (False, True):
            for pointer in (False, True):
                with self.subTest(pe32=pe32, pointer=pointer):
                    identity = parse_pe_identity(pe_fixture(pe32=pe32, raw_pointer=pointer))
                    self.assertEqual(identity["rsds"], {"guid": GUID, "age": 7,
                                                        "pdb_path": "C:\\build\\sample.pdb"})
                    self.assertEqual(identity["format"], "PE32" if pe32 else "PE32+")

    def test_pe_rejects_unmapped_debug_directory_and_truncated_payload(self):
        unmapped = pe_fixture()
        struct.pack_into("<II", unmapped, 0x98 + 112 + 48, 0x9000, 28)
        missing_directory = pe_fixture()
        struct.pack_into("<I", missing_directory, 0x98 + 108, 6)
        missing_terminator = pe_fixture()
        struct.pack_into("<I", missing_terminator, 0x480 + 16, 25)
        for data in (unmapped, missing_directory, missing_terminator, pe_fixture()[:0x510]):
            with self.subTest(size=len(data)), self.assertRaises(IdentityError):
                parse_pe_identity(data)

    def test_pdb_info_stream_uses_directory_block_lists(self):
        self.assertEqual(parse_pdb_identity(pdb_fixture()), {
            "guid": GUID, "age": 7, "version": 20000404, "signature_or_timestamp": 123})

    def test_pdb_rejects_absent_truncated_and_out_of_range_streams(self):
        absent = pdb_fixture()
        struct.pack_into("<I", absent, 512 * 5 + 8, 0xFFFFFFFF)
        outside = pdb_fixture()
        struct.pack_into("<I", outside, 512 * 5 + 20, 10)
        bad_map = pdb_fixture()
        struct.pack_into("<I", bad_map, 512 * 3, 10)
        for data in (absent, outside, bad_map, pdb_fixture()[:-1]):
            with self.subTest(size=len(data)), self.assertRaises(IdentityError):
                parse_pdb_identity(data)

    def test_archive_revalidation_detects_replaced_bytes_and_identity_mismatch(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            binary, pdb = base / "sample.exe", base / "sample.pdb"
            binary.write_bytes(pe_fixture())
            pdb.write_bytes(pdb_fixture())
            pair = inspect_pair(binary, pdb)
            pair["binary"]["path"], pair["pdb"]["path"] = binary.name, pdb.name
            manifest = base / "manifest.json"
            manifest.write_text(json.dumps({"schema": 1, **pair}), encoding="utf-8")
            self.assertTrue(verify_manifest(manifest)["identity_matches"])
            incomplete = {"schema": 1, **pair, "binary": {"path": binary.name}}
            manifest.write_text(json.dumps(incomplete), encoding="utf-8")
            with self.assertRaisesRegex(IdentityError, "Incomplete"):
                verify_manifest(manifest)
            manifest.write_text(json.dumps({"schema": 1, **pair}), encoding="utf-8")
            data = pdb_fixture()
            data[-1] = 1  # Same GUID/age, different file: reject the stale hash.
            pdb.write_bytes(data)
            with self.assertRaisesRegex(IdentityError, "hash"):
                verify_manifest(manifest)
            pdb.write_bytes(pdb_fixture(age=8))
            with self.assertRaisesRegex(IdentityError, "GUID/age"):
                inspect_pair(binary, pdb)

    def test_manifest_paths_cannot_escape_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "manifest.json"
            manifest.write_text(json.dumps({"schema": 1, "identity_matches": True,
                                           "binary": {"path": "../sample.exe"},
                                           "pdb": {"path": "sample.pdb"}}), encoding="utf-8")
            with self.assertRaisesRegex(IdentityError, "relative"):
                verify_manifest(manifest)


if __name__ == "__main__":
    unittest.main()
