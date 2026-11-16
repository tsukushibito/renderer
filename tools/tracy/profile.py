"""Plan, capture, export and analyze non-admin, owned Bistro Tracy runs.

Use matched portable Tracy 0.14.1 binaries. READY follows warmup; the private
capture connects to localhost, then GO permits the target to verify connection
and begin its ROI. DONE precedes capture SIGINT/flush; END releases post-ROI
readback and shutdown. Disconnected mode measures the same instrumented bytes
without a collector. No command changes UAC, ETW, kernel or machine settings.
"""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import queue
import re
import socket
import subprocess
import sys
import threading
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/cpu_profile"))
sys.path.insert(0, str(ROOT / "tools/bistro"))
from identity import sha256, verify_manifest
from measure_runtime import validate_report
from windows import hidden_console, owned_listener, require_nonadmin
from zone_analysis import analyze_zones

TRACY_VERSION = "0.14.1"
TRACY_FEATURES = {"enable", "ondemand", "only-localhost"}
DIAGNOSTICS = {"development-validation", "vulkan-diagnostics", "d3d12-diagnostics", "graph-diagnostics", "memory-profiling"}


def utc():
    return datetime.now(timezone.utc).isoformat()


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2, ensure_ascii=False, allow_nan=False) + "\n", encoding="utf-8")


