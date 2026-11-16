"""Aggregate validated flat CPU events; never interpret samples as call counts."""

from collections import defaultdict
import csv
import json
import math
from pathlib import Path
import re


class AnalysisError(ValueError):
    pass


def number(value, name):
    try:
        result = float(value)
    except (ValueError, TypeError) as error:
        raise AnalysisError(f"{name}: expected an unlocalized finite number, got {value!r}") from error
    if not math.isfinite(result):
        raise AnalysisError(f"{name}: non-finite value")
    return result


def integer(value, name):
    result = number(value, name)
    if result < 0 or result != int(result):
        raise AnalysisError(f"{name}: expected nonnegative integer")
    return int(result)


def process_id(value):
    if re.fullmatch(r"\d+", value.strip()):
        return int(value)
    match = re.fullmatch(r".* \((\d+)\)", value.strip())
    if match:
        return int(match.group(1))
    raise AnalysisError(f"Cannot identify exact PID from {value!r}")


def read_rows(path):
    with Path(path).open(encoding="utf-8-sig", newline="") as stream:
        reader = csv.DictReader(stream)
        headers = reader.fieldnames or []
        if len(headers) != len(set(headers)):
            raise AnalysisError("Duplicate CSV headers: aggregated WPA presets are unsupported")
        if not headers or any(not name for name in headers):
            raise AnalysisError("CSV requires a complete header")
    def rows():
        # Keep large scheduler exports streaming instead of retaining their rows.
        with Path(path).open(encoding="utf-8-sig", newline="") as stream:
            reader = csv.DictReader(stream)
            if reader.fieldnames != headers:
                raise AnalysisError("CSV headers changed while opening")
            for row in reader:
                if None in row or any(value is None for value in row.values()):
                    raise AnalysisError("CSV row width differs from its header")
                yield row
    return headers, rows()


def column(headers, *names, required=True):
    found = [name for name in names if name in headers]
    if len(found) > 1:
        raise AnalysisError(f"Ambiguous columns: {found}")
    if not found and required:
        raise AnalysisError(f"Missing explicit column: {' or '.join(names)}")
    return found[0] if found else None


def timed_column(headers, canonical, vendor, unit=None):
    """Return column and seconds multiplier; bare vendor units require override."""
    candidates = [(canonical, 1.0)] + [(f"{vendor} ({label})", scale) for label, scale in (
        ("s", 1.0), ("ms", 0.001), ("us", 0.000001), ("ns", 0.000000001))]
    if unit:
        candidates.append((vendor, {"s": 1.0, "ms": 0.001, "us": 0.000001, "ns": 0.000000001}[unit]))
    present = [(name, scale) for name, scale in candidates if name in headers]
    if len(present) != 1:
        raise AnalysisError(f"{vendor}: need one timestamp column with explicit unit")
    return present[0]


def time_value(row, spec):
    name, scale = spec
    return number(row[name], name) * scale


def overlap(start, end, roi):
    if end < start:
        raise AnalysisError(f"Reversed interval {start}..{end}")
    return max(0.0, min(end, roi[1]) - max(start, roi[0])) * 1000


def ranking(values, field="weight_ms"):
    return [{"name": key, field: value} for key, value in sorted(values.items(), key=lambda item: (-item[1], item[0]))]


