"""Plan or run the serial B3 Renderer/Godot comparison after image acceptance.

This does not implement a quality-equivalence test. Renderer Neutral/offscreen
and Godot native AgX/presentation/prefilter/AA differences remain explicit.
Strict GPU correctness and zero-allocation acceptance use separate artifacts.
"""
import argparse
import ctypes
from ctypes import wintypes
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import time

from compare_runtime import digest, load_capture, load_json, compare_reports
import measure_runtime as renderer_measure


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from tools.godot_bistro import benchmark as godot
from tools.godot_bistro import summarize as godot_summary


PHASES = renderer_measure.PHASES
VIEWS = {"overview", "street", "material-detail", "glass-detail", "label-detail", "street-mid", "street-end"}
WINDOW = {"frames": 1800, "warmup": 3600, "inflight": 2, "width": 1920, "height": 1080, "view": "route"}
SCOPE = (
    "B3 whole-renderer reference on one machine, not isolated RenderGraph overhead or pixel parity. "
    "Both B3 profiles explicitly disable the packed source AO channel while preserving source bytes/materials. "
    "Renderer: Neutral, post-tonemap FXAA, consumer IBL cook, offscreen. Godot: native AgX, "
    "pre-tonemap FXAA, RGBE-quantized panorama and native BRDF/prefilter/PCF/depth-prepass/batching/presentation. "
    "Godot all-culling-disabled is the primary reference; frustum culling is supplementary. "
    "Both full-scene lighting profiles disable source AO contribution following the reference default; "
    "all ORM pixels and material references are retained. "
    "CPU active host elapsed excludes explicit yield waits but includes polling and possible OS descheduling; "
    "charged process/main-thread CPU and wall/N remain separate. Godot CPU is setup+viewport and excludes "
    "script/physics. Timestamp/query and native CPU timer observations are opt-in and may affect cost. "
    "Iteration intervals include pipeline backpressure from an earlier submitted frame; per-frame "
    "build-to-observed-completion latency is never converted to FPS. Neither engine measures scanout/input latency. "
    "GPU correctness, final resource release, zero allocation and B5 window/interaction acceptance are separate."
)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def write(path, value):
    Path(path).write_text(json.dumps(value, ensure_ascii=False, allow_nan=False, indent=2) + "\n", encoding="utf-8")


def now():
    return datetime.now(timezone.utc).isoformat()


def ordinary_host_token(pid=None):
    """Use the existing host-evidence token classes 20/29 for this exact PID.

    Matches tools/tracy/windows.py and the checked cpu-opt host_token helper,
    with one limited-information process handle shared by both queries.
    """
    require(sys.platform == "win32", "Formal comparison requires a Windows ordinary host")
    pid = pid or os.getpid()
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    security = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    security.OpenProcessToken.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.POINTER(wintypes.HANDLE)]
    security.GetTokenInformation.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p,
                                           wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
    process = kernel.OpenProcess(0x1000, False, pid)
    if not process:
        raise ctypes.WinError(ctypes.get_last_error())
    token = wintypes.HANDLE()
    try:
        if not security.OpenProcessToken(process, 8, ctypes.byref(token)):
            raise ctypes.WinError(ctypes.get_last_error())
        result = {"pid": pid, "supported": True}
        for field, information_class in (("token_elevation", 20), ("token_is_app_container", 29)):
            value, returned = wintypes.DWORD(), wintypes.DWORD()
            if not security.GetTokenInformation(token, information_class, ctypes.byref(value),
                                                ctypes.sizeof(value), ctypes.byref(returned)):
                raise ctypes.WinError(ctypes.get_last_error())
            require(returned.value == ctypes.sizeof(value), "Unexpected process token record size")
            result[field] = value.value
        require(result["token_elevation"] == 0 and result["token_is_app_container"] == 0,
                "Controller and targets require TokenElevation=0 and TokenIsAppContainer=0")
        return result
    finally:
        if token:
            kernel.CloseHandle(token)
        kernel.CloseHandle(process)


