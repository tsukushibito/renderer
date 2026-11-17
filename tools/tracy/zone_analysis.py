"""Analyze Tracy 0.14.1 raw CPU zones using bounded, disk-backed workspaces.

Raw events never accumulate in Python lists. SQLite sorts/pairs normalized
instances on disk, streams the nested trees, and indexes exact order statistics.
The configured cache and batches are bounded independently of draw/frame count;
the returned report retains only source/edge/thread aggregates. Times are wall
spans including waits/descheduling, not samples or charged CPU time.
"""
from collections import Counter, OrderedDict
import csv
from itertools import zip_longest
import math
from pathlib import Path
import sqlite3
import tempfile


HEADER = ["name", "src_file", "src_line", "ns_since_start", "exec_time_ns", "thread", "value"]
BATCH_ROWS = 2048
INTERN_CACHE_ENTRIES = 512
AGGREGATE_CACHE_ENTRIES = 512
SQLITE_CACHE_KIB = 8192
MAX_NESTING = 4096


def integer(value, label):
    try:
        result = int(value)
    except (TypeError, ValueError) as error:
        raise ValueError(f"Invalid Tracy integer {label}: {value!r}") from error
    if result < 0 or result > 2**63 - 1 or str(result) != str(value):
        raise ValueError(f"Expected nonnegative canonical 64-bit Tracy integer {label}")
    return result


def read_zones(path):
    """Yield one raw record at a time; upstream TSV has no quoting/escaping."""
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
            if row["ns_since_start"] > 2**63 - 1 - row["exec_time_ns"]:
                raise ValueError("Tracy interval exceeds the analysis integer range")
            yield row


