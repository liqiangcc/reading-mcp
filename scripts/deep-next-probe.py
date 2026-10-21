#!/usr/bin/env python3
"""Profile one deep `get_text_units` Next call: open outside perf, then sample only the reads.

The probe speaks newline-delimited JSON-RPC MCP over stdio against a release binary:

1. open the document in a fresh state directory and read its structure, without perf;
2. seek to `--start-fraction` of the section's sentence stream with a few large pages, still
   without perf, so that the profile holds only the deep reads;
3. attach `perf record -F 999 -g --call-graph fp` to the already-running server and run
   `--calls` timed reads of `--mode`, timing each one:
   - `next` (default): forward `get_text_units` calls of `--max-items` that follow `next_cursor`;
   - `anchored`: `get_text_units` calls anchored at the unit at the seek position;
   - `context`: `get_context` calls on that unit (two neighbours each side);
   - `read`: `read_document` calls on that unit's exact locator.
   `--kind` and `--policy` choose the stream and the unit;
4. stop perf and report the mean/median/p95 ms per call plus the perf file path.

`--largest-section` picks the section with the largest `--kind`/`--policy` stream instead of the
`--start-fraction` one, and `--max-items`/`--max-chars` bound each timed payload.

If `perf record -p` cannot attach, the probe falls back to starting the server under
`perf record ... -- <binary>` with the same state directory, and says so in `perf_mode`; that
profile also contains the (much shorter) cached open.

Standard library only. This script never builds anything: pass an existing release binary.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time

INITIALIZE_MESSAGE = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "deep-next-probe", "version": "0"},
    },
}

INITIALIZED_NOTIFICATION = {
    "jsonrpc": "2.0",
    "method": "notifications/initialized",
}

REQUEST_TIMEOUT = 900.0
SEEK_PAGE = 256  # the server caps get_text_units max_items at 256
PERF_ARGS = ["-F", "999", "-g", "--call-graph", "fp"]


class ProbeError(RuntimeError):
    """A transport, protocol, tool or deadline failure."""


def send(process: subprocess.Popen, message: dict) -> None:
    assert process.stdin is not None
    process.stdin.write((json.dumps(message) + "\n").encode("utf-8"))
    process.stdin.flush()


def pump_stdout(stream, responses: queue.Queue) -> None:
    """Read JSON-RPC responses on a background thread."""
    try:
        for line in stream:
            try:
                response = json.loads(line.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError):
                continue
            responses.put(response)
    except Exception:  # noqa: BLE001 - draining is best effort
        pass
    finally:
        responses.put(None)


def read_response(responses: queue.Queue, awaited_id: int, timeout: float) -> dict:
    deadline = time.monotonic() + timeout
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError(f"timed out waiting for response to id {awaited_id}")
        try:
            item = responses.get(timeout=remaining)
        except queue.Empty:
            raise TimeoutError(f"timed out waiting for response to id {awaited_id}") from None
        if item is None:
            raise RuntimeError(f"server closed stdout before responding to id {awaited_id}")
        if isinstance(item, dict) and item.get("id") == awaited_id:
            return item


def extract_error(response: dict) -> str | None:
    error = response.get("error")
    if isinstance(error, dict):
        return "JSON-RPC error: " + str(error.get("message", error))
    if error is not None:
        return "JSON-RPC error: " + str(error)
    result = response.get("result")
    if isinstance(result, dict) and result.get("isError"):
        content = result.get("content")
        if isinstance(content, list):
            texts = [
                item.get("text", "")
                for item in content
                if isinstance(item, dict) and item.get("type") == "text"
            ]
            if texts:
                return "tool error: " + " ".join(texts)
        return "tool error: tool reported an error"
    return None


def tool_payload(result: dict):
    """Return the tool's JSON payload, from structuredContent or content[0].text."""
    structured = result.get("structuredContent")
    if structured is None:
        structured = result.get("structured_content")
    if isinstance(structured, (dict, list)):
        return structured
    content = result.get("content")
    if isinstance(content, list):
        for block in content:
            if isinstance(block, dict) and block.get("type") == "text":
                text = block.get("text")
                if isinstance(text, str):
                    try:
                        return json.loads(text)
                    except json.JSONDecodeError:
                        return None
    return None


