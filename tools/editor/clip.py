"""Scrub the evaluated vertices of a cooked character clip."""
import json
import math
from pathlib import Path
import subprocess
import tkinter as tk
from tkinter import filedialog, messagebox, ttk


def show_clip(parent, inspector, catalog, model, clip):
    if catalog is None:
        picked = filedialog.askopenfilename(parent=parent, title="Character catalog",
                                            filetypes=[("Character catalog", "*.json")])
        if not picked:
            return
        catalog = Path(picked)
    try:
        entries = json.loads(catalog.read_text(encoding="utf-8"))["models"]
        if not entries:
            raise ValueError("Catalog has no models")
    except (OSError, KeyError, ValueError) as err:
        messagebox.showerror("Clip preview", str(err), parent=parent)
        return
    window = tk.Toplevel(parent)
    window.title("Character clips")
    bar = ttk.Frame(window, padding=6)
    bar.pack(fill="x")
    model_name = tk.StringVar(value=model if model in entries else next(iter(entries)))
    name = tk.StringVar(value=clip or "")
    models = ttk.Combobox(bar, values=list(entries), textvariable=model_name, state="readonly")
    models.pack(side="left")
    names = ttk.Combobox(bar, textvariable=name, state="readonly")
    names.pack(side="left")
    image = tk.Canvas(window, background="#18242c", width=440, height=440)
    image.pack(fill="both", expand=True)
    frame = tk.IntVar(value=0)
    scrub = tk.Scale(window, from_=0, to=1, variable=frame, orient="horizontal", label="Sample")
    scrub.pack(fill="x")
    playing = False
    timer = None
    cache = {}

    def render(_=None):
        sample = frame.get()
        key = (model_name.get(), name.get(), sample)
        try:
            if key not in cache:
                proc = subprocess.run([inspector.resolve(), "clip", catalog.resolve(), key[0],
                                       key[1], str(sample)], capture_output=True, text=True,
                                      encoding="utf-8")
                if proc.returncode:
                    raise ValueError(proc.stderr.strip())
                data = json.loads(proc.stdout)
                coords = [(x*.82-z*.57, -y*.97+(.57*x+.82*z)*.24, .57*x+.82*z)
                          for x, y, z in data["vertices"]]
                faces = []
                indices = data["indices"]
                for a,b,c in zip(indices[::3], indices[1::3], indices[2::3]):
                    p, q, r = coords[a], coords[b], coords[c]
                    u = [j-i for i,j in zip(p,q)]
                    v = [j-i for i,j in zip(p,r)]
                    nx, ny, nz = (u[1]*v[2]-u[2]*v[1], u[2]*v[0]-u[0]*v[2], u[0]*v[1]-u[1]*v[0])
                    length = math.hypot(nx, ny, nz)
                    if not length:
                        continue
                    shade = round(85 + 135*abs((nx*.36+ny*.48+nz*.8)/length))
                    faces.append((p[2]+q[2]+r[2], p[:2]+q[:2]+r[:2],
                                  f"#{shade*3//4:02x}{shade:02x}{shade:02x}"))
                ys = [p[1] for p in coords]
                xs = [p[0] for p in coords]
                if len(cache) >= 24:
                    cache.pop(next(iter(cache)))
                cache[key] = (sorted(faces, key=lambda f:f[0]), min(xs), max(xs), min(ys), max(ys))
            faces, left, right, top, bottom = cache[key]
            w, h = max(50, image.winfo_width()), max(50, image.winfo_height())
            scale = min((w-30)/max(0.01,right-left), (h-30)/max(0.01,bottom-top))
            off = (w/2-scale*(right+left)/2, h/2-scale*(bottom+top)/2)
            image.delete("all")
            for _, poly, color in faces:
                image.create_polygon([n*scale+off[i%2] for i,n in enumerate(poly)],
                                     fill=color, outline=color)
        except (OSError, KeyError, ValueError, IndexError) as err:
            stop()
            image.delete("all")
            image.create_text(14, 14, anchor="nw", text=str(err), fill="white", width=370)

    def stop():
        nonlocal timer, playing
        playing = False
        if timer is not None:
            window.after_cancel(timer)
            timer = None

    def advance():
        nonlocal timer
        if not playing:
            return
        meta = entries[model_name.get()]["clips"][name.get()]
        frame.set((frame.get()+1) % meta["frames"])
        render()
        if playing:
            timer = window.after(max(1, round(1000/meta["hz"])), advance)

    def play():
        nonlocal playing
        stop()
        playing = True
        advance()

    def select_clip(_=None):
        stop()
        meta = entries[model_name.get()]["clips"].get(name.get())
        if not meta:
            return
        frame.set(0)
        scrub.config(to=max(0,meta["frames"]-1))
        render()

    def select_model(_=None):
        clips = list(entries[model_name.get()]["clips"])
        names.config(values=clips)
        if name.get() not in clips:
            name.set(clips[0] if clips else "")
        select_clip()

    def close():
        stop()
        window.destroy()

    ttk.Button(bar, text="Play", command=play).pack(side="left")
    ttk.Button(bar, text="Stop", command=stop).pack(side="left")
    models.bind("<<ComboboxSelected>>", select_model)
    names.bind("<<ComboboxSelected>>", select_clip)
    scrub.config(command=render)
    image.bind("<Configure>", render)
    window.protocol("WM_DELETE_WINDOW", close)
    select_model()

