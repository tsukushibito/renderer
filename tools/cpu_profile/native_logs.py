"""Bounded byte logs for small native metadata exports, never Renderer frames."""

import subprocess
import threading
import time


def run_bounded(command, stdout, stderr, timeout, max_output_bytes, env=None, *,
                launcher=subprocess.Popen, monotonic=time.monotonic, sleeper=time.sleep):
    if timeout <= 0 or max_output_bytes <= 0:
        raise ValueError("Native log timeout and byte limit must be positive")
    process = launcher(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
    lock, stop, exceeded = threading.Lock(), threading.Event(), threading.Event()
    total, errors = [0], []

    def copy(pipe, destination):
        try:
            while not stop.is_set():
                chunk = pipe.read(64 * 1024)
                if not chunk or stop.is_set():
                    break
                with lock:
                    remaining = max_output_bytes - total[0]
                    destination.write(chunk[:remaining])
                    total[0] += min(len(chunk), remaining)
                    if len(chunk) > remaining:
                        exceeded.set()
                        break
        except Exception as error:
            with lock:
                errors.append(error)
        finally:
            try:
                pipe.close()
            except Exception as error:
                with lock:
                    errors.append(error)

    readers = [threading.Thread(target=copy, args=(pipe, destination), daemon=True)
               for pipe, destination in ((process.stdout, stdout), (process.stderr, stderr))]
    deadline = monotonic() + timeout
    try:
        for reader in readers:
            reader.start()
        while True:
            code = process.poll()
            complete = code is not None and not any(reader.is_alive() for reader in readers)
            if exceeded.is_set():
                raise RuntimeError(f"Native output exceeded {max_output_bytes} bytes; only the bounded prefix was retained")
            if errors:
                raise RuntimeError(f"Native log reader failed: {errors[0]}")
            if complete:
                return code
            remaining = deadline - monotonic()
            if remaining <= 0:
                raise subprocess.TimeoutExpired(command, timeout)
            sleeper(min(0.01, remaining))
    finally:
        stop.set()
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        for reader in readers:
            if reader.ident is not None:
                reader.join(timeout=1)
        for reader, pipe in zip(readers, (process.stdout, process.stderr)):
            if reader.ident is None:
                pipe.close()