def schedule():
    cases = [("renderer", "vulkan", False), ("godot", "all", False),
             ("renderer", "d3d12", False), ("godot", "frustum", False),
             ("renderer", "vulkan", True), ("godot", "all", True),
             ("renderer", "d3d12", True), ("godot", "frustum", True)]
    result = []
    for repetition in (1, 2, 3):
        offset = repetition - 1
        for engine, variant, timing in cases[offset:] + cases[:offset]:
            run_id = (f"{variant}-gpu-{'on' if timing else 'off'}-{repetition}" if engine == "renderer"
                      else f"{variant}-timing-{int(timing)}-{repetition}")
            result.append({"engine": engine, "variant": variant, "timing": timing,
                           "repetition": repetition, "run_id": run_id})
    return result


def describe(path):
    path = Path(path).resolve()
    return {"path": str(path), "sha256": digest(path), "byte_length": path.stat().st_size}


def cook_inputs(manifest_path, records):
    root = Path(manifest_path).resolve().parent
    result = []
    for record in records:
        relative = Path(record["path"])
        path = (root / relative).resolve()
        require(not relative.is_absolute() and path.is_relative_to(root), "Cook input escapes its manifest directory")
        actual = describe(path)
        require(actual["sha256"] == record["sha256"] and actual["byte_length"] == record["byte_length"],
                "Cook payload differs from its manifest")
        result.append(actual)
    return result


def make_plan(args):
    output = args.output.resolve()
    require(not output.exists(), "Comparison output must be a new directory")
    require(0 < args.timeout <= 3600, "Timeout must be in (0,3600] seconds")
    build, binary = renderer_measure.verified_release(args.build_manifest)
    prepared = args.godot_prepared.resolve()
    godot_identity = godot.verify_prepared(prepared)
    require(godot_identity["profile"] == "b3", "Comparison requires an explicit Godot B3 profile")
    require(build["git"]["commit"] == godot_identity["source_commit"],
            "Prepare both engines from the same committed source")
    manifest, ibl_manifest = load_json(args.manifest), load_json(args.ibl_manifest)
    require([len(manifest[name]) for name in ("meshes", "primitives", "materials", "images")]
            == [1296, 1591, 132, 405], "Require the complete B2/B3 Bistro cook")
    require(manifest["indices"]["count"] == 8496360, "Bistro triangle inventory differs")
    require(digest(args.ibl_manifest) == godot_identity["ibl"]["manifest_sha256"],
            "Renderer/Godot must use the same IBL cook manifest")
    source_gltf = [entry for entry in godot_identity["inputs"] if entry["path"].endswith("BistroExterior.gltf")]
    require(len(source_gltf) == 1 and manifest["source"]["gltf_sha256"] == source_gltf[0]["sha256"],
            "Renderer/Godot source scene identity differs")
    inputs = [describe(path) for path in (args.build_manifest, binary,
        args.build_manifest.resolve().parent / build["pdb"]["path"], args.manifest, args.ibl_manifest,
        prepared / "identity.json", prepared / "conditions.json", prepared / "bistro-reference.exe",
        prepared / "bistro-reference.pck", prepared / "godot-panorama.hdr",
        prepared / "orientation-fixture.json", Path(__file__).resolve(),
        ROOT / "tools/bistro/measure_runtime.py", ROOT / "tools/bistro/compare_runtime.py",
        ROOT / "tools/cpu_profile/identity.py",
        ROOT / "tools/godot_bistro/benchmark.py", ROOT / "tools/godot_bistro/profiles.py",
        ROOT / "tools/godot_bistro/summarize.py")]
    inputs.append(describe(ROOT / "tools/godot_bistro/repeat.py"))
    inputs += cook_inputs(args.manifest, [manifest["vertices"], manifest["indices"], *manifest["images"]])
    inputs += cook_inputs(args.ibl_manifest, list(ibl_manifest["images"].values()))
    inputs += [{"path": str(Path(godot_identity["asset_root"]) / record["path"]),
                "sha256": record["sha256"], "byte_length": record["size"]}
               for record in godot_identity["inputs"]]
    runs = schedule()
    for run in runs:
        if run["engine"] == "renderer":
            command = [str(binary), "--api", run["variant"], "--manifest", str(args.manifest.resolve()),
                       "--ibl-manifest", str(args.ibl_manifest.resolve()), "--view", "route", "--width", "1920",
                       "--height", "1080", "--warmup", "3600", "--frames", "1800", "--inflight", "2",
                       "--capture-all-views", "--output", str(output / "renderer" / run["run_id"])]
            if run["timing"]:
                command.append("--gpu-timing")
            run["command"] = command
    return {"schema_version": 1, "created_utc": now(), "output": str(output), "cwd": str(args.cwd.resolve()),
            "godot_prepared": str(prepared), "godot_identity": godot_identity,
            "renderer_build": build, "binary": str(binary), "inputs": inputs,
            "cook_manifest_sha256": digest(args.manifest), "ibl_manifest_sha256": digest(args.ibl_manifest),
            "conditions_sha256": godot_identity["conditions_sha256"], "window": WINDOW,
            "timeout_seconds": args.timeout, "runs": runs, "scope": SCOPE}


