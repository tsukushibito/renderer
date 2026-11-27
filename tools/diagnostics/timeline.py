"""Browse a Sashiko report or run a game with an interactive live inspector."""
import argparse
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
        self.ticks = {int(t["snapshot"]["tick"]): t for t in self.rows("ticks")}
        self.inputs = json.loads(self.path("replay").read_bytes())["inputs"]

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
            if frame not in self.frames:
                return {"frame": frame, "status": "NOT_CAPTURED", "gaps": self.manifest["gaps"]}
            tick = int(self.frames[frame]["tick"])
        row = self.ticks.get(tick)
        frames = [f for f in self.frames.values() if int(f["tick"]) == tick]
        detail = {"session": self.manifest["session"], "world": self.manifest["world"],
                  "tick": tick, "frames": frames, "outcome": self.manifest["outcome"],
                  "media": {role: str(self.path(role)) for role in ("image", "video")
                            if role in self.manifest["files"]}}
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
        if args.frame is not None or args.tick is not None:
            emit(report.at(frame=args.frame, tick=args.tick))
        else:
            emit({"session": report.manifest["session"], "outcome": report.manifest["outcome"],
                  "gaps": report.manifest["gaps"]})
            for row in report.overview():
                emit(row)


if __name__ == "__main__":
    main()