class Server:
    """One MCP server process over stdio."""

    def __init__(
        self,
        command: list[str],
        env: dict,
        log_path: str,
        request_timeout: float = REQUEST_TIMEOUT,
    ) -> None:
        self.request_timeout = request_timeout
        self.log_handle = open(log_path, "wb")
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.log_handle,
            env=env,
        )
        self.responses: queue.Queue = queue.Queue()
        self.reader = threading.Thread(
            target=pump_stdout, args=(self.process.stdout, self.responses), daemon=True
        )
        self.reader.start()
        self.next_id = 0

    def request(self, method: str, params: dict | None = None) -> dict:
        self.next_id += 1
        request_id = self.next_id
        message = {"jsonrpc": "2.0", "id": request_id, "method": method}
        if params is not None:
            message["params"] = params
        try:
            send(self.process, message)
            return read_response(self.responses, request_id, self.request_timeout)
        except TimeoutError as exc:
            self.kill()
            raise ProbeError(str(exc)) from None
        except Exception as exc:  # noqa: BLE001 - surfaced as a probe error
            self.kill()
            raise ProbeError(f"{type(exc).__name__}: {exc}") from None

    def initialize(self) -> None:
        response = self.request("initialize", INITIALIZE_MESSAGE["params"])
        error = extract_error(response)
        if error is not None:
            raise ProbeError(error)
        send(self.process, {"jsonrpc": "2.0", "method": "notifications/initialized"})

    def tool(self, name: str, arguments: dict) -> dict:
        response = self.request("tools/call", {"name": name, "arguments": arguments})
        error = extract_error(response)
        if error is not None:
            raise ProbeError(f"{name}: {error}")
        result = response.get("result")
        if not isinstance(result, dict):
            raise ProbeError(f"{name}: result is not an object")
        return result

    def kill(self) -> None:
        try:
            self.process.kill()
        except Exception:  # noqa: BLE001 - already gone
            pass

    def close(self, timeout: float = 120.0) -> None:
        try:
            if self.process.stdin is not None:
                self.process.stdin.close()
        except Exception:  # noqa: BLE001 - server may already be gone
            pass
        try:
            self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.kill()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
        self.reader.join(timeout=1.0)
        try:
            self.log_handle.close()
        except Exception:  # noqa: BLE001 - best effort
            pass


def flatten_structure(structure_result: dict) -> list[dict]:
    flattened: dict[str, dict] = {}

    def walk(nodes) -> None:
        if not isinstance(nodes, list):
            return
        for node in nodes:
            if not isinstance(node, dict):
                continue
            section_id = node.get("section_id")
            if isinstance(section_id, str) and section_id not in flattened:
                flattened[section_id] = node
            walk(node.get("children"))

    walk(structure_result.get("sections"))
    return sorted(flattened.values(), key=lambda node: node.get("body_order", 0))


def read_structure(server: Server, document_id: str) -> list[dict]:
    sections: dict[str, dict] = {}
    cursor = None
    for _ in range(10_000):
        arguments = {"document_id": document_id, "max_nodes": 500}
        if cursor is not None:
            arguments["cursor"] = cursor
        payload = tool_payload(server.tool("get_document_structure", arguments))
        if not isinstance(payload, dict):
            raise ProbeError("get_document_structure returned no JSON object")
        for node in flatten_structure(payload):
            sections.setdefault(node["section_id"], node)
        cursor = payload.get("next_cursor")
        if not cursor or payload.get("complete") is True:
            break
    else:
        raise ProbeError("structure pagination did not terminate")
    return sorted(sections.values(), key=lambda node: node.get("body_order", 0))