class Workspace:
    """Disposable single-connection database; no journal, mmap or RAM temp store."""
    def __init__(self, directory):
        self.path = Path(directory) / "zones.sqlite"
        self.db = sqlite3.connect(self.path)
        for pragma in ("journal_mode=OFF", "synchronous=OFF", "temp_store=FILE", "mmap_size=0",
                       f"cache_size=-{SQLITE_CACHE_KIB}", "threads=1"):
            self.db.execute("PRAGMA " + pragma)
        self.db.executescript("""
            CREATE TABLE sources(id INTEGER PRIMARY KEY, name TEXT, file TEXT, line INTEGER,
                                 UNIQUE(name,file,line));
            CREATE TABLE texts(id INTEGER PRIMARY KEY, value TEXT UNIQUE);
            INSERT INTO texts VALUES(0,'');
            CREATE TABLE inclusive(seq INTEGER PRIMARY KEY, source_id INTEGER, kind INTEGER,
                                   start INTEGER, duration INTEGER, thread INTEGER, value_id INTEGER);
            CREATE TABLE exclusive(seq INTEGER PRIMARY KEY, source_id INTEGER, kind INTEGER,
                                   start INTEGER, duration INTEGER, thread INTEGER, value_id INTEGER);
            CREATE TABLE zones(seq INTEGER PRIMARY KEY, source_id INTEGER, kind INTEGER, start INTEGER,
                               duration INTEGER, self_ns INTEGER, thread INTEGER, value_id INTEGER, in_roi INTEGER DEFAULT 0);
            CREATE TABLE frames(id INTEGER PRIMARY KEY, start INTEGER, end INTEGER, duration INTEGER);
            CREATE TABLE edges(parent INTEGER, child INTEGER, calls INTEGER, PRIMARY KEY(parent,child)) WITHOUT ROWID;
            CREATE TABLE costs(source_id INTEGER, frame_id INTEGER, calls INTEGER, inclusive INTEGER,
                               self_ns INTEGER, PRIMARY KEY(source_id,frame_id)) WITHOUT ROWID;
        """)
        self.sources, self.texts = OrderedDict(), OrderedDict()
        self.peak_batch = self.peak_sources = self.peak_texts = 0
        self.peak_database_bytes = 0

    def close(self):
        self.db.close()

    def sample_database_size(self):
        self.peak_database_bytes = max(self.peak_database_bytes, self.path.stat().st_size)

    def intern(self, value, *, text=False):
        if text and value == "":
            return 0
        cache = self.texts if text else self.sources
        if value in cache:
            cache.move_to_end(value)
            return cache[value]
        if text:
            self.db.execute("INSERT OR IGNORE INTO texts(value) VALUES(?)", (value,))
            result = self.db.execute("SELECT id FROM texts WHERE value=?", (value,)).fetchone()[0]
        else:
            self.db.execute("INSERT OR IGNORE INTO sources(name,file,line) VALUES(?,?,?)", value)
            result = self.db.execute("SELECT id FROM sources WHERE name=? AND file=? AND line=?", value).fetchone()[0]
        if len(cache) >= INTERN_CACHE_ENTRIES:
            cache.popitem(last=False)
        cache[value] = result
        self.peak_sources, self.peak_texts = max(self.peak_sources, len(self.sources)), max(self.peak_texts, len(self.texts))
        return result

    def insert_batch(self, statement, batch):
        if batch:
            self.peak_batch = max(self.peak_batch, len(batch))
            self.db.executemany(statement, batch)
            batch.clear()

    def load(self, table, path):
        batch = []
        statement = f"INSERT INTO {table}(source_id,kind,start,duration,thread,value_id) VALUES(?,?,?,?,?,?)"
        for row in read_zones(path):
            source = self.intern((row["name"], row["src_file"], row["src_line"]))
            kind = {"bistro.measurement": 1, "bistro.frame": 2, "bistro.capture_identity": 3}.get(row["name"], 0)
            batch.append((source, kind, row["ns_since_start"], row["exec_time_ns"], row["thread"],
                          self.intern(row["value"], text=True)))
            if len(batch) >= BATCH_ROWS:
                self.insert_batch(statement, batch)
        self.insert_batch(statement, batch)
        self.db.execute(f"CREATE INDEX {table}_pair ON {table}(source_id,start,thread,value_id,seq,duration,kind)")
        self.db.commit()
        self.sample_database_size()

    def pair(self):
        # Independent exporter processes may enumerate source groups differently.
        # Match per-key occurrences by each export's original FIFO sequence, not
        # by whole-file row position and not by a Python map containing N keys.
        def rows(table):
            return self.db.execute(f"SELECT seq,source_id,kind,start,duration,thread,value_id FROM {table} "
                                   "ORDER BY source_id,start,thread,value_id,seq")
        batch = []
        statement = "INSERT INTO zones(seq,source_id,kind,start,duration,self_ns,thread,value_id) VALUES(?,?,?,?,?,?,?,?)"
        for inc, exc in zip_longest(rows("inclusive"), rows("exclusive")):
            if (inc is None or exc is None or (inc[1], inc[3], inc[5], inc[6]) != (exc[1], exc[3], exc[5], exc[6])):
                raise ValueError("Inclusive and self exports describe different zone instances")
            if exc[4] > inc[4]:
                raise ValueError("Self duration exceeds inclusive elapsed duration")
            batch.append((inc[0], inc[1], inc[2], inc[3], inc[4], exc[4], inc[5], inc[6]))
            if len(batch) >= BATCH_ROWS:
                self.insert_batch(statement, batch)
        self.insert_batch(statement, batch)
        self.db.commit()
        self.sample_database_size()
        self.db.executescript("DROP TABLE inclusive; DROP TABLE exclusive;")

    def source(self, source_id):
        name, file, line = self.db.execute("SELECT name,file,line FROM sources WHERE id=?", (source_id,)).fetchone()
        return {"name": name, "src_file": file, "src_line": line}

    def percentiles(self, table, column, count, *, source_id=None, zeros=0):
        # Indexed per-source ranges make all source queries together O(N), not
        # U full-event scans. Missing frame/source pairs contribute exact zeros.
        where, args = "", ()
        if source_id is not None:
            where, args = " WHERE source_id=?", (source_id,)
            if table == "zones":
                where += " AND in_roi=1"
        ranks = {}
        def at(rank):
            if rank < zeros:
                return 0
            rank -= zeros
            if rank not in ranks:
                row = self.db.execute(f"SELECT {column} FROM {table}{where} ORDER BY {column} LIMIT 1 OFFSET ?", (*args, rank)).fetchone()
                if row is None:
                    raise ValueError("Indexed percentile count differs from complete exported instances")
                ranks[rank] = row[0]
            return ranks[rank]
        result = []
        for fraction in (0.5, 0.95):
            index = (count - 1) * fraction
            low, high = math.floor(index), math.ceil(index)
            result.append(at(low) + (at(high) - at(low)) * (index - low))
        return result


