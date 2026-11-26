"""Exercise the persistent NDJSON endpoint as an external client on Linux or Windows."""
import argparse
import json
import queue
import subprocess
import threading
import time
from pathlib import Path


class Client:
    def __init__(self, binary):
        self.process = subprocess.Popen(
            [binary, "--inspect-stdio"], stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, text=True, encoding="utf-8",
        )
        self.lines = queue.Queue()
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        self.counter = 0

    def _read(self):
        for line in self.process.stdout:
            self.lines.put(json.loads(line))
        self.lines.put(None)

    def next(self):
        line = self.lines.get(timeout=10)
        assert line is not None, "unexpected EOF"
        return line

    def send(self, **request):
        self.counter += 1
        ident = str(self.counter)
        self.raw(json.dumps(dict(id=ident, **request)))
        return ident

    def raw(self, text):
        self.process.stdin.write(text + "\n")
        self.process.stdin.flush()

    def call(self, op, error=None, **args):
        ident = self.send(op=op, **args)
        reply = self.next()
        assert reply["id"] == ident, reply
        if error:
            assert not reply["ok"] and reply["error"]["code"] == error, reply
            return None
        assert reply["ok"], reply
        return reply["result"]

    def close(self):
        self.process.stdin.close()
        try:
            assert self.process.wait(timeout=5) == 0
        finally:
            self.process.kill()
            self.process.wait()


def verify(client, expected):
    hello = client.call("hello")
    assert hello["state"]["paused"] and hello["state"]["tick"] == "0"
    client.raw("{")
    assert client.next()["error"]["code"] == "BAD_JSON"
    client.raw("a" * 20_000)
    assert client.next()["error"]["code"] == "LINE_TOO_LONG"
    client.call("unknown", error="BAD_REQUEST")
    schema = client.call("schema", kind="Door")
    event = next(e for e in schema["events"] if e["id"] == "unlock_denied")
    assert {f["id"] for f in event["fields"]} == {"required_key", "presented_key"}
    first = client.call("find", limit=1)
    door = client.call("find", kind="Door")["objects"][0]["id"]
    player = client.call("find", kind="Player")["objects"][0]["id"]
    before = client.call("snapshot", objects={door: ["state"], player: ["held_key"]})["id"]
    wait = client.call("wait", event=event["id"], object=door, pause=True,
                       capture={"self": ["state"], "actor": ["held_key"]})
    assert wait["state"] == "registered"
    assert client.call("read", object=player, fields=["held_key"])["values"]["held_key"] == "garden"
    client.call("resume")
    result = client.next()
    assert result["id"] == wait["subscription"] and result["ok"], result
    data = result["result"]
    assert data["event"]["self"] == door and data["event"]["actor"] == player
    assert data["event"]["facts"] == {"required_key": "basement", "presented_key": "garden"}
    assert data["event"]["tick"] == data["snapshot"]["tick"] == "1"
    assert client.call("hello")["state"]["paused"]
    assert client.call("read", snapshot=data["snapshot"]["id"], object=player, fields=["held_key"])["values"]["held_key"] == "basement"
    client.call("read", snapshot=before, object=door, fields=["required_key"], error="NOT_CAPTURED")
    client.call("find", revision=first["revision"], offset=first["next_offset"], error="STALE_PAGE")
    assert client.call("step")["tick"] == "2"
    assert client.call("read", object=door, fields=["state"])["values"]["state"] == "open"
    removed = client.call("wait", event="despawn", object=door, capture={"self":["state"]})
    client.call("step")
    terminal = client.next()
    assert terminal["id"] == removed["subscription"]
    death = terminal["result"]["snapshot"]
    assert death["missing"][door] == "OBJECT_GONE"
    client.call("read", object=door, fields=["state"], snapshot=death["id"], error="OBJECT_GONE")
    client.call("step")
    client.call("read", object=door, fields=["state"], error="OBJECT_GONE")
    assert client.call("find", kind="Door")["objects"][0]["id"] != door
    assert client.call("read", snapshot=before, object=door, fields=["state"])["values"]["state"] == "locked"
    client.call("wait", event="spawn", timeout_ms=15)
    assert client.next()["error"]["code"] == "TIMEOUT"
    token = client.call("snapshot", objects={player:["held_key"]}, ttl_ms=1)["id"]
    time.sleep(.02)
    client.call("read", object=player, fields=["held_key"], snapshot=token, error="SNAPSHOT_EXPIRED")
    subscription = client.call("watch", event="unlock_denied")["subscription"]
    client.call("step")
    changed = client.next()
    assert changed["id"] == subscription and changed["result"]["event"]["tick"] == "5"
    client.call("cancel", subscription=subscription)
    terminal = client.next()
    assert terminal["id"] == subscription and terminal["result"]["state"] == "ended"
    for _ in range(64):
        client.call("step")
    fixed = client.call("hello")["state"]
    assert fixed["tick"] == expected["tick"] == "69"
    final_door = client.call("find", kind="Door")["objects"][0]["id"]
    open_now = client.call("read", object=final_door, fields=["state"])["values"]["state"] == "open"
    held = client.call("read", object=player, fields=["held_key"])["values"]["held_key"]
    assert open_now == expected["open"] and (held == "basement") == expected["held_basement_key"]
    client.call("watch", event="spawn", since="0")
    gap = client.next()["result"]
    assert gap["state"] == "gap" and int(gap["through"]) >= int(gap["from"])


def slow_reader(binary):
    # Pause the reader long enough to fill both the OS pipe and the output queue.
    process = subprocess.Popen([binary, "--inspect-stdio"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    watchdog = threading.Timer(15, process.kill)
    watchdog.start()

    def flood():
        try:
            process.stdin.write(b'{"op":"resume","id":"run"}\n')
            for i in range(512):
                process.stdin.write(f'{{"op":"schema","id":"{i}"}}\n'.encode())
            process.stdin.write(b'{"op":"pause","id":"paused"}\n')
            process.stdin.flush()
        except (BrokenPipeError, OSError):
            pass
    sender = threading.Thread(target=flood, daemon=True)
    sender.start()
    try:
        # Wait for resume to take effect before deliberately stalling the reader.
        assert json.loads(process.stdout.readline())["id"] == "run"
        time.sleep(.5)
        for _ in range(512):
            assert json.loads(process.stdout.readline())["ok"]
        paused = json.loads(process.stdout.readline())
        assert paused["id"] == "paused" and int(paused["result"]["tick"]) >= 2, paused
        sender.join(timeout=5)
        assert not sender.is_alive()
        process.stdout.close()
        process.stdin.close()
        assert process.wait(timeout=5) == 0
    finally:
        watchdog.cancel()
        process.kill()
        process.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--shipping", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    expected = json.loads(subprocess.check_output([binary, "--ticks", "69"], timeout=5))
    if args.shipping:
        shipping = args.shipping.resolve()
        assert json.loads(subprocess.check_output([shipping, "--ticks", "69"], timeout=5)) == expected
        disabled = subprocess.run([shipping, "--inspect-stdio"], input=b"", capture_output=True, timeout=5)
        assert disabled.returncode != 0 and not disabled.stdout
    client = Client(binary)
    try:
        verify(client, expected)
    finally:
        client.close()
    slow_reader(binary)
    print("Live NDJSON inspection passed.")
