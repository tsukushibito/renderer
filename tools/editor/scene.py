"""Inspect and edit a registered Sashiko scene using its game's inspector."""
import argparse
from contextlib import contextmanager
import errno
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


@contextmanager
def edit_lock(path):
    # Keep a stable lock inode across atomic scene replacements. The OS releases
    # its lock on close or process death; the sidecar itself can remain empty.
    lock = path.with_name(path.name + ".sashiko-edit.lock")
    with lock.open("a+b") as handle:
        try:
            if os.name == "nt":
                import msvcrt
                handle.seek(0)
                msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as err:
            if err.errno not in (errno.EAGAIN, errno.EACCES, errno.EDEADLK):
                raise
            raise ValueError(f"Edit in progress for {path}; reload after the writer finishes") from err
        yield


def execute(inspector, document, edit):
    result = subprocess.run(
        [str(inspector.resolve())],
        input=json.dumps({"document": document, "edit": edit}, allow_nan=False),
        text=True, encoding="utf-8", capture_output=True, check=False,
    )
    if result.returncode:
        raise ValueError(result.stderr.strip() or f"inspector exited {result.returncode}")
    return json.loads(result.stdout)


def apply_changes(source, previous, updated):
    """Keep implicit defaults and the source's key order for unchanged objects."""
    if previous == updated:
        return source
    if isinstance(previous, dict) and isinstance(updated, dict) and isinstance(source, dict):
        result = {k: v for k, v in source.items() if k in updated}
        for key, value in updated.items():
            if key not in previous or previous[key] != value:
                result[key] = apply_changes(source.get(key), previous.get(key), value)
        return result
    if isinstance(previous, list) and isinstance(updated, list) and isinstance(source, list):
        if len(previous) == len(source) and len(updated) >= len(previous):
            return [apply_changes(s, p, n) for s, p, n in zip(source, previous, updated)] + updated[len(previous):]
    return updated


class Document:
    def __init__(self, path, inspector):
        self.path = path.resolve()
        self.inspector = inspector
        self.reload()

    def reload(self):
        original = self.path.read_bytes()
        view = execute(self.inspector, json.loads(original), {"op": "inspect"})
        self.original, self.view = original, view

    def edit(self, operation):
        candidate = execute(self.inspector, self.view["document"], operation)
        updated = apply_changes(json.loads(self.original), self.view["document"], candidate["document"])
        content = (json.dumps(updated, indent=2, ensure_ascii=False) + "\n").encode()
        with edit_lock(self.path):
            if self.path.read_bytes() != self.original:
                raise ValueError("Scene changed on disk. Reload before editing.")
            temporary = None
            try:
                with tempfile.NamedTemporaryFile(dir=self.path.parent, delete=False) as output:
                    temporary = Path(output.name)
                    output.write(content)
                    output.flush()
                    os.fsync(output.fileno())
                temporary.chmod(self.path.stat().st_mode)
                os.replace(temporary, self.path)
                self.original, self.view = content, candidate
            finally:
                if temporary is not None:
                    temporary.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scene", type=Path)
    parser.add_argument("--inspector", type=Path, required=True)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("inspect")
    gui = commands.add_parser("gui")
    gui.add_argument("--game", required=True, type=Path)
    gui.add_argument("--replay", type=Path)
    gui.add_argument("--catalog", type=Path, help="cooked character catalog for clip preview")
    for op in ("move", "place", "rotate", "scale", "set", "set-default", "reset"):
        cmd = commands.add_parser(op)
        cmd.add_argument("prefab" if op == "set-default" else "id")
        if op == "place":
            cmd.add_argument("prefab")
        if op in ("move", "place"):
            cmd.add_argument("translation", type=float, nargs=3, metavar="METRES")
        elif op == "rotate":
            cmd.add_argument("rotation", type=float, nargs=4, metavar="QUAT")
        elif op == "scale":
            cmd.add_argument("scale", type=float, nargs=3, metavar="FACTOR")
        else:
            cmd.add_argument("field")
            if op != "reset":
                cmd.add_argument("value", type=json.loads, help="JSON value")
    args = vars(parser.parse_args())
    path, inspector, op = args.pop("scene"), args.pop("inspector"), args.pop("command")
    document = Document(path, inspector)
    if op == "gui":
        from view import open_editor
        open_editor(document, args["game"].resolve(), args["replay"], args["catalog"])
    else:
        if op != "inspect":
            document.edit({"op": op.replace("-", "_"), **args})
        print(json.dumps(document.view, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as err:
        sys.exit(str(err))