def _validate_trees(workspace, root_thread, expected_frames):
    db = workspace.db
    db.execute("CREATE INDEX zones_tree ON zones(thread,start,duration DESC,self_ns,seq) WHERE in_roi=1")
    edges = Counter()
    threads = Counter()
    stack = []
    current_thread, frame_count, previous_frame_end, ambiguous = None, 0, None, 0
    def finish(node):
        nonlocal ambiguous
        if node["self"] != node["duration"] - node["child_ns"]:
            raise ValueError("Nested zone/self export is incomplete or inconsistent")
        parent = node["parent"]
        if parent is not None:
            if (node["ambiguous_equal"] or parent["ambiguous_equal"]
                    or (node["start"] == parent["start"] and node["end"] == parent["end"])):
                # Equal span/self ancestors are interchangeable in raw CSV.
                # Neither their incoming edge nor a shorter child's immediate
                # parent can be attributed to one member of that group.
                ambiguous += 1
            else:
                edges[(parent["source"], node["source"])] += 1
                if len(edges) >= AGGREGATE_CACHE_ENTRIES:
                    flush_edges()
    def flush_edges():
        db.executemany("INSERT INTO edges VALUES(?,?,?) ON CONFLICT(parent,child) DO UPDATE SET calls=calls+excluded.calls",
                       ((parent, child, calls) for (parent, child), calls in edges.items()))
        edges.clear()
    for source, kind, start, duration, self_ns, thread in db.execute(
            "SELECT source_id,kind,start,duration,self_ns,thread FROM zones WHERE in_roi=1 "
            "ORDER BY thread,start,duration DESC,self_ns,seq"):
        if thread != current_thread:
            for node in stack:
                finish(node)
            stack.clear()
            current_thread = thread
        while stack and stack[-1]["end"] <= start:
            finish(stack.pop())
        end = start + duration
        parent, ambiguous_equal = None, False
        if stack:
            parent = stack[-1]
            if end > parent["end"]:
                raise ValueError("CPU zones cross instead of nesting on one thread")
            parent["child_ns"] += duration
            if start == parent["start"] and end == parent["end"] and self_ns == parent["self"]:
                parent["ambiguous_equal"] = ambiguous_equal = True
        if len(stack) >= MAX_NESTING:
            raise ValueError(f"Trace exceeds explicit analysis nesting capacity {MAX_NESTING}")
        stack.append({"source": source, "start": start, "end": end, "duration": duration, "self": self_ns,
                      "child_ns": 0, "parent": parent, "ambiguous_equal": ambiguous_equal})
        threads[thread] += 1
        if kind == 2:
            if thread != root_thread:
                raise ValueError("Frame zones must run on the measurement thread")
            if previous_frame_end is not None and previous_frame_end > start:
                raise ValueError("Measured frame zones overlap")
            db.execute("INSERT INTO frames VALUES(?,?,?,?)", (frame_count, start, end, duration))
            frame_count += 1
            previous_frame_end = end
    for node in stack:
        finish(node)
    flush_edges()
    if frame_count != expected_frames:
        raise ValueError(f"Expected {expected_frames} complete frame zones; exported {frame_count}")
    db.execute("CREATE INDEX frames_start ON frames(start,id)")
    db.execute("CREATE INDEX frames_duration ON frames(duration)")
    db.commit()
    return threads, ambiguous


def _frame_costs(workspace, root_thread):
    """One chronological pass for all sources, including equal-interval parents."""
    db = workspace.db
    frames = iter(db.execute("SELECT id,start,end FROM frames ORDER BY start,id"))
    following, current = next(frames, None), None
    costs = {}
    def flush():
        db.executemany("INSERT INTO costs VALUES(?,?,?,?,?) ON CONFLICT(source_id,frame_id) DO UPDATE SET "
                       "calls=calls+excluded.calls,inclusive=inclusive+excluded.inclusive,self_ns=self_ns+excluded.self_ns",
                       ((source, frame, *values) for (source, frame), values in costs.items()))
        costs.clear()
    for source, start, duration, self_ns in db.execute(
            "SELECT source_id,start,duration,self_ns FROM zones WHERE in_roi=1 AND thread=? "
            "ORDER BY start,duration DESC,self_ns,seq", (root_thread,)):
        while following is not None and following[1] <= start:
            current, following = following, next(frames, None)
        if current is not None and start + duration <= current[2]:
            values = costs.setdefault((source, current[0]), [0, 0, 0])
            values[0] += 1
            values[1] += duration
            values[2] += self_ns
            if len(costs) >= AGGREGATE_CACHE_ENTRIES:
                flush()
    flush()
    for table, column in (("zones", "duration"), ("zones", "self_ns"), ("costs", "inclusive"), ("costs", "self_ns")):
        suffix = " WHERE in_roi=1" if table == "zones" else ""
        db.execute(f"CREATE INDEX {table}_{column} ON {table}(source_id,{column})" + suffix)
    db.commit()
    workspace.sample_database_size()


