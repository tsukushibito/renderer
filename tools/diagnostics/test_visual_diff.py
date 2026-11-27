import json
from pathlib import Path
import tempfile
import unittest
from PIL import Image
from visual_diff import compare, write


class VisualDiffTest(unittest.TestCase):
    def test_input_and_tick_travel_with_the_failing_frame(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for name in ("before", "after"):
                d = root / name
                d.mkdir()
                im = Image.new("RGBA", (4, 4), "black")
                if name == "after":
                    im.putpixel((2, 1), (3, 0, 0, 255))
                im.save(d / "frame.png")
                write(
                    d / "manifest.json",
                    {
                        "version": 1,
                        "world": "game",
                        "outcome": {"state": "complete"},
                        "frames": [{"frame": "8", "tick": "12", "view": "main", "stage": "sdr"}],
                        "files": {"image": "frame.png", "replay": "replay.json"},
                    },
                )
                write(d / "replay.json", {"inputs": ["left"] * 12})
            before, after = root / "before", root / "after"
            self.assertTrue(compare(before, after, root / "allowed", channel=3))
            self.assertTrue(compare(before, after, root / "fraction", fraction=1 / 16))
            self.assertFalse(compare(before, after, root / "failed", channel=2))
            report = json.loads((root / "failed/comparison.json").read_bytes())
            self.assertEqual((report["frames"][0]["tick"], report["frames"][0]["changed"]), (12, 1))
            with Image.open(root / "failed" / report["frames"][0]["diff"]) as diff:
                self.assertEqual(diff.getpixel((2, 1)), 24)
            self.assertEqual(
                json.loads((root / "failed/candidate/replay.json").read_bytes())["inputs"][11],
                "left",
            )
