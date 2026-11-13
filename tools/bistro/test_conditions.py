import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

from conditions import (environment_uv, gltf_to_blender, load_conditions, route_frames, sample_route,
                        validate_conditions, verify_input_files)


def fixture():
    return {"schema_version": 1, "resolution": [1920, 1080],
            "camera": {"near_m": 0.02, "far_m": 500},
            "views": [{"name": "street", "position": [0, 2, 10], "target": [0, 2, 0],
                       "vertical_fov_degrees": 50},
                      {"name": "alley", "position": [10, 3, 10], "target": [10, 3, 0],
                       "vertical_fov_degrees": 70}],
            "route": {"fps": 60, "duration_seconds": 30,
                      "keyframes": [{"seconds": 0, "view": "street"},
                                    {"seconds": 10, "view": "alley"},
                                    {"seconds": 30, "view": "street"}]},
            "lighting": {"sun": {"direction_to_light": [0, 1, 0],
                                 "irradiance_rgb": [3, 3, 3], "angular_diameter_degrees": 0.53},
                         "ibl": {"path": "environment.hdr", "sha256": "a" * 64,
                                 "strength": 1, "yaw_degrees": 0,
                                 "preprocess": {"mode": "luminance-cap", "cap": 4}}},
            "output": {"exposure_ev": 0, "tone_mapper": "Khronos PBR Neutral", "srgb": True}}


