"""CPU-only regression gates for accepting benchmark evidence, without Godot."""
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from benchmark import CAPTURE_VIEWS, ROOT, digest, percentile, read, run_logged, validate_report
from profiles import compare_settings, orientation_fixture, settings, split_offsets, verify_ibl_manifest
from summarize import aggregate, assemble


CONDITIONS = read(ROOT / "tools/bistro/conditions.json")
B3_CONDITIONS = copy.deepcopy(CONDITIONS)
B3_CONDITIONS["lighting"]["ambient_occlusion"] = {"enabled": False}


def mip_inventory():
    images = []
    for index in range(405):
        width = 256 if index < 289 else 128
        count = width.bit_length() - 1
        channels = 3 if index < 273 else 4
        images.append({"index": index, "class": "ImageTexture", "width": width, "height": width,
            "format": 4 if channels == 3 else 5, "compressed": False, "mipmaps_before": 0,
            "mipmap_count": count, "has_mipmaps": True, "normal_renormalized": index < 131,
            "base_bytes": width * width * channels,
            "native_data_bytes": sum((width >> level) ** 2 * channels for level in range(count + 1)),
            "base_sha256": "a" * 64, "native_base_sha256": "a" * 64, "resource_identity_preserved": True})
    references = []
    for material in range(131):
        for role, image in (("normal", material), ("albedo", 131 + material),
                            ("metallic", 262 + material), ("roughness", 262 + material),
                            ("ao", 262 + material)):
            references.append({"material": material, "role": role, "image": image,
                               "resource_id": image + 1, "rid": image + 1})
    for material in range(12):
        references.append({"material": material, "role": "emission", "image": 393 + material,
                           "resource_id": 394 + material, "rid": 394 + material})
    return {"policy": "b3-explicit-png-full-mips-v1", "outside_roi": True, "verified": True,
            "image_count": 405, "material_count": 132, "material_reference_count": 667,
            "normal_images": 131, "total_levels_including_base": 3529,
            "images": images, "references": references}


def fixture(timing=True, profile="b2"):
    config = {"frames": 2, "timing": timing, "profile": profile,
              "quality": settings(profile, B3_CONDITIONS if profile == "b3" else CONDITIONS)}
    samples = [{"frame": frame, "frame_interval_ms": 10.0} for frame in range(2)]
    if timing:
        for sample in samples:
            sample.update(cpu_setup_ms=.1, cpu_viewport_ms=.2, cpu_render_ms=.3,
                          gpu_ms=2., draw_calls=1591, primitives=2832120)
    report = {"config": config, "quality": copy.deepcopy(config["quality"]),
                    "debug": False, "driver": "vulkan", "renderer": "forward_plus",
                    "godot": {"hash": "ed1daf0bf001b61586d9930840f2f1394092c079"},
                    "import_inventory": {"materials": 132, "images": 405},
                    "inventory": {"mesh_instances": 1296, "surfaces": 1591, "triangles": 2832120},
                    "viewport_size": [1920, 1080], "vsync": 0, "max_fps": 0,
                    "roi_wall_ms": 20., "samples": samples}
    if profile == "b3":
        report["material_mips"] = mip_inventory()
        config["ibl"] = {"sha256": "a" * 64, "width": 4096, "height": 2048}
        report["ibl"] = dict(config["ibl"])
        report["material_overrides"] = {"policy": "bistro-source-ao-disabled", "inspected_materials": 132,
                                        "ao_enabled_before": 132, "ao_enabled_after": 0}
        report["capture_validation"] = {"settling_frames": 8, "maximum_frames": 16,
            "consecutive_equal_readbacks": 2, "outside_roi": True,
            "frames_by_view": dict.fromkeys(CAPTURE_VIEWS, 9)}
    return config, report