def distribution(values):
    return {"mean": statistics.mean(values), "p50": renderer_measure.percentile(values, .5),
            "p95": renderer_measure.percentile(values, .95), "p99": renderer_measure.percentile(values, .99),
            "minimum": min(values), "maximum": max(values)}


def renderer_metrics(report, condition, window):
    mode = {"api": condition["variant"], "gpu_timing": "on" if condition["timing"] else "off"}
    legacy = renderer_measure.validate_report(report, mode, window)
    samples = report["measurement"].get("frame_samples")
    require(isinstance(samples, list) and len(samples) == window["frames"], "Complete raw Renderer samples are required")
    values = {name + "_ms": [] for name in PHASES}
    values.update({key: [] for key in ("poll_calls_ms", "yield_ms", "active_host_ms", "iteration_ms")})
    if condition["timing"]:
        values["gpu_ms"] = []
    for index, sample in enumerate(samples):
        require(type(sample["frame"]) is int and sample["frame"] == index, "Renderer raw frame identity is discontinuous")
        phases = sample["phase_ns"]
        require(isinstance(phases, list) and len(phases) == 5, "Renderer raw phase array must have five entries")
        for value in phases:
            renderer_measure._integer(value, "phase_ns")
        for key in ("poll_calls_elapsed_ns", "yield_elapsed_ns", "active_host_elapsed_ns", "iteration_wall_ns"):
            renderer_measure._integer(sample[key], key)
        active = sum(phases[:3]) + sample["poll_calls_elapsed_ns"] + phases[4]
        require(sample["active_host_elapsed_ns"] == active, "Active host elapsed includes missing or overlapping work")
        require(sample["poll_calls_elapsed_ns"] + sample["yield_elapsed_ns"] <= phases[3],
                "Poll and yield accounting exceeds the containing retire phase")
        for name, value in zip(PHASES, phases):
            values[name + "_ms"].append(value / 1e6)
        for metric, key in (("poll_calls_ms", "poll_calls_elapsed_ns"), ("yield_ms", "yield_elapsed_ns"),
                            ("active_host_ms", "active_host_elapsed_ns"), ("iteration_ms", "iteration_wall_ns")):
            values[metric].append(sample[key] / 1e6)
        if condition["timing"]:
            renderer_measure._integer(sample["gpu_ns"], "gpu_ns", positive=True)
            values["gpu_ms"].append(sample["gpu_ns"] / 1e6)
        else:
            require("gpu_ns" in sample and sample["gpu_ns"] is None, "Timing-disabled samples must have null GPU time")
    for phase, name in enumerate(PHASES):
        require(sum(sample["phase_ns"][phase] for sample in samples) == report["measurement"]["phase_totals_ns"][name],
                "Raw Renderer phases do not reproduce reported totals")
    if condition["timing"]:
        require(sum(sample["gpu_ns"] for sample in samples) == report["measurement"]["gpu"]["total_ns"],
                "Raw GPU samples do not reproduce the reported total")
        require(min(sample["gpu_ns"] for sample in samples) == report["measurement"]["gpu"]["min_ns"]
                and max(sample["gpu_ns"] for sample in samples) == report["measurement"]["gpu"]["max_ns"],
                "Raw GPU extrema do not reproduce the report")
    scalars = {"wall_ms_per_frame": legacy["wall_ns_per_frame"] / 1e6,
               "process_charged_cpu_ms_per_frame": legacy["process_cpu_ns_per_frame"] / 1e6,
               "main_thread_charged_cpu_ms_per_frame": legacy["main_thread_cpu_ns_per_frame"] / 1e6,
               "offscreen_throughput_fps": legacy["throughput_frames_per_second"]}
    return {"distributions": {name: distribution(data) for name, data in values.items()}, "scalars": scalars,
            "build_to_observed_completion_latency": legacy["individual_frame_wall_ns"]}