def analyze_sampled(path, pid, roi, timestamp_unit=None, stack_separator=None):
    headers, rows = read_rows(path)
    process = column(headers, "pid", "Process")
    tid = column(headers, "tid", "Thread ID")
    cpu = column(headers, "cpu", "CPU")
    timestamp = timed_column(headers, "time_s", "TimeStamp", timestamp_unit)
    count = column(headers, "count", "Count")
    weight = column(headers, "weight_ms", "Weight (ms)")
    function = column(headers, "function", "Function")
    module = column(headers, "module", "Module")
    address = column(headers, "ip", "Address")
    stack = column(headers, "stack_json", "Stack", required=False)
    totals, self_time, inclusive, stacks = defaultdict(float), defaultdict(float), defaultdict(float), defaultdict(float)
    seen, samples, total, ips = set(), 0, 0.0, defaultdict(float)
    for row in rows:
        if process_id(row[process]) != pid:
            continue
        timestamp_s = time_value(row, timestamp)
        if not roi[0] <= timestamp_s < roi[1]:
            continue
        if integer(row[count], count) != 1:
            raise AnalysisError("Sampled rows must be leaf events with Count=1; grouped/subtotal rows rejected")
        thread, processor = integer(row[tid], tid), integer(row[cpu], cpu)
        try:
            if not row[address].startswith(("0x", "0X")):
                raise ValueError("missing explicit hexadecimal prefix")
            ip = int(row[address], 16)
        except ValueError as error:
            raise AnalysisError("Sample Address must be the raw instruction pointer with explicit 0x prefix") from error
        key = (thread, processor, timestamp_s, ip)
        if key in seen:
            raise AnalysisError("Duplicate sampled event; refusing to double-count")
        seen.add(key)
        value = number(row[weight], weight)
        if value < 0:
            raise AnalysisError("Sample weight cannot be negative")
        function_name = row[function] or f"<unresolved@0x{ip:x}>"
        name = (row[module] or "<unknown-module>") + "!" + function_name
        samples += 1
        total += value
        totals[str(thread)] += value
        self_time[name] += value
        ips[f"0x{ip:x}"] += value
        if stack and row[stack]:
            raw = row[stack]
            stacks[raw] += value
            if stack == "stack_json":
                try:
                    frames = json.loads(raw)
                except json.JSONDecodeError as error:
                    raise AnalysisError("stack_json must be a JSON string array ordered root-to-leaf") from error
                if not isinstance(frames, list) or not all(isinstance(frame, str) for frame in frames):
                    raise AnalysisError("stack_json must be a JSON string array")
            elif stack_separator:
                frames = raw.split(stack_separator)
            else:
                frames = []
            # Recursion receives this sample once per distinct function, not once
            # per occurrence. Inclusive totals across functions are not additive.
            for frame in set(frames):
                if frame:
                    inclusive[frame] += value
    if not samples:
        raise AnalysisError("No target sampled events inside the requested lifetime/ROI")
    return {"sample_count": samples, "weight_ms": total, "threads": ranking(totals),
            "self": ranking(self_time), "inclusive": ranking(inclusive), "stacks": ranking(stacks),
            "instruction_pointers": ranking(ips),
            "inclusive_available": bool(inclusive),
            "inclusive_identity": "provided frame strings; cross-module canonicalization is not inferred",
            "limits": ["Count is samples, never function invocation count.",
                       "Weight estimates executing CPU time; short activity can be absent.",
                       "ROI selects sample timestamps; weight at ROI boundaries is approximate.",
                       "Inclusive attribution requires stack_json or an explicitly verified stack separator.",
                       "Function/source lines may be unresolved or ambiguous under optimization."]}