class EvidenceTests(unittest.TestCase):
    def test_b3_requires_complete_native_mips_base_pixels_and_material_references(self):
        config, report = fixture(profile="b3")
        validate_report(report, config)
        mutations = [
            lambda r: r.pop("material_mips"),
            lambda r: r["material_mips"]["images"][0].update(mipmap_count=0, has_mipmaps=False),
            lambda r: r["material_mips"]["images"][0].update(native_base_sha256="b" * 64),
            lambda r: r["material_mips"]["images"][0].update(resource_identity_preserved=False),
            lambda r: r["material_mips"]["images"][0].update(native_data_bytes=0),
            lambda r: r["material_mips"]["images"].pop(),
            lambda r: r["material_mips"]["references"].pop(),
            lambda r: r["material_mips"]["references"][1].update(image=0),
            lambda r: r["material_mips"].update(outside_roi=False),
        ]
        for change in mutations:
            altered = copy.deepcopy(report)
            change(altered)
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, "material mip"):
                validate_report(altered, config)

    def test_suspended_launch_requires_configuration_and_resume_observer(self):
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "stdout.log"
            with patch("benchmark.subprocess.Popen") as launch:
                with self.assertRaisesRegex(ValueError, "observer"):
                    run_logged(["target"], log, start_suspended=True)
                launch.assert_not_called()
                events = []
                process = launch.return_value
                process.pid = 123
                process.wait.side_effect = lambda timeout: events.append("wait") or 0
                run_logged(["target"], log, process_observer=lambda pid: events.append("configure-and-resume"),
                           start_suspended=True)
                self.assertEqual(events, ["configure-and-resume", "wait"])
                self.assertEqual(launch.call_args.kwargs["creationflags"] & 0x4, 0x4)

    def test_b3_requires_complete_bounded_capture_stability_outside_roi(self):
        config, report = fixture(profile="b3")
        validate_report(report, config)
        for mutate in (
                lambda r: r.pop("capture_validation"),
                lambda r: r["capture_validation"].update(outside_roi=False),
                lambda r: r["capture_validation"].update(consecutive_equal_readbacks=1),
                lambda r: r["capture_validation"]["frames_by_view"].pop("street"),
                lambda r: r["capture_validation"]["frames_by_view"].update(street=8),
                lambda r: r["capture_validation"]["frames_by_view"].update(street=17),
                lambda r: r["capture_validation"]["frames_by_view"].update(street=True)):
            altered = copy.deepcopy(report)
            mutate(altered)
            with self.subTest(mutate=mutate), self.assertRaises(ValueError):
                validate_report(altered, config)

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

    def test_budget_rounds_slowest_run_p95_up_and_rejects_incomplete_batches(self):
        runs = []
        for culling in ("all", "frustum"):
            for timing in (False, True):
                for repetition in (1, 2, 3):
                    metrics = ["frame_interval_ms"] + (["cpu_render_ms", "gpu_ms"] if timing else [])
                    runs.append({"run_id": f"{culling}-timing-{int(timing)}-{repetition}",
                                 "culling": culling, "timing": timing,
                                 "summary": {m: {s: 1.1 * repetition for s in ("mean", "p50", "p95", "p99")} for m in metrics}})
        _, budgets = aggregate(runs)
        self.assertEqual(budgets, {"cpu_render_ms": 3.5, "gpu_ms": 3.5})
        for invalid in (runs[:-1], runs[:-1] + [runs[0]]):
            with self.assertRaises(ValueError):
                aggregate(invalid)

    def test_b3_accepts_native_float_roundoff_but_rejects_quality_drift(self):
        config, report = fixture(profile="b3")
        report["quality"]["shadow_bias"] = .10000000149
        validate_report(report, config)
        for key, value in (("sun_angular_distance", .53), ("shadow_atlas_size", 2048),
                           ("shadow_enabled", False), ("tonemap_mode", 0),
                           ("screen_space_aa", 0), ("glow", True),
                           ("ambient_source", 2), ("physical_light_units", True),
                           ("ambient_occlusion_enabled_materials", 1),
                           ("exposure", float("nan")), ("taa", 0)):
            with self.subTest(setting=key):
                altered = copy.deepcopy(report)
                altered["quality"][key] = value
                with self.assertRaises(ValueError):
                    validate_report(altered, config)
        for change in (lambda r: r["quality"].pop("shadow_bias"),
                       lambda r: r["quality"].update(hidden_effect=True),
                       lambda r: r["ibl"].update(sha256="b" * 64)):
            altered = copy.deepcopy(report)
            change(altered)
            with self.assertRaises(ValueError):
                validate_report(altered, config)

    def test_b2_and_b3_are_distinct_reference_profiles(self):
        b2, b3 = settings("b2", CONDITIONS), settings("b3", B3_CONDITIONS)
        self.assertEqual((b2["exposure"], b2["ambient_energy"], b2["shadow_enabled"]), (1, .03, False))
        self.assertEqual((b3["exposure"], b3["ibl_energy"], b3["shadow_enabled"]), (2, 3, True))
        with self.assertRaises(ValueError):
            compare_settings(b3, b2)

    def test_b3_requires_explicit_source_ao_policy_and_real_material_override(self):
        missing = copy.deepcopy(B3_CONDITIONS)
        missing["lighting"].pop("ambient_occlusion")
        with self.assertRaises(ValueError):
            settings("b3", missing)
        config, report = fixture(profile="b3")
        for key, invalid in (("ao_enabled_after", 1), ("ao_enabled_before", 0),
                             ("inspected_materials", 131), ("policy", "authored")):
            altered = copy.deepcopy(report)
            altered["material_overrides"][key] = invalid
            with self.subTest(field=key), self.assertRaises(ValueError):
                validate_report(altered, config)

    def test_split_offsets_and_cardinal_fixture_establish_native_mapping(self):
        offsets = split_offsets(.02)
        distances = [.02 + offset * (120 - .02) for offset in offsets]
        for observed, expected in zip(distances, (12.1116134084, 24.9335160031, 44.1827794386)):
            self.assertAlmostEqual(observed, expected)
        fixture = orientation_fixture()
        self.assertIs(fixture["native_gpu_verified"], False)
        axes = {row["name"]: row for row in fixture["directions"]}
        for name, expected in (("+X", (.25, .5)), ("-X", (.75, 0)),
                               ("+Z", (.5, .75)), ("-Z", (0, .25))):
            self.assertEqual((axes[name]["godot_u"], axes[name]["source_u"]), expected)
        self.assertEqual((axes["+Y"]["v"], axes["-Y"]["v"]), (0, 1))

    def test_ibl_cook_requires_orientation_cap_and_single_energy_application(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            image = directory / "godot-panorama.hdr"
            image.write_bytes(b"fixture panorama")
            manifest = {
                "schema_version": 1,
                "source": {"sha256": CONDITIONS["lighting"]["ibl"]["sha256"], "width": 4096, "height": 2048},
                "lighting": {"strength": 3, "yaw_degrees": 0},
                "max_prefilter_lod": 4, "recipe": {"cap": 4, "max_prefilter_lod": 4},
                "godot_panorama": {"path": image.name, "sha256": digest(image),
                    "byte_length": image.stat().st_size, "width": 4096, "height": 2048,
                    "format": "radiance-rgbe", "rotation_columns": 1024,
                    "mapping": "new[x]=source[(x+W/4)%W]", "strength_applied": False, "yaw_applied": False},
            }
            path = directory / "manifest.json"
            path.write_text(json.dumps(manifest), encoding="utf-8")
            accepted, identity = verify_ibl_manifest(path, CONDITIONS, read, digest)
            self.assertEqual(accepted, image)
            self.assertEqual(identity["manifest_sha256"], digest(path))
            for key, invalid in (("strength_applied", True), ("rotation_columns", 0),
                                 ("path", "../escape.hdr"), ("sha256", "0" * 64)):
                with self.subTest(field=key):
                    changed = copy.deepcopy(manifest)
                    changed["godot_panorama"][key] = invalid
                    path.write_text(json.dumps(changed), encoding="utf-8")
                    with self.assertRaises(ValueError):
                        verify_ibl_manifest(path, CONDITIONS, read, digest)

    def test_archive_recomputes_samples_and_rejects_mixed_profiles_and_conditions(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            conditions_path = directory / "conditions.json"
            conditions_path.write_text(json.dumps(CONDITIONS), encoding="utf-8")
            runs = []
            names = ("route0", "overview", "street", "material-detail", "glass-detail",
                     "label-detail", "street-mid", "street-end")
            for culling in ("all", "frustum"):
                for timing in (False, True):
                    for repetition in (1, 2, 3):
                        name = f"{culling}-timing-{int(timing)}-{repetition}"
                        folder = directory / name
                        folder.mkdir()
                        config, report = fixture(timing)
                        config.update(frames=1800, warmup=3600, culling=culling)
                        prototype = report["samples"][0]
                        report["samples"] = [{**prototype, "frame": index} for index in range(1800)]
                        report["roi_wall_ms"] = 18000.
                        for file, value in (("config.json", config), ("report.json", report)):
                            (folder / file).write_text(json.dumps(value), encoding="utf-8")
                        captures = {}
                        for view in names:
                            capture = folder / (view + ".png")
                            capture.write_bytes(b"already-validated-capture")
                            captures[view] = digest(capture)
                        for log in ("engine.log", "stdout.log"):
                            (folder / log).write_text("", encoding="utf-8")
                        runs.append({"run_id": name, "culling": culling, "timing": timing,
                                     "summary": validate_report(report, config),
                                     "report_sha256": digest(folder / "report.json"),
                                     "captures_sha256": captures})
            manifest = {"smoke": False, "validation": False, "runs": runs,
                        "identity": {"profile": "b2", "quality": settings("b2", CONDITIONS),
                                     "conditions_sha256": digest(conditions_path)}}
            (directory / "runs.json").write_text(json.dumps(manifest), encoding="utf-8")
            self.assertEqual(len(assemble(directory)["runs"]), 12)
            config_path = directory / runs[0]["run_id"] / "config.json"
            saved = config_path.read_text(encoding="utf-8")
            changed = json.loads(saved)
            changed["profile"] = "b3"
            config_path.write_text(json.dumps(changed), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "mixed quality profiles"):
                assemble(directory)
            config_path.write_text(saved, encoding="utf-8")
            conditions_path.write_text("{}", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "conditions differ"):
                assemble(directory)


if __name__ == "__main__":
    unittest.main()