def aggregate_renderer(runs):
    result = {}
    for api in ("vulkan", "d3d12"):
        for timing in (False, True):
            selected = [run for run in runs if run["variant"] == api and run["timing"] is timing]
            require(len(selected) == 3 and {run["repetition"] for run in selected} == {1, 2, 3},
                    "Renderer groups require exactly three independent processes")
            grouped = {}
            for metric in selected[0]["metrics"]["distributions"]:
                grouped[metric] = {}
                for statistic in ("mean", "p50", "p95", "p99", "minimum", "maximum"):
                    data = [run["metrics"]["distributions"][metric][statistic] for run in selected]
                    grouped[metric][statistic] = {"run_values": data, "run_median": statistics.median(data),
                                                 "run_min": min(data), "run_max": max(data)}
            scalars = {}
            for metric in selected[0]["metrics"]["scalars"]:
                data = [run["metrics"]["scalars"][metric] for run in selected]
                scalars[metric] = {"run_values": data, "run_median": statistics.median(data),
                                   "run_min": min(data), "run_max": max(data)}
            result[f"{api}/timing-{int(timing)}"] = {"distributions": grouped, "scalars": scalars}
    return result


def reference_ratios(renderer_groups, godot_groups):
    """Descriptive p95 ratios; CPU scopes differ and this is not Graph overhead."""
    reference = godot_groups["all-timing-1"]
    result = {}
    for api in ("vulkan", "d3d12"):
        measured = renderer_groups[f"{api}/timing-1"]["distributions"]
        result[api] = {}
        for name, renderer_metric, godot_metric in (("cpu_elapsed_scope_comparison", "active_host_ms", "cpu_render_ms"),
                                                    ("gpu", "gpu_ms", "gpu_ms")):
            baseline = reference[godot_metric]["p95"]["run_max"]
            observed = measured[renderer_metric]["p95"]["run_max"]
            result[api][name] = {"godot_slowest_run_p95_ms": baseline,
                                 "renderer_slowest_run_p95_ms": observed,
                                 "renderer_over_godot": observed / baseline}
    return result


