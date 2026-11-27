#!/usr/bin/env python3
"""obr: one stdio server process, pipelined JSON-RPC, a `read` every 50 ms on an open book while
one heavy call runs. Reports the read latency distribution during the heavy call.

Cases: open-epub, open-pdf, search, source-view.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import statistics
import subprocess
import sys
import threading
import time

INIT = {
    "protocolVersion": "2025-06-18",
    "capabilities": {},
    "clientInfo": {"name": "obr-probe", "version": "0"},
}


class Server:
    def __init__(
        self,
        binary: str,
        state: str,
        env_extra: dict[str, str] | None = None,
        stderr_path: str = "",
    ):
        env = dict(os.environ)
        env["READING_MCP_STATE_DIR"] = state
        env["READING_MCP_LOCAL_ROOTS"] = "/"
        env.setdefault("READING_MCP_TELEMETRY", "0")
        if env_extra:
            env.update(env_extra)
        stderr = open(stderr_path, "wb") if stderr_path else subprocess.DEVNULL
        self.stderr_file = stderr if stderr_path else None
        self.proc = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=stderr,
            env=env,
            bufsize=0,
        )
        self.next_id = 0
        self.lock = threading.Lock()
        self.cond = threading.Condition()
        self.responses: dict[int, dict] = {}
        self.arrivals: dict[int, float] = {}
        self.bytes: dict[int, int] = {}
        self.thread = threading.Thread(target=self._pump, daemon=True)
        self.thread.start()
        self.request("initialize", INIT)
        self.notify("notifications/initialized", {})

    def _pump(self):
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if "id" in message:
                with self.cond:
                    self.arrivals[message["id"]] = time.perf_counter()
                    self.bytes[message["id"]] = len(line)
                    self.responses[message["id"]] = message
                    self.cond.notify_all()

    def notify(self, method: str, params: dict):
        message = {"jsonrpc": "2.0", "method": method, "params": params}
        self._write(message)

    def _write(self, message: dict):
        assert self.proc.stdin is not None
        self.proc.stdin.write((json.dumps(message) + "\n").encode())
        self.proc.stdin.flush()

    def send(self, method: str, params: dict) -> int:
        with self.lock:
            self.next_id += 1
            request_id = self.next_id
        self._write({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        return request_id

    def wait(self, request_id: int) -> dict:
        with self.cond:
            while request_id not in self.responses:
                self.cond.wait()
            return self.responses.pop(request_id)

    def request(self, method: str, params: dict) -> dict:
        return self.wait(self.send(method, params))

    def call(self, tool: str, arguments: dict) -> dict:
        return self.request("tools/call", {"name": tool, "arguments": arguments})

    def call_text(self, tool: str, arguments: dict) -> tuple[str, dict]:
        request_id = self.send("tools/call", {"name": tool, "arguments": arguments})
        self.last_id = request_id
        message = self.wait(request_id)
        result = message.get("result", {})
        content = result.get("content") or []
        text = content[0].get("text", "") if content else ""
        return text, result

    def close(self):
        try:
            if self.proc.stdin:
                self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def read_json(text: str) -> dict:
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return {}


def prewalk(server: Server, handle: str, count: int) -> list[str]:
    """Collect `count` sentence handles by walking forward one sentence per call."""
    handles = [handle]
    current = handle
    for _ in range(count):
        text, _ = server.call_text("read", {"from": current})
        data = read_json(text)
        nxt = data.get("next")
        if not nxt:
            break
        handles.append(nxt)
        current = nxt
    return handles


def percentile(values: list[float], fraction: float) -> float:
    if not values:
        return float("nan")
    ordered = sorted(values)
    index = min(len(ordered) - 1, int(round(fraction * (len(ordered) - 1))))
    return ordered[index]


def run_case(args) -> dict:
    server = Server(args.binary, args.state, args.env, args.server_stderr)

    # The reader's book: opened once, before the heavy call.
    text, _ = server.call_text("open", {"source": args.reader_source})
    reader_open = read_json(text)
    reader_handle = reader_open.get("doc")
    if not reader_handle:
        raise SystemExit(f"reader open failed: {text[:400]}")

    # A heavy-page handle for source_view: walk deep into the reader's book.
    handles = prewalk(server, reader_handle, args.prewalk)

    heavy_handle = None
    if args.case in ("search", "source-view", "source-view-cached", "read-batch") and args.heavy_source:
        text, _ = server.call_text("open", {"source": args.heavy_source})
        heavy_handle = read_json(text).get("doc")
        if not heavy_handle:
            raise SystemExit(f"heavy open failed: {text[:400]}")

    sent_at: dict[int, float] = {}
    stop = threading.Event()

    def reader():
        index = 0
        while not stop.is_set():
            from_handle = handles[index % len(handles)]
            index += 1
            sent = time.perf_counter()
            request_id = server.send(
                "tools/call", {"name": "read", "arguments": {"from": from_handle}}
            )
            sent_at[request_id] = sent
            stop.wait(args.interval)

    # Warm the read path so the first samples are not cold.
    for handle in handles[:20]:
        server.call_text("read", {"from": handle})

    reader_thread = threading.Thread(target=reader, daemon=True)
    reader_thread.start()
    time.sleep(0.5)

    # Optional thread dumps while the heavy call runs.
    dumps: list[str] = []

    def dumper():
        for index, delay in enumerate(args.dump_at):
            wait = heavy_start + delay - time.perf_counter()
            if wait > 0:
                time.sleep(wait)
            path = os.path.join(args.dump_dir, f"dump-{index}-{args.case}.txt")
            with open(path, "wb") as handle:
                subprocess.run(
                    [
                        "gdb",
                        "-batch",
                        "-p",
                        str(server.proc.pid),
                        "-ex",
                        "thread apply all bt 12",
                    ],
                    stdout=handle,
                    stderr=subprocess.DEVNULL,
                )
            dumps.append(path)

    dump_thread = None
    # The heavy call, timed on its own.
    dump_paths = []
    heavy_start = time.perf_counter()
    if args.dump_at:
        dump_thread = threading.Thread(target=dumper, daemon=True)
        dump_thread.start()
    if args.case == "open-epub":
        text, _ = server.call_text("open", {"source": args.heavy_source})
        heavy_kind = "open-epub"
    elif args.case in ("open-pdf", "open-pdf-entropy"):
        text, _ = server.call_text("open", {"source": args.heavy_source})
        heavy_kind = "open-pdf"
    elif args.case == "search":
        text, _ = server.call_text(
            "search", {"doc": heavy_handle, "query": args.query, "limit": 10}
        )
        heavy_kind = "search"
    elif args.case == "read-batch":
        heavy_handles = prewalk(server, heavy_handle, 20)
        text, _ = server.call_text("read", {"from": heavy_handles[-1], "max_chars": 16000})
        heavy_kind = "read-batch"
    elif args.case in ("source-view", "source-view-cached"):
        if heavy_handle:
            heavy_handles = prewalk(server, heavy_handle, args.prewalk)
            target = heavy_handles[min(len(heavy_handles) - 1, args.prewalk // 2)]
        else:
            target = handles[min(len(handles) - 1, args.prewalk // 2)]
        if args.case == "source-view-cached":
            server.call_text("source_view", {"at": target})
        text, _ = server.call_text("source_view", {"at": target})
        heavy_kind = args.case
    else:
        raise SystemExit(f"unknown case {args.case}")
    heavy_id = server.last_id
    heavy_end = time.perf_counter()

    time.sleep(1.0)
    stop.set()
    reader_thread.join(timeout=30)
    # Let the last in-flight reads land before closing.
    deadline = time.perf_counter() + 60
    while time.perf_counter() < deadline:
        with server.cond:
            missing = [i for i in sent_at if i and i not in server.arrivals]
        if not missing:
            break
        time.sleep(0.05)
    with server.cond:
        samples = [
            (sent_at[request_id], server.arrivals[request_id], 1)
            for request_id in sent_at
            if request_id and request_id in server.arrivals
        ]
    server.close()

    during = [
        (received - sent) * 1000.0
        for sent, received, ok in samples
        if ok and received >= heavy_start and sent <= heavy_end
    ]
    idle = [
        (received - sent) * 1000.0
        for sent, received, ok in samples
        if ok and received < heavy_start - 0.2
    ]
    return {
        "case": heavy_kind,
        "heavy_ms": (heavy_end - heavy_start) * 1000.0,
        "heavy_answer_chars": len(text),
        "heavy_answer_head": text[:120],
        "heavy_response_bytes": server.bytes.get(heavy_id, 0),
        "reader_open": {k: reader_open.get(k) for k in ("doc", "sections", "chars", "format")},
        "reads_during": len(during),
        "p50": round(percentile(during, 0.5), 2),
        "p95": round(percentile(during, 0.95), 2),
        "max": round(max(during), 2) if during else None,
        "reads_idle": len(idle),
        "idle_p50": round(percentile(idle, 0.5), 2),
        "idle_p95": round(percentile(idle, 0.95), 2),
        "idle_max": round(max(idle), 2) if idle else None,
        "all_p50": round(statistics.median([(r - s) * 1000.0 for s, r, _ in samples]), 2),
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--state", required=True)
    parser.add_argument("--case", required=True)
    parser.add_argument("--reader-source", required=True)
    parser.add_argument("--heavy-source", default="")
    parser.add_argument("--query", default="")
    parser.add_argument("--interval", type=float, default=0.05)
    parser.add_argument("--prewalk", type=int, default=200)
    parser.add_argument("--env", action="append", default=[])
    parser.add_argument("--out", default="")
    parser.add_argument("--server-stderr", default="")
    parser.add_argument("--dump-dir", default="")
    parser.add_argument("--dump-at", default="", help="comma separated seconds after the heavy start")
    args = parser.parse_args()
    args.env = dict(pair.split("=", 1) for pair in args.env)
    args.dump_at = [float(value) for value in args.dump_at.split(",") if value]
    result = run_case(args)
    encoded = json.dumps(result, ensure_ascii=False)
    print(encoded)
    if args.out:
        with open(args.out, "w", encoding="utf-8") as handle:
            handle.write(encoded + "\n")


if __name__ == "__main__":
    main()