def text_units(server: Server, arguments: dict) -> dict:
    payload = tool_payload(server.tool("get_text_units", arguments))
    if not isinstance(payload, dict):
        raise ProbeError("get_text_units returned no JSON object")
    stream = payload.get("stream")
    if not isinstance(stream, dict):
        raise ProbeError("get_text_units returned no stream segment")
    return payload


def text_unit_base(kind: str, policy: str) -> dict:
    return {
        "requested_kind": kind,
        "direction": "forward",
        "coverage_policy": policy,
    }


def largest_section(
    server: Server,
    document_id: str,
    sections: list[dict],
    kind: str,
    policy: str,
) -> tuple[int, int, int]:
    """The section with the most `kind`/`policy` stream items; returns (index, items, calls)."""
    best_index = 0
    best_total = -1
    calls = 0
    for index, section in enumerate(sections):
        arguments = {
            "document_id": document_id,
            "section_id": section["section_id"],
            **text_unit_base(kind, policy),
            "max_items": 1,
        }
        try:
            payload = text_units(server, arguments)
        except ProbeError:
            # A section without coverage of the requested kind cannot hold the largest stream.
            continue
        calls += 1
        total = payload["stream"].get("total_items")
        if isinstance(total, int) and total > best_total:
            best_index, best_total = index, total
    if best_total <= 0:
        raise ProbeError(f"no section reports a {kind}/{policy} stream")
    return best_index, best_total, calls


def section_unit_locator(
    server: Server,
    document_id: str,
    section_id: str,
    cursor: str | None,
    kind: str,
    policy: str,
) -> dict:
    """The locator of the unit at the seek position, read untimed."""
    arguments = {
        "document_id": document_id,
        "section_id": section_id,
        **text_unit_base(kind, policy),
        "max_items": 1,
    }
    if cursor is not None:
        arguments["cursor"] = cursor
    payload = text_units(server, arguments)
    items = payload.get("items")
    if not isinstance(items, list) or not items:
        raise ProbeError("the seek position has no unit to anchor on")
    locator = items[0].get("locator")
    if not isinstance(locator, dict):
        raise ProbeError("the anchor unit has no locator")
    return locator


def choose_section(
    server: Server,
    document_id: str,
    sections: list[dict],
    args: argparse.Namespace,
) -> tuple[int, str, int, int]:
    """Pick the profiled section: (index, section_id, total_items, selection_calls)."""
    selection_calls = 0
    if args.largest_section:
        index, _, selection_calls = largest_section(
            server, document_id, sections, args.kind, args.policy
        )
    else:
        index = min(len(sections) - 1, int(len(sections) * args.start_fraction))
    section_id = sections[index]["section_id"]
    head = text_units(
        server,
        {
            "document_id": document_id,
            "section_id": section_id,
            **text_unit_base(args.kind, args.policy),
            "max_items": 1,
        },
    )
    total_items = head["stream"].get("total_items")
    if not isinstance(total_items, int) or total_items <= 0:
        raise ProbeError(f"section reported {total_items!r} stream items")
    return index, section_id, total_items, selection_calls


def open_document(server: Server, source: str) -> str:
    payload = tool_payload(server.tool("open_document", {"source": source}))
    if not isinstance(payload, dict):
        raise ProbeError("open_document did not return an object")
    document_id = payload.get("document_id")
    if not isinstance(document_id, str):
        raise ProbeError("open_document did not return a document_id")
    return document_id


def seek(
    server: Server,
    document_id: str,
    section_id: str,
    target: int,
    kind: str,
    policy: str,
    page: int = SEEK_PAGE,
) -> tuple[str | None, int, int]:
    """Page forward to `target` stream position; return (cursor, position, calls)."""
    position = 0
    cursor = None
    calls = 0
    while position < target:
        want = min(page, target - position)
        arguments = {
            "document_id": document_id,
            "section_id": section_id,
            **text_unit_base(kind, policy),
            "max_items": want,
        }
        if cursor is not None:
            arguments["cursor"] = cursor
        payload = text_units(server, arguments)
        calls += 1
        end = payload["stream"].get("end_index")
        if not isinstance(end, int) or end <= position:
            raise ProbeError(f"seek made no progress at position {position} (end_index {end!r})")
        position = end
        cursor = payload.get("next_cursor")
        if not cursor:
            break
        if calls > 100_000:
            raise ProbeError("seek did not terminate")
    return cursor, position, calls