def captures(path, report):
    result = {"route0": (Path(path), report)}
    require({entry["view"] for entry in report["views"]} == VIEWS and len(report["views"]) == 7,
            "All seven Renderer fixed-view captures are required")
    for entry in report["views"]:
        view_path = Path(entry["report"]).resolve()
        require(view_path.is_relative_to(Path(path).resolve().parent), "Fixed-view report escapes its run directory")
        result[entry["view"]] = (view_path, load_json(view_path))
    identity = {}
    for name, (capture_path, capture_report) in result.items():
        require(capture_report["identity"]["view"] == ("route" if name == "route0" else name),
                "Fixed-view report identifies a different camera")
        planes = {plane: load_capture(capture_path, capture_report, plane)[1] for plane in ("hdr", "sdr", "depth")}
        require(all([plane["width"], plane["height"]] == [1920, 1080] for plane in planes.values()),
                "Renderer capture resolution differs from the comparison")
        identity[name] = {"report": str(capture_path), "report_sha256": digest(capture_path), "planes": planes}
    return identity


def verify_inputs(inputs):
    for entry in inputs:
        require(Path(entry["path"]).stat().st_size == entry["byte_length"] and digest(entry["path"]) == entry["sha256"],
                f"Comparison input changed: {entry['path']}")


def verify_renderer_outputs(record):
    require(digest(record["report"]) == record["report_sha256"], "Renderer report changed after collection")
    for capture in record["captures"].values():
        path = capture["report"]
        require(digest(path) == capture["report_sha256"], "Renderer capture report changed after collection")
        report = load_json(path)
        for plane, expected in capture["planes"].items():
            require(load_capture(path, report, plane)[1] == expected, "Renderer raw capture changed after collection")