def _analyze(workspace, inclusive_path, self_path, expected_frames, capture_id):
    workspace.load("inclusive", inclusive_path)
    workspace.load("exclusive", self_path)
    workspace.pair()
    db = workspace.db
    identity_rows = db.execute("SELECT z.start,z.duration,z.thread,t.value FROM zones z JOIN texts t ON t.id=z.value_id WHERE kind=3").fetchmany(2)
    if len(identity_rows) != 1 or identity_rows[0][3] != capture_id:
        raise ValueError("Trace does not identify the exact owned target capture nonce")
    roots = db.execute("SELECT start,duration,thread FROM zones WHERE kind=1").fetchmany(2)
    if len(roots) != 1:
        raise ValueError("Trace must contain exactly one complete bistro.measurement ROI")
    begin, elapsed, root_thread = roots[0]
    end = begin + elapsed
    if elapsed <= 0:
        raise ValueError("Measurement ROI must have positive duration")
    identity = identity_rows[0]
    if identity[2] != root_thread or identity[0] + identity[1] > begin:
        raise ValueError("Owned capture identity must precede the ROI on its target thread")
    db.execute("UPDATE zones SET in_roi=1 WHERE start>=? AND start+duration<=?", (begin, end))
    outside = db.execute("SELECT COUNT(*) FROM zones WHERE in_roi=0").fetchone()[0]
    threads, ambiguous = _validate_trees(workspace, root_thread, expected_frames)
    _frame_costs(workspace, root_thread)
    zones = []
    for source, count, total, self_total in db.execute(
            "SELECT source_id,COUNT(*),SUM(duration),SUM(self_ns) FROM zones WHERE in_roi=1 GROUP BY source_id"):
        inc_p50, inc_p95 = workspace.percentiles("zones", "duration", count, source_id=source)
        self_p50, self_p95 = workspace.percentiles("zones", "self_ns", count, source_id=source)
        observed, frame_contained = db.execute("SELECT COUNT(*),COALESCE(SUM(calls),0) FROM costs WHERE source_id=?", (source,)).fetchone()
        frame_aggregation = None
        if frame_contained:
            frame_inc = workspace.percentiles("costs", "inclusive", expected_frames, source_id=source, zeros=expected_frames - observed)
            frame_self = workspace.percentiles("costs", "self_ns", expected_frames, source_id=source, zeros=expected_frames - observed)
            frame_aggregation = {"samples": expected_frames, "inclusive_p50_ns": frame_inc[0], "inclusive_p95_ns": frame_inc[1],
                                 "self_p50_ns": frame_self[0], "self_p95_ns": frame_self[1],
                                 "scope": "Sum complete instances contained in each main-thread frame zone, including zero for frames without a call; excludes final drain and calls outside frames."}
        zones.append({**workspace.source(source), "calls": count, "calls_per_frame": count / expected_frames,
                      "inclusive_ns": total, "self_ns": self_total, "inclusive_ns_per_frame": total / expected_frames,
                      "self_ns_per_frame": self_total / expected_frames, "inclusive_call_p50_ns": inc_p50,
                      "inclusive_call_p95_ns": inc_p95, "self_call_p50_ns": self_p50, "self_call_p95_ns": self_p95,
                      "frame_contained_calls": frame_contained, "outside_frame_calls": count - frame_contained,
                      "per_frame_aggregation": frame_aggregation})
    zones.sort(key=lambda row: row["self_ns"], reverse=True)
    edges = [{"parent": workspace.source(parent), "child": workspace.source(child), "calls": calls}
             for parent, child, calls in db.execute("SELECT parent,child,calls FROM edges ORDER BY parent,child")]
    edges.sort(key=lambda row: (*row["parent"].values(), *row["child"].values()))
    frame_p50, frame_p95 = workspace.percentiles("frames", "duration", expected_frames)
    frame_total = db.execute("SELECT SUM(duration) FROM frames").fetchone()[0]
    return {"schema_version": 1, "capture_id": capture_id, "trace_identity_verified": True,
            "roi": {"start_ns": begin, "end_ns": end, "elapsed_ns": elapsed},
            "measured_frame_zones": expected_frames, "cpu_zone_events": sum(threads.values()),
            "cpu_threads_with_zones": len(threads), "thread_zone_counts": dict(threads),
            "outside_roi_zone_events": outside, "zones": zones, "parent_edges": edges,
            "ambiguous_equal_interval_parent_edges": ambiguous,
            "frame_zone_elapsed": {"samples": expected_frames, "mean_ns": frame_total / expected_frames,
                                   "p50_ns": frame_p50, "p95_ns": frame_p95},
            "analysis_storage": {"backend": "temporary disk-backed SQLite", "sqlite_cache_kib": SQLITE_CACHE_KIB,
                "temp_store": "FILE", "mmap_enabled": False, "event_batch_capacity": BATCH_ROWS,
                "peak_event_batch_rows": workspace.peak_batch, "intern_cache_capacity": INTERN_CACHE_ENTRIES,
                "peak_source_cache_entries": workspace.peak_sources, "peak_text_cache_entries": workspace.peak_texts,
                "aggregate_cache_capacity": AGGREGATE_CACHE_ENTRIES, "nesting_capacity": MAX_NESTING,
                "peak_database_file_bytes": workspace.peak_database_bytes,
                "scope": "Event storage/sorts are disk-backed; configured SQLite cache, batches, intern caches and nesting stack are bounded. Returned source/edge/thread aggregates scale with report metadata. SQLite transient bookkeeping and OS file cache are not a total-process RSS guarantee."},
            "semantics": SEMANTICS}


