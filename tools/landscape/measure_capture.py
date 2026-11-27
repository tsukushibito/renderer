"""Measure one headless Windows Vulkan capture using the SDK's API dump layer."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import time


def fields(block, name):
    return re.findall(r"^\s*" + name + r":\s+[^=\n]+ = (\w+)", block, re.MULTILINE)


def calls(path):
    # The detailed text format starts each API call at column zero. Ignore the
    # bodies of unrelated calls, including long shader and descriptor records.
    keep = {
        "vkCreateDevice", "vkGetPhysicalDeviceMemoryProperties", "vkGetPhysicalDeviceMemoryProperties2",
        "vkAllocateMemory", "vkFreeMemory", "vkBeginCommandBuffer", "vkQueueSubmit2",
        "vkCmdDraw", "vkCmdDrawIndexed",
    }
    name, block = None, []
    with path.open(encoding="utf-8", errors="strict") as stream:
        for line in stream:
            if line.startswith("vk"):
                if name:
                    yield name, "".join(block)
                name = line.partition("(")[0]
                if name.startswith(("vkCmdDraw", "vkCmdExecuteCommands", "vkQueueSubmit")) and name not in keep:
                    raise ValueError(f"unhandled submission or draw command: {name}")
                if name not in keep:
                    name = None
                block = [line]
            elif name and not line.startswith("Thread "):
                block.append(line)
        if name:
            yield name, "".join(block)


def submitted(trace):
    devices, allocations, commands = {}, {}, {}
    current, peak = {}, {}
    adapter = None
    draws = indexed = triangles = submissions = 0
    for name, block in calls(trace):
        if "returns VkResult" in block.partition("\n")[0] and "VK_SUCCESS" not in block.partition("\n")[0]:
            raise ValueError(f"unsuccessful {name}")
        def field(key):
            value, = fields(block, key)
            return value
        if name.startswith("vkGetPhysicalDeviceMemoryProperties"):
            count = int(field("memoryTypeCount"))
            devices[field("physicalDevice")] = list(map(int, fields(block, "heapIndex")[:count]))
        elif name == "vkCreateDevice":
            if adapter is not None:
                raise ValueError("expected one viewer device")
            adapter = field("physicalDevice")
        elif name == "vkAllocateMemory":
            heap = devices[adapter][int(field("memoryTypeIndex"))]
            size = int(field("allocationSize"))
            memory = field("pMemory")
            if memory in allocations:
                raise ValueError("duplicate live memory handle")
            allocations[memory] = heap, size
            current[heap] = current.get(heap, 0) + size
            peak[heap] = max(peak.get(heap, 0), current[heap])
        elif name == "vkFreeMemory":
            memory = field("memory")
            if memory != "NULL":
                heap, size = allocations.pop(memory)
                current[heap] -= size
        elif name == "vkBeginCommandBuffer":
            commands[field("commandBuffer")] = [0, 0, 0]
        elif name in ("vkCmdDraw", "vkCmdDrawIndexed"):
            record = commands[field("commandBuffer")]
            record[0] += 1
            if name == "vkCmdDrawIndexed":
                record[1] += 1
                record[2] += int(field("indexCount")) // 3 * int(field("instanceCount"))
        elif name == "vkQueueSubmit2":
            for command in fields(block, "commandBuffer"):
                total, idx, tris = commands[command]
                draws += total
                indexed += idx
                triangles += tris
            submissions += 1
    if not submissions or not draws or not peak or allocations:
        raise ValueError("incomplete viewer trace")
    return dict(queue_submits=submissions, native_draws=draws, indexed_draws=indexed,
                indexed_triangles=triangles, native_heap_peak_bytes=peak)


def run_viewer(argv, env):
    import psutil
    start = time.monotonic()
    peak, cpu = 0, 0.
    with psutil.Popen(argv, env=env) as process:
        while process.poll() is None:
            try:
                peak = max(peak, process.memory_info().peak_wset)
                times = process.cpu_times()
                cpu = max(cpu, times.user + times.system)
            except psutil.NoSuchProcess:
                break
            time.sleep(.02)
        if process.wait():
            raise subprocess.CalledProcessError(process.returncode, argv)
    return dict(wall_seconds=time.monotonic() - start, observed_cpu_seconds=cpu,
                observed_peak_working_set_bytes=peak)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace", type=Path)
    parser.add_argument("command", nargs=argparse.REMAINDER,
                        help="optional scene-viewer invocation; otherwise read an existing trace")
    args = parser.parse_args()
    timing = {}
    if args.command:
        if args.trace.exists():
            parser.error("choose an unused trace filename")
        env = os.environ.copy()
        env.update(VK_INSTANCE_LAYERS="VK_LAYER_LUNARG_api_dump", VK_APIDUMP_FILE="true",
                   VK_APIDUMP_LOG_FILENAME=str(args.trace.resolve()), VK_APIDUMP_DETAILED="true",
                   VK_APIDUMP_OUTPUT_FORMAT="text", VK_APIDUMP_NO_ADDR="false")
        # Time a normal run. Layer logging adds substantial CPU and file I/O.
        timing = run_viewer(args.command, os.environ.copy())
        subprocess.run(args.command, env=env, check=True)
    print(json.dumps({**timing, **submitted(args.trace)}, indent=2))


if __name__ == "__main__":
    main()
