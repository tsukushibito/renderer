"""Check the optional clock against a pinned editor, holding the B3 lock externally."""
import argparse
import json
from pathlib import Path
import subprocess

from build import COMMIT, digest, write


SCRIPT = '''extends SceneTree
func _initialize() -> void:
\tvar status := GDExtensionManager.load_extension(OS.get_cmdline_user_args()[0])
\tif status != GDExtensionManager.LOAD_STATUS_OK:
\t\tpush_error("CPU clock extension did not load: " + str(status))
\t\tquit(1)
\t\treturn
\tvar before = ClassDB.class_call_static("B3CpuClock", "snapshot")
\tvar total := 0
\tvar busy_start := Time.get_ticks_usec()
\twhile Time.get_ticks_usec() - busy_start < 100000:
\t\tfor index in 10000:
\t\t\ttotal += index % 17
\tvar after = ClassDB.class_call_static("B3CpuClock", "snapshot")
\tOS.delay_msec(40)
\tvar slept = ClassDB.class_call_static("B3CpuClock", "snapshot")
\tvar valid := before is PackedInt64Array and after is PackedInt64Array and slept is PackedInt64Array
\tvalid = valid and before.size() == 7 and after.size() == 7 and slept.size() == 7
\tif valid:
\t\tvalid = before[0] >= 0 and before[1] > 0 and before[6] > 0
\t\tvalid = valid and before[1] == after[1] and after[1] == slept[1]
\t\tvalid = valid and before[6] == after[6] and after[6] == slept[6]
\t\tfor field in [0, 2, 3, 4, 5]:
\t\t\tvalid = valid and before[field] <= after[field] and after[field] <= slept[field]
\t\tvalid = valid and after[4] + after[5] > before[4] + before[5]
\t\tvalid = valid and float(slept[0] - after[0]) / after[1] >= 0.035
\tvar report := {"passed": valid, "before": Array(before), "after_busy": Array(after),
\t\t"after_sleep": Array(slept), "busy_control": total, "godot": Engine.get_version_info()}
\tFileAccess.open(OS.get_cmdline_user_args()[1], FileAccess.WRITE).store_string(JSON.stringify(report))
\tprint(JSON.stringify(report))
\tquit(0 if valid else 1)
'''


def run(editor, manifest, output):
    editor, manifest, output = Path(editor).resolve(), Path(manifest).resolve(), Path(output).resolve()
    if output.exists():
        raise ValueError("CPU clock smoke output must be new")
    identity = json.loads(manifest.read_text(encoding="utf-8"))
    for entry in identity["files"]:
        path = manifest.parent / entry["path"]
        if path.resolve().parent != manifest.parent or digest(path) != entry["sha256"]:
            raise ValueError("CPU clock artifact changed")
    output.mkdir(parents=True)
    (output / "project.godot").write_text('config_version=5\n[application]\nconfig/name="B3 CPU clock smoke"\n', encoding="utf-8")
    (output / "smoke.gd").write_text(SCRIPT, encoding="utf-8")
    command = [str(editor), "--headless", "--path", str(output), "--script", str(output / "smoke.gd"), "--",
               str(manifest.parent / "b3_cpu_clock.gdextension"), str(output / "result.json")]
    result = subprocess.run(command, encoding="utf-8", stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=90, check=False)
    (output / "engine.log").write_text(result.stdout, encoding="utf-8")
    if result.returncode or any(marker in result.stdout for marker in ("ERROR:", "WARNING:", "SCRIPT ERROR:")):
        raise ValueError(f"CPU clock smoke failed; see {output / 'engine.log'}")
    report = json.loads((output / "result.json").read_text(encoding="utf-8"))
    if report["passed"] is not True or report["godot"]["hash"] != COMMIT:
        raise ValueError("CPU clock smoke did not verify the fixed engine")
    write(output / "evidence.json", {"passed": True, "editor": str(editor), "editor_sha256": digest(editor),
        "clock_manifest": str(manifest), "clock_manifest_sha256": digest(manifest), "command": command,
        "result": report, "engine_log_sha256": digest(output / "engine.log")})
    return output / "evidence.json"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--editor", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    print(run(args.editor, args.manifest, args.output))


if __name__ == "__main__":
    main()