def analyze_zones(inclusive_path, self_path, expected_frames, capture_id):
    if type(expected_frames) is not int or expected_frames <= 0:
        raise ValueError("Expected a positive measured frame count")
    if not isinstance(capture_id, str) or len(capture_id) != 32 or any(char not in "0123456789abcdef" for char in capture_id):
        raise ValueError("Analysis requires the planned per-run capture nonce")
    # Keep temporary storage beside the exports: it is owned output, uses their
    # chosen volume, and is removed on success or failure. Source TSVs are read-only.
    with tempfile.TemporaryDirectory(prefix=".tracy-analysis-", dir=Path(inclusive_path).resolve().parent) as directory:
        workspace = Workspace(directory)
        try:
            return _analyze(workspace, inclusive_path, self_path, expected_frames, capture_id)
        except (sqlite3.Error, csv.Error) as error:
            raise ValueError(f"Disk-backed Tracy analysis failed: {error}") from error
        finally:
            workspace.close()


SEMANTICS = {
    "time_unit": "ns",
    "inclusive": "Elapsed zone wall span including nested children, waits and descheduling; never sum nested inclusive costs.",
    "self": "Elapsed zone wall span less direct child spans from matched csvexport -u -e; not charged CPU or sampling evidence.",
    "frame_zone": "bistro.frame CPU iteration zone; excludes final drain, inter-iteration gaps, load, warmup, post-ROI readback and shutdown. Separate runner frame_wall_ns retains its documented latency scope.",
    "coverage": "Completed exported positive-duration explicit zones only. csvexport omits incomplete/non-positive spans, so coverage of omitted zones elsewhere is unknown. Closed measurement/frame zones, nonce and nested self arithmetic are verified. Uninstrumented driver, OS and Rust work has no independent function attribution; no scheduler/running/ready/blocked evidence.",
    "percentiles": "Linear interpolation over individual complete instances, including all raw frame zones; per-call zone percentiles are not per-frame percentiles.",
    "thread_identifiers": "Identifiers printed by csvexport's ZoneThreadData::Thread(); thread counts concern exported zones, not all process threads or an independently verified mapping to Windows TIDs.",
    "frame_marks": "Tracy 0.14.1 csvexport does not export frame sets; measured_frame_zones verifies explicit frame zones, not viewer bootstrap/continuous frame mark count.",
    "parent_edges": "Only recoverable immediate-parent edges are emitted. Equal-interval edges and edges into or out of an interchangeable equal-interval/self ancestor group are counted as ambiguous; raw source-group enumeration never establishes their exact nesting order."
}