def start_perf_attach(perf: str, perf_out: str, pid: int, log_path: str):
    """Attach perf to the running server. Returns the perf process, or None if it failed."""
    handle = open(log_path, "wb")
    command = [perf, "record", *PERF_ARGS, "-o", perf_out, "-p", str(pid)]
    process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=handle)
    # perf exits at once when it cannot attach (permissions, missing events, ...).
    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline:
        if process.poll() is not None:
            handle.close()
            with open(log_path, encoding="utf-8", errors="replace") as reader:
                sys.stderr.write(reader.read()[-2000:])
            return None
        time.sleep(0.25)
    return process


def stop_perf(process: subprocess.Popen, timeout: float = 180.0) -> int | None:
    """SIGINT so perf finalizes its file; returns the exit code."""
    try:
        process.send_signal(signal.SIGINT)
    except Exception:  # noqa: BLE001 - already gone
        return process.poll()
    try:
        return process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        process.kill()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            pass
        return None


def proc_io(pid: int) -> dict[str, int]:
    """The kernel I/O counters of a process; empty when `/proc` is unavailable."""
    counters: dict[str, int] = {}
    try:
        with open(f"/proc/{pid}/io", encoding="ascii") as handle:
            for line in handle:
                name, _, value = line.partition(":")
                try:
                    counters[name.strip()] = int(value.strip())
                except ValueError:
                    continue
    except OSError:
        pass
    return counters


def proc_rss_kib(pid: int) -> int | None:
    try:
        with open(f"/proc/{pid}/status", encoding="ascii") as handle:
            for line in handle:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except (OSError, ValueError):
        pass
    return None


def io_delta(before: dict[str, int], after: dict[str, int]) -> dict[str, int]:
    keys = ("rchar", "wchar", "syscr", "syscw", "read_bytes", "write_bytes")
    return {key: after.get(key, 0) - before.get(key, 0) for key in keys}


def run_reads(
    server: Server,
    document_id: str,
    section_id: str,
    cursor: str | None,
    calls: int,
    kind: str,
    policy: str,
    max_items: int,
    measure_io: bool = False,
):
    """Run `calls` forward `max_items` reads from `cursor`, following `next_cursor`."""
    seconds: list[float] = []
    io_per_call: list[dict[str, int]] = []
    position = None
    for _ in range(calls):
        arguments = {
            "document_id": document_id,
            "section_id": section_id,
            **text_unit_base(kind, policy),
            "max_items": max_items,
        }
        if cursor is not None:
            arguments["cursor"] = cursor
        before_io = proc_io(server.process.pid) if measure_io else None
        start = time.monotonic()
        payload = text_units(server, arguments)
        seconds.append(time.monotonic() - start)
        if before_io is not None:
            after_io = proc_io(server.process.pid)
            io_per_call.append(io_delta(before_io, after_io))
        stream = payload["stream"]
        position = stream.get("end_index")
        cursor = payload.get("next_cursor")
        if not cursor:
            break
    return seconds, io_per_call, position


def run_repeated(
    server: Server,
    tool: str,
    arguments: dict,
    calls: int,
    measure_io: bool = False,
):
    """Run the same `tool` call `calls` times, timing each."""
    seconds: list[float] = []
    io_per_call: list[dict[str, int]] = []
    for _ in range(calls):
        before_io = proc_io(server.process.pid) if measure_io else None
        start = time.monotonic()
        server.tool(tool, arguments)
        seconds.append(time.monotonic() - start)
        if before_io is not None:
            after_io = proc_io(server.process.pid)
            io_per_call.append(io_delta(before_io, after_io))
    return seconds, io_per_call, None


