"""Map a captured workload's QPC interval to ETL-relative seconds exactly.

Inputs are capture.json, xperf tracestats, and the bounded header dump from:
  xperf -i capture.etl -a dumper -provider {68fdd900-4a3e-11d1-84f4-0000f80464e3} -add_rawdata -add_fieldnames
Optional marks must come from `xperf -i capture.etl -a marks -nanoseconds`.
No native commands are run. Marker command exit codes never prove ETL presence.
"""

import argparse
import csv
from fractions import Fraction
import hashlib
import io
import json
from pathlib import Path
import re

from identity import sha256


class ClockError(ValueError):
    """The supplied evidence cannot establish a single owned QPC/ETL interval."""


def positive_integer(value, name):
    if type(value) is not int or value <= 0:
        raise ClockError(f"{name} must be a positive integer")
    return value


def evidence_text(path):
    path = Path(path).resolve()
    if path.stat().st_size > 16 * 1024 * 1024:
        raise ClockError(f"Expected bounded text evidence, not a full event dump: {path}")
    raw = path.read_bytes()
    encoding = "utf-16" if raw.startswith((b"\xff\xfe", b"\xfe\xff")) else "utf-8-sig"
    text = raw.decode(encoding)
    return text, {"path": str(path), "sha256": hashlib.sha256(raw).hexdigest(), "size_bytes": len(raw)}


def parse_header(text, frequency):
    """Require exact agreement of every observed raw-QPC/trace-nanosecond pair."""
    positive_integer(frequency, "QPC frequency")
    schema = re.compile(r"^#\s*Cpu\s*#\s*RawTimeStamp\s*#\s*TimeStamp\[ns\]\s*#\s*EventKey\s*$")
    pair = re.compile(r"^#\s*\d+\s*#\s*(0[xX][0-9a-fA-F]+|\d+)\s*#\s*(\d+)\s*#\s*(?:0[xX][0-9a-fA-F]+|\d+)\s*$")
    origins, timestamps, count, schemas = set(), set(), 0, 0
    for line in text.splitlines():
        line = line.strip()
        if not line.startswith("#"):
            continue
        if schema.fullmatch(line):
            schemas += 1
            continue
        match = pair.fullmatch(line)
        if match is None or not schemas:
            raise ClockError("Malformed or missing raw-QPC/nanosecond header schema")
        raw, ns = match.groups()
        qpc = int(raw, 16) if raw.lower().startswith("0x") else int(raw)
        trace_ns = int(ns)
        origins.add(Fraction(qpc) - Fraction(trace_ns * frequency, 1_000_000_000))
        timestamps.add(trace_ns)
        count += 1
    if len(timestamps) < 2:
        raise ClockError("At least two distinct ETL nanosecond timestamps are required")
    if len(origins) != 1:
        raise ClockError("Raw QPC/ETL timestamp pairs have contradictory exact origins")
    names = re.findall(r"\bTrace Name:\s*(.+)$", text, re.MULTILINE)
    if len(names) != 1:
        raise ClockError("Header must identify exactly one trace file")
    return {"origin": origins.pop(), "schema_rows": schemas, "pair_rows": count,
            "distinct_trace_timestamps": len(timestamps), "minimum_ns": min(timestamps),
            "maximum_ns": max(timestamps), "trace_name": names[0].strip(),
            "lost_events_fields": [int(value) for value in re.findall(r"\bEvents Lost:\s*(\d+)", text, re.I)],
            "lost_buffers_fields": [int(value) for value in re.findall(r"\bBuffers Lost:\s*(\d+)", text, re.I)]}