def analyze_precise(path, pid, roi, timestamp_unit=None):
    headers, rows = read_rows(path)
    process = column(headers, "pid", "New Process")
    tid = column(headers, "tid", "New Thread Id")
    old_process = column(headers, "old_pid", "Old Process", required=False)
    old_tid = column(headers, "old_tid", "Old Thread Id", required=False)
    cpu = column(headers, "cpu", "CPU", "Cpu")
    count = column(headers, "count", "Count")
    times = {key: timed_column(headers, canonical, vendor, timestamp_unit) for key, canonical, vendor in (
        ("in", "switch_in_s", "Switch-In Time"), ("out", "next_switch_out_s", "Next Switch-Out Time"),
        ("last", "last_switch_out_s", "Last Switch-Out Time"), ("ready", "ready_time_s", "Ready Time"))}
    state = column(headers, "previous_state", "New Prev State")
    reason = column(headers, "wait_reason", "New Prev Wait Reason")
    new_stack = column(headers, "wait_stack", "New Thread Stack")
    ready_stack = column(headers, "ready_stack", "Ready Thread Stack")
    threads, waits, readying, runnable_stacks = {}, defaultdict(float), defaultdict(float), defaultdict(float)
    unclassified, states = defaultdict(float), defaultdict(int)
    missing_start_details = []
    self_switch_details = []
    seen, events, incomplete, missing_starts, stale_ready, intervals = set(), 0, 0, 0, 0, defaultdict(list)
    # These are ETW CSwitch states, not System.Diagnostics.ThreadState (whose
    # value 7 means Unknown). Ready/Standby/DeferredReady are runnable states.
    # https://learn.microsoft.com/en-us/windows/win32/etw/cswitch
    # WPA New Prev State / LastOut / Ready definitions:
    # https://learn.microsoft.com/en-us/windows-hardware/test/wpt/cpu-analysis
    runnable_states = {"ready", "1", "standby", "3", "deferredready", "deferred ready", "7"}
    waiting_states = {"waiting", "wait", "5"}
    def remember(thread, start, end, kind):
        clipped = max(start, roi[0]), min(end, roi[1])
        if clipped[0] < clipped[1]:
            intervals[thread].append((*clipped, kind, observation))
    for record_index, row in enumerate(rows, start=1):
        if process_id(row[process]) != pid:
            continue
        if integer(row[count], count) != 1:
            raise AnalysisError("Scheduler rows must be leaf events with Count=1")
        thread, processor = integer(row[tid], tid), integer(row[cpu], cpu)
        self_switch = False
        if old_tid and old_process and row[old_tid].strip().isdigit() and int(row[old_tid]) == thread:
            try:
                self_switch = process_id(row[old_process]) == pid
            except AnalysisError:
                pass  # Unknown old process identity cannot establish a self-switch.
        switch_in, switch_out = time_value(row, times["in"]), time_value(row, times["out"])
        running_ms = overlap(switch_in, switch_out, roi)
        last_raw, ready_raw = row[times["last"][0]], row[times["ready"][0]]
        last = time_value(row, times["last"]) if last_raw else None
        ready = time_value(row, times["ready"]) if ready_raw else None
        previous_state = row[state].strip().lower()
        if not self_switch and last is not None and last > switch_in:
            raise AnalysisError("Scheduler timestamps must satisfy last_out <= switch_in")
        if not self_switch and previous_state in waiting_states and ready is not None and (
                ready > switch_in or (last is not None and last > ready)):
            raise AnalysisError("Waiting scheduler timestamps must satisfy last_out <= ready <= switch_in")
        # A later switch-in can describe an off-CPU span inside the ROI. With no
        # LastOut only a Waiting wake establishes a partial observed interval;
        # a runnable/ambiguous state's ReadyTime may precede earlier running.
        off_cpu_start = None if self_switch else last if last is not None else ready if previous_state in waiting_states else None
        off_cpu_ms = overlap(off_cpu_start, switch_in, roi) if off_cpu_start is not None else 0
        if not running_ms and not off_cpu_ms:
            continue
        key = (thread, processor, switch_in)
        if key in seen:
            raise AnalysisError("Duplicate scheduler event; refusing to double-count")
        seen.add(key)
        values = threads.setdefault(str(thread), {"running_ms": 0.0, "ready_ms": 0.0, "blocked_ms": 0.0,
                                                "unclassified_off_cpu_ms": 0.0})
        observation = {"record_index": record_index, "cpu": processor, "previous_state": row[state],
                       "last_switch_out_s": last, "ready_time_s": ready,
                       "switch_in_s": switch_in, "next_switch_out_s": switch_out,
                       "old_process": row[old_process] if old_process else None,
                       "old_tid": row[old_tid] if old_tid else None}
        values["running_ms"] += running_ms
        remember(thread, switch_in, switch_out, "running")
        events += 1
        states[row[state]] += 1
        if self_switch:
            # CSwitch Old/New IDs identify the threads immediately before/after
            # this one event. Equal thread+process identities establish zero
            # off-CPU time here; WPA may retain an earlier LastOut/Ready pair.
            # Do not infer this case from adjacent timestamps or clamp a gap.
            self_switch_details.append(observation)
            continue
        if last is None and switch_in > roi[0]:
            missing_starts += 1
            missing_start_details.append({"tid": thread, "previous_state": row[state],
                                          "switch_in_s": switch_in, "ready_time_s": ready})
        if previous_state in runnable_states and last is not None:
            # WPA can retain an earlier wake's Ready Time after preemption.
            # The previous runnable state instead covers LastOut -> SwitchIn;
            # no blocked time is inferred from this unrelated wake timestamp.
            ready_ms = overlap(last, switch_in, roi)
            blocked_ms = 0.0
            remember(thread, last, switch_in, "ready")
            if ready is not None and ready < last:
                stale_ready += 1
        elif previous_state in waiting_states and last is not None and ready is not None:
            ready_ms = overlap(ready, switch_in, roi)
            blocked_ms = overlap(last, ready, roi)
            remember(thread, ready, switch_in, "ready")
            remember(thread, last, ready, "blocked")
        else:
            # Running/unknown previous states do not establish what happened
            # between switches. Retain that observed interval as unclassified,
            # including partial spans when an endpoint is missing.
            if off_cpu_ms or (last is None and switch_in > roi[0]):
                incomplete += 1
            values["unclassified_off_cpu_ms"] += off_cpu_ms
            if off_cpu_ms:
                remember(thread, off_cpu_start, switch_in, "unclassified")
                unclassified[json.dumps({"previous_state": row[state], "reason": row[reason],
                                        "stack": row[new_stack]}, sort_keys=True)] += off_cpu_ms
            continue
        values["ready_ms"] += ready_ms
        values["blocked_ms"] += blocked_ms
        if blocked_ms:
            waits[json.dumps({"reason": row[reason], "stack": row[new_stack]}, sort_keys=True)] += blocked_ms
        if ready_ms:
            if previous_state in waiting_states:
                readying[row[ready_stack] or "<unavailable>"] += ready_ms
            else:
                runnable_stacks[row[new_stack] or "<unavailable>"] += ready_ms
    if not events:
        raise AnalysisError("No target scheduler interval evidence inside the requested lifetime/ROI")
    for thread, spans in intervals.items():
        spans.sort(key=lambda span: (span[0], span[1], span[2]))
        for previous, following in zip(spans, spans[1:]):
            if following[0] < previous[1]:
                details = [{"start_s": span[0], "end_s": span[1], "kind": span[2], "source": span[3]}
                           for span in (previous, following)]
                raise AnalysisError(f"Overlapping scheduler state intervals for TID {thread}; refusing to double-count: "
                                    + json.dumps(details, sort_keys=True))
    return {"event_count": events, "threads": threads,
            "totals": {name: sum(values[name] for values in threads.values())
                       for name in ("running_ms", "ready_ms", "blocked_ms", "unclassified_off_cpu_ms")},
            "wait_reason_and_stack": ranking(waits, "blocked_ms"),
            "readying_stacks": ranking(readying, "ready_ms"),
            "runnable_new_thread_stacks": ranking(runnable_stacks, "ready_ms"),
            "incomplete_off_cpu_events": incomplete,
            "missing_off_cpu_start_events": missing_starts,
            "missing_off_cpu_starts": missing_start_details,
            "self_switch_identity_columns_available": bool(old_process and old_tid),
            "self_switch_events": len(self_switch_details),
            "self_switches": self_switch_details,
            "runnable_ready_precedes_last_out_events": stale_ready,
            "previous_states": dict(states),
            "unclassified_reason_and_stack": ranking(unclassified, "unclassified_off_cpu_ms"),
            "observed_interval_classification_complete": incomplete == 0 and missing_starts == 0,
            "limits": ["Times are per-thread; their sum can exceed elapsed wall time.",
                       "Event count includes only running or known off-CPU intervals intersecting the ROI.",
                       "Blocked time may be normal pacing or synchronization, not necessarily a defect.",
                       "Ready stack belongs to the thread that made the target runnable.",
                       "Ready-stack ranking covers Waiting wakeups; runnable New Thread Stack is reported separately from possibly stale wake stacks.",
                       "Stack rankings preserve raw WPA stack columns as context, not proof of the delay's cause.",
                       "Runnable Ready/Standby/DeferredReady use last switch-out through switch-in, not a stale wake timestamp.",
                       "Explicit equal Old/New thread and process identities establish a self-switch with zero off-CPU duration; stale WPA bounds are retained as context.",
                       "Missing off-CPU endpoints/states remain unclassified, never zero-filled.",
                       "Unclassified duration covers only observed bounds; missing starts and trace boundaries can hide additional time.",
                       "Per-TID totals combine any sequential thread-ID reuse within the selected process lifetime.",
                       "Trace-start/end censoring and lost events limit completeness."]}
