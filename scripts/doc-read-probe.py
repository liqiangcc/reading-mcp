#!/usr/bin/env python3
"""Measure what one per-call read costs (R1c-3a).

The probe speaks newline-delimited JSON-RPC MCP over stdio against a release binary:

1. open the document and read its structure, without timing;
2. seek to `--start-fraction` of the 50% section's sentence stream, without timing, and take the
   locator of the unit at that position (the anchor every non-cursor shape uses);
3. time `--calls` calls of every read shape:
   - `get_text_units`: forward sentence pages of `--max-items`, following `next_cursor` (and
     restarting from the first cursor when the stream ends, counted in `restarts`);
   - `get_context`: two sentence neighbours each side of the anchor locator;
   - `read_document`: the anchor's exact locator;
   - `search_document`: a term taken from the middle of the anchor's text;
   - `get_document_structure`: the first page of the document;
   - `v2_read`: the v2 `read` follow-up after one v2 `open`, following `next`; skipped when the
     server does not expose the v2 tools (`READING_MCP_TOOLS`);
4. sample the server's RSS in the background for the whole run and report its peak, plus the RSS
   right after `open_document` and after the timed loops.

`--io-bytes` records the `/proc/<pid>/io` `rchar`/`read_bytes` deltas around every timed call.
`--side` names the binary in the report (`main` or `branch`); there is no document-cache dimension.

Standard library only. This script never builds anything: pass an existing release binary.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import statistics
import subprocess
import sys
import tempfile
import threading
import time

REQUEST_TIMEOUT = 1800.0
SEEK_PAGE = 256  # the server caps get_text_units max_items at 256
SHAPES = (
    "get_text_units",
    "get_context",
    "read_document",
    "search_document",
    "get_document_structure",
    "v2_read",
)


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

    def __init__(self, command: list[str], env: dict, log_path: str) -> None:
        self.log_path = log_path
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
            return read_response(self.responses, request_id, REQUEST_TIMEOUT)
        except TimeoutError as exc:
            self.kill()
            raise ProbeError(str(exc)) from None
        except Exception as exc:  # noqa: BLE001 - surfaced as a probe error
            self.kill()
            raise ProbeError(f"{type(exc).__name__}: {exc}") from None

    def initialize(self) -> None:
        response = self.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "doc-read-probe", "version": "0"},
            },
        )
        error = extract_error(response)
        if error is not None:
            raise ProbeError(error)
        send(self.process, {"jsonrpc": "2.0", "method": "notifications/initialized"})

    def tool_names(self) -> list[str]:
        response = self.request("tools/list", {})
        error = extract_error(response)
        if error is not None:
            raise ProbeError(error)
        tools = response.get("result", {}).get("tools")
        if not isinstance(tools, list):
            return []
        return [tool.get("name", "") for tool in tools if isinstance(tool, dict)]

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

    def log_offset(self) -> int:
        try:
            return os.path.getsize(self.log_path)
        except OSError:
            return 0

    def log_since(self, offset: int) -> list[dict]:
        """The JSON telemetry events the server wrote to stderr after `offset`."""
        try:
            with open(self.log_path, "rb") as handle:
                handle.seek(offset)
                data = handle.read()
        except OSError:
            return []
        events = []
        for line in data.splitlines():
            line = line.strip()
            if not line.startswith(b"{"):
                continue
            try:
                value = json.loads(line.decode("utf-8", errors="replace"))
            except json.JSONDecodeError:
                continue
            if isinstance(value, dict):
                events.append(value)
        return events


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


def seek(
    server: Server, document_id: str, section_id: str, target: int
) -> tuple[str | None, int, dict | None]:
    """Page forward to `target` stream position; returns (cursor, position, last item)."""
    position = 0
    cursor = None
    last_item = None
    while position < target:
        want = min(SEEK_PAGE, target - position)
        arguments = {
            "document_id": document_id,
            "section_id": section_id,
            "requested_kind": "sentence",
            "direction": "forward",
            "coverage_policy": "preserve_source",
            "max_items": want,
        }
        if cursor is not None:
            arguments["cursor"] = cursor
        payload = text_units(server, arguments)
        items = payload.get("items")
        if isinstance(items, list) and items:
            last_item = items[-1]
        end = payload["stream"].get("end_index")
        if not isinstance(end, int) or end <= position:
            raise ProbeError(f"seek made no progress at position {position} (end_index {end!r})")
        position = end
        cursor = payload.get("next_cursor")
        if not cursor:
            break
    return cursor, position, last_item


def search_term(text: str) -> str:
    """A contiguous run of up to 8 word/CJK characters from the middle of `text`."""
    run: list[str] = []
    runs: list[list[str]] = []
    for char in text:
        if char.isalnum():
            run.append(char)
        elif run:
            runs.append(run)
            run = []
    if run:
        runs.append(run)
    if not runs:
        raise ProbeError("the anchor text has no searchable characters")
    # The longest run of the middle half of the text: a distinctive term, not a stop word.
    low = len(runs) // 4
    high = max(low + 1, (3 * len(runs)) // 4)
    middle = max(runs[low:high], key=len)
    start = max(0, (len(middle) - 8) // 2)
    return "".join(middle[start : start + 8])


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


class RssSampler:
    """Samples the server's RSS in the background and keeps the peak."""

    def __init__(self, pid: int) -> None:
        self.pid = pid
        self.peak = 0
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self._run, daemon=True)

    def _run(self) -> None:
        while not self.stop.is_set():
            rss = proc_rss_kib(self.pid)
            if rss is not None and rss > self.peak:
                self.peak = rss
            time.sleep(0.001)

    def __enter__(self) -> "RssSampler":
        self.thread.start()
        return self

    def __exit__(self, *_: object) -> None:
        self.stop.set()
        self.thread.join(timeout=1.0)


