"""Plan, capture, export and summarize owned Windows CPU traces.

No command elevates privileges or changes OS settings. WPR control always uses
one UUID instance, and cleanup is permitted only after its start succeeds.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import queue
import re
import shutil
import subprocess
import sys
import threading
import time
import uuid

from analyze import analyze_precise, analyze_sampled
from wpa import prepare_profile


ROOT = Path(__file__).resolve().parents[2]
WPT = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Windows Kits/10/Windows Performance Toolkit"
WPR = Path(os.environ.get("SystemRoot", "C:/Windows")) / "System32/wpr.exe"


def utc():
    return datetime.now(timezone.utc).isoformat()


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def sha256(path):
    result = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def verified_build(path):
    # Shared helper rehashes both artifacts and checks the PE/PDB GUID+age.
    from identity import verify_manifest
    manifest = verify_manifest(Path(path).resolve())
    return manifest, (Path(path).resolve().parent / manifest["binary"]["path"]).resolve()


def valid_limits(args):
    if not 0 < args.timeout_seconds <= 3600:
        raise ValueError("--timeout-seconds must be in (0, 3600]")
    if not 16 <= args.max_output_mib <= 65536:
        raise ValueError("--max-output-mib must be in [16, 65536]")


def make_plan(args):
    valid_limits(args)
    manifest, binary = verified_build(args.manifest)
    run_id = args.run_id or datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ_") + uuid.uuid4().hex[:12]
    if not run_id or any(char not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-" for char in run_id):
        raise ValueError("--run-id may contain only ASCII letters, digits, _ and -")
    output = (args.output_root / run_id).resolve()
    instance = "CodexRendererCpu_" + uuid.uuid4().hex
    command = [str(binary), *args.target_args]
    if args.target_args[:1] == ["--"]:
        command = [str(binary), *args.target_args[1:]]
    if args.handshake and "--capture-handshake" not in command:
        command.append("--capture-handshake")
    start = [str(args.wpr), "-start", args.wpr_profile, "-filemode", "-recordtempto", str(output / "temp"),
             "-instancename", instance]
    return {"schema": 1, "created_utc": utc(), "output": str(output), "manifest": str(Path(args.manifest).resolve()),
            "manifest_sha256": sha256(args.manifest), "binary": manifest["binary"], "pdb": manifest["pdb"],
            "command": command, "cwd": str(args.cwd.resolve()), "instance": instance,
            "handshake": args.handshake,
            "timeout_seconds": args.timeout_seconds, "max_output_bytes": args.max_output_mib * 1024 * 1024,
            "native_command_timeout_seconds": 30, "cleanup_timeout_seconds": 120,
            "quota_contract": "Polling guard during workload; WPR stop/merge can transiently exceed quota.",
            "commands": {"start": start,
                         "status": [str(args.wpr), "-status", "profiles", "collectors", "-details", "-instancename", instance],
                         "stop": [str(args.wpr), "-stop", str(output / "capture.etl"), "renderer CPU profile", "-skipPdbGen", "-instancename", instance],
                         "marker_begin": [str(args.wpr), "-marker", instance + "_BEGIN", "-instancename", instance],
                         "marker_end": [str(args.wpr), "-marker", instance + "_END", "-instancename", instance],
                         "cancel": [str(args.wpr), "-cancel", "-instancename", instance]},
            "prerequisites": ["Administrative Windows CPU/ETW collection rights are supplied by the caller.",
                              "No unrelated WPR sessions will be stopped or cancelled.",
                              "Raw ETL, symbols and logs remain local; no upload is performed."]}


def directory_bytes(path):
    total = 0
    for entry in Path(path).rglob("*"):
        try:
            if entry.is_file():
                total += entry.stat().st_size
        except FileNotFoundError:
            pass  # WPR may rename/merge a temporary file between enumeration/stat.
    return total


def loss_fields(path):
    """Retain observed fields without treating missing/localized fields as zero."""
    path = Path(path)
    if not path.is_file():
        return {"path": str(path), "events_lost_fields": [], "buffers_lost_fields": []}
    raw = path.read_bytes()
    encoding = "utf-16" if raw.startswith((b"\xff\xfe", b"\xfe\xff")) else "utf-8-sig"
    text = raw.decode(encoding, errors="replace")
    result = {"path": str(path)}
    for field, label in (("events_lost_fields", "Events"), ("buffers_lost_fields", "Buffers")):
        result[field] = [int(value) for value in re.findall(r"\b" + label + r"\s*Lost\s*[:=]\s*(\d+)", text, re.I)]
    return result


def logged(command, output, name, timeout=120, env=None):
    with (output / (name + ".stdout.txt")).open("wb") as stdout, (output / (name + ".stderr.txt")).open("wb") as stderr:
        completed = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=timeout, env=env, check=False)
    return completed.returncode


def terminate_target(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def validate_plan(plan):
    if sha256(plan["manifest"]) != plan["manifest_sha256"]:
        raise ValueError("Build manifest changed after planning")
    manifest, binary = verified_build(plan["manifest"])
    if str(binary) != plan["command"][0] or manifest["binary"] != plan["binary"] or manifest["pdb"] != plan["pdb"]:
        raise ValueError("Build artifacts changed after planning")


def drain_stdout(process, path, events):
    """Drain a trusted workload continuously; retain logs without pipe deadlock."""
    try:
        with path.open("w", encoding="utf-8", newline="") as log:
            while True:
                line = process.stdout.readline(1024 * 1024 + 1)
                if not line:
                    break
                if len(line) > 1024 * 1024:
                    raise ValueError("Workload stdout line exceeds 1 MiB")
                log.write(line)
                log.flush()
                for kind in ("READY", "DONE"):
                    prefix = "PROFILE_WORKLOAD_" + kind + " "
                    if line.startswith(prefix):
                        payload = json.loads(line[len(prefix):])
                        events.put_nowait((kind, payload))
    except Exception as error:
        try:
            events.put_nowait(("ERROR", str(error)))
        except queue.Full:
            pass


def wait_event(process, events, kind, guard, sleeper):
    while True:
        guard()
        try:
            actual, payload = events.get_nowait()
            if actual != kind:
                raise RuntimeError(f"Expected handshake {kind}, received {actual}: {payload}")
            if not isinstance(payload, dict) or payload.get("pid") != process.pid:
                raise ValueError(f"Handshake {kind} must identify the launched target PID")
            return payload
        except queue.Empty:
            if process.poll() is not None:
                raise RuntimeError(f"Target exited before handshake {kind}")
            sleeper(0.05)


def send_input(process, text):
    process.stdin.write(text + "\n")
    process.stdin.flush()


def record_owned(plan, runner=logged, launcher=subprocess.Popen, monotonic=time.monotonic, sleeper=time.sleep,
                 size_reader=directory_bytes, validator=validate_plan):
    """Pure orchestration seam permits failure-path tests without starting ETW."""
    output = Path(plan["output"])
    output.mkdir(parents=True, exist_ok=False)
    (output / "temp").mkdir()
    state = {"schema": 1, "plan": plan, "started_utc": utc(), "own_start_succeeded": False,
             "session_state": "not_started", "capture_valid": False, "target": None,
             "quality": {"lost_events": None, "lost_buffers": None,
                         "acceptance": "pending offline trace statistics and stack/symbol coverage"}}
    metadata = output / "capture.json"
    write_json(metadata, state)
    started, process, reader = False, None, None
    stderr = (output / "target.stderr.txt").open("wb")
    stdout = None
    begin = monotonic()
    events = queue.Queue(maxsize=8)

    def guard():
        if monotonic() - begin >= plan["timeout_seconds"]:
            raise RuntimeError("Target exceeded --timeout-seconds; capture is incomplete")
        if size_reader(output) >= plan["max_output_bytes"]:
            raise RuntimeError("Capture output quota reached; capture is incomplete")

    def launch():
        nonlocal process, reader, stdout
        guard()
        if plan.get("handshake"):
            process = launcher(plan["command"], cwd=plan["cwd"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=stderr, text=True, encoding="utf-8", bufsize=1)
            reader = threading.Thread(target=drain_stdout, args=(process, output / "target.stdout.txt", events), daemon=True)
            reader.start()
        else:
            stdout = (output / "target.stdout.txt").open("wb")
            process = launcher(plan["command"], cwd=plan["cwd"], stdout=stdout, stderr=stderr)
        state["target"] = {"pid": process.pid, "launched_utc": utc(), "command": plan["command"]}
        write_json(metadata, state)

    def control(key, name, cleanup=False):
        if cleanup:
            timeout = plan.get("cleanup_timeout_seconds", 120)
        else:
            guard()
            timeout = min(plan.get("native_command_timeout_seconds", 30), plan["timeout_seconds"] - (monotonic() - begin))
        return runner(plan["commands"][key], output, name, timeout=timeout)

    try:
        validator(plan)
        if plan.get("handshake"):
            launch()
            state["handshake_ready"] = wait_event(process, events, "READY", guard, sleeper)
        try:
            code = control("start", "wpr-start")
        except (subprocess.TimeoutExpired, KeyboardInterrupt):
            state["session_state"] = "start_outcome_unknown_no_cleanup_attempted"
            state["inspection_required"] = {"instance": plan["instance"],
                                            "status_command": plan["commands"]["status"],
                                            "reason": "WPR start did not return an outcome; inspect this unique instance before cleanup."}
            raise
        state["start_exit_code"] = code
        if code:
            # Failed start may leave partial system state. Never infer ownership
            # from a failed command and never issue a blind cleanup operation.
            state["session_state"] = "start_failed_no_cleanup_attempted"
            raise RuntimeError(f"WPR start failed ({code}); inspect wpr-start logs. No cancellation was issued.")
        started = True
        state["own_start_succeeded"] = True
        state["session_state"] = "recording"
        write_json(metadata, state)
        state["initial_status_exit_code"] = control("status", "wpr-status-initial")
        if state["initial_status_exit_code"]:
            raise RuntimeError("Owned WPR status failed immediately after start")
        validator(plan)
        if plan.get("handshake"):
            state["marker_begin"] = {"before_utc": utc(), "text": plan["instance"] + "_BEGIN"}
            state["marker_begin"]["exit_code"] = control("marker_begin", "wpr-marker-begin")
            state["marker_begin"]["after_utc"] = utc()
            guard()
            send_input(process, "GO")
            state["handshake_done"] = wait_event(process, events, "DONE", guard, sleeper)
            state["marker_end"] = {"before_utc": utc(), "text": plan["instance"] + "_END"}
            state["marker_end"]["exit_code"] = control("marker_end", "wpr-marker-end")
            state["marker_end"]["after_utc"] = utc()
        else:
            launch()
            while process.poll() is None:
                guard()
                sleeper(0.1)
        state["final_status_exit_code"] = control("status", "wpr-status-final")
        state["quality"]["final_status_loss_fields"] = loss_fields(output / "wpr-status-final.stdout.txt")
        code = control("stop", "wpr-stop", cleanup=True)
        state["stop_exit_code"] = code
        if code:
            raise RuntimeError(f"Owned WPR stop failed ({code})")
        started = False
        state["session_state"] = "stopped"
        if plan.get("handshake"):
            send_input(process, "END")
            process.wait(timeout=15)
        state["target"].update(exit_code=process.returncode, ended_utc=utc(), elapsed_seconds=monotonic() - begin)
        if reader:
            reader.join(timeout=5)
            if reader.is_alive():
                raise RuntimeError("Target stdout reader did not finish")
        state["output_bytes"] = size_reader(output)
        markers_valid = not plan.get("handshake") or all(state[key]["exit_code"] == 0 for key in ("marker_begin", "marker_end"))
        state["quality"]["marker_commands_succeeded"] = markers_valid if plan.get("handshake") else None
        etl = output / "capture.etl"
        if not etl.is_file() or etl.stat().st_size == 0:
            state["capture_valid"] = False
            raise RuntimeError("WPR reported success without a nonempty capture.etl")
        state["etl"] = {"path": str(etl), "size_bytes": etl.stat().st_size, "sha256": sha256(etl)}
        state["capture_valid"] = (process.returncode == 0 and markers_valid
                                  and state["initial_status_exit_code"] == state["final_status_exit_code"] == 0
                                  and state["output_bytes"] <= plan["max_output_bytes"])
        if not state["capture_valid"]:
            raise RuntimeError("Target, marker, status or output quota check failed; capture is retained as invalid")
        return state
    except BaseException as error:
        state["capture_valid"] = False
        state["error"] = f"{type(error).__name__}: {error}"
        if process is not None:
            try:
                terminate_target(process)
                if state["target"]:
                    state["target"].update(exit_code=process.returncode, ended_utc=utc())
            except Exception as target_error:
                state["target_cleanup_error"] = str(target_error)
        if started:
            try:
                state["failure_status_exit_code"] = control("status", "wpr-status-failure", cleanup=True)
            except Exception as status_error:
                state["failure_status_error"] = str(status_error)
            try:
                code = control("cancel", "wpr-cancel-owned", cleanup=True)
                state["cancel_exit_code"] = code
                state["session_state"] = "cancelled" if code == 0 else "owned_cleanup_failed"
            except Exception as cleanup_error:
                state["session_state"] = "owned_cleanup_failed"
                state["cleanup_error"] = str(cleanup_error)
        raise
    finally:
        cleanup_errors = []
        active_error = sys.exc_info()[0] is not None

        def close_stream(name, stream):
            if stream is not None:
                try:
                    stream.close()
                except Exception as error:
                    cleanup_errors.append({"resource": name, "error": f"{type(error).__name__}: {error}"})

        if process is not None:
            close_stream("target_stdin", process.stdin)
            if reader:
                try:
                    reader.join(timeout=5)
                except Exception as error:
                    cleanup_errors.append({"resource": "stdout_reader", "error": f"{type(error).__name__}: {error}"})
            close_stream("target_stdout", process.stdout)
        close_stream("target_stdout_log", stdout)
        close_stream("target_stderr_log", stderr)
        if cleanup_errors:
            state["stream_cleanup_errors"] = cleanup_errors
            state["capture_valid"] = False
        state["finished_utc"] = utc()
        write_json(metadata, state)
        if cleanup_errors and not active_error:
            raise RuntimeError("Target stream cleanup failed; inspect capture.json")


def export_trace(args):
    if not 0 < args.timeout_seconds <= 3600:
        raise ValueError("--timeout-seconds must be in (0, 3600]")
    metadata_path = args.capture.resolve()
    capture = json.loads(metadata_path.read_text(encoding="utf-8"))
    if capture.get("session_state") != "stopped" or not capture.get("capture_valid"):
        raise ValueError("Export requires a successfully stopped, valid owned capture")
    plan = capture["plan"]
    manifest, binary = verified_build(plan["manifest"])
    if sha256(plan["manifest"]) != plan["manifest_sha256"]:
        raise ValueError("Build manifest changed since capture")
    etl = Path(capture["etl"]["path"])
    if sha256(etl) != capture["etl"]["sha256"]:
        raise ValueError("Raw ETL changed since capture")
    output = args.output.resolve() if args.output else metadata_path.parent / "export"
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    cache = output / "symcache"
    cache.mkdir()
    env["_NT_SYMBOL_PATH"] = str(binary.parent)
    if args.microsoft_symbols:
        env["_NT_SYMBOL_PATH"] += ";srv*" + str(output / "symbols") + "*https://msdl.microsoft.com/download/symbols"
    env["_NT_SYMCACHE_PATH"] = str(cache)
    source_profile = (args.wpa_profile or args.wpt / "Catalog/AppLaunch.wpaProfile").resolve()
    profile = prepare_profile(source_profile, output / "cpu-events.wpaProfile")
    commands = {"wpa": [str(args.wpt / "wpaexporter.exe"), "-i", str(etl), "-symbols", "-profile", str(profile),
                        "-outputfolder", str(output)],
                "trace-statistics": [str(args.wpt / "xperf.exe"), "-i", str(etl), "-a", "tracestats", "-timespan", "actual", "-detail", "stack"],
                "sampled-functions": [str(args.wpt / "xperf.exe"), "-i", str(etl), "-symbols", "-a", "profile", "-detail"],
                "sampling-frequency": [str(args.wpt / "xperf.exe"), "-i", str(etl), "-a", "profile", "-freq"],
                "scheduler": [str(args.wpt / "xperf.exe"), "-i", str(etl), "-a", "cswitch", "-process", "-thread"],
                "ready-stacks": [str(args.wpt / "xperf.exe"), "-i", str(etl), "-symbols", "-a", "readythread", "-stacks"]}
    report = {"schema": 1, "capture": str(metadata_path), "etl_sha256": capture["etl"]["sha256"],
              "binary_sha256": manifest["binary"]["sha256"], "profile_source": str(source_profile),
              "profile_sha256": sha256(profile), "commands": commands, "exit_codes": {}, "stages": {},
              "acceptance": "pending real-ETL header/unit, lost-event, stack and symbol coverage verification"}
    report_path = output / "export.json"
    write_json(report_path, report)
    for name, command in commands.items():
        stage = {"status": "running", "started_utc": utc(), "timeout_seconds": args.timeout_seconds}
        report["stages"][name] = stage
        write_json(report_path, report)
        try:
            code = logged(command, output, name, timeout=args.timeout_seconds, env=env)
            report["exit_codes"][name] = code
            stage.update(status="succeeded" if code == 0 else "failed", exit_code=code)
        except BaseException as error:
            stage.update(status="timeout" if isinstance(error, subprocess.TimeoutExpired) else "interrupted"
                         if isinstance(error, KeyboardInterrupt) else "failed",
                         error=f"{type(error).__name__}: {error}")
            raise
        finally:
            stage["finished_utc"] = utc()
            write_json(report_path, report)
        if name == "trace-statistics":
            report["loss_fields"] = loss_fields(output / "trace-statistics.stdout.txt")
        write_json(report_path, report)
    report["csv_files"] = [{"path": str(path), "sha256": sha256(path), "size_bytes": path.stat().st_size}
                           for path in output.glob("*.csv")]
    write_json(report_path, report)
    if any(code for code in report["exit_codes"].values()):
        raise RuntimeError(f"Export failed: {report['exit_codes']}; raw reports retained in {output}")
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="operation", required=True)
    for operation in ("plan", "capture"):
        command = commands.add_parser(operation)
        command.add_argument("--manifest", type=Path, required=True)
        command.add_argument("--output-root", type=Path, default=ROOT / ".tools/cpu-profile")
        command.add_argument("--run-id")
        command.add_argument("--handshake", action="store_true", help="Warmup READY / GO / DONE / END protocol with two owned WPR markers")
        command.add_argument("--cwd", type=Path, default=ROOT)
        command.add_argument("--wpr", type=Path, default=WPR)
        command.add_argument("--wpr-profile", default="CPU.Verbose",
                             help="Built-in name or explicit custom .wprp!Profile; no marker provider is forced")
        command.add_argument("--timeout-seconds", type=float, default=90)
        command.add_argument("--max-output-mib", type=int, default=2048)
        command.add_argument("target_args", nargs=argparse.REMAINDER)
    export = commands.add_parser("export")
    export.add_argument("--capture", type=Path, required=True)
    export.add_argument("--wpt", type=Path, default=WPT)
    export.add_argument("--wpa-profile", type=Path)
    export.add_argument("--output", type=Path)
    export.add_argument("--microsoft-symbols", action="store_true", help="Allow Microsoft symbol server downloads")
    export.add_argument("--timeout-seconds", type=float, default=300)
    analyze = commands.add_parser("analyze")
    analyze.add_argument("--capture", type=Path, required=True)
    analyze.add_argument("--sampled", type=Path, required=True)
    analyze.add_argument("--precise", type=Path, required=True)
    analyze.add_argument("--start-s", type=float, required=True, help="Verified ETL-relative target lifetime/ROI start")
    analyze.add_argument("--end-s", type=float, required=True, help="Verified ETL-relative target lifetime/ROI end")
    analyze.add_argument("--timestamp-unit", choices=("s", "ms", "us", "ns"), help="Explicit unit for bare WPA timestamp headers")
    analyze.add_argument("--stack-separator", help="Only set after verifying WPA raw stack serialization/orientation")
    analyze.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        if args.operation in ("plan", "capture"):
            plan = make_plan(args)
            if args.operation == "plan":
                result = plan
            else:
                if os.name != "nt":
                    raise ValueError("CPU collection requires Windows")
                if not args.wpr.is_file():
                    raise ValueError(f"WPR not found: {args.wpr}")
                if shutil.disk_usage(args.output_root.resolve().anchor).free < plan["max_output_bytes"] * 2:
                    raise ValueError("Insufficient free space for quota plus WPR stop/merge headroom")
                result = record_owned(plan)
        elif args.operation == "export":
            result = export_trace(args)
        else:
            if not (0 <= args.start_s < args.end_s < float("inf")):
                raise ValueError("Require finite 0 <= --start-s < --end-s")
            capture = json.loads(args.capture.read_text(encoding="utf-8"))
            if not capture.get("capture_valid") or capture.get("session_state") != "stopped":
                raise ValueError("Analysis requires a valid stopped capture; incomplete traces require manual investigation")
            pid = capture["target"]["pid"]
            roi = (args.start_s, args.end_s)
            result = {"schema": 1, "capture": str(args.capture.resolve()), "pid": pid, "roi_s": roi,
                      "inputs": {"sampled": {"path": str(args.sampled.resolve()), "sha256": sha256(args.sampled)},
                                 "precise": {"path": str(args.precise.resolve()), "sha256": sha256(args.precise)}},
                      "acceptance": "diagnostic summary; verify trace quality before performance conclusions",
                      "sampled": analyze_sampled(args.sampled, pid, roi, args.timestamp_unit, args.stack_separator),
                      "precise": analyze_precise(args.precise, pid, roi, args.timestamp_unit)}
            write_json(args.output, result)
        print(json.dumps(result, indent=2, ensure_ascii=False))
        return 0
    except (ValueError, RuntimeError, OSError, subprocess.SubprocessError, KeyError) as error:
        print(f"cpu-profile: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
