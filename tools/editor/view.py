"""Desktop view for the same scene commands as scene.py."""
import json
import subprocess
import tempfile
import tkinter as tk
from tkinter import messagebox, simpledialog, ttk


class Editor:
    def __init__(self, window, document, game, replay, catalog):
        self.window, self.doc, self.game, self.replay = window, document, game, replay
        self.job = None
        self.catalog = catalog
        self.drag = None
        self.keys = []
        self.selected = None
        self.tick = tk.StringVar(value="48")
        window.title(f"Sashiko · {document.path.name}")
        window.geometry("1050x620")
        window.protocol("WM_DELETE_WINDOW", self.close)

        bar = ttk.Frame(window, padding=6)
        bar.pack(fill="x")
        for label, fn in [("Reload", self.reload), ("Play", self.play), ("Stop", self.stop)]:
            ttk.Button(bar, text=label, command=fn).pack(side="left", padx=2)
        ttk.Label(bar, text="Tick").pack(side="left", padx=(16, 4))
        ttk.Entry(bar, textvariable=self.tick, width=7).pack(side="left")
        ttk.Button(bar, text="Inspect state", command=self.snapshot).pack(side="left", padx=3)
        ttk.Button(bar, text="Preview clip", command=self.preview_clip).pack(side="left", padx=3)

        panes = ttk.Panedwindow(window, orient="horizontal")
        panes.pack(fill="both", expand=True)
        self.items = tk.Listbox(panes, exportselection=False, width=19)
        self.items.bind("<<ListboxSelect>>", self.select_list)
        panes.add(self.items, weight=0)
        self.map = tk.Canvas(panes, background="#19232b", highlightthickness=0)
        self.map.bind("<Configure>", lambda _: self.draw())
        self.map.bind("<ButtonPress-1>", self.press)
        self.map.bind("<ButtonRelease-1>", self.release)
        panes.add(self.map, weight=3)

        right = ttk.Frame(panes, padding=8)
        panes.add(right, weight=1)
        self.title = ttk.Label(right, text="Properties")
        self.title.pack(anchor="w", pady=4)
        self.table = ttk.Treeview(right, columns=("value", "original", "source"), show="tree headings",
                                  selectmode="browse", height=15)
        self.table.heading("#0", text="Field")
        self.table.heading("value", text="Value")
        self.table.heading("original", text="Prefab")
        self.table.heading("source", text="Source")
        for col, width in [("#0", 96), ("value", 100), ("original", 90), ("source", 75)]:
            self.table.column(col, width=width, stretch=True)
        self.table.pack(fill="both", expand=True)
        self.table.bind("<Double-1>", lambda _: self.edit_value())
        ttk.Button(right, text="Edit value", command=self.edit_value).pack(fill="x", pady=(5, 2))
        ttk.Button(right, text="Use prefab value", command=self.reset_value).pack(fill="x")
        ttk.Button(right, text="Edit prefab default", command=self.edit_default).pack(fill="x")
        ttk.Button(right, text="Place instance", command=self.place).pack(fill="x")
        self.status = tk.StringVar()
        ttk.Label(window, textvariable=self.status, anchor="w", padding=5).pack(fill="x")
        self.refresh()

    def error(self, exc):
        messagebox.showerror("Sashiko", str(exc), parent=self.window)

    def reload(self):
        try:
            self.doc.reload()
            self.refresh()
        except (ValueError, OSError) as err:
            self.error(err)

    def refresh(self):
        self.entities = {e["id"]: e for e in self.doc.view["entities"]}
        self.placements = {p["id"]: p for p in self.doc.view["document"]["placements"]}
        self.keys = list(self.entities)
        self.items.delete(0, "end")
        for key in self.keys:
            self.items.insert("end", key)
        self.choose(self.selected if self.selected in self.keys else next(iter(self.keys), None))

    def choose(self, key):
        self.selected = key
        self.items.selection_clear(0, "end")
        self.table.delete(*self.table.get_children())
        if key is not None:
            self.items.selection_set(self.keys.index(key))
            self.items.see(self.keys.index(key))
            e, p = self.entities[key], self.placements[key]
            self.title.config(text=f"{e['kind']}  ·  {p['prefab']}")
            for field in ("translation", "rotation", "scale"):
                self.table.insert("", "end", iid="@" + field, text=field,
                                  values=(json.dumps(p[field]), "", "local"))
            defaults = self.doc.view["document"]["prefabs"][p["prefab"]]["fields"]
            for name in e["registered_fields"]:
                source = "override" if name in p.get("overrides", {}) else p["prefab"]
                self.table.insert("", "end", iid=name, text=name,
                                  values=(json.dumps(e["fields"].get(name)), json.dumps(defaults.get(name)), source))
        self.draw()

    def select_list(self, _):
        selection = self.items.curselection()
        if selection:
            self.choose(self.keys[selection[0]])

    def draw(self):
        if not hasattr(self, "entities"):
            return
        self.map.delete("all")
        w, h = self.map.winfo_width(), self.map.winfo_height()
        boxes = []
        for key, e in self.entities.items():
            x, _, z = e["world"]
            dims = e["fields"].get("half_extent")
            dx, dz = 0.4, 0.4
            if isinstance(dims, list) and len(dims) == 2:
                u, v = e["axis_x"], e["axis_z"]
                dx = abs(dims[0] * u[0]) + abs(dims[1] * v[0])
                dz = abs(dims[0] * u[2]) + abs(dims[1] * v[2])
            boxes.append((key, x, z, dx, dz))
        ex = max([2] + [abs(x) + dx for _, x, _, dx, _ in boxes])
        ez = max([2] + [abs(z) + dz for _, _, z, _, dz in boxes])
        self.scale = max(1e-6, min((w - 24) / (2*ex), (h - 24) / (2*ez)))
        self.hits = {}
        for key, x, z, dx, dz in sorted(boxes, key=lambda b: b[3]*b[4], reverse=True):
            a, b = w/2 + x*self.scale, h/2 + z*self.scale
            sx, sz = max(4, dx*self.scale), max(4, dz*self.scale)
            item = self.map.create_rectangle(a-sx, b-sz, a+sx, b+sz,
                                             fill="#35505a", outline=("#ffd28d" if key == self.selected else "#8ab5b2"),
                                             width=(3 if key == self.selected else 1))
            self.hits[item] = key

    def press(self, event):
        self.drag = None
        for item in reversed(self.map.find_overlapping(event.x-1, event.y-1, event.x+1, event.y+1)):
            if item in self.hits:
                key = self.hits[item]
                self.choose(key)
                self.drag = (key, event.x, event.y, self.scale)
                break

    def release(self, event):
        if not self.drag:
            return
        key, x, y, scale = self.drag
        self.drag = None
        if abs(x-event.x) + abs(y-event.y) < 4:
            return
        self.commit({"op": "translate_world", "id": key, "delta": [(event.x-x)/scale, 0, (event.y-y)/scale]})

    def commit(self, edit):
        try:
            self.doc.edit(edit)
            self.refresh()
            self.status.set(f"Saved {self.doc.path}")
        except (ValueError, OSError) as err:
            self.error(err)

    def edit_value(self):
        entries = self.table.selection()
        if not self.selected or not entries:
            return
        field = entries[0]
        p = self.placements[self.selected]
        if field.startswith("@"):
            original = p[field[1:]]
        else:
            original = self.entities[self.selected]["fields"].get(field)
        value = simpledialog.askstring("Edit value", f"{self.selected} / {field}",
                                       initialvalue=json.dumps(original), parent=self.window)
        if value is None:
            return
        try:
            parsed = json.loads(value)
        except ValueError as err:
            self.error(err)
            return
        if field.startswith("@"):
            name = field[1:]
            op = {"translation":"move", "rotation":"rotate", "scale":"scale"}[name]
            self.commit({"op":op, "id":self.selected, name:parsed})
        else:
            self.commit({"op":"set", "id":self.selected, "field":field, "value":parsed})

    def reset_value(self):
        selected = self.table.selection()
        if selected and self.selected and selected[0] in self.placements[self.selected].get("overrides", {}):
            self.commit({"op":"reset", "id":self.selected, "field":selected[0]})

    def edit_default(self):
        selected = self.table.selection()
        if not self.selected or not selected or selected[0].startswith("@"):
            return
        name = selected[0]
        prefab = self.placements[self.selected]["prefab"]
        default = self.doc.view["document"]["prefabs"][prefab]["fields"].get(name)
        value = simpledialog.askstring("Edit prefab", f"{prefab} / {name}",
                                       initialvalue=json.dumps(default), parent=self.window)
        if value is not None:
            try:
                self.commit({"op":"set_default", "prefab":prefab, "field":name, "value":json.loads(value)})
            except ValueError as err:
                self.error(err)

    def place(self):
        prefab = simpledialog.askstring("Place instance", "Prefab ID",
                                      initialvalue=self.placements[self.selected]["prefab"] if self.selected else "",
                                      parent=self.window)
        if not prefab:
            return
        key = simpledialog.askstring("Place instance", "New placement ID", parent=self.window)
        if not key:
            return
        point = simpledialog.askstring("Place instance", "Position [X, Y, Z] in metres",
                                       initialvalue="[0, 0, 0]", parent=self.window)
        if point:
            try:
                self.commit({"op":"place", "prefab":prefab, "id":key, "translation":json.loads(point)})
                if key in self.placements:
                    self.choose(key)
            except ValueError as err:
                self.error(err)

    def run(self, args):
        if self.job:
            self.status.set("Stop the current preview first")
            return
        command = [self.game, "--arena", self.doc.path.resolve(), *args]
        if self.replay:
            command.extend(["--replay", self.replay.resolve()])
        try:
            self.log = tempfile.TemporaryFile()
            self.job = subprocess.Popen(command, stdout=self.log, stderr=subprocess.STDOUT)
        except OSError as err:
            if getattr(self, "log", None):
                self.log.close()
            self.error(err)
            return
        self.status.set("Preview running")
        self.window.after(80, self.poll)

    def poll(self):
        if not self.job:
            return
        if self.job.poll() is None:
            self.window.after(80, self.poll)
            return
        size = self.log.seek(0, 2)
        self.log.seek(max(0, size - 65536))
        log = self.log.read().decode("utf-8", "replace").strip()
        self.log.close()
        code = self.job.returncode
        self.job = None
        self.status.set("Preview finished")
        if code:
            self.error(log[-2400:] or f"Preview exited with {code}")
        elif log:
            result = tk.Toplevel(self.window)
            result.title("Game state")
            text = tk.Text(result, width=55, height=24, wrap="word")
            text.pack(fill="both", expand=True)
            try:
                log = json.dumps(json.loads(log.splitlines()[-1]), indent=2)
            except ValueError:
                pass
            text.insert("1.0", log)
            text.config(state="disabled")

    def play(self):
        self.run(["--window"])

    def snapshot(self):
        try:
            tick = int(self.tick.get())
            if tick < 0:
                raise ValueError("Tick must be non-negative")
        except ValueError as err:
            self.error(err)
            return
        self.run(["--headless", "--ticks", str(tick)])

    def preview_clip(self):
        from clip import show_clip
        fields = self.entities.get(self.selected, {}).get("fields", {})
        show_clip(self.window, self.doc.inspector, self.catalog,
                  fields.get("model"), fields.get("clip"))

    def stop(self):
        if self.job:
            self.job.terminate()
            self.job.wait()
            self.log.close()
            self.job = None
            self.status.set("Preview stopped")

    def close(self):
        self.stop()
        self.window.destroy()


def open_editor(document, game, replay=None, catalog=None):
    root = tk.Tk()
    Editor(root, document, game, replay, catalog)
    root.mainloop()

