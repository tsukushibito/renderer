import argparse
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import b5_evidence
import b5_images
from b5_window_acceptance import street_reference
from compare_runtime import load_json
from resource_acceptance import write_json


MODES = {"samples": 1, "pipeline": "forward", "submission": "direct", "instances": "off"}
INPUTS = {"cook_manifest_sha256": "cook", "ibl_manifest_sha256": "ibl",
          "conditions_sha256": "conditions", "shaders_sha256": "shaders"}


def offscreen_report():
    return {"schema_version": 1, "runtime": {"api": "vulkan"},
            "identity": {**MODES, "view": "street",
                         "cook_manifest_sha256": INPUTS["cook_manifest_sha256"],
                         "environment": {"manifest_sha256": INPUTS["ibl_manifest_sha256"]},
                         "conditions_sha256": INPUTS["conditions_sha256"],
                         "shaders_sha256": INPUTS["shaders_sha256"]}}


class ReportIdentityTests(unittest.TestCase):
    def test_explicit_render_modes_are_required_in_both_report_shapes(self):
        for windowed in (False, True):
            with self.subTest(windowed=windowed):
                report = {"schema_version": 1, **MODES} if windowed else offscreen_report()
                identity = report if windowed else report["identity"]
                self.assertEqual(b5_evidence.render_modes(report, windowed=windowed), MODES)
                for field, value in MODES.items():
                    del identity[field]
                    with self.assertRaisesRegex(ValueError, field):
                        b5_evidence.render_modes(report, windowed=windowed)
                    identity[field] = value
                for wrong_samples in (True, 1.0, "1", 2):
                    identity["samples"] = wrong_samples
                    with self.assertRaisesRegex(ValueError, "samples"):
                        b5_evidence.render_modes(report, windowed=windowed)
                identity["samples"] = 1
                report["schema_version"] = True
                with self.assertRaisesRegex(ValueError, "current Bistro report"):
                    b5_evidence.render_modes(report, windowed=windowed)

    def test_actual_manifest_inputs_must_match(self):
        reports = ((False, offscreen_report()), (True, {"inputs": INPUTS}))
        for windowed, report in reports:
            with self.subTest(windowed=windowed):
                self.assertEqual(b5_evidence.loaded_inputs(report, windowed=windowed), INPUTS)
                b5_evidence.validate_loaded_manifests(report, "cook", "ibl", windowed=windowed)
                for cook, ibl in (("other", "ibl"), ("cook", "other")):
                    with self.assertRaisesRegex(ValueError, "different cook or IBL manifest"):
                        b5_evidence.validate_loaded_manifests(report, cook, ibl, windowed=windowed)


class ImageProgressTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.args = argparse.Namespace(
            output=self.root / "images", strict=self.root / "strict.exe",
            shipping=self.root / "shipping.exe", manifest=self.root / "cook.json",
            ibl_manifest=self.root / "ibl.json", stage="pilot", resume=False, msaa=1,
            pipeline="forward",
        )
        for field in ("strict", "shipping", "manifest", "ibl_manifest"):
            getattr(self.args, field).write_text(field, encoding="utf-8")
        self.settings = {"source": b5_images.source_identity(self.args),
                         "regions": b5_images.ROI, "modes": MODES}
        self.progress = b5_images.begin_cohort(self.args, self.settings, None)

    def test_resume_refuses_replaced_input_at_same_path(self):
        self.args.resume = True
        self.assertEqual(b5_images.begin_cohort(self.args, self.settings, None)["runs"], {})
        for field in ("strict", "shipping", "manifest", "ibl_manifest"):
            with self.subTest(field=field):
                path = getattr(self.args, field)
                path.write_text("replacement", encoding="utf-8")
                current = {**self.settings, "source": b5_images.source_identity(self.args)}
                with self.assertRaisesRegex(ValueError, "resume identity differs"):
                    b5_images.begin_cohort(self.args, current, None)
                path.write_text(field, encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "resume identity differs"):
            b5_images.begin_cohort(self.args, self.settings, "other calibration")
        self.args.stage = "accept"
        with self.assertRaisesRegex(ValueError, "resume identity differs"):
            b5_images.begin_cohort(self.args, self.settings, None)

    def test_uncommitted_report_is_retried_and_committed_report_is_verified(self):
        label = "strict-vulkan-1"
        first = self.args.output / label / "report.json"
        retried = self.args.output / f"{label}-attempt2" / "report.json"

        def failed_run(command, **_kwargs):
            write_json(first, {"report": "exists before the executable returns failure"})
            raise subprocess.CalledProcessError(1, command)

        def successful_run(_command, **_kwargs):
            write_json(retried, offscreen_report())

        def sample(progress):
            return b5_images.sample_path(
                self.args, progress, label, self.args.strict, "vulkan", [],
            )

        with patch("b5_images.subprocess.run", side_effect=failed_run):
            with self.assertRaises(subprocess.CalledProcessError):
                sample(self.progress)
        self.args.resume = True
        progress = b5_images.begin_cohort(self.args, self.settings, None)
        self.assertEqual(progress["runs"], {})
        with patch("b5_images.subprocess.run", side_effect=successful_run):
            self.assertEqual(sample(progress), (retried, offscreen_report()))
        b5_images.record_run(self.args, progress, label, retried)

        committed = b5_images.begin_cohort(self.args, self.settings, None)
        with patch("b5_images.subprocess.run") as run:
            self.assertEqual(sample(committed), (retried, offscreen_report()))
            run.assert_not_called()
            write_json(retried, {"report": "changed"})
            with self.assertRaisesRegex(ValueError, "Stored report differs"):
                sample(committed)
        self.assertTrue(first.is_file())

    def test_tampered_child_report_is_rejected_before_capture_reuse(self):
        label = "strict-vulkan-1"
        run = self.args.output / label
        parent = run / "report.json"
        street = run / "views" / "street" / "report.json"
        street.parent.mkdir(parents=True)
        write_json(parent, offscreen_report())
        write_json(run / "command.json", ["binary"])
        write_json(street, offscreen_report())
        samples = {"view/street": b5_evidence.report_record(self.args.output, street)}
        b5_images.record_run(self.args, self.progress, label, parent, samples)
        prior = self.progress["runs"][label]
        self.assertEqual(street_reference(
            self.args.output, {"runs": {label: prior}, "settings": self.settings}, "vulkan",
        ), (street, offscreen_report()))

        write_json(street, {"report": "changed"})
        with self.assertRaisesRegex(ValueError, "Stored report differs"):
            street_reference(self.args.output,
                             {"runs": {label: prior}, "settings": self.settings}, "vulkan")
        with patch("b5_images.reports", return_value={"view/street": street}):
            with self.assertRaisesRegex(ValueError, "Stored capture reports differ"):
                b5_images.collect(parent, offscreen_report(), self.args.output, {}, (), prior)

    def test_failed_checkpoint_publication_preserves_last_valid_state(self):
        path = self.args.output / "progress.json"

        def interrupted_write(temporary, _value):
            temporary.write_text("{partial", encoding="utf-8")
            raise OSError("interrupted")

        with patch("b5_evidence.write_json", side_effect=interrupted_write):
            with self.assertRaisesRegex(OSError, "interrupted"):
                b5_evidence.write_checkpoint(path, {"invalid": True})
        self.assertEqual(load_json(path)["runs"], {})
        self.assertEqual(list(self.args.output.iterdir()), [path])

    def test_pilot_resume_finishes_after_profile_was_published(self):
        output = self.root / "interrupted-pilot"
        profile = self.root / "profile.json"
        argv = ["b5_images.py", "pilot", "--output", str(output), "--profile", str(profile)]
        for field in ("strict", "shipping", "manifest", "ibl_manifest"):
            argv += ["--" + field.replace("_", "-"), str(getattr(self.args, field))]
        seen = {"vulkan": {"view/street": object()}}

        def interrupt_summary(path, value):
            if path.name == "summary.json":
                raise OSError("interrupted before summary")
            b5_evidence.write_checkpoint(path, value)

        with patch("b5_images.cohort", side_effect=lambda *_: (seen, {})), \
                patch("b5_images.probe_faults", return_value={"test": "passed"}):
            with patch("sys.argv", argv), patch("b5_images.write_checkpoint", side_effect=interrupt_summary):
                with self.assertRaisesRegex(OSError, "interrupted before summary"):
                    b5_images.main()
            self.assertFalse((output / "summary.json").exists())
            self.assertEqual(load_json(profile)["status"], "passed")

            with patch("sys.argv", argv + ["--resume"]):
                b5_images.main()
        self.assertEqual(load_json(output / "summary.json"), load_json(profile))


if __name__ == "__main__":
    unittest.main()
