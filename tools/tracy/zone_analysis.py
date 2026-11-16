"""Interpret pinned Tracy 0.14.1 raw CPU zones without calling them CPU samples.

Upstream csvexport -u uses nanoseconds for start and elapsed duration. -u -e
subtracts direct nested child elapsed spans. These are wall spans, including
waits/descheduling, not scheduler running time or charged process CPU time.
Upstream joins unquoted fields; exports use tabs and reject malformed rows.
"""
from collections import Counter, defaultdict, deque
from bisect import bisect_right
import csv
import math
from pathlib import Path


HEADER = ["name", "src_file", "src_line", "ns_since_start", "exec_time_ns", "thread", "value"]


def integer(value, label):
    try:
        result = int(value)
    except (TypeError, ValueError) as error:
        raise ValueError(f"Invalid Tracy integer {label}: {value!r}") from error
    if result < 0 or str(result) != str(value):
        raise ValueError(f"Expected nonnegative canonical Tracy integer {label}")
    return result


def read_zones(path):
    result = []
    with Path(path).open(encoding="utf-8-sig", newline="") as stream:
        rows = csv.reader(stream, delimiter="\t", quoting=csv.QUOTE_NONE)
        if next(rows, None) != HEADER:
            raise ValueError("Expected verified Tracy 0.14.1 raw zone header")
        for index, values in enumerate(rows, 2):
            if len(values) != len(HEADER):
                raise ValueError(f"Malformed/unescaped Tracy TSV record {index}")
            row = dict(zip(HEADER, values))
            for name in ("src_line", "ns_since_start", "exec_time_ns", "thread"):
                row[name] = integer(row[name], name)
            if not row["name"] or not row["src_file"]:
                raise ValueError("Tracy zones require name and source location")
            result.append(row)
    return result


def percentile(values, fraction):
    ordered = sorted(values)
    index = (len(ordered) - 1) * fraction
    low, high = math.floor(index), math.ceil(index)
    return ordered[low] + (ordered[high] - ordered[low]) * (index - low)


def key(row):
    return tuple(row[name] for name in ("name", "src_file", "src_line", "ns_since_start", "thread", "value"))


