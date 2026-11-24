"""Compare a presented research-facility run with the same inputs on the CPU."""

import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess


ROOT = Path(__file__).resolve().parents[2]
CAPTURE_SIZE = (960, 540)
SEED = 506


def write_json(path, data):
    path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def capture_pixels(path):
    width, height = CAPTURE_SIZE
    data = path.read_bytes()
    header = f"P6\n{width} {height}\n255\n".encode()
    if not data.startswith(header) or len(data) != len(header) + 3 * width * height:
        raise ValueError(f"expected a {width}x{height} P6 capture in {path}")
    return data[len(header):]


def run(exe, arena_path, output, backend, playthrough):
    output.mkdir(parents=True, exist_ok=False)
    report = {
        "backend": backend,
        "seed": SEED,
        "exe_sha256": hashlib.sha256(exe.read_bytes()).hexdigest(),
        "cases": {},
        "passed": False,
    }

    def execute(name, args):
        with (output / f"{name}.log").open("w", encoding="utf-8") as log:
            proc = subprocess.run([exe, "--seed", str(SEED), *args], stdout=log, stderr=subprocess.STDOUT)
        if proc.returncode:
            raise RuntimeError(f"{name} exited with {proc.returncode}; see {log.name}")
        lines = Path(log.name).read_text(encoding="utf-8").splitlines()
        for line in reversed(lines):
            if line.startswith("{"):
                return json.loads(line)
        raise ValueError(f"{name}: missing final game state in {log.name}")

    arena = json.loads(arena_path.read_text(encoding="utf-8"))
    # Walk directly toward the marker. An edited lab table blocks that exact route.
    open_arena = output / "open.json"
    write_json(open_arena, arena)
    arena["obstacles"].append({"center": [0, 4.5], "half_extent": [0.8, 0.3]})
    edited_arena = output / "edited.json"
    write_json(edited_arena, arena)
    movement = {"movement": [0, -1], "restart": False, "upgrade": None}
    route = [movement] * 48
    input_path = output / "route.json"
    write_json(input_path, route)

    try:
        for name, layout in [("open", open_arena), ("edited", edited_arena), ("alternate", edited_arena)]:
            recorded = output / f"{name}-inputs.json"
            capture = output / f"{name}.ppm"
            options = ["--alternate-model"] if name == "alternate" else []
            presented = execute(
                name,
                ["--window", "--backend", backend, "--arena", layout,
                 "--width", str(CAPTURE_SIZE[0]), "--height", str(CAPTURE_SIZE[1]),
                 "--frames", str(len(route)), "--replay", input_path, "--record", recorded,
                 "--capture", capture, *options],
            )
            if json.loads(recorded.read_text(encoding="utf-8"))["inputs"] != route:
                raise ValueError(f"{name}: window consumed a different input sequence")
            cpu = execute(f"{name}-cpu", ["--headless", "--replay", recorded])
            if presented != cpu:
                raise ValueError(f"{name}: window state {presented} differs from replay {cpu}")
            if presented["tick"] != len(route):
                raise ValueError(f"{name}: game did not consume one tick per recorded input")
            pixels = capture_pixels(capture)
            report["cases"][name] = {
                "state": presented,
                "pixels_sha256": hashlib.sha256(pixels).hexdigest(),
            }

        ordinary = report["cases"]["open"]["state"]
        blocked = report["cases"]["edited"]["state"]
        if ordinary["marker_entries"] != 1 or ordinary["contacts"] != 0:
            raise ValueError(f"open route did not reach marker freely: {ordinary}")
        if blocked["marker_entries"] != 0 or blocked["contacts"] == 0:
            raise ValueError(f"edited route did not register the expected collision: {blocked}")
        if blocked["player"][1] <= ordinary["player"][1] + 1.0:
            raise ValueError("edited lab table did not stop the player before the marker")
        if blocked != report["cases"]["alternate"]["state"]:
            raise ValueError("swapping the character model changed the gameplay result")

        primary = capture_pixels(output / "edited.ppm")
        alternate = capture_pixels(output / "alternate.ppm")
        changed = sum(a != b for a, b in zip(
            struct.iter_unpack("BBB", primary), struct.iter_unpack("BBB", alternate), strict=True,
        ))
        report["model_swap_changed_pixels"] = changed
        if changed == 0:
            raise ValueError("swapping the character model did not change the rendered pixels")
        if playthrough:
            # This seed lets a stationary player collect XP, choose Pierce and
            # eventually lose to the horde. Only the final input requests a retry.
            inputs = [{"upgrade": "pierce"}] * 3000 + [{"restart": True}]
            replay = output / "retry.json"
            write_json(replay, inputs)
            common = ["--arena", open_arena, "--replay", replay]
            initial = execute("initial", ["--headless", *common, "--ticks", "0"])
            defeated = execute("defeated", ["--headless", *common, "--ticks", str(len(inputs) - 1)])
            if (defeated["phase"].lower() != "defeated" or defeated["hp"] != 0
                    or defeated["level"] <= initial["level"] or defeated["upgrades"][1] == 0):
                raise ValueError(f"replay did not choose Pierce and reach defeat: {defeated}")
            retry = execute("retry-window", ["--window", "--backend", backend, *common,
                            "--frames", str(len(inputs)), "--width", "640", "--height", "360"])
            replayed = execute("retry-cpu", ["--headless", *common])
            if retry != replayed or retry != (initial | {"tick": len(inputs)}):
                raise ValueError(f"retry did not restore initial game state: {retry}, replay {replayed}")
            report["playthrough"] = {"defeated": defeated, "retry": retry}
        report["passed"] = True
    finally:
        write_json(output / "report.json", report)
    print(f"Passed {len(report['cases'])} window/CPU comparisons; model swap changed {changed} pixels.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--backend", choices=["vulkan", "d3d12"], default="vulkan")
    parser.add_argument("--playthrough", action="store_true", help="also replay a complete upgrade/defeat/retry run")
    parser.add_argument("--arena", type=Path, default=ROOT / "assets/survivor/arena.json")
    parser.add_argument("--output", type=Path, required=True, help="new directory for logs, input, images and report")
    args = parser.parse_args()
    run(args.exe.resolve(), args.arena.resolve(), args.output.resolve(), args.backend, args.playthrough)
