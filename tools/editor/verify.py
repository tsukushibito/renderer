"""Exercise the CLI backend, file updates and an optional native editor session."""
import argparse
from pathlib import Path
import json
import multiprocessing
import subprocess
import tempfile
import threading
from scene import Document, edit_lock


def hold_lock(path, ready):
    with edit_lock(path):
        ready.set()
        threading.Event().wait()


def interrupt_writer(doc):
    ctx = multiprocessing.get_context("spawn")
    ready = ctx.Event()
    writer = ctx.Process(target=hold_lock, args=(doc.path, ready))
    writer.start()
    try:
        assert ready.wait(15), "failed to acquire the editor lock"
        try:
            doc.edit({"op":"move", "id":"bench-west","translation":[-4,0,0]})
        except ValueError as err:
            assert "Edit in progress" in str(err), err
        else:
            raise AssertionError("two writers held the editor lock")
        assert doc.path.read_bytes() == doc.original
    finally:
        writer.kill()
        writer.join()
        writer.close()


def check(inspector, game, arena, catalog, gui):
    with tempfile.TemporaryDirectory() as tmp:
        working = Path(tmp) / "arena.json"
        working.write_bytes(arena.read_bytes())
        doc = Document(working, inspector)
        stale = Document(working, inspector)
        interrupt_writer(doc)
        doc.edit({"op":"set","id":"bench-west","field":"half_extent","value":[2, 1]})
        before = working.read_bytes()
        for editor, op in [(stale, {"op":"move","id":"bench-west","translation":[-4,0,0]}),
                           (doc, {"op":"set","id":"bench-west","field":"half_extent","value":[-1,1]})]:
            try:
                editor.edit(op)
            except ValueError:
                pass
            else:
                raise AssertionError("stale/invalid edit unexpectedly succeeded")
            assert working.read_bytes() == before
        doc.edit({"op":"reset","id":"bench-west","field":"half_extent"})
        doc.edit({"op":"place","id":"extra","prefab":"table","translation":[0,0,4.5]})
        doc.edit({"op":"set","id":"extra","field":"half_extent","value":[0.8,0.3]})
        route = Path(tmp) / "route.json"
        route.write_text(json.dumps([{"movement":[0,-1]}]*48), encoding="utf-8")
        def run(path):
            result = subprocess.check_output([game, "--headless", "--arena", path, "--replay", route], text=True)
            return json.loads(result.strip().splitlines()[-1])
        assert run(arena)["marker_entries"] == 1
        assert run(working)["marker_entries"] == 0
        def pose(sample):
            args = [inspector, "clip", catalog, "primary", "walk", str(sample), "60"]
            return json.loads(subprocess.check_output(args))
        first, second = pose(0), pose(20)
        assert first["indices"] == second["indices"] and first["vertices"] != second["vertices"]
        if gui:
            import tkinter as tk
            from view import Editor
            from clip import show_clip
            root = tk.Tk()
            editor = Editor(root, doc, game, route, catalog)
            root.update()
            editor.choose("bench-west")
            assert editor.table.exists("half_extent") and editor.map.find_all()
            show_clip(root, inspector, catalog, "primary", "walk")
            root.update()
            def widgets(parent):
                for widget in parent.winfo_children():
                    yield widget
                    yield from widgets(widget)
            assert any(isinstance(w, tk.Canvas) and any(w.type(i) == "polygon" for i in w.find_all())
                       for w in widgets(root))
            editor.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inspector", required=True, type=Path)
    parser.add_argument("--game", required=True, type=Path)
    parser.add_argument("--arena", type=Path, default=Path("assets/survivor/arena.json"))
    parser.add_argument("--catalog", type=Path, default=Path("assets/survivor/characters/catalog.json"))
    parser.add_argument("--gui", action="store_true")
    args = parser.parse_args()
    check(args.inspector.resolve(), args.game.resolve(), args.arena.resolve(), args.catalog.resolve(), args.gui)
    print("Scene property, disk conflict, game replay and clip preview checks passed.")

