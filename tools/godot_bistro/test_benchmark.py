"""CPU-only regression gates for accepting benchmark evidence, without Godot."""
import copy
import unittest

from benchmark import percentile, validate_report


def fixture(timing=True):
    config = {"frames": 2, "timing": timing}
    samples = [{"frame": frame, "frame_interval_ms": 10.0} for frame in range(2)]
    if timing:
        for sample in samples:
            sample.update(cpu_setup_ms=.1, cpu_viewport_ms=.2, cpu_render_ms=.3,
                          gpu_ms=2., draw_calls=1591, primitives=2832120)
    return config, {"config": config, "debug": False, "driver": "vulkan", "renderer": "forward_plus",
                    "godot": {"hash": "ed1daf0bf001b61586d9930840f2f1394092c079"},
                    "import_inventory": {"materials": 132, "images": 405},
                    "inventory": {"mesh_instances": 1296, "surfaces": 1591, "triangles": 2832120},
                    "viewport_size": [1920, 1080], "vsync": 0, "max_fps": 0,
                    "roi_wall_ms": 20., "samples": samples}


class EvidenceTests(unittest.TestCase):
    def test_timing_disabled_is_missing_instead_of_zero(self):
        config, report = fixture(False)
        self.assertEqual(set(validate_report(report, config)), {"frame_interval_ms"})
        report["samples"][0]["gpu_ms"] = 0
        with self.assertRaises(ValueError):
            validate_report(report, config)

    def test_refuses_missing_zero_nonfinite_gpu_results(self):
        config, report = fixture()
        for invalid in (None, 0, -1, float("nan"), float("inf"), True):
            with self.subTest(invalid=invalid):
                altered = copy.deepcopy(report)
                altered["samples"][1]["gpu_ms"] = invalid
                with self.assertRaises(ValueError):
                    validate_report(altered, config)

    def test_refuses_wrong_backend_debug_inventory_resolution_and_pacing(self):
        config, report = fixture()
        for key, invalid in (("driver", "dummy"), ("renderer", "mobile"), ("debug", True),
                             ("inventory", {}), ("import_inventory", {}), ("godot", {}), ("viewport_size", [1280, 720]),
                             ("vsync", 1), ("max_fps", 60)):
            with self.subTest(key=key):
                altered = copy.deepcopy(report)
                altered[key] = invalid
                with self.assertRaises(ValueError):
                    validate_report(altered, config)

    def test_refuses_partial_or_discontinuous_roi(self):
        config, report = fixture()
        for mutate in (lambda r: r["samples"].pop(),
                       lambda r: r["samples"][1].update(frame=0),
                       lambda r: r.update(roi_wall_ms=40)):
            altered = copy.deepcopy(report)
            mutate(altered)
            with self.assertRaises(ValueError):
                validate_report(altered, config)

    def test_uses_frame_percentiles_not_run_average_percentiles(self):
        self.assertAlmostEqual(percentile([1, 2, 3, 100], .95), 85.45)
        config, report = fixture()
        self.assertEqual(validate_report(report, config)["gpu_ms"]["mean"], 2.)


if __name__ == "__main__":
    unittest.main()