def parse_statistics(text):
    clocks = re.findall(r"^\s*Clock type\s*:\s*(\S+)\s*$", text, re.MULTILINE | re.I)
    if not clocks or any(clock != "PerfCounter" for clock in clocks):
        raise ClockError("Trace statistics must explicitly identify the PerfCounter clock")
    traces = re.findall(r"^\s*Number of Traces\s*:\s*(\d+)\s*$", text, re.MULTILINE | re.I)
    names = re.findall(r"^\s*Trace name:\s*(.+)$", text, re.MULTILINE | re.I)
    if traces != ["1"] or len(names) != 1:
        raise ClockError("Statistics must describe exactly one trace file")
    losses = {}
    for kind in ("Events", "Buffers"):
        losses["lost_" + kind.lower() + "_fields"] = [int(value) for value in re.findall(
            r"^\s*(?:Total\s+)?#\s*Lost " + kind + r"\s*:\s*(\d+)\s*$", text, re.MULTILINE | re.I)]
    return {"clock_type": "PerfCounter", "trace_name": names[0].strip(), **losses}


def parse_marks(text, instance, start, end):
    rows = list(csv.reader(io.StringIO(text)))
    rows = [row for row in rows if any(value.strip() for value in row)]
    if not rows or [value.strip() for value in rows[0]] != ["Mark Type", "TimeStamp", "Label"]:
        raise ClockError("Expected xperf marks CSV with nanosecond TimeStamp values")
    own = {}
    for index, row in enumerate(rows[1:]):
        if len(row) != 3:
            raise ClockError("Malformed xperf marks CSV row")
        kind, timestamp, label = (value.strip() for value in row)
        if label not in (instance + "_BEGIN", instance + "_END"):
            continue
        if kind != "Mark" or not re.fullmatch(r"\d+", timestamp) or label in own:
            raise ClockError("Owned BEGIN/END marks must each occur exactly once")
        own[label] = (index, int(timestamp))
    if len(own) != 2:
        raise ClockError("Actual owned BEGIN and END marks are missing from ETL evidence")
    begin, finish = own[instance + "_BEGIN"], own[instance + "_END"]
    if not (begin[0] < finish[0] and Fraction(begin[1], 1_000_000_000) <= start < end
            <= Fraction(finish[1], 1_000_000_000)):
        raise ClockError("Owned nanosecond marks do not bracket the QPC ROI in order")
    return {"data_rows": len(rows) - 1, "own_mark_rows": 2, "timestamp_unit": "nanoseconds",
            "begin_ns": begin[1], "end_ns": finish[1], "brackets_qpc_roi": True}


def exact(value):
    return {"numerator": value.numerator, "denominator": value.denominator}