def load(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def file_identity(path):
    path = Path(path).resolve()
    return {"path": str(path), "sha256": sha256(path), "size_bytes": path.stat().st_size}


def verify_file(record):
    if file_identity(record["path"]) != record:
        raise ValueError("Archived input/tool identity changed: " + record["path"])


def check_tool_provenance(path, tools):
    provenance = load(path)
    if provenance.get("tracy_version") != TRACY_VERSION or provenance.get("token_elevation") != 0:
        raise ValueError("Tool provenance must identify matched non-admin Tracy " + TRACY_VERSION)
    for role in ("capture", "csvexport"):
        recorded = provenance.get("tools", {}).get("tracy-" + role, {})
        if (recorded.get("sha256") != tools[role]["sha256"]
                or Path(recorded.get("path", "")).resolve() != Path(tools[role]["path"])):
            raise ValueError("Tool executable differs from archived bundle provenance")
    archive = provenance.get("archive", {})
    if not archive.get("path") or sha256(archive["path"]) != archive.get("sha256"):
        raise ValueError("Portable archive bytes differ from bundle provenance")
    return file_identity(archive["path"])


def check_instrumented(manifest):
    build = manifest["build"]
    features = set(build.get("effective_features", []))
    if (build.get("instrumentation") != "tracy" or build.get("artifact_class") != "tracy-instrumented"
            or "tracy" not in features or build.get("example") != "bistro"):
        raise ValueError("Tracy requires an explicitly archived instrumented Bistro build")
    if build.get("profile") not in ("release", "profiling") or features & DIAGNOSTICS:
        raise ValueError("Tracy requires release-derived optimization without validation diagnostics")
    if not {"vulkan", "d3d12"}.issubset(features):
        raise ValueError("Use one instrumented artifact containing both APIs")
    dependencies = build.get("instrumentation_dependencies", [])
    if {item["target"] for item in dependencies} != {"tracy_client", "tracy_client_sys"}:
        raise ValueError("Missing actual Tracy dependency feature proof")
    for item in dependencies:
        if set(item.get("features", [])) != TRACY_FEATURES:
            raise ValueError("Tracy dependency enables unsupported collection features")
        version = "0.19.0" if item["target"] == "tracy_client" else "0.30.1"
        if not item.get("package_id", "").endswith("@" + version):
            raise ValueError("Tracy dependency version differs from the matched 0.14.1 tool contract")
    source = manifest.get("git", {})
    if (source.get("dirty_status") != "" or not re.fullmatch(r"[a-f0-9]{40}", source.get("commit", ""))
            or not source.get("source_hashes")):
        raise ValueError("Tracy measurements require committed, identified build sources")


def make_plan(args):
    for name in ("frames", "warmup", "width", "height", "inflight"):
        value = getattr(args, name)
        if type(value) is not int or value <= 0:
            raise ValueError(name + " must be a positive integer")
    if not 0 < args.timeout_seconds <= 3600 or not 0 < args.flush_timeout_seconds <= 120:
        raise ValueError("Timeout must be (0,3600]; flush timeout (0,120]")
    if not 16 <= args.max_output_mib <= 65536:
        raise ValueError("max-output-mib must be [16,65536]")
    manifest_path = Path(args.build_manifest).resolve()
    manifest = verify_manifest(manifest_path)
    check_instrumented(manifest)
    output = Path(args.output).resolve()
    if output.exists():
        raise ValueError("Output must be a new directory; existing artifacts are never overwritten")
    binary = (manifest_path.parent / manifest["binary"]["path"]).resolve()
    command = [str(binary), "--api", args.api, "--view", args.view, "--width", str(args.width),
               "--height", str(args.height), "--frames", str(args.frames), "--warmup", str(args.warmup),
               "--inflight", str(args.inflight), "--output", str(output / "target")]
    inputs = {"conditions": file_identity(ROOT / "tools/bistro/conditions.json")}
    if args.fixture:
        command += ["--fixture"]
    else:
        inputs["cook_manifest"] = file_identity(args.manifest)
        command += ["--manifest", inputs["cook_manifest"]["path"]]
    tools = {}
    if args.mode == "connected":
        if not args.capture_exe or not args.csvexport_exe:
            raise ValueError("Connected capture requires explicit matched capture and csvexport executables")
        tools = {"capture": file_identity(args.capture_exe), "csvexport": file_identity(args.csvexport_exe)}
        command += ["--capture-handshake"]
    if args.tool_provenance:
        inputs["tool_provenance"] = file_identity(args.tool_provenance)
        if tools:
            inputs["tool_archive"] = check_tool_provenance(args.tool_provenance, tools)
    return {"schema_version": 1, "created_utc": utc(), "mode": args.mode, "api": args.api,
            "output": str(output), "cwd": str(Path(args.cwd).resolve()),
            "build_manifest": file_identity(manifest_path), "binary": manifest["binary"], "pdb": manifest["pdb"],
            "source": manifest["git"], "build": manifest["build"], "inputs": inputs, "tools": tools,
            "tracy_version": TRACY_VERSION, "command": command, "frames": args.frames,
            "warmup": args.warmup, "width": args.width, "height": args.height, "inflight": args.inflight,
            "fixture": args.fixture, "view": "fixture" if args.fixture else args.view,
            "timeout_seconds": args.timeout_seconds, "flush_timeout_seconds": args.flush_timeout_seconds,
            "max_output_bytes": args.max_output_mib * 1024 * 1024,
            "quota_scope": "Polling guard over owned output files; collector in-memory buffers and a single native write may transiently exceed the file quota. No preexisting outputs are reused.",
            "capture_command": ([tools["capture"]["path"], "-a", "127.0.0.1", "-p", "8086", "-o", str(output / "capture.tracy")] if tools else None),
            "target_environment": {"TRACY_PORT": "8086", "RENDERER_TRACY_CAPTURE_ID": uuid.uuid4().hex},
            "contract": "Instrumented elapsed wall zones only; not normal-release results, CPU samples or scheduler attribution. Serial owned process, localhost only, hidden private capture console, verified non-elevated tokens, no system settings changes."}


def verify_plan(plan):
    verify_file(plan["build_manifest"])
    manifest = verify_manifest(plan["build_manifest"]["path"])
    check_instrumented(manifest)
    if manifest["binary"] != plan["binary"] or manifest["pdb"] != plan["pdb"]:
        raise ValueError("Build identity differs from plan")
    for record in [*plan["inputs"].values(), *plan["tools"].values()]:
        verify_file(record)


def drain(process, path, events, prefixes):
    try:
        with Path(path).open("w", encoding="utf-8", newline="") as log:
            while line := process.stdout.readline(1024 * 1024 + 1):
                if len(line) > 1024 * 1024:
                    raise ValueError("Protocol/log line exceeds 1 MiB")
                log.write(line)
                log.flush()
                for prefix in prefixes:
                    if line.startswith(prefix + " "):
                        events.put_nowait((prefix, json.loads(line[len(prefix) + 1:])))
    except Exception as error:
        events.put_nowait(("ERROR", str(error)))
    finally:
        process.stdout.close()


def wait_event(process, events, expected, guard):
    while True:
        guard()
        try:
            kind, payload = events.get(timeout=0.05)
        except queue.Empty:
            if process.poll() is not None:
                # Drain can deliver a final line immediately after process exit.
                try:
                    kind, payload = events.get(timeout=0.2)
                except queue.Empty as error:
                    raise RuntimeError("Process exited before " + expected) from error
            else:
                continue
        if kind != expected or not isinstance(payload, dict) or payload.get("pid") != process.pid:
            raise ValueError(f"Expected owned {expected}, received {kind}: {payload}")
        return payload


def send(process, value):
    process.stdin.write(value + "\n")
    process.stdin.flush()


def stop_owned(process):
    if process is not None and process.poll() is None:
        process.kill()
        process.wait(timeout=10)


def checked_handshake(payload, plan, *, ready):
    if payload.get("api") != plan["api"] or payload.get("measured_frames") != plan["frames"]:
        raise ValueError("Handshake workload/ROI differs from plan")
    if payload.get("tracy_connected") is not (not ready):
        raise ValueError("Unexpected Tracy connection at handshake boundary")
    if payload.get("capture_id") != plan["target_environment"]["RENDERER_TRACY_CAPTURE_ID"]:
        raise ValueError("Handshake does not identify the exact planned run")
    if ready:
        if payload.get("warmup_done") is not True or payload.get("warmup_frames") != plan["warmup"]:
            raise ValueError("Target did not establish requested warmup")
        if payload.get("conditions_sha256") != plan["inputs"]["conditions"]["sha256"]:
            raise ValueError("Target embedded conditions differ from the planned source")
        if not plan["fixture"] and payload.get("cook_manifest_sha256") != plan["inputs"]["cook_manifest"]["sha256"]:
            raise ValueError("Target cook input differs from plan")


def require_available_local_port():
    # A known occupied port is refused before the target/collector is launched.
    # The target's own disconnected->connected handshake also prevents accepting
    # a trace if a different process wins the port after this observation.
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        if sys.platform == "win32":
            probe.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
        probe.bind(("127.0.0.1", 8086))


def capture(plan):
    if sys.platform != "win32":
        raise ValueError("Capture requires Windows; analysis and planning are portable")
    verify_plan(plan)
    output = Path(plan["output"])
    output.mkdir(parents=True, exist_ok=False)
    write_json(output / "plan.json", plan)
    result = {"schema_version": 1, "status": "running", "capture_completed": False,
              "trace_content_verified": False, "plan": plan,
              "started_utc": utc(), "tokens": {}, "events": {}}
    write_json(output / "capture.json", result)
    target = broker = None
    readers = []
    started = time.monotonic()
    deadline = started + plan["timeout_seconds"]

    def guard():
        if time.monotonic() > deadline:
            raise TimeoutError("Owned Tracy run exceeded its bounded timeout")
        size = sum(path.stat().st_size for path in output.rglob("*") if path.is_file())
        if size > plan["max_output_bytes"]:
            raise ValueError("Owned Tracy output exceeded its bounded quota")

    def watch(process, name, prefixes):
        events = queue.Queue(maxsize=16)
        reader = threading.Thread(target=drain, args=(process, output / (name + ".stdout.txt"), events, prefixes), daemon=True)
        readers.append(reader)
        reader.start()
        return events

    try:
        result["tokens"]["controller"] = require_nonadmin()
        if plan["mode"] == "connected":
            result["capture_help"] = run_logged([plan["tools"]["capture"]["path"]], output, "capture-help", 15, (1,))
            help_text = (output / "capture-help.stdout.txt").read_text(encoding="utf-8", errors="replace")
            if "tracy-capture " + TRACY_VERSION not in help_text:
                raise ValueError("Capture help does not identify matched Tracy " + TRACY_VERSION)
        require_available_local_port()
        # Start no instrumented target before proving caller is non-elevated.
        with (output / "target.stderr.txt").open("wb") as stderr:
            target = subprocess.Popen(plan["command"], cwd=plan["cwd"], stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE, stderr=stderr, text=True, encoding="utf-8", errors="replace",
                                      env=dict(os.environ, **plan["target_environment"]),
                                      creationflags=subprocess.CREATE_NO_WINDOW)
            result["tokens"]["target"] = require_nonadmin(target.pid, handle=target._handle)
            target_events = watch(target, "target", ("BISTRO_READY", "BISTRO_DONE"))
            if plan["mode"] == "connected":
                ready = wait_event(target, target_events, "BISTRO_READY", guard)
                checked_handshake(ready, plan, ready=True)
                result["events"]["ready"] = ready
                result["listener"] = owned_listener(target.pid)
                specification = {"command": plan["capture_command"], "cwd": plan["cwd"],
                                 "stdout": str(output / "collector.stdout.txt"), "stderr": str(output / "collector.stderr.txt"),
                                 "readiness_timeout_seconds": 30,
                                 "flush_timeout_seconds": plan["flush_timeout_seconds"]}
                write_json(output / "broker-spec.json", specification)
                with (output / "broker.stderr.txt").open("wb") as broker_stderr:
                    broker = subprocess.Popen([sys.executable, str(Path(__file__).with_name("windows.py")), str(output / "broker-spec.json")],
                        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=broker_stderr, text=True, encoding="utf-8", **hidden_console())
                    broker_events = watch(broker, "broker", ("BROKER_READY", "BROKER_DONE"))
                    started_broker = wait_event(broker, broker_events, "BROKER_READY", guard)
                    result["tokens"].update({"capture": started_broker["capture"], "broker": started_broker["broker"]})
                    send(target, "GO")
                    done = wait_event(target, target_events, "BISTRO_DONE", guard)
                    checked_handshake(done, plan, ready=False)
                    result["events"]["done"] = done
                    send(broker, "STOP")
                    stopped = wait_event(broker, broker_events, "BROKER_DONE", guard)
                    if (stopped.get("returncode") != 0 or stopped.get("sigint_handler_ready_observed") is not True
                            or broker.wait(timeout=10) != 0):
                        raise RuntimeError("Capture failed to stop and flush")
                    broker.stdin.close()
                    broker = None
                    trace = file_identity(output / "capture.tracy")
                    if trace["size_bytes"] < 16:
                        raise ValueError("Capture did not produce a complete nonempty trace")
                    result["trace"] = trace
                    result["events"]["capture_flushed_utc"] = utc()
                    result["events"]["capture_sigint_handler_ready_observed"] = True
                    result["collector_stdout_scope"] = "Rendered snapshots of the owned hidden console; real console stdout exposes Tracy's post-SIGINT-handler progress marker. No unrelated console is read."
                    send(target, "END")
            while target.poll() is None:
                guard()
                time.sleep(0.05)
            if target.wait() != 0:
                raise RuntimeError("Bistro target failed; see retained logs")
            report_path = output / "target/report.json"
            report = load(report_path)
            expected_connected = plan["mode"] == "connected"
            tracing = report.get("tracy", {})
            if (tracing.get("enabled") is not True
                    or tracing.get("connected_at_measurement_start") is not expected_connected
                    or tracing.get("connected_at_measurement_end") is not expected_connected
                    or tracing.get("capture_handshake") is not expected_connected
                    or tracing.get("capture_id") != plan["target_environment"]["RENDERER_TRACY_CAPTURE_ID"]):
                raise ValueError("Target Tracy connection boundaries differ from requested mode")
            result["tracy"] = tracing
            result["connection_observation_scope"] = "Target observed connection state at ROI start/end; disconnected mode launches no collector. No per-frame connection polling, so boundary observations do not prove absence of an unrelated transient collector during the ROI."
            result["runner_metrics"] = validate_report(report, {"api": plan["api"], "gpu_timing": "off"}, plan)
            result["report"] = file_identity(report_path)
            result["identity"] = report["identity"]
            if plan["mode"] == "connected" and result["events"]["done"]["measurement"] != report["measurement"]:
                raise ValueError("DONE measurement changed during post-ROI output")
            if "panicked at" in (output / "target.stderr.txt").read_text(encoding="utf-8", errors="replace"):
                raise ValueError("Target emitted a Rust panic")
            result.update(status="passed", capture_completed=plan["mode"] == "connected")
    except BaseException as error:
        result.update(status="failed", failure=f"{type(error).__name__}: {error}")
        raise
    finally:
        cleanup_errors = []
        def clean(label, operation):
            try:
                operation()
            except Exception as error:
                cleanup_errors.append(f"{label}: {type(error).__name__}: {error}")
        if broker is not None:
            # EOF asks the broker to kill its owned capture before exiting.
            clean("close broker input", broker.stdin.close)
            try:
                broker.wait(timeout=15)
            except (OSError, subprocess.TimeoutExpired) as error:
                clean("terminate broker and its capture job", lambda: stop_owned(broker))
                if broker.poll() is None:
                    cleanup_errors.append(f"broker completion unknown: {error}")
        clean("terminate target", lambda: stop_owned(target))
        if target is not None and target.stdin:
            clean("close target input", target.stdin.close)
        for reader in readers:
            reader.join(timeout=2)
        previously_passed = result["status"] == "passed"
        if cleanup_errors:
            result.update(status="failed", capture_completed=False, cleanup_errors=cleanup_errors)
        result["finished_utc"] = utc()
        result["controller_wall_seconds"] = time.monotonic() - started
        write_json(output / "capture.json", result)
        if cleanup_errors and previously_passed:
            raise RuntimeError("Owned process cleanup failed; see retained capture.json")
    return result


def run_logged(command, output, name, timeout, expected_codes=(0,), *, stage=None,
               max_output_bytes=2 * 1024 * 1024 * 1024):
    stdout, stderr = output / (name + ".stdout.txt"), output / (name + ".stderr.txt")
    with stdout.open("wb") as out, stderr.open("wb") as err:
        process = subprocess.Popen(command, stdout=out, stderr=err, **hidden_console())
        try:
            evidence = require_nonadmin(process.pid, handle=process._handle)
            if stage is not None:
                stage.update(token=evidence, pid=process.pid)
            deadline = time.monotonic() + timeout
            while process.poll() is None:
                if time.monotonic() > deadline:
                    raise subprocess.TimeoutExpired(command, timeout)
                if stdout.stat().st_size + stderr.stat().st_size > max_output_bytes:
                    raise ValueError("Native export/help output exceeded the per-stage polling quota")
                try:
                    process.wait(timeout=0.05)
                except subprocess.TimeoutExpired:
                    pass
            code = process.wait()
            if stdout.stat().st_size + stderr.stat().st_size > max_output_bytes:
                raise ValueError("Native export/help output exceeded the per-stage polling quota")
        finally:
            stop_owned(process)
    if code not in expected_codes:
        raise RuntimeError(f"Tool {name} exited {code}; see logs")
    return {"command": command, "returncode": code, "token": evidence,
            "stdout": file_identity(stdout), "stderr": file_identity(stderr)}


def export_stage(result, command, output, name, timeout):
    stage = {"command": command, "status": "running", "started_utc": utc()}
    result["stages"][name] = stage
    try:
        stage.update(run_logged(command, output, name, timeout, stage=stage), status="passed")
    except BaseException as error:
        stage.update(status="failed", error=f"{type(error).__name__}: {error}")
        for role in ("stdout", "stderr"):
            path = output / (name + "." + role + ".txt")
            if path.exists():
                stage[role] = file_identity(path)
        raise
    finally:
        stage["finished_utc"] = utc()


def export(args):
    if sys.platform != "win32":
        raise ValueError("Portable Windows export requires Windows")
    if not 0 < args.timeout_seconds <= 3600:
        raise ValueError("Export timeout must be (0,3600]")
    record = load(args.capture)
    if record.get("status") != "passed" or record.get("capture_completed") is not True:
        raise ValueError("Export requires a completed valid connected capture")
    plan = record["plan"]
    verify_plan(plan)
    verify_file(record["trace"])
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    result = {"schema_version": 1, "status": "running", "capture": file_identity(args.capture),
              "trace": record["trace"], "plan": plan, "stages": {}, "started_utc": utc(), "export_valid": False,
              "source_contract": "https://github.com/wolfpld/tracy/blob/v0.14.1/csvexport/src/csvexport.cpp"}
    try:
        result["controller_token"] = require_nonadmin()
        exe = plan["tools"]["csvexport"]["path"]
        export_stage(result, [exe, "--help"], output, "help", args.timeout_seconds)
        help_text = (output / "help.stdout.txt").read_text(encoding="utf-8", errors="replace") + (output / "help.stderr.txt").read_text(encoding="utf-8", errors="replace")
        if "tracy-csvexport " + TRACY_VERSION not in help_text or "--self" not in help_text or "--unwrap" not in help_text:
            raise ValueError("Actual csvexport help differs from matched pinned tool semantics")
        for name, flags in (("inclusive", []), ("self", ["-e"])):
            export_stage(result, [exe, "-u", "-s", "\t", *flags, record["trace"]["path"]], output, name, args.timeout_seconds)
        result["analysis"] = analyze_zones(output / "inclusive.stdout.txt", output / "self.stdout.txt", plan["frames"],
                                          plan["target_environment"]["RENDERER_TRACY_CAPTURE_ID"])
        result.update(status="passed", export_valid=True, trace_content_verified=True)
    except BaseException as error:
        result.update(status="failed", failure=f"{type(error).__name__}: {error}")
        raise
    finally:
        result["finished_utc"] = utc()
        write_json(output / "export.json", result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="action", required=True)
    for action in ("plan", "capture"):
        command = subparsers.add_parser(action)
        command.add_argument("--build-manifest", type=Path, required=True)
        command.add_argument("--capture-exe", type=Path)
        command.add_argument("--csvexport-exe", type=Path)
        command.add_argument("--tool-provenance", type=Path)
        command.add_argument("--mode", choices=("connected", "disconnected"), default="connected")
        command.add_argument("--api", choices=("vulkan", "d3d12"), required=True)
        assets = command.add_mutually_exclusive_group(required=True)
        assets.add_argument("--manifest", type=Path)
        assets.add_argument("--fixture", action="store_true")
        command.add_argument("--view", default="route")
        for name, default in (("frames", 1800), ("warmup", 3600), ("width", 1920), ("height", 1080), ("inflight", 2)):
            command.add_argument("--" + name, type=int, default=default)
        command.add_argument("--timeout-seconds", type=float, default=300)
        command.add_argument("--flush-timeout-seconds", type=float, default=120)
        command.add_argument("--max-output-mib", type=int, default=2048)
        command.add_argument("--cwd", type=Path, default=ROOT)
        command.add_argument("--output", type=Path, required=True)
    command = subparsers.add_parser("export")
    command.add_argument("--capture", type=Path, required=True)
    command.add_argument("--output", type=Path, required=True)
    command.add_argument("--timeout-seconds", type=float, default=120)
    command = subparsers.add_parser("analyze")
    command.add_argument("--export", dest="export_path", type=Path, required=True)
    command.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.action in ("plan", "capture"):
            plan = make_plan(args)
            result = plan if args.action == "plan" else capture(plan)
        elif args.action == "export":
            result = export(args)
        else:
            record = load(args.export_path)
            if record.get("export_valid") is not True:
                raise ValueError("Analysis requires a successful identified export")
            for stage in ("inclusive", "self"):
                verify_file(record["stages"][stage]["stdout"])
            result = analyze_zones(record["stages"]["inclusive"]["stdout"]["path"], record["stages"]["self"]["stdout"]["path"], record["plan"]["frames"],
                                   record["plan"]["target_environment"]["RENDERER_TRACY_CAPTURE_ID"])
            if args.output.exists():
                raise ValueError("Analysis output must be new")
            write_json(args.output, result)
        print(json.dumps(result if args.action == "plan" else {"status": result.get("status", "passed"), "action": args.action}, indent=2))
    except (ValueError, OSError, KeyError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(f"Tracy {args.action} failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
