import copy
import unittest

import resource_acceptance as acceptance


def report():
    snapshot = {"api": "vulkan", "native": {"allocation_count": 8,
        "allocation_bytes": 1000, "block_count": 2, "block_bytes": 2048},
        "logical": {"registered_bytes": 500, "retained_bytes": 0, "cached_bytes": 100},
        "process_usage_bytes": 3000, "process_budget_bytes": 10000}
    cycle = {"old_pending_matches": True, "stale_image_rejected": True,
        "stale_buffer_rejected": True, "depth_unchanged": True, "restored_matches": True,
        "changed_pixels": {"hdr": 20, "sdr": 20}, "mip_probes": [{"passed": True}]}
    cycle.update(texture_changed_pixels={"hdr":10,"sdr":10},
                 material_changed_pixels={"hdr":10,"sdr":10},
                 pending_memory={"logical":{"retained_bytes":80}}, expected_retained_bytes=80)
    return {"schema_version": 1, "runtime": {"api": "vulkan", "validation_mode": "strict"},
        "scene": {"meshes": 1296, "primitives": 1591, "materials": 132,
                  "resident_images_with_defaults": 409,
                  "source": {"image_count":405,"triangle_count":2832120}},
        "validation": {"clean": True, "mode": "strict", "warnings": 0, "errors": 0,
            "callback_failures": 0, "requested": "Required", "core_validation": True,
            "synchronization_validation": True},
        "memory": {"normal_allocation_free": True},
        "resource_checks": {"passed": True,
            "stages": {name: copy.deepcopy(snapshot) for name in
                       ("prepared", "uploaded", "warmed", "after_frames", "after_checks")},
            "views": {"names": ["street", "overview"], "queued_before_completion_observation": 2,
                "serial_matches": True, "capacity": {"resource": "Executions",
                    "recovery": "Poll", "limit": 2, "required": 3, "retry_matches": True}},
            "replacement": {"mip_levels": 1, "cycles": [copy.deepcopy(cycle) for _ in range(4)]},
            "memory_samples": [copy.deepcopy(snapshot) for _ in range(4)],
            "plateau": {"passed": True}}}


class ResourceAcceptanceTests(unittest.TestCase):
    def check(self, value):
        return acceptance.validate_report(value, api="vulkan", mode="strict", full_scene=True)

    def test_accepts_native_usage_changes_separate_from_resource_growth(self):
        value = report()
        value["resource_checks"]["memory_samples"][-1]["process_usage_bytes"] = 3500
        self.assertTrue(self.check(value)["passed"])

    def test_rejects_incomplete_scope_or_false_positive_acceptance(self):
        changes = [
            lambda r: r["scene"].update(primitives=1590),
            lambda r: r["validation"].update(errors=1),
            lambda r: r["validation"].update(synchronization_validation=None),
            lambda r: r["memory"].update(normal_allocation_free=False),
            lambda r: r["resource_checks"]["stages"]["uploaded"].update(process_budget_bytes=None),
            lambda r: r["resource_checks"]["views"].update(queued_before_completion_observation=1),
            lambda r: r["resource_checks"]["views"]["capacity"].update(required=2),
            lambda r: r["resource_checks"]["replacement"]["cycles"][0].update(old_pending_matches=False),
            lambda r: r["resource_checks"]["replacement"]["cycles"][0]["changed_pixels"].update(sdr=0),
            lambda r: r["resource_checks"]["replacement"]["cycles"][0]["texture_changed_pixels"].update(sdr=0),
            lambda r: r["resource_checks"]["replacement"]["cycles"][0]["material_changed_pixels"].update(hdr=0),
            lambda r: r["resource_checks"]["replacement"]["cycles"][0]["pending_memory"]["logical"].update(retained_bytes=0),
            lambda r: r["resource_checks"]["replacement"].update(mip_levels=2),
            lambda r: r["resource_checks"]["memory_samples"][-1]["native"].update(allocation_bytes=1001),
            lambda r: r["resource_checks"]["memory_samples"][-1]["logical"].update(retained_bytes=4),
        ]
        for change in changes:
            value = report()
            change(value)
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.check(value)


if __name__ == "__main__":
    unittest.main()