def derive_clock(capture_path, header_path, statistics_path, marks_path=None):
    capture_path = Path(capture_path).resolve()
    text, capture_evidence = evidence_text(capture_path)
    capture = json.loads(text)
    try:
        if (capture["schema"] != 1 or capture["capture_valid"] is not True
                or capture["own_start_succeeded"] is not True or capture["session_state"] != "stopped"
                or capture["target"]["exit_code"] != 0 or capture["plan"]["handshake"] is not True):
            raise ClockError("A valid, stopped, owned handshake capture is required")
        instance = capture["plan"]["instance"]
        if not isinstance(instance, str) or not re.fullmatch(r"[A-Za-z][A-Za-z0-9_]{0,127}", instance):
            raise ClockError("Invalid owned capture session name")
        pid = positive_integer(capture["target"]["pid"], "target PID")
        ready, done = capture["handshake_ready"], capture["handshake_done"]
        if (positive_integer(ready["pid"], "READY PID") != pid
                or positive_integer(done["pid"], "DONE PID") != pid):
            raise ClockError("Handshake PID does not match the captured target")
        window = done["window"]
        frequency = positive_integer(window["qpc_frequency"], "QPC frequency")
        if positive_integer(ready["qpc_frequency"], "READY QPC frequency") != frequency:
            raise ClockError("Handshake QPC frequencies disagree")
        first = positive_integer(window["qpc_start"], "QPC start")
        last = positive_integer(window["qpc_end"], "QPC end")
        elapsed = positive_integer(window.get("qpc_elapsed_ticks", last - first), "QPC elapsed ticks")
        if last <= first or elapsed != last - first:
            raise ClockError("Invalid or inconsistent QPC measurement interval")
        for name, suffix in (("marker_begin", "_BEGIN"), ("marker_end", "_END")):
            if capture[name]["text"] != instance + suffix or capture[name]["exit_code"] != 0:
                raise ClockError("Capture marker command does not belong to this session")
        etl = Path(capture["etl"]["path"])
        if not etl.is_absolute():
            etl = capture_path.parent / etl
        etl = etl.resolve()
        digest = capture["etl"]["sha256"]
        if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest) or sha256(etl) != digest:
            raise ClockError("ETL hash does not match this capture")
        if "size_bytes" in capture["etl"] and capture["etl"]["size_bytes"] != etl.stat().st_size:
            raise ClockError("ETL size does not match this capture")
    except (KeyError, TypeError) as error:
        raise ClockError("Incomplete capture identity or handshake window") from error
    header_text, header_evidence = evidence_text(header_path)
    statistics_text, statistics_evidence = evidence_text(statistics_path)
    header, statistics = parse_header(header_text, frequency), parse_statistics(statistics_text)
    for item, source in ((header, header_path), (statistics, statistics_path)):
        trace = Path(item["trace_name"])
        if not trace.is_absolute():
            trace = Path(source).resolve().parent / trace
        if trace.resolve() != etl:
            raise ClockError("Header/statistics identify a different ETL file")
    origin = header["origin"]
    start, end = (Fraction(value - origin, frequency) for value in (first, last))
    span_start = Fraction(header["minimum_ns"], 1_000_000_000)
    span_end = Fraction(header["maximum_ns"], 1_000_000_000)
    if not span_start <= start < end <= span_end:
        raise ClockError("QPC ROI is outside the observed ETL timestamp data")
    marks = {"actual_presence_verified": None, "brackets_qpc_roi": None}
    if marks_path is not None:
        marks_text, marks_evidence = evidence_text(marks_path)
        marks = {"actual_presence_verified": True, **parse_marks(marks_text, instance, start, end),
                 "evidence": marks_evidence}
    quality = capture.get("quality", {})
    if not isinstance(quality, dict):
        raise ClockError("Invalid capture quality metadata")
    return {"schema": 1, "pid": pid, "session": instance, "clock_type": statistics["clock_type"],
            "qpc_frequency": frequency, "origin_qpc_exact": exact(origin),
            "roi": {"qpc_start": first, "qpc_end": last,
                    "start_seconds": float(start), "end_seconds": float(end),
                    "start_seconds_exact": exact(start), "end_seconds_exact": exact(end)},
            "header": {key: value for key, value in header.items() if key not in ("origin", "trace_name")},
            "loss": {"capture_lost_events": quality.get("lost_events"),
                     "capture_lost_buffers": quality.get("lost_buffers"),
                     "header_lost_events_fields": header["lost_events_fields"],
                     "header_lost_buffers_fields": header["lost_buffers_fields"],
                     "statistics_lost_events_fields": statistics["lost_events_fields"],
                     "statistics_lost_buffers_fields": statistics["lost_buffers_fields"]},
            "marks": marks, "evidence": {"capture": capture_evidence, "header": header_evidence,
                                         "statistics": statistics_evidence,
                                         "etl": {"path": str(etl), "sha256": digest, "size_bytes": etl.stat().st_size}},
            "limits": ["JSON numeric seconds are approximations; exact rational fields are authoritative.",
                       "The ROI must lie inside the observed bounded-header timestamp span.",
                       "Rounded timestamp pairs that do not share an exact origin are rejected.",
                       "The caller supplies xperf text from this immutable ETL; hashes record supplied bytes.",
                       "Only the target handshake window and owned marks are selected.",
                       "Missing loss observations remain unknown; this mapping does not prove stack coverage."]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("capture", "header", "statistics", "output"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--marks", type=Path, help="xperf -a marks -nanoseconds output")
    args = parser.parse_args()
    try:
        result = derive_clock(args.capture, args.header, args.statistics, args.marks)
        with args.output.open("x", encoding="utf-8") as output:
            output.write(json.dumps(result, indent=2) + "\n")
        print(args.output.resolve())
    except (ClockError, OSError, UnicodeError, json.JSONDecodeError) as error:
        parser.exit(1, f"trace-clock: {error}\n")


if __name__ == "__main__":
    main()