def run_renderer(plan, condition, output):
    stdout, stderr = output / "stdout.log", output / "stderr.log"
    started = time.monotonic_ns()
    with stdout.open("wb") as out, stderr.open("wb") as err:
        process = subprocess.Popen(condition["command"], cwd=plan["cwd"], stdout=out, stderr=err,
                                   creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        try:
            token = ordinary_host_token(process.pid)
            code = process.wait(timeout=plan["timeout_seconds"])
        except BaseException:
            if process.poll() is None:
                process.kill()
            process.wait()
            raise
    require(code == 0, f"Renderer {condition['run_id']} failed: {code}")
    require("panicked at" not in stderr.read_text(encoding="utf-8", errors="replace"), "Renderer panic in successful process")
    path = output / "report.json"
    report = load_json(path)
    validate_renderer_scene(report)
    for key, expected in (("cook_manifest_sha256", plan["cook_manifest_sha256"]),
                          ("conditions_sha256", plan["conditions_sha256"])):
        require(report["identity"].get(key) == expected, f"Renderer loaded a different {key}")
    require(report["identity"]["environment"]["manifest_sha256"] == plan["ibl_manifest_sha256"],
            "Renderer loaded a different IBL manifest")
    require(report["identity"].get("shadows") is True and report["identity"].get("aa") is True,
            "Renderer comparison requires shadows and FXAA")
    return {**condition, "pid": process.pid, "token": token, "returncode": code,
            "controller_wall_ns": time.monotonic_ns() - started, "report": str(path),
            "report_sha256": digest(path), "stdout_sha256": digest(stdout), "stderr_sha256": digest(stderr),
            "metrics": renderer_metrics(report, condition, plan["window"]), "captures": captures(path, report)}


def validate_renderer_scene(report):
    scene = report["scene"]
    require((scene["meshes"], scene["primitives"], scene["materials"],
             scene["source"]["image_count"], scene["source"]["triangle_count"])
            == (1296, 1591, 132, 405, 2832120), "Incomplete full Bistro inventory")
    policy = scene["source"].get("applied_material_policy", {})
    require(policy.get("ambient_occlusion_enabled") is False
            and policy.get("source_pixels_and_references_preserved") is True,
            "Renderer must apply the fixed default-disabled Bistro AO profile")
    identity = report["identity"]
    require(identity.get("alpha_only") is False and identity.get("alpha_override") is False,
            "Renderer comparison requires the original material alpha inputs")


def run_plan(plan):
    controller = ordinary_host_token()
    verify_inputs(plan["inputs"])
    output = Path(plan["output"])
    output.mkdir(parents=True, exist_ok=False)
    (output / "renderer").mkdir()
    (output / "godot").mkdir()
    write(output / "plan.json", plan)
    for name in ("conditions.json", "ibl-manifest.json", "orientation-fixture.json"):
        shutil.copyfile(Path(plan["godot_prepared"]) / name, output / "godot" / name)
    summary = {"schema_version": 1, "status": "running", "started_utc": now(), "controller_token": controller,
               "plan_sha256": digest(output / "plan.json"), "scope": SCOPE, "runs": [], "active_condition": None}
    godot_runs = []
    try:
        for condition in plan["runs"]:
            summary["active_condition"] = condition
            write(output / "summary.json", summary)
            folder = output / condition["engine"] / condition["run_id"]
            if condition["engine"] == "godot":
                record = godot.run_case(plan["godot_prepared"], plan["godot_identity"], folder,
                    culling=condition["variant"], timing=condition["timing"], repetition=condition["repetition"],
                    timeout=plan["timeout_seconds"], process_observer=ordinary_host_token)
                godot_runs.append(record)
                write(output / "godot/runs.json", {"identity": plan["godot_identity"], "smoke": False,
                                                    "validation": False, "runs": godot_runs})
                record = {**condition, "pid": record["pid"],
                          "token": record["process_observation"], "result": record}
            else:
                folder.mkdir()
                record = run_renderer(plan, condition, folder)
            summary["runs"].append(record)
            summary["active_condition"] = None
            write(output / "summary.json", summary)
            print(condition["engine"], condition["run_id"], "passed", flush=True)
        verify_inputs(plan["inputs"])
        godot_evidence = godot_summary.assemble(output / "godot")
        require(all(entry["passed"] for entry in godot_evidence["capture_reproducibility"]["comparisons"]),
                "Godot fixed views exceeded the frozen B3 repeat tolerance")
        write(output / "godot-evidence.json", godot_evidence)
        renderer_runs = [run for run in summary["runs"] if run["engine"] == "renderer"]
        for record in renderer_runs:
            verify_renderer_outputs(record)
        for api in ("vulkan", "d3d12"):
            selected = [run for run in renderer_runs if run["variant"] == api]
            reference = selected[0]["captures"]
            for run in selected[1:]:
                for name, capture in run["captures"].items():
                    comparison = compare_reports(reference[name]["report"], capture["report"], profile="repeat")
                    require(comparison["passed"], f"Renderer {api} repeated {name} capture changed")
        summary["renderer_groups"] = aggregate_renderer(renderer_runs)
        summary["godot_groups"] = godot_evidence["groups"]
        summary["descriptive_reference_ratios"] = reference_ratios(summary["renderer_groups"], summary["godot_groups"])
        summary["reference_p95_budgets_ms"] = godot_evidence["reference_p95_budgets_ms"]
        summary["godot_evidence_sha256"] = digest(output / "godot-evidence.json")
        summary["status"] = "passed"
    except BaseException as error:
        summary["status"] = "failed"
        summary["failure"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        summary["finished_utc"] = now()
        write(output / "summary.json", summary)
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("plan", "run"))
    parser.add_argument("--build-manifest", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--ibl-manifest", type=Path, required=True)
    parser.add_argument("--godot-prepared", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cwd", type=Path, default=ROOT)
    parser.add_argument("--timeout", type=float, default=600)
    args = parser.parse_args()
    try:
        plan = make_plan(args)
        if args.action == "plan":
            print(json.dumps(plan, ensure_ascii=False, allow_nan=False, indent=2))
        else:
            result = run_plan(plan)
            print(json.dumps({"status": result["status"], "runs": len(result["runs"]), "output": plan["output"]}))
    except (ValueError, KeyError, OSError) as error:
        print(f"B3 comparison failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