def analyze_zones(inclusive_path, self_path, expected_frames, capture_id):
    if type(expected_frames) is not int or expected_frames <= 0:
        raise ValueError("Expected a positive measured frame count")
    inclusive, exclusive = read_zones(inclusive_path), read_zones(self_path)
    if not isinstance(capture_id, str) or len(capture_id) != 32 or any(char not in "0123456789abcdef" for char in capture_id):
        raise ValueError("Analysis requires the planned per-run capture nonce")
    identities = [row for row in inclusive if row["name"] == "bistro.capture_identity"]
    if len(identities) != 1 or identities[0]["value"] != capture_id:
        raise ValueError("Trace does not identify the exact owned target capture nonce")
    # Pair exact instances, retaining duplicate multiplicities if clocks coincide.
    self_instances = defaultdict(deque)
    for row in exclusive:
        self_instances[key(row)].append(row["exec_time_ns"])
    if Counter(map(key, inclusive)) != Counter(map(key, exclusive)):
        raise ValueError("Inclusive and self exports describe different zone instances")
    measurements = [row for row in inclusive if row["name"] == "bistro.measurement"]
    if len(measurements) != 1:
        raise ValueError("Trace must contain exactly one complete bistro.measurement ROI")
    root = measurements[0]
    begin, end = root["ns_since_start"], root["ns_since_start"] + root["exec_time_ns"]
    identity_zone = identities[0]
    if (identity_zone["thread"] != root["thread"] or identity_zone["ns_since_start"] + identity_zone["exec_time_ns"] > begin):
        raise ValueError("Owned capture identity must precede the ROI on its target thread")
    if end <= begin:
        raise ValueError("Measurement ROI must have positive duration")
    grouped, threads, frames, instances = defaultdict(list), Counter(), [], []
    outside = 0
    for row in inclusive:
        duration = row["exec_time_ns"]
        self_ns = self_instances[key(row)].popleft()
        if self_ns > duration:
            raise ValueError("Self duration exceeds inclusive elapsed duration")
        start = row["ns_since_start"]
        if start < begin or start + duration > end:
            outside += 1
            continue
        identity = (row["name"], row["src_file"], row["src_line"])
        grouped[identity].append((duration, self_ns))
        instances.append({**row, "self_ns": self_ns})
        threads[row["thread"]] += 1
        if row["name"] == "bistro.frame":
            if row["thread"] != root["thread"]:
                raise ValueError("Frame zones must run on the measurement thread")
            frames.append(row)
    frames.sort(key=lambda row: row["ns_since_start"])
    if len(frames) != expected_frames:
        raise ValueError(f"Expected {expected_frames} complete frame zones; exported {len(frames)}")
    if any(a["ns_since_start"] + a["exec_time_ns"] > b["ns_since_start"] for a, b in zip(frames, frames[1:])):
        raise ValueError("Measured frame zones overlap")
    # Complete explicit nested zones form a laminar interval tree per thread.
    # Verify upstream self arithmetic against that tree; crossed/incomplete
    # parents must not become apparently credible hotspot rankings.
    edges, ambiguous = Counter(), 0
    for thread in threads:
        stack = []
        ordered = sorted((row for row in instances if row["thread"] == thread),
                         key=lambda row: (row["ns_since_start"], -row["exec_time_ns"], row["self_ns"]))
        for row in ordered:
            start = row["ns_since_start"]
            finish = start + row["exec_time_ns"]
            while stack and stack[-1]["end"] <= start:
                completed = stack.pop()
                if completed["row"]["self_ns"] != completed["row"]["exec_time_ns"] - completed["child_ns"]:
                    raise ValueError("Nested zone/self export is incomplete or inconsistent")
            if stack:
                parent = stack[-1]
                if finish > parent["end"]:
                    raise ValueError("CPU zones cross instead of nesting on one thread")
                parent["child_ns"] += row["exec_time_ns"]
                parent_row = parent["row"]
                if start == parent_row["ns_since_start"] and finish == parent["end"]:
                    ambiguous += 1
                else:
                    edges[(parent_row["name"], parent_row["src_file"], parent_row["src_line"],
                           row["name"], row["src_file"], row["src_line"])] += 1
            stack.append({"row": row, "end": finish, "child_ns": 0})
        for completed in stack:
            if completed["row"]["self_ns"] != completed["row"]["exec_time_ns"] - completed["child_ns"]:
                raise ValueError("Nested zone/self export is incomplete or inconsistent")
    zones = []
    frame_starts = [row["ns_since_start"] for row in frames]
    for (name, file, line), samples in grouped.items():
        total, self_total = sum(item[0] for item in samples), sum(item[1] for item in samples)
        frame_costs = [[0, 0] for _ in frames]
        frame_contained = 0
        for row in instances:
            if (row["name"], row["src_file"], row["src_line"]) != (name, file, line) or row["thread"] != root["thread"]:
                continue
            index = bisect_right(frame_starts, row["ns_since_start"]) - 1
            if (index >= 0 and row["ns_since_start"] + row["exec_time_ns"]
                    <= frames[index]["ns_since_start"] + frames[index]["exec_time_ns"]):
                frame_costs[index][0] += row["exec_time_ns"]
                frame_costs[index][1] += row["self_ns"]
                frame_contained += 1
        zones.append({"name": name, "src_file": file, "src_line": line, "calls": len(samples),
                      "calls_per_frame": len(samples) / expected_frames,
                      "inclusive_ns": total, "self_ns": self_total,
                      "inclusive_ns_per_frame": total / expected_frames,
                      "self_ns_per_frame": self_total / expected_frames,
                      "inclusive_call_p50_ns": percentile([item[0] for item in samples], 0.5),
                      "inclusive_call_p95_ns": percentile([item[0] for item in samples], 0.95),
                      "self_call_p50_ns": percentile([item[1] for item in samples], 0.5),
                      "self_call_p95_ns": percentile([item[1] for item in samples], 0.95),
                      "frame_contained_calls": frame_contained, "outside_frame_calls": len(samples) - frame_contained,
                      "per_frame_aggregation": ({"samples": expected_frames,
                          "inclusive_p50_ns": percentile([item[0] for item in frame_costs], 0.5),
                          "inclusive_p95_ns": percentile([item[0] for item in frame_costs], 0.95),
                          "self_p50_ns": percentile([item[1] for item in frame_costs], 0.5),
                          "self_p95_ns": percentile([item[1] for item in frame_costs], 0.95),
                          "scope": "Sum complete instances contained in each main-thread frame zone, including zero for frames without a call; excludes final drain and calls outside frames."} if frame_contained else None)})
    zones.sort(key=lambda row: row["self_ns"], reverse=True)
    frame_times = [row["exec_time_ns"] for row in frames]
    return {"schema_version": 1, "capture_id": capture_id, "trace_identity_verified": True,
            "roi": {"start_ns": begin, "end_ns": end, "elapsed_ns": end - begin},
            "measured_frame_zones": len(frames), "cpu_zone_events": sum(threads.values()),
            "cpu_threads_with_zones": len(threads), "thread_zone_counts": dict(threads),
            "outside_roi_zone_events": outside, "zones": zones,
            "parent_edges": [{"parent": {"name": entry[0], "src_file": entry[1], "src_line": entry[2]},
                              "child": {"name": entry[3], "src_file": entry[4], "src_line": entry[5]}, "calls": calls}
                             for entry, calls in sorted(edges.items())],
            "ambiguous_equal_interval_parent_edges": ambiguous,
            "frame_zone_elapsed": {"samples": len(frames), "mean_ns": sum(frame_times) / len(frames),
                                   "p50_ns": percentile(frame_times, 0.5), "p95_ns": percentile(frame_times, 0.95)},
            "semantics": {"time_unit": "ns", "inclusive": "Elapsed zone wall span including nested children, waits and descheduling; never sum nested inclusive costs.",
                          "self": "Elapsed zone wall span less direct child spans from matched csvexport -u -e; not charged CPU or sampling evidence.",
                          "frame_zone": "bistro.frame CPU iteration zone; excludes final drain, inter-iteration gaps, load, warmup, post-ROI readback and shutdown. Separate runner frame_wall_ns retains its documented latency scope.",
                          "coverage": "Completed exported positive-duration explicit zones only. csvexport omits incomplete/non-positive spans, so coverage of omitted zones elsewhere is unknown. Closed measurement/frame zones, nonce and nested self arithmetic are verified. Uninstrumented driver, OS and Rust work has no independent function attribution; no scheduler/running/ready/blocked evidence.",
                          "percentiles": "Linear interpolation over individual complete instances, including all raw frame zones; per-call zone percentiles are not per-frame percentiles.",
                          "thread_identifiers": "Identifiers printed by csvexport's ZoneThreadData::Thread(); thread counts concern exported zones, not all process threads or an independently verified mapping to Windows TIDs.",
                          "frame_marks": "Tracy 0.14.1 csvexport does not export frame sets; measured_frame_zones verifies explicit frame zones, not viewer bootstrap/continuous frame mark count."}}