class ConditionsTests(unittest.TestCase):
    def test_route_samples_keyframes_midpoints_and_half_open_frame_schedule(self):
        config = validate_conditions(fixture())
        middle = sample_route(config, 5)
        self.assertEqual(middle["position"], [5, 2.5, 10])
        self.assertEqual(middle["target"], [5, 2.5, 0])
        self.assertEqual(middle["vertical_fov_degrees"], 60)
        self.assertEqual(middle["near_m"], 0.02)
        self.assertEqual(sample_route(config, 10)["position"], [10, 3, 10])
        self.assertEqual(sample_route(config, 30)["position"], sample_route(config, 0)["position"])
        frames = list(route_frames(config))
        self.assertEqual(len(frames), 1800)
        self.assertEqual(frames[0]["time_seconds"], 0)
        self.assertEqual(frames[600]["position"], [10, 3, 10])
        self.assertEqual(frames[-1]["time_seconds"], 1799 / 60)
        self.assertLess(frames[-1]["time_seconds"], 30)
        self.assertEqual(frames, list(route_frames(config)))

    def test_route_rejects_unknown_views_timing_and_nonclosing_poses(self):
        invalid = []
        config = fixture()
        config["route"]["keyframes"][1]["view"] = "missing"
        invalid.append(config)
        config = fixture()
        config["route"]["keyframes"][1]["seconds"] = 0
        invalid.append(config)
        config = fixture()
        config["route"]["keyframes"][-1]["view"] = "alley"
        invalid.append(config)
        config = fixture()
        config["route"]["duration_seconds"] = 30.001
        config["route"]["keyframes"][-1]["seconds"] = 30.001
        invalid.append(config)
        config = fixture()
        config["route"]["duration_seconds"] = 1e-12
        config["route"]["keyframes"] = [{"seconds": 0, "view": "street"},
                                            {"seconds": 1e-12, "view": "street"}]
        invalid.append(config)
        config = fixture()
        config["route"]["capture_frames"] = [0, 1800]
        invalid.append(config)
        for config in invalid:
            with self.subTest(config=config), self.assertRaises(ValueError):
                validate_conditions(config)
        for seconds in (-0.1, 30.1, float("nan")):
            with self.assertRaises(ValueError):
                sample_route(fixture(), seconds)

    def test_route_rejects_singular_look_between_valid_endpoint_poses(self):
        config = fixture()
        # Each endpoint is valid, but the interpolated look vector crosses +Y.
        config["views"][0]["target"] = [0, 3, 9]
        config["views"][1]["target"] = [10, 4, 11]
        with self.assertRaisesRegex(ValueError, "parallel to world-up"):
            validate_conditions(config)
        config = fixture()
        config["views"][0]["target"] = config["views"][0]["position"]
        with self.assertRaisesRegex(ValueError, "degenerate"):
            validate_conditions(config)

    def test_nonfinite_projection_and_lighting_are_rejected(self):
        mutations = [lambda c: c["views"][0]["position"].__setitem__(1, float("nan")),
                     lambda c: c["views"][0].__setitem__("vertical_fov_degrees", 180),
                     lambda c: c["camera"].__setitem__("near_m", 500),
                     lambda c: c["lighting"]["sun"].__setitem__("direction_to_light", [0, 2, 0]),
                     lambda c: c["lighting"]["ibl"].__setitem__("strength", float("inf")),
                     lambda c: c["lighting"]["ibl"]["preprocess"].__setitem__("cap", 5),
                     lambda c: c["output"].__setitem__("srgb", False)]
        for mutate in mutations:
            config = fixture()
            mutate(config)
            with self.subTest(mutate=mutate), self.assertRaises(ValueError):
                validate_conditions(config)

    def test_pinned_input_verification_is_opt_in_and_detects_content_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            payload = b"fixture HDR bytes"
            asset = root / "environment.hdr"
            asset.write_bytes(payload)
            digest = hashlib.sha256(payload).hexdigest()
            config = fixture()
            config["lighting"]["ibl"]["sha256"] = digest
            config["inputs"] = [{"path": "environment.hdr", "sha256": digest, "size": len(payload)}]
            path = root / "conditions.json"
            path.write_text(json.dumps(config), encoding="utf-8")
            self.assertEqual(load_conditions(path, verify_inputs=True, repository_root=root), config)
            asset.write_bytes(b"Fixture HDR bytes")
            self.assertEqual(load_conditions(path), config)
            with self.assertRaisesRegex(ValueError, "SHA-256 differs"):
                load_conditions(path, verify_inputs=True, repository_root=root)
            config["inputs"][0]["size"] += 1
            with self.assertRaisesRegex(ValueError, "size differs"):
                verify_input_files(config, root)

    def test_input_paths_digests_and_duplicate_records_cannot_bypass_pinning(self):
        for name in ("../environment.hdr", "C:/environment.hdr", "file:environment.hdr", "a\\b.hdr"):
            config = fixture()
            config["lighting"]["ibl"]["path"] = name
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "escapes"):
                validate_conditions(config)
        config = fixture()
        config["inputs"] = [{"path": "environment.hdr", "sha256": "a" * 64, "size": 0}]
        config["inputs"].append(copy.deepcopy(config["inputs"][0]))
        with self.assertRaisesRegex(ValueError, "Duplicate"):
            validate_conditions(config)
        config = fixture()
        config["lighting"]["ibl"]["sha256"] = "not a hash"
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            validate_conditions(config)

    def test_coordinate_basis_preserves_orientation_and_distance(self):
        self.assertEqual(gltf_to_blender([1, 2, 3]), [1, -3, 2])
        self.assertEqual(gltf_to_blender([0, 1, 0]), [0, 0, 1])
        self.assertEqual(gltf_to_blender([0, 0, -1]), [0, 1, 0])

    def test_environment_mapping_matches_cardinals_seam_and_inverse_yaw(self):
        for direction, expected in [([1, 0, 0], [0.5, 0.5]), ([-1, 0, 0], [0, 0.5]),
                                    ([0, 0, 1], [0.75, 0.5]), ([0, 0, -1], [0.25, 0.5])]:
            self.assertEqual(environment_uv(direction), expected)
        self.assertEqual(environment_uv([0, 1, 0])[1], 0)
        self.assertEqual(environment_uv([0, -2, 0])[1], 1)
        self.assertAlmostEqual(environment_uv([1, 0, 0], 90)[0], 0.75)
        self.assertAlmostEqual(environment_uv([0, 0, 1], 90)[0], 0)
        with self.assertRaisesRegex(ValueError, "cannot be zero"):
            environment_uv([0, 0, 0])


if __name__ == "__main__":
    unittest.main()
