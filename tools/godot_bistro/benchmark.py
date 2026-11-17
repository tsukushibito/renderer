"""Prepare and measure a pinned Godot release-template Bistro reference.

No global install, editor asset import, system setting change, or headless GPU
measurement. Runtime GLTFDocument import is outside ROI. B2 retains native
base-only PNG textures; B3 explicitly generates and verifies full material mips.
Godot materials and generated mips differ from the Renderer DDS cook; this is a renderer reference,
not an isolated RenderGraph-overhead measurement.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
import re
from pathlib import Path
import shutil
import statistics
import subprocess
import time

if __package__:
    from .profiles import (PROFILES, compare_settings, orientation_fixture, project_settings,
                           settings, verify_ibl_manifest)
else:
    from profiles import (PROFILES, compare_settings, orientation_fixture, project_settings,
                          settings, verify_ibl_manifest)


ROOT = Path(__file__).resolve().parents[2]
SOURCE = Path(__file__).resolve().parent
CAPTURE_VIEWS = {"route0", "overview", "street", "material-detail", "glass-detail",
                 "label-detail", "street-mid", "street-end"}
PROJECT_SOURCES = ("project.godot", "main.gd", "main.tscn", "material_mips.gd")


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def read(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def write(path, value):
    Path(path).write_text(json.dumps(value, indent=2, ensure_ascii=False, allow_nan=False) + "\n", encoding="utf-8")


def run_logged(command, log, timeout=600, process_observer=None, start_suspended=False):
    if start_suspended and process_observer is None:
        raise ValueError("Suspended launch requires an observer to configure and resume the target")
    environment = os.environ.copy()
    # Keep editor settings, release shader caches and user:// inside task output.
    environment["APPDATA"] = str(Path(log).resolve().parent / "appdata")
    environment["LOCALAPPDATA"] = str(Path(log).resolve().parent / "localappdata")
    with Path(log).open("w", encoding="utf-8") as stream:
        process = subprocess.Popen([str(x) for x in command], stdout=stream,
                                   stderr=subprocess.STDOUT, env=environment,
                                   creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0) | (0x4 if start_suspended else 0))
        try:
            observation = process_observer(process.pid) if process_observer else None
            returncode = process.wait(timeout=timeout)
        except BaseException:
            if process.poll() is None:
                process.kill()
            process.wait()
            raise
    if returncode:
        raise ValueError(f"Process failed ({returncode}); see {log}")
    text = Path(log).read_text(encoding="utf-8", errors="replace")
    if any(marker in text for marker in ("ERROR:", "SCRIPT ERROR:", "WARNING:", "Validation Error", "Validation Warning")):
        raise ValueError(f"Engine diagnostics require investigation; see {log}")
    return {"pid": process.pid, "process_observation": observation, "returncode": returncode}


def prepare(args):
    profile = args.profile
    if (profile == "b3") != (args.ibl_manifest is not None):
        raise ValueError("The B3 profile requires --ibl-manifest; B2 does not use an IBL cook")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    cpu_clock = None
    if getattr(args, "cpu_clock_manifest", None):
        source = args.cpu_clock_manifest.resolve()
        clock = read(source)
        clock_output = output / "cpu-clock"
        clock_output.mkdir()
        for entry in clock["files"]:
            relative = Path(entry["path"])
            original = (source.parent / relative).resolve()
            if relative.is_absolute() or not original.is_relative_to(source.parent) or relative.name != str(relative):
                raise ValueError("CPU clock files must be flat paths inside their manifest directory")
            if original.stat().st_size != entry["byte_length"] or digest(original) != entry["sha256"]:
                raise ValueError("CPU clock input identity differs")
            shutil.copyfile(original, clock_output / relative)
        shutil.copyfile(source, clock_output / "manifest.json")
        cpu_clock = {"manifest": clock, "manifest_sha256": digest(source)}
    assets = args.asset_root.resolve()
    expected = read(ROOT / "assets/bistro/manifest.json")
    verified = []
    for item in expected["files"]:
        if not item["path"].startswith("assets/bistro/converted/"):
            continue
        path = assets / item["path"]
        if path.stat().st_size != item["size"] or digest(path) != item["sha256"]:
            raise ValueError(f"Input identity mismatch: {path}")
        verified.append(item)
    if len(verified) != 407:
        raise ValueError("Expected all 407 converted Bistro files")
    project = output / "project"
    project.mkdir()
    hashes = {}
    for name in PROJECT_SOURCES:
        shutil.copyfile(SOURCE / name, project / name)
    shutil.copyfile(args.conditions, output / "conditions.json")
    conditions = read(output / "conditions.json")
    quality = settings(profile, conditions)
    ibl_identity = None
    if profile == "b3":
        panorama, ibl_identity = verify_ibl_manifest(args.ibl_manifest, conditions, read, digest)
        shutil.copyfile(panorama, output / "godot-panorama.hdr")
        shutil.copyfile(args.ibl_manifest, output / "ibl-manifest.json")
        write(output / "orientation-fixture.json", orientation_fixture())
        ibl_identity["orientation_fixture_sha256"] = digest(output / "orientation-fixture.json")
        project_file = project / "project.godot"
        project_file.write_text(project_file.read_text(encoding="utf-8").replace(
            "[audio]", project_settings(profile) + "\n[audio]"), encoding="utf-8")
    for name in PROJECT_SOURCES:
        hashes[name] = digest(project / name)
    editor, template = args.editor.resolve(), args.template.resolve()
    # Custom template avoids any modification to the user's installed Godot.
    (project / "export_presets.cfg").write_text(
        '[preset.0]\nname="Windows"\nplatform="Windows Desktop"\nrunnable=true\n'
        'export_filter="all_resources"\ninclude_filter=""\nexclude_filter=""\n'
        '[preset.0.options]\ncustom_template/release="' + template.as_posix() + '"\n'
        'binary_format/architecture="x86_64"\napplication/modify_resources=false\n'
        'graphics/opengl3/include=false\ngraphics/d3d12/include=false\n', encoding="utf-8")
    run_logged([editor, "--headless", "--path", project, "--editor", "--import"], output / "import.log")
    run_logged([editor, "--headless", "--path", project, "--export-release", "Windows", output / "bistro-reference.exe"], output / "export.log")
    identity = {"schema_version": 2, "created_utc": datetime.now(timezone.utc).isoformat(),
                "profile": profile, "quality": quality, "ibl": ibl_identity,
                "editor_sha256": digest(editor), "template_sha256": digest(template),
                "binary_sha256": digest(output / "bistro-reference.exe"),
                "pack_sha256": digest(output / "bistro-reference.pck"), "project_sources": hashes,
                "conditions_sha256": digest(output / "conditions.json"), "inputs": verified,
                "gltf": str(assets / "assets/bistro/converted/BistroExterior.gltf"),
                "asset_root": str(assets),
                "source_commit": subprocess.check_output(["git", "-c", f"safe.directory={ROOT.as_posix()}", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
                "benchmark_tool_sha256": digest(SOURCE / "benchmark.py"),
                "profile_tool_sha256": digest(SOURCE / "profiles.py"),
                "contract": "Forward+ Vulkan release template; runtime glTF full exterior; generated tangents, uncompressed ImageTexture material images; no LOD/occlusion, optional frustum culling; windowed 1080p vsync off; fixed frame-indexed route. B2 retains base-only PNG images, authored AO, constant ambient .03, no IBL/shadow/AA, linear exposure1. B3 explicitly generates full PNG mip chains before warmup, renormalizing only normal-map coarser levels, and verifies actual native readbacks, base bytes and shared material references. Renderer retains authored DDS mip chains; Godot generated mip pixels are not equivalent. B3 disables source AO maps while retaining all source bytes/materials; cap4 static IBL strength3, four 2048-effective CSM splits/PCF/no PCSS, native AgX exposure2 and FXAA, no Bloom/extra effects. Godot's BRDF, prefilter, shadow filter, depth prepass, batching, mip chain and presentation differ from Renderer; AgX and pre-tonemap FXAA differ from Renderer Neutral and post-tonemap FXAA."}
    identity["cpu_clock"] = cpu_clock
    write(output / "identity.json", identity)
    print(output / "identity.json")


def percentile(values, p):
    ordered = sorted(values)
    position = (len(ordered) - 1) * p
    lower = math.floor(position)
    upper = math.ceil(position)
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def charged_cpu_scalars(cpu, frames):
    """Whole-window OS accounting; these means have no per-frame percentiles."""
    keys = ("qpc_start", "qpc_end", "qpc_frequency", "process_kernel_100ns", "process_user_100ns",
            "main_thread_kernel_100ns", "main_thread_user_100ns")
    if type(frames) is not int or frames <= 0 or any(type(cpu.get(k)) is not int or cpu[k] < 0 for k in keys):
        raise ValueError("Invalid charged CPU boundary counters")
    ticks = cpu["qpc_end"] - cpu["qpc_start"]
    if ticks <= 0 or cpu["qpc_frequency"] <= 0:
        raise ValueError("Invalid charged CPU QPC window")
    wall_ms = ticks * 1000 / cpu["qpc_frequency"]
    process = (cpu["process_kernel_100ns"] + cpu["process_user_100ns"]) / 10000
    caller = (cpu["main_thread_kernel_100ns"] + cpu["main_thread_user_100ns"]) / 10000
    # OS process/thread counters are read sequentially at each boundary; their
    # intervals differ slightly. Do not demand a strict subset inequality.
    if process <= 0 or caller <= 0:
        raise ValueError("Charged CPU window must contain process and caller execution")
    return {"process_charged_cpu_ms_per_frame": process / frames,
            "caller_thread_charged_cpu_ms_per_frame": caller / frames,
            "qpc_wall_ms_per_frame": wall_ms / frames,
            "throughput_frames_per_second": frames * 1000 / wall_ms}


def cpu_window_metrics(report, frames):
    window = report["cpu_window"]
    start, end = window["start"], window["end"]
    if (not isinstance(start, list) or not isinstance(end, list) or len(start) != 7 or len(end) != 7
            or any(type(x) is not int or x < 0 for x in start + end)
            or start[1] != end[1] or start[6] != end[6] or start[6] == 0):
        raise ValueError("Invalid native CPU snapshot, frequency or caller thread")
    if any(end[i] < start[i] for i in range(2, 6)):
        raise ValueError("Native CPU counter decreased")
    cpu = {"qpc_start": start[0], "qpc_end": end[0], "qpc_frequency": start[1]}
    cpu.update({name: end[i] - start[i] for i, name in enumerate(
        ("process_kernel_100ns", "process_user_100ns", "main_thread_kernel_100ns", "main_thread_user_100ns"), 2)})
    brackets = window["call_brackets_usec"]
    if (not isinstance(brackets, list) or len(brackets) != 4
            or any(type(x) is not int or x < 0 for x in brackets)
            or brackets != sorted(brackets)):
        raise ValueError("Invalid CPU clock endpoint brackets")
    qpc_usec = (end[0] - start[0]) * 1e6 / start[1]
    if not brackets[2] - brackets[1] - 2 <= qpc_usec <= brackets[3] - brackets[0] + 2:
        raise ValueError("Native QPC interval lies outside endpoint clock brackets")
    return charged_cpu_scalars(cpu, frames)


def validate_material_mips(inventory):
    """Accept the fixed full-scene preparation proof, not a config echo."""
    def require(condition, message):
        if not condition:
            raise ValueError("B3 material mip inventory: " + message)

    require(isinstance(inventory, dict), "missing native preparation proof")
    for key, expected in (("policy", "b3-explicit-png-full-mips-v1"), ("outside_roi", True),
                          ("verified", True), ("image_count", 405), ("material_count", 132),
                          ("material_reference_count", 667), ("normal_images", 132),
                          ("total_levels_including_base", 3529)):
        require(type(inventory.get(key)) is type(expected) and inventory[key] == expected, key + " differs")
    records, references = inventory.get("images"), inventory.get("references")
    require(isinstance(records, list) and len(records) == 405, "all source images are required")
    require(isinstance(references, list) and len(references) == 667, "all material references are required")
    require(all(isinstance(record, dict) and type(record.get("index")) is int for record in records)
            and {record["index"] for record in records} == set(range(405)), "image indices are incomplete or duplicated")
    by_index = {record["index"]: record for record in records}
    levels = normal_images = 0
    roles = {index: set() for index in by_index}
    slots = set()
    for record in records:
        width, height, format = record.get("width"), record.get("height"), record.get("format")
        require(type(width) is int and width > 0 and type(height) is int and height > 0, "invalid extent")
        require(type(format) is int and format in (4, 5), "expected native RGB8/RGBA8 readback format")
        count, bytes, w, h = 0, 0, width, height
        while True:
            bytes += w * h * (3 if format == 4 else 4)
            if w == h == 1:
                break
            w, h = max(1, w // 2), max(1, h // 2)
            count += 1
        require(record.get("class") == "ImageTexture" and record.get("compressed") is False,
                "source image is compressed or not the native runtime texture type")
        require(type(record.get("mipmap_count")) is int and record["mipmap_count"] == count
                and record.get("has_mipmaps") is (count > 0), "native chain is incomplete")
        require(type(record.get("mipmaps_before")) is int and record["mipmaps_before"] == 0,
                "unexpected pre-generation chain")
        require(type(record.get("base_bytes")) is int and record["base_bytes"] == width * height * (3 if format == 4 else 4)
                and type(record.get("native_data_bytes")) is int and record["native_data_bytes"] == bytes,
                "readback byte size differs from its full chain")
        require(record.get("resource_identity_preserved") is True, "texture ownership/reference identity changed")
        require(isinstance(record.get("base_sha256"), str) and re.fullmatch(r"[0-9a-f]{64}", record["base_sha256"])
                and record.get("native_base_sha256") == record["base_sha256"], "base pixels changed")
        require(type(record.get("normal_renormalized")) is bool, "missing normal mip policy")
        levels += count + 1
        normal_images += record["normal_renormalized"]
    for reference in references:
        require(isinstance(reference, dict), "invalid material reference")
        material, role, image = reference.get("material"), reference.get("role"), reference.get("image")
        require(type(material) is int and 0 <= material < 132
                and role in ("albedo", "normal", "metallic", "roughness", "emission", "ao")
                and type(image) is int and image in by_index, "invalid material/image slot")
        require((material, role) not in slots, "duplicated material slot")
        # ObjectID/RID are opaque 64-bit identities. RefCounted ObjectIDs can
        # set the high bit and therefore appear negative in GDScript/JSON.
        require(type(reference.get("resource_id")) is int and reference["resource_id"] != 0
                and type(reference.get("rid")) is int and reference["rid"] != 0, "missing live resource identities")
        slots.add((material, role))
        roles[image].add(role)
    require(levels == 3529 and normal_images == 132, "full chain or normal image count changed")
    require(all(uses and (uses == {"normal"}) == by_index[index]["normal_renormalized"]
                and ("normal" not in uses or uses == {"normal"}) for index, uses in roles.items()),
            "unused image or mixed normal/color roles")


def validate_report(report, config):
    if report.get("config") != config or report.get("debug") is not False:
        raise ValueError("Run config or release identity mismatch")
    if report.get("driver") != "vulkan" or report.get("renderer") != "forward_plus":
        raise ValueError("Requested Vulkan Forward+ backend was not used")
    if report.get("inventory") != {"mesh_instances": 1296, "surfaces": 1591, "triangles": 2832120}:
        raise ValueError("Incomplete Bistro inventory")
    if report.get("import_inventory") != {"materials": 132, "images": 405}:
        raise ValueError("Incomplete Bistro material/image inventory")
    version = report.get("godot", {})
    if version.get("hash") != "ed1daf0bf001b61586d9930840f2f1394092c079":
        raise ValueError("Expected the pinned official Godot 4.7.2 release")
    if report.get("viewport_size") != [1920, 1080] or report.get("vsync") != 0 or report.get("max_fps") != 0:
        raise ValueError("Resolution or frame pacing mismatch")
    if config.get("profile") not in PROFILES:
        raise ValueError("Missing explicit quality profile")
    if config["quality"].get("profile") != config["profile"]:
        raise ValueError("Requested profile differs from its quality settings")
    compare_settings(report.get("quality"), config["quality"])
    if config["profile"] == "b3":
        validate_material_mips(report.get("material_mips"))
        if report.get("ibl") != config["ibl"]:
            raise ValueError("Effective IBL input differs from the archived cook")
        override = report.get("material_overrides", {})
        if (override.get("policy") != "bistro-source-ao-disabled"
                or override.get("inspected_materials") != 132
                or type(override.get("ao_enabled_before")) is not int
                or not 0 < override["ao_enabled_before"] <= 132
                or override.get("ao_enabled_after") != 0):
            raise ValueError("B3 source AO material override was not applied")
        capture = report.get("capture_validation", {})
        if ({key: capture.get(key) for key in ("settling_frames", "maximum_frames",
                "consecutive_equal_readbacks", "outside_roi")}
                != {"settling_frames": 8, "maximum_frames": 16,
                    "consecutive_equal_readbacks": 2, "outside_roi": True}):
            raise ValueError("B3 fixed-view capture stability protocol differs")
        frames = capture.get("frames_by_view", {})
        if (not isinstance(frames, dict) or set(frames) != CAPTURE_VIEWS
                or any(type(value) is not int or not 9 <= value <= 16 for value in frames.values())):
            raise ValueError("B3 fixed-view captures did not establish bounded byte stability")
    samples = report.get("samples", [])
    if len(samples) != config["frames"]:
        raise ValueError("Incomplete samples")
    metrics = ["frame_interval_ms", "render_pre_post_ms"]
    if config["timing"]:
        metrics += ["cpu_setup_ms", "cpu_viewport_ms", "gpu_ms", "draw_calls", "primitives"]
    for index, sample in enumerate(samples):
        if sample.get("frame") != index:
            raise ValueError("Discontinuous frame identity")
        for metric in metrics:
            value = sample.get(metric)
            if type(value) not in (int, float) or not math.isfinite(value) or value <= 0:
                raise ValueError(f"Invalid or missing {metric} at frame {index}")
        if not config["timing"] and any(key in sample for key in ("gpu_ms", "cpu_setup_ms", "cpu_viewport_ms", "cpu_render_ms")):
            raise ValueError("Timing-disabled run contains timer readings")
    if abs(sum(x["frame_interval_ms"] for x in samples) - report["roi_wall_ms"]) > 0.01:
        raise ValueError("Frame intervals do not cover exact ROI")
    if config.get("cpu_clock_library"):
        if report.get("schema_version") != 2 or report.get("render_thread_model") != 1:
            raise ValueError("CPU scope requires schema 2 and Safe/main-thread rendering")
        cpu_window_metrics(report, config["frames"])
        drawn = [sample.get("observation_frame_drawn") for sample in samples]
        if (any(type(value) is not int or value < 0 for value in drawn)
                or any(second != first + 1 for first, second in zip(drawn, drawn[1:]))):
            raise ValueError("Post-draw observation frame IDs are discontinuous")
        if any(sample["render_pre_post_ms"] > sample["frame_interval_ms"] + .002 for sample in samples):
            raise ValueError("Render pre/post interval exceeds its callback interval")
        brackets = report["cpu_window"]["call_brackets_usec"]
        if abs((brackets[2] - brackets[0]) / 1000 - report["roi_wall_ms"]) > .002:
            raise ValueError("Native CPU boundary brackets cover a different callback ROI")
    return {metric: {"mean": statistics.mean(x[metric] for x in samples),
                     "p50": percentile([x[metric] for x in samples], .5),
                     "p95": percentile([x[metric] for x in samples], .95),
                     "p99": percentile([x[metric] for x in samples], .99)} for metric in metrics}


def verify_prepared(prepared):
    prepared = Path(prepared).resolve()
    identity = read(prepared / "identity.json")
    if identity.get("cpu_clock"):
        clock_root = prepared / "cpu-clock"
        if digest(clock_root / "manifest.json") != identity["cpu_clock"]["manifest_sha256"]:
            raise ValueError("Prepared CPU clock manifest was modified")
        for entry in identity["cpu_clock"]["manifest"]["files"]:
            path = (clock_root / entry["path"]).resolve()
            if (not path.is_relative_to(clock_root.resolve()) or path.stat().st_size != entry["byte_length"]
                    or digest(path) != entry["sha256"]):
                raise ValueError("Prepared CPU clock was modified")
    if digest(prepared / "bistro-reference.exe") != identity["binary_sha256"] or digest(prepared / "bistro-reference.pck") != identity["pack_sha256"]:
        raise ValueError("Prepared executable or pack was modified")
    if digest(prepared / "conditions.json") != identity["conditions_sha256"]:
        raise ValueError("Prepared conditions were modified")
    if (digest(SOURCE / "benchmark.py") != identity["benchmark_tool_sha256"]
            or digest(SOURCE / "profiles.py") != identity["profile_tool_sha256"]):
        raise ValueError("Measurement tools differ from the prepared source identity")
    for name, expected in identity["project_sources"].items():
        if digest(prepared / "project" / name) != expected:
            raise ValueError(f"Prepared project source was modified: {name}")
    for item in identity["inputs"]:
        source = Path(identity["asset_root"]) / item["path"]
        if source.stat().st_size != item["size"] or digest(source) != item["sha256"]:
            raise ValueError(f"Runtime Bistro input was modified: {source}")
    profile = identity["profile"]
    conditions = read(prepared / "conditions.json")
    compare_settings(identity["quality"], settings(profile, conditions))
    if profile == "b3":
        ibl = identity["ibl"]
        if (digest(prepared / "ibl-manifest.json") != ibl["manifest_sha256"]
                or digest(prepared / "godot-panorama.hdr") != ibl["panorama"]["sha256"]
                or digest(prepared / "orientation-fixture.json") != ibl["orientation_fixture_sha256"]
                or read(prepared / "orientation-fixture.json") != orientation_fixture()):
            raise ValueError("Prepared IBL input was modified")
    return identity


def run_case(prepared, identity, folder, *, culling, timing, repetition,
             frames=1800, warmup=3600, validation=False, timeout=600,
             process_observer=None, start_suspended=False):
    """One unchanged CLI case, shared with the serial cross-engine scheduler."""
    prepared, folder = Path(prepared), Path(folder)
    folder.mkdir()
    config = {"gltf": identity["gltf"], "conditions": str(prepared / "conditions.json"),
              "profile": identity["profile"], "quality": identity["quality"],
              "frames": frames, "warmup": warmup, "timing": timing, "culling": culling,
              "output": str(folder / "report.json"), "capture": str(folder / "route0.png")}
    if identity.get("cpu_clock"):
        config["cpu_clock_library"] = str(prepared / "cpu-clock/b3_cpu_clock.gdextension")
    if identity["profile"] == "b3":
        config["ibl_path"] = str(prepared / "godot-panorama.hdr")
        config["ibl"] = {"sha256": identity["ibl"]["panorama"]["sha256"],
                         "width": 4096, "height": 2048}
    write(folder / "config.json", config)
    command = [prepared / "bistro-reference.exe", "--rendering-driver", "vulkan", "--rendering-method", "forward_plus",
               "--resolution", "1920x1080", "--position", "0,0", "--disable-vsync", "--audio-driver", "Dummy",
               "--log-file", folder / "engine.log"]
    if validation:
        command.append("--gpu-validation")
    command += ["--", folder / "config.json"]
    start = time.monotonic()
    process = run_logged(command, folder / "stdout.log", timeout, process_observer, start_suspended=start_suspended)
    summary = validate_report(read(folder / "report.json"), config)
    captures = {}
    import struct
    for name in ["route0"] + [view["name"] for view in read(prepared / "conditions.json")["views"]]:
        path = folder / f"{name}.png"
        header = path.read_bytes()[:24]
        if header[:8] != b"\x89PNG\r\n\x1a\n" or struct.unpack(">II", header[16:24]) != (1920, 1080):
            raise ValueError(f"Capture has wrong format/size: {path}")
        captures[name] = digest(path)
    return {"run_id": f"{culling}-timing-{int(timing)}-{repetition}",
            "culling": culling, "timing": timing, **process,
            "controller_wall_seconds": time.monotonic() - start,
            "command": [str(x) for x in command], "summary": summary,
            "report_sha256": digest(folder / "report.json"), "captures_sha256": captures}


def measure(args):
    prepared = args.prepared.resolve()
    identity = verify_prepared(prepared)
    profile = identity["profile"]
    if args.frames <= 0 or args.warmup < 2 or args.repeats < 1:
        raise ValueError("frames/repeats must be positive and warmup >= 2")
    if not args.smoke and (args.frames != 1800 or args.warmup != 3600 or args.repeats != 3):
        raise ValueError("Formal baseline requires 3600 warmup, 1800 frames and exactly 3 independent runs")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    shutil.copyfile(prepared / "conditions.json", output / "conditions.json")
    if profile == "b3":
        for name in ("ibl-manifest.json", "orientation-fixture.json"):
            shutil.copyfile(prepared / name, output / name)
    runs = []
    for repetition in range(args.repeats):
        cases = [(culling, timing) for culling in args.culling for timing in (False, True)]
        offset = repetition % len(cases)
        for culling, timing in cases[offset:] + cases[:offset]:
            run_id = f"{culling}-timing-{int(timing)}-{repetition + 1}"
            folder = output / run_id
            record = run_case(prepared, identity, folder, culling=culling, timing=timing,
                              repetition=repetition + 1, frames=args.frames, warmup=args.warmup,
                              validation=args.validation, timeout=args.timeout)
            runs.append(record)
            write(output / "runs.json", {"identity": identity, "smoke": args.smoke, "validation": args.validation, "runs": runs})
            print(run_id, json.dumps(record["summary"], ensure_ascii=False), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("prepare")
    build.add_argument("--editor", type=Path, required=True)
    build.add_argument("--template", type=Path, required=True)
    build.add_argument("--asset-root", type=Path, default=ROOT)
    build.add_argument("--conditions", type=Path, default=ROOT / "tools/bistro/conditions.json")
    build.add_argument("--output", type=Path, required=True)
    build.add_argument("--profile", choices=PROFILES, default="b2")
    build.add_argument("--ibl-manifest", type=Path)
    build.add_argument("--cpu-clock-manifest", type=Path)
    build.set_defaults(func=prepare)
    run = commands.add_parser("run")
    run.add_argument("--prepared", type=Path, required=True)
    run.add_argument("--output", type=Path, required=True)
    run.add_argument("--frames", type=int, default=1800)
    run.add_argument("--warmup", type=int, default=3600)
    run.add_argument("--repeats", type=int, default=3)
    run.add_argument("--culling", nargs="+", choices=("all", "frustum"), default=["all", "frustum"])
    run.add_argument("--timeout", type=int, default=600)
    run.add_argument("--validation", action="store_true")
    run.add_argument("--smoke", action="store_true")
    run.set_defaults(func=measure)
    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