def timed_tool(server: Server, tool: str, arguments: dict, measure_io: bool):
    before_io = proc_io(server.process.pid) if measure_io else None
    start = time.monotonic()
    result = server.tool(tool, arguments)
    elapsed = time.monotonic() - start
    after_io = proc_io(server.process.pid) if measure_io else None
    delta = io_delta(before_io, after_io) if before_io is not None else None
    return elapsed, delta, result


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


def summarize_io(io_per_call: list[dict[str, int]]) -> dict:
    if not io_per_call:
        return {}
    return {
        "io_calls": len(io_per_call),
        "io_rchar_mean": round(
            statistics.fmean(entry.get("rchar", 0) for entry in io_per_call), 1
        ),
        "io_read_bytes_mean": round(
            statistics.fmean(entry.get("read_bytes", 0) for entry in io_per_call), 1
        ),
    }


def run_cursor_series(
    server: Server,
    tool: str,
    arguments: dict,
    request_key: str,
    response_key: str,
    cursor: str | None,
    calls: int,
    measure_io: bool,
) -> tuple[list[float], list[dict[str, int]], int]:
    """`calls` calls that pass `request_key` and follow `response_key`, restarting at the end."""
    seconds: list[float] = []
    io_per_call: list[dict[str, int]] = []
    restarts = 0
    current = cursor
    for _ in range(calls):
        call_arguments = dict(arguments)
        if current is not None:
            call_arguments[request_key] = current
        elapsed, delta, result = timed_tool(server, tool, call_arguments, measure_io)
        seconds.append(elapsed)
        if delta is not None:
            io_per_call.append(delta)
        payload = tool_payload(result)
        next_cursor = payload.get(response_key) if isinstance(payload, dict) else None
        if not isinstance(next_cursor, str):
            current = cursor
            restarts += 1
        else:
            current = next_cursor
    return seconds, io_per_call, restarts


