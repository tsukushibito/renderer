"""Check the Blender writer's output against the Rust reader's shared fixtures."""
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/sashiko-asset-cook/python"))
import sashiko_skin


def fixture(with_bind):
    vertices = [
        sashiko_skin.pack_vertex(p, (0, 0, 1), (1, 0, 0, -1), uv, (1, 0, 0, 0), (.75, .25, 0, 0))
        for p, uv in [((0, 0, 0), (0, 0)), ((1, 0, 0), (1, 0)), ((0, 1, 0), (0, 1))]
    ]

    def translated(x):
        return [(1, 0, 0, x), (0, 1, 0, 0), (0, 0, 1, 0)]

    poses = b"".join(sashiko_skin.pack_transform(translated(x)) for x in range(6))
    bind = [(None, translated(0)), (0, translated(1))] if with_bind else None
    return sashiko_skin.encode_skin(vertices, [2, 0, 1], 2, poses, bind_joints=bind)


class TestSkinWriter(unittest.TestCase):
    def test_cooked_fixtures(self):
        for version in [1, 2]:
            path = ROOT / f"assets/verification/animation/v{version}.skin"
            with self.subTest(version=version):
                self.assertEqual(fixture(version == 2), path.read_bytes())
