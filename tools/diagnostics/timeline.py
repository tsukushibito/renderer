"""Browse a Sashiko report or run a game with an interactive live inspector."""
import argparse
import csv
import json
from pathlib import Path
import subprocess
import sys
import threading


class Report:
    def __init__(self, directory):
        self.root = Path(directory).resolve()
        self.manifest = json.loads((self.root / "manifest.json").read_bytes())
        if self.manifest["version"] != 1:
            raise ValueError("unsupported report version")
        self.frames = {int(f["frame"]): f for f in self.manifest["frames"]}
        if "frame_map" in self.manifest["files"]:
            for row in self.rows("frame_map"):
                frame = dict(row["frame"], index=row["index"], pts=row["pts"])
                self.frames[int(frame["frame"])] = frame
        self.ticks = {int(t["snapshot"]["tick"]): t for t in self.rows("ticks")} if "ticks" in self.manifest["files"] else {}
        self.inputs = json.loads(self.path("replay").read_bytes())["inputs"] if "replay" in self.manifest["files"] else []
        self.performance = {r["frame"]: r for r in self.rows("performance")} if "performance" in self.manifest["files"] else {}
        self.native = {}
        session = self.manifest["session"]
        if Path(session).name == session:
            index = self.root.parent / (session + ".native.json")
            if index.is_file():
                native = json.loads(index.read_bytes())
                if native["session"] != session or native["file"] != session + ".native.csv":
                    raise ValueError("native metrics session mismatch")
                with (index.parent / native["file"]).open(encoding="utf-8", newline="") as source:
                    self.native = {int(row["frame"]) + native["warmup"]:row for row in csv.DictReader(source)}

    def path(self, role):
        path = (self.root / self.manifest["files"][role]).resolve()
        if not path.is_relative_to(self.root):
            raise ValueError(f"artifact {role} escapes report")
        return path

    def rows(self, role):
        with self.path(role).open(encoding="utf-8") as source:
            yield from (json.loads(line) for line in source)

    def at(self, *, tick=None, frame=None):
        if frame is not None:
            if frame in self.performance:
                tick = self.performance[frame]["tick"]
            elif frame not in self.frames:
                return {"frame": frame, "status": "NOT_CAPTURED", "gaps": self.manifest["gaps"]}
            else:
                tick = int(self.frames[frame]["tick"])
        row = self.ticks.get(tick)
        frames = [f for f in self.frames.values() if int(f["tick"]) == tick]
        detail = {"session": self.manifest["session"], "world": self.manifest["world"],
                  "tick": tick, "frames": frames, "outcome": self.manifest["outcome"],
                  "media": {role: str(self.path(role)) for role in ("video",)
                            if role in self.manifest["files"]}}
        for f in frames:
            if frame is not None and int(f["frame"]) != frame:
                continue
            role = f"frame_{int(f['frame']):06d}"
            if role not in self.manifest["files"] and len(self.manifest["frames"]) == 1:
                role = "image"
            if role in self.manifest["files"]:
                detail["media"][f["frame"]] = str(self.path(role))
        if frame in self.performance:
            sample = self.performance[frame]
            settings = self.manifest["settings"]["performance"]
            detail["performance"] = dict(sample, cpu_trace=str(self.path("cpu_trace")),
                tracy_marker=f"{self.manifest['session']} frame {frame} tick {tick}",
                clock=settings["clock"], native=self.native.get(sample["presentation"]),
                memory=next((m for m in reversed(settings["memory_samples"]) if m["frame"] <= frame), None))
            session = self.manifest["session"]
            if Path(session).name == session and (trace := self.root.parent / (session + ".tracy")).is_file():
                detail["performance"]["tracy"] = str(trace)
        if self.manifest["world"]=="bistro" and frames:
            return dict(detail, status="recorded_view", source=self.manifest["settings"]["source"])
        if row is None:
            return dict(detail, status="NOT_CAPTURED", gaps=self.manifest["gaps"])
        prefix = self.manifest["session"] + "/o"
        player = row.get("player_id", 0)
        events = {key: val for key, val in row["events"].items() if val}
        objects = {"subject": prefix + str(player)}
        decision = {}
        actor_end = {"status": "NOT_CAPTURED"}
        if damage := events.get("damage"):
            objects["actor"] = prefix + str(player + 1 + damage["enemy"])
            decision = damage
            actor_end = ({"position": row["actor_position"]} if "actor_position" in row
                         else {"status": "OBJECT_GONE"})
        return dict(detail, input=self.inputs[tick - 1] if 0 < tick <= len(self.inputs) else None,
                    events=events, decision=decision, objects=objects, phase="end_of_tick",
                    snapshot=row["snapshot"], actor_at_end=actor_end)

    def overview(self):
        frames = {}
        for frame in self.frames.values():
            frames.setdefault(int(frame["tick"]), []).append(frame["frame"])
        for tick in sorted(self.ticks.keys() | frames.keys()):
            events = self.ticks.get(tick, {}).get("events", {})
            yield {"tick": tick, "frames": frames.get(tick, []),
                   "input": self.inputs[tick - 1] if 0 < tick <= len(self.inputs) else None,
                   "events": {k: v for k, v in events.items() if v}}


def emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)


def interactive(command):
    """Keep the original stdio connection alive as the user inspects saved frames."""
    session = None
    world = None
    selected = None
    report = None
    with subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          encoding="utf-8") as game:
        def read():
            nonlocal session, world
            for line in game.stdout:
                value = json.loads(line)
                if value.get("id") == "hello" and value.get("ok"):
                    session = value["result"]["session"]
                    world = value["result"]["world"]
                emit(value)

        reader = threading.Thread(target=read, daemon=True)
        reader.start()

        def send(value):
            game.stdin.write(json.dumps(value) + "\n")
            game.stdin.flush()

        send({"id": "hello", "op": "hello"})
        print("open DIR | frame N | tick N | find | read [actor|subject] | wait | watch | pause | step | resume | JSON", file=sys.stderr)
        try:
            for seq, line in enumerate(sys.stdin, 1):
                try:
                    verb, _, arg = line.strip().partition(" ")
                    if verb == "open":
                        report = Report(arg)
                        selected = None
                        emit({"opened": str(report.root), "session": report.manifest["session"]})
                    elif verb in ("frame", "tick"):
                        if report is None:
                            raise ValueError("open a report first")
                        selected = report.at(**{verb: int(arg)})
                        emit(selected)
                    elif verb.startswith("{"):
                        send(json.loads(line))
                    elif verb in ("find", "read", "wait", "watch", "pause", "step", "resume"):
                        request = {"id": str(seq), "op": verb}
                        if verb in ("read", "wait", "watch"):
                            if not selected or (selected["session"], selected["world"]) != (session, world):
                                raise ValueError("select a frame from this live session and world")
                            role = arg or "subject"
                            request["object"] = selected["objects"][role]
                            if verb == "read":
                                request["fields"] = ["x", "z"]
                            else:
                                request["event"] = "damage"
                                request["role"] = role
                        send(request)
                    elif verb == "quit":
                        break
                    elif verb:
                        raise ValueError("unknown command")
                except (KeyError, OSError, ValueError) as error:
                    emit({"error": str(error)})
        finally:
            game.stdin.close()
            print("Inspector disconnected. Close the game window to finish pending reports.", file=sys.stderr)
            game.wait()
            reader.join(timeout=2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    saved = sub.add_parser("show")
    saved.add_argument("directory", type=Path)
    target = saved.add_mutually_exclusive_group()
    target.add_argument("--tick", type=int)
    target.add_argument("--frame", type=int)
    target.add_argument("--slowest", action="store_true", help="Show the slowest recorded presentation with CPU and native data")
    live = sub.add_parser("live")
    live.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.action == "live":
        command = args.command[1:] if args.command[:1] == ["--"] else args.command
        if not command:
            parser.error("live requires a game command with --inspect-stdio")
        interactive(command)
    else:
        report = Report(args.directory)
        if args.slowest:
            if not report.performance:
                parser.error("report has no presented frame timings")
            emit(report.at(frame=max(report.performance.values(), key=lambda r:r["interval_ns"])["frame"]))
        elif args.frame is not None or args.tick is not None:
            emit(report.at(frame=args.frame, tick=args.tick))
        else:
            emit({"session": report.manifest["session"], "outcome": report.manifest["outcome"],
                  "gaps": report.manifest["gaps"]})
            for row in report.overview():
                emit(row)


if __name__ == "__main__":
    main()