def run_repeated(
    server: Server, tool: str, arguments: dict, calls: int, measure_io: bool
) -> tuple[list[float], list[dict[str, int]]]:
    seconds: list[float] = []
    io_per_call: list[dict[str, int]] = []
    for _ in range(calls):
        elapsed, delta, _ = timed_tool(server, tool, arguments, measure_io)
        seconds.append(elapsed)
        if delta is not None:
            io_per_call.append(delta)
    return seconds, io_per_call


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
    if not 1 <= args.max_items <= SEEK_PAGE:
        raise ProbeError(f"--max-items must be in [1, {SEEK_PAGE}]")
    scratch_root = os.path.join(os.getcwd(), ".tmp")
    os.makedirs(scratch_root, exist_ok=True)
    state_dir = tempfile.mkdtemp(prefix="doc-read-state-", dir=scratch_root)
    env = dict(os.environ)
    env["READING_MCP_STATE_DIR"] = state_dir
    env["READING_MCP_LOCAL_ROOTS"] = os.path.dirname(document)
    env["READING_MCP_MAX_DOCUMENT_BYTES"] = "134217728"
    env["READING_MCP_MAX_NORMALIZED_CHARS"] = "200000000"
    env["READING_MCP_PARSE_TIMEOUT_SECS"] = "900"
    env["READING_MCP_TOOLS"] = "both"
    env["READING_MCP_TELEMETRY"] = "true"
    for item in args.env:
        key, separator, value = item.partition("=")
        if not separator or not key:
            raise ProbeError(f"--env expects KEY=VALUE, got {item!r}")
        env[key] = value

    server_log = os.path.join(scratch_root, f"doc-read-server-{args.label}.log")
    server = Server([binary], env, server_log)
    sampler = RssSampler(server.process.pid)
    sampler.__enter__()
    shapes: dict[str, dict] = {}
    skipped: list[dict] = []
    open_seconds = None
    rss_after_open = None
    rss_after_calls = None
    try:
        server.initialize()
        tool_names = server.tool_names()
        opened = time.monotonic()
        payload = tool_payload(server.tool("open_document", {"source": document}))
        open_seconds = time.monotonic() - opened
        rss_after_open = proc_rss_kib(server.process.pid)
        if not isinstance(payload, dict) or not isinstance(payload.get("document_id"), str):
            raise ProbeError("open_document did not return a document_id")
        document_id = payload["document_id"]
        sections = read_structure(server, document_id)
        if not sections:
            raise ProbeError("document has no sections")
        index = min(len(sections) - 1, int(len(sections) * args.start_fraction))
        section_id = sections[index]["section_id"]
        head = text_units(
            server,
            {
                "document_id": document_id,
                "section_id": section_id,
                "requested_kind": "sentence",
                "direction": "forward",
                "coverage_policy": "preserve_source",
                "max_items": 1,
            },
        )
        total_items = head["stream"].get("total_items")
        if not isinstance(total_items, int) or total_items <= 0:
            raise ProbeError(f"section reported {total_items!r} sentence items")
        cursor, start_position, item = seek(
            server, document_id, section_id, int(total_items * args.start_fraction)
        )
        if not isinstance(item, dict) or not isinstance(item.get("locator"), dict):
            raise ProbeError("the seek position has no sentence locator")
        locator = item["locator"]
        query = search_term(str(item.get("text", "")))
        print(
            f"{args.label}: opened in {open_seconds:.2f} s; section {index}/{len(sections)} has "
            f"{total_items} sentences; anchored at {start_position}; query {query!r}",
            flush=True,
        )

        base_units = {
            "document_id": document_id,
            "section_id": section_id,
            "requested_kind": "sentence",
            "direction": "forward",
            "coverage_policy": "preserve_source",
            "max_items": args.max_items,
        }
        seconds, io_per_call, restarts = run_cursor_series(
            server,
            "get_text_units",
            base_units,
            "cursor",
            "next_cursor",
            cursor,
            args.calls,
            args.io_bytes,
        )
        shapes["get_text_units"] = {
            **summarize(seconds),
            **summarize_io(io_per_call),
            "restarts": restarts,
        }

        context_arguments = {
            "document_id": document_id,
            "target_locator": locator,
            "relation": {"type": "neighbor", "unit": "sentence", "before": 2, "after": 2},
        }
        seconds, io_per_call = run_repeated(
            server, "get_context", context_arguments, args.calls, args.io_bytes
        )
        shapes["get_context"] = {**summarize(seconds), **summarize_io(io_per_call)}

        read_arguments = {"document_id": document_id, "target_locator": locator}
        seconds, io_per_call = run_repeated(
            server, "read_document", read_arguments, args.calls, args.io_bytes
        )
        shapes["read_document"] = {**summarize(seconds), **summarize_io(io_per_call)}

        search_arguments = {"document_id": document_id, "query": query, "limit": 10}
        seconds, io_per_call = run_repeated(
            server, "search_document", search_arguments, args.calls, args.io_bytes
        )
        hits = tool_payload(server.tool("search_document", search_arguments))
        shapes["search_document"] = {
            **summarize(seconds),
            **summarize_io(io_per_call),
            "hits": len(hits.get("hits", [])) if isinstance(hits, dict) else None,
            "query": query,
        }

        structure_arguments = {"document_id": document_id, "max_nodes": 50}
        seconds, io_per_call = run_repeated(
            server, "get_document_structure", structure_arguments, args.calls, args.io_bytes
        )
        shapes["get_document_structure"] = {**summarize(seconds), **summarize_io(io_per_call)}

        if "open" in tool_names and "read" in tool_names:
            opened_v2 = tool_payload(server.tool("open", {"source": document}))
            handle = opened_v2.get("doc") if isinstance(opened_v2, dict) else None
            if not isinstance(handle, str):
                skipped.append({"shape": "v2_read", "reason": "v2 open returned no handle"})
            else:
                seconds, io_per_call, restarts = run_cursor_series(
                    server,
                    "read",
                    {"from": handle},
                    "from",
                    "next",
                    handle,
                    args.calls,
                    args.io_bytes,
                )
                shapes["v2_read"] = {
                    **summarize(seconds),
                    **summarize_io(io_per_call),
                    "restarts": restarts,
                }
        else:
            skipped.append(
                {
                    "shape": "v2_read",
                    "reason": "the server exposes no v2 open/read tools",
                }
            )

        rss_after_calls = proc_rss_kib(server.process.pid)
    finally:
        sampler.__exit__()
        server.close()

    result = {
        "label": args.label,
        "document": document,
        "binary": binary,
        "side": args.side,
        "calls": args.calls,
        "start_fraction": args.start_fraction,
        "max_items": args.max_items,
        "sections": len(sections),
        "section_index": index,
        "section_id": section_id,
        "total_items": total_items,
        "start_position": start_position,
        "open_seconds": None if open_seconds is None else round(open_seconds, 2),
        "rss_after_open_kib": rss_after_open,
        "rss_after_calls_kib": rss_after_calls,
        "peak_rss_kib": sampler.peak,
        "io_bytes": args.io_bytes,
        "shapes": shapes,
        "skipped": skipped,
    }
    out_path = os.path.abspath(args.out)
    os.makedirs(os.path.dirname(out_path) or ".", exist_ok=True)
    with open(out_path, "w", encoding="utf-8") as handle:
        json.dump(result, handle, indent=2)
        handle.write("\n")

    for shape in SHAPES:
        summary = shapes.get(shape)
        if summary is None:
            continue
        print(
            "{label} side={side} {shape}: mean_ms={mean_ms} median_ms={median_ms} "
            "p95_ms={p95_ms} rchar/call={io_rchar_mean} read_bytes/call={io_read_bytes_mean}".format(
                label=args.label,
                side=args.side,
                shape=shape,
                **{**summary, "io_rchar_mean": summary.get("io_rchar_mean", 0)},
            ),
            flush=True,
        )
    print(
        f"{args.label} side={args.side}: open_s={open_seconds} "
        f"rss_after_open_kib={rss_after_open} rss_after_calls_kib={rss_after_calls} "
        f"peak_rss_kib={sampler.peak}",
        flush=True,
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Measure per-call document reads of one binary."
    )
    parser.add_argument("--binary", required=True, help="reading-mcp release binary")
    parser.add_argument("--document", required=True, help="document to open and read")
    parser.add_argument("--label", required=True, help="input label for the report")
    parser.add_argument("--calls", type=int, required=True, help="timed calls per shape")
    parser.add_argument(
        "--side",
        default="branch",
        help="binary side label for the report (main or branch)",
    )
    parser.add_argument(
        "--start-fraction",
        type=float,
        default=0.5,
        help="fraction of the section list and of the sentence stream to read at",
    )
    parser.add_argument(
        "--max-items",
        type=int,
        default=1,
        help="items per timed get_text_units page (server limit 256)",
    )
    parser.add_argument(
        "--io-bytes",
        action="store_true",
        help="record /proc/<pid>/io read-byte deltas around every timed call",
    )
    parser.add_argument("--env", action="append", default=[], metavar="KEY=VALUE")
    parser.add_argument("--out", required=True, help="JSON result path")
    args = parser.parse_args()
    try:
        return probe(args)
    except ProbeError as exc:
        print(f"doc-read-probe: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