def summarize_io(io_per_call: list[dict[str, int]]) -> dict[str, float | int]:
    if not io_per_call:
        return {}
    read_bytes = [entry.get("read_bytes", 0) for entry in io_per_call]
    rchar = [entry.get("rchar", 0) for entry in io_per_call]
    return {
        "io_calls": len(io_per_call),
        "io_read_bytes_mean": round(statistics.fmean(read_bytes), 1),
        "io_read_bytes_total": sum(read_bytes),
        "io_rchar_mean": round(statistics.fmean(rchar), 1),
        "io_rchar_total": sum(rchar),
    }


def summarize(seconds: list[float]) -> dict:
    if not seconds:
        return {}
    ordered = sorted(seconds)
    return {
        "calls": len(seconds),
        "mean_ms": round(1000.0 * statistics.fmean(seconds), 3),
        "median_ms": round(1000.0 * statistics.median(seconds), 3),
        "p95_ms": round(1000.0 * ordered[min(len(ordered) - 1, int(0.95 * len(ordered)))], 3),
        "min_ms": round(1000.0 * ordered[0], 3),
        "max_ms": round(1000.0 * ordered[-1], 3),
        "total_s": round(sum(seconds), 3),
    }


def probe(args: argparse.Namespace) -> int:
    document = os.path.abspath(args.document)
    if not os.path.isfile(document):
        raise ProbeError(f"file not found: {document}")
    binary = os.path.abspath(args.binary)
    if not os.path.isfile(binary):
        raise ProbeError(f"binary not found: {binary}")
    if args.calls <= 0:
        raise ProbeError("--calls must be positive")
    if not 0.0 <= args.start_fraction < 1.0:
        raise ProbeError("--start-fraction must be in [0, 1)")
    if not 1 <= args.seek_page <= SEEK_PAGE:
        raise ProbeError(f"--seek-page must be in [1, {SEEK_PAGE}]")
    if not 1 <= args.max_items <= SEEK_PAGE:
        raise ProbeError(f"--max-items must be in [1, {SEEK_PAGE}]")
    if args.max_chars is not None and args.max_chars <= 0:
        raise ProbeError("--max-chars must be positive")

    scratch_root = os.path.join(os.getcwd(), ".tmp")
    os.makedirs(scratch_root, exist_ok=True)
    state_dir = tempfile.mkdtemp(prefix="deep-next-state-", dir=scratch_root)
    env = dict(os.environ)
    env["READING_MCP_STATE_DIR"] = state_dir
    env["READING_MCP_LOCAL_ROOTS"] = os.path.dirname(document)
    env["READING_MCP_TOOLS"] = "v1"
    env["READING_MCP_MAX_DOCUMENT_BYTES"] = "134217728"
    env["READING_MCP_MAX_NORMALIZED_CHARS"] = "200000000"
    env["READING_MCP_PARSE_TIMEOUT_SECS"] = "900"
    for item in args.env:
        key, separator, value = item.partition("=")
        if not separator or not key:
            raise ProbeError(f"--env expects KEY=VALUE, got {item!r}")
        env[key] = value

    server_log = os.path.join(scratch_root, "deep-next-server.log")
    perf_log = os.path.join(scratch_root, "deep-next-perf.log")
    perf_out = os.path.abspath(args.perf_out) if args.perf_out else None
    if perf_out:
        os.makedirs(os.path.dirname(perf_out), exist_ok=True)

    server = Server([binary], env, server_log)
    perf_process = None
    perf_mode = "none"
    open_seconds = None
    seek_calls = 0
    total_items = None
    start_position = None
    rss_phases: dict[str, int] = {}
    try:
        server.initialize()
        opened = time.monotonic()
        document_id = open_document(server, document)
        open_seconds = time.monotonic() - opened
        if args.rss_phases:
            rss = proc_rss_kib(server.process.pid)
            if rss is not None:
                rss_phases["after_open"] = rss
        sections = read_structure(server, document_id)
        if not sections:
            raise ProbeError("document has no sections")
        index, section_id, total_items, selection_calls = choose_section(
            server, document_id, sections, args
        )
        target = int(total_items * args.start_fraction)
        cursor, start_position, seek_calls = seek(
            server,
            document_id,
            section_id,
            target,
            args.kind,
            args.policy,
            args.seek_page,
        )
        if args.rss_phases:
            rss = proc_rss_kib(server.process.pid)
            if rss is not None:
                rss_phases["after_seek"] = rss
        print(
            f"opened in {open_seconds:.2f} s; section {index}/{len(sections)} has "
            f"{total_items} {args.kind} items; seeked to {start_position} in "
            f"{seek_calls} pages",
            flush=True,
        )

        if perf_out:
            perf_process = start_perf_attach(args.perf, perf_out, server.process.pid, perf_log)
            if perf_process is not None:
                perf_mode = "attach"
                print(f"perf attached to pid {server.process.pid}", flush=True)
            else:
                print("perf could not attach; restarting the server under perf", flush=True)
                server.close()
                server = Server(
                    [args.perf, "record", *PERF_ARGS, "-o", perf_out, "--", binary],
                    env,
                    server_log,
                )
                perf_mode = "wrapper"
                server.initialize()
                document_id = open_document(server, document)
                sections = read_structure(server, document_id)
                index, section_id, total_items, _ = choose_section(
                    server, document_id, sections, args
                )
                cursor, start_position, seek_calls = seek(
                    server,
                    document_id,
                    section_id,
                    int(total_items * args.start_fraction),
                    args.kind,
                    args.policy,
                    args.seek_page,
                )
                print(f"wrapper open done; seeked to {start_position}", flush=True)

        if args.mode == "next":
            seconds, io_per_call, end_position = run_reads(
                server,
                document_id,
                section_id,
                cursor,
                args.calls,
                args.kind,
                args.policy,
                args.max_items,
                measure_io=args.io_bytes,
            )
        else:
            locator = section_unit_locator(
                server, document_id, section_id, cursor, args.kind, args.policy
            )
            if args.mode == "anchored":
                tool = "get_text_units"
                timed_arguments = {
                    "document_id": document_id,
                    "section_id": section_id,
                    **text_unit_base(args.kind, args.policy),
                    "anchor_locator": locator,
                    "max_items": args.max_items,
                }
            elif args.mode == "context":
                tool = "get_context"
                timed_arguments = {
                    "document_id": document_id,
                    "target_locator": locator,
                    "relation": {
                        "type": "neighbor",
                        "unit": args.kind,
                        "before": 2,
                        "after": 2,
                    },
                    "before": 2,
                    "after": 2,
                }
            else:
                tool = "read_document"
                timed_arguments = {
                    "document_id": document_id,
                    "target_locator": locator,
                }
            if args.max_chars is not None:
                timed_arguments["max_chars"] = args.max_chars
            seconds, io_per_call, end_position = run_repeated(
                server,
                tool,
                timed_arguments,
                args.calls,
                measure_io=args.io_bytes,
            )
        if args.rss_phases:
            rss = proc_rss_kib(server.process.pid)
            if rss is not None:
                rss_phases["after_reads"] = rss
    finally:
        server.close()
        if perf_process is not None:
            code = stop_perf(perf_process)
            print(f"perf exited with {code}", flush=True)

    summary = summarize(seconds)
    io_summary = {
        "io_calls": 0,
        "io_read_bytes_mean": 0.0,
        "io_read_bytes_total": 0,
        "io_rchar_mean": 0.0,
        "io_rchar_total": 0,
        **summarize_io(io_per_call),
    }
    perf_bytes = os.path.getsize(perf_out) if perf_out and os.path.exists(perf_out) else 0
    if perf_out and perf_bytes < 4096:
        print(f"::warning::perf data {perf_out} is only {perf_bytes} bytes", file=sys.stderr)
    result = {
        "document": document,
        "binary": binary,
        "state_dir": state_dir,
        "section_index": index,
        "sections": len(sections),
        "largest_section": args.largest_section,
        "selection_calls": selection_calls,
        "total_items": total_items,
        "mode": args.mode,
        "kind": args.kind,
        "policy": args.policy,
        "max_items": args.max_items,
        "max_chars": args.max_chars,
        "start_fraction": args.start_fraction,
        "start_position": start_position,
        "end_position": end_position,
        "seek_pages": seek_calls,
        "seek_page": args.seek_page,
        "open_seconds": None if open_seconds is None else round(open_seconds, 2),
        "perf_mode": perf_mode,
        "perf_out": perf_out,
        "perf_bytes": perf_bytes,
        "io_bytes": args.io_bytes,
        "rss_phases": rss_phases if args.rss_phases else {},
        **summary,
        **io_summary,
    }
    out_path = os.path.abspath(args.out)
    os.makedirs(os.path.dirname(out_path) or ".", exist_ok=True)
    with open(out_path, "w", encoding="utf-8") as handle:
        json.dump(result, handle, indent=2)
        handle.write("\n")

    print(
        "mode={mode} kind={kind} policy={policy} max_items={max_items} "
        "fraction={start_fraction} calls={calls} mean_ms={mean_ms} median_ms={median_ms} "
        "p95_ms={p95_ms} max_ms={max_ms} io_rchar_mean={io_rchar_mean} "
        "io_read_bytes_mean={io_read_bytes_mean} perf_mode={perf_mode} "
        "perf_out={perf_out}".format(
            mode=args.mode,
            kind=args.kind,
            policy=args.policy,
            max_items=args.max_items,
            start_fraction=args.start_fraction,
            perf_mode=perf_mode,
            perf_out=perf_out,
            **{**summary, **io_summary},
        ),
        flush=True,
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Profile deep get_text_units reads under perf.")
    parser.add_argument("--binary", required=True, help="reading-mcp release binary")
    parser.add_argument("--document", required=True, help="document to open and read")
    parser.add_argument("--calls", type=int, required=True, help="timed calls")
    parser.add_argument(
        "--start-fraction",
        type=float,
        default=0.5,
        help="fraction of the chosen section's kind stream to start at",
    )
    parser.add_argument("--perf", default="perf", help="perf binary")
    parser.add_argument(
        "--seek-page",
        type=int,
        default=SEEK_PAGE,
        help=f"sentence items per untimed seek page (server limit {SEEK_PAGE})",
    )
    parser.add_argument("--perf-out", default=None, help="perf data output path")
    parser.add_argument(
        "--mode",
        choices=("next", "anchored", "context", "read"),
        default="next",
        help="timed read: cursor pages, anchored get_text_units, get_context or read_document",
    )
    parser.add_argument(
        "--kind",
        choices=("sentence", "paragraph"),
        default="sentence",
        help="stream kind to seek, read and anchor on",
    )
    parser.add_argument(
        "--policy",
        choices=("preserve_source", "eligible_only"),
        default="preserve_source",
        help="coverage policy of the stream",
    )
    parser.add_argument(
        "--max-items",
        type=int,
        default=1,
        help="items per timed get_text_units page (server limit 256)",
    )
    parser.add_argument(
        "--max-chars",
        type=int,
        default=None,
        help="max_chars for context and read modes",
    )
    parser.add_argument(
        "--largest-section",
        action="store_true",
        help="profile the section with the most stream items instead of the start-fraction one",
    )
    parser.add_argument(
        "--io-bytes",
        action="store_true",
        help="record /proc/<pid>/io read-byte deltas around every timed call",
    )
    parser.add_argument(
        "--rss-phases",
        action="store_true",
        help="record VmRSS after open, after seek and after the timed reads",
    )
    parser.add_argument("--env", action="append", default=[], metavar="KEY=VALUE")
    parser.add_argument("--out", required=True, help="JSON result path")
    args = parser.parse_args()
    try:
        return probe(args)
    except ProbeError as exc:
        print(f"deep-next-probe: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
