"""Inspect and edit a registered Sashiko scene using its game's inspector."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def execute(inspector, document, edit):
    result = subprocess.run(
        [str(inspector.resolve())],
        input=json.dumps({"document": document, "edit": edit}, allow_nan=False),
        text=True, encoding="utf-8", capture_output=True, check=False,
    )
    if result.returncode:
        raise ValueError(result.stderr.strip() or f"inspector exited {result.returncode}")
    return json.loads(result.stdout)


class Document:
    def __init__(self, path, inspector):
        self.path = path
        self.inspector = inspector
        self.reload()

    def reload(self):
        original = self.path.read_bytes()
        view = execute(self.inspector, json.loads(original), {"op": "inspect"})
        self.original, self.view = original, view

    def edit(self, operation):
        candidate = execute(self.inspector, self.view["document"], operation)
        content = (json.dumps(candidate["document"], indent=2, ensure_ascii=False) + "\n").encode()
        # A short exclusive edit lock serializes compare-and-replace among editors.
        lock = self.path.with_name(self.path.name + ".edit-lock")
        try:
            lock.mkdir()
        except FileExistsError as err:
            raise ValueError(f"Edit in progress ({lock}); reload after the writer finishes") from err
        temporary = None
        try:
            if self.path.read_bytes() != self.original:
                raise ValueError("Scene changed on disk. Reload before editing.")
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
            lock.rmdir()


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
    for op in ("move", "place", "set", "set-default", "reset"):
        cmd = commands.add_parser(op)
        cmd.add_argument("prefab" if op == "set-default" else "id")
        if op == "place":
            cmd.add_argument("prefab")
        if op in ("move", "place"):
            cmd.add_argument("translation", type=float, nargs=3, metavar="METRES")
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

