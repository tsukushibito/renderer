"""Check game manifests for direct dependencies on physics and math backends."""

import json
from pathlib import Path
import subprocess


BACKENDS = {
    "glam", "nalgebra",
    "rapier2d", "rapier2d-f64", "rapier3d", "rapier3d-f64",
    "parry2d", "parry2d-f64", "parry3d", "parry3d-f64",
}


def main():
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--no-deps", "--format-version", "1", "--offline",
    ]))
    root = Path(metadata["workspace_root"])
    violations = []
    for package in metadata["packages"]:
        manifest = Path(package["manifest_path"])
        # Cargo reports the original package name even for renamed dependencies.
        dependencies = {dep["name"] for dep in package["dependencies"]}
        if manifest.is_relative_to(root / "examples") or "sashiko" in dependencies:
            for backend in sorted(dependencies & BACKENDS):
                violations.append(f"{manifest}: direct dependency on {backend}")
    if violations:
        raise SystemExit("\n".join(violations + ["Use sashiko::math and sashiko::physics."]))


if __name__ == "__main__":
    main()
