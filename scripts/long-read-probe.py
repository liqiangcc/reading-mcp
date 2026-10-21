#!/usr/bin/env python3
"""Measure RSS growth while reading one document sentence by sentence.

The probe speaks newline-delimited JSON-RPC MCP over stdio.  It opens one server process on a
fresh state directory, walks the document's sections in body order with `get_text_units`
(`max_items: 1`), and repeats the book from the beginning when it reaches the end, so `--calls`
may exceed the sentence count.  Every 1,000 calls it samples the server's `VmRSS` and `VmHWM`
from `/proc/<pid>/status`.  With `--wrapper`, the real server is the child of the wrapper.

Standard library only.  This script never builds or starts anything itself: pass an existing
release binary with `--binary`.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import shlex
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
        "clientInfo": {"name": "long-read-probe", "version": "0"},
    },
}

INITIALIZED_NOTIFICATION = {
    "jsonrpc": "2.0",
    "method": "notifications/initialized",
}

TEXT_UNIT_BASE = {
    "requested_kind": "sentence",
    "direction": "forward",
    "coverage_policy": "preserve_source",
}

SAMPLE_EVERY = 1_000
REQUEST_TIMEOUT = 900.0


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


def result_text(result: dict) -> str | None:
    content = result.get("content")
    if not isinstance(content, list):
        return None
    for block in content:
        if isinstance(block, dict) and block.get("type") == "text":
            text = block.get("text")
            return text if isinstance(text, str) else None
    return None


def result_structured(result: dict):
    structured = result.get("structuredContent")
    if structured is None:
        structured = result.get("structured_content")
    return structured if isinstance(structured, (dict, list)) else None


def tool_payload(result: dict):
    """Return the tool's JSON payload, from structuredContent or content[0].text."""
    structured = result_structured(result)
    if structured is not None:
        return structured
    text = result_text(result)
    if text is None:
        return None
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return None


class Server:
    """One MCP server process over stdio."""

    def __init__(
        self,
        command: list[str],
        env: dict,
        log_path: str,
        request_timeout: float,
        cwd: str | None = None,
    ) -> None:
        self.request_timeout = request_timeout
        self.log_handle = open(log_path, "wb")
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.log_handle,
            env=env,
            cwd=cwd,
        )
        self.responses: queue.Queue = queue.Queue()
        self.reader = threading.Thread(
            target=pump_stdout, args=(self.process.stdout, self.responses), daemon=True
        )
        self.reader.start()
        self.next_id = 0

    def _new_id(self) -> int:
        self.next_id += 1
        return self.next_id

    def request(self, method: str, params: dict | None = None) -> dict:
        request_id = self._new_id()
        message = {"jsonrpc": "2.0", "id": request_id, "method": method}
        if params is not None:
            message["params"] = params
        try:
            send(self.process, message)
            response = read_response(self.responses, request_id, self.request_timeout)
        except TimeoutError as exc:
            self.kill()
            raise ProbeError(str(exc)) from None
        except Exception as exc:  # noqa: BLE001 - surfaced as a probe error
            self.kill()
            raise ProbeError(f"{type(exc).__name__}: {exc}") from None
        return response

    def notify(self, method: str) -> None:
        send(self.process, {"jsonrpc": "2.0", "method": method})

    def initialize(self) -> None:
        response = self.request("initialize", INITIALIZE_MESSAGE["params"])
        error = extract_error(response)
        if error is not None:
            raise ProbeError(error)
        self.notify("notifications/initialized")

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

    def close(self, timeout: float = 60.0) -> None:
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


def server_environment(state_dir: str, roots: str, overrides: list[str]) -> dict:
    env = dict(os.environ)
    env["READING_MCP_STATE_DIR"] = state_dir
    env["READING_MCP_LOCAL_ROOTS"] = roots
    env["READING_MCP_TOOLS"] = "v1"
    for item in overrides:
        key, separator, value = item.partition("=")
        if not separator or not key:
            raise ProbeError(f"--env expects KEY=VALUE, got {item!r}")
        env[key] = value
    return env


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


def child_pids(pid: int) -> list[int]:
    pids: list[int] = []
    task_dir = f"/proc/{pid}/task"
    try:
        tids = os.listdir(task_dir)
    except OSError:
        return pids
    for tid in tids:
        try:
            with open(f"{task_dir}/{tid}/children", encoding="ascii") as handle:
                for token in handle.read().split():
                    try:
                        pids.append(int(token))
                    except ValueError:
                        continue
        except OSError:
            continue
    return pids


def process_exe(pid: int) -> str | None:
    try:
        return os.path.realpath(f"/proc/{pid}/exe")
    except OSError:
        return None


def descendants(pid: int) -> list[int]:
    found: list[int] = []
    stack = [pid]
    seen: set[int] = set()
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        for child in child_pids(current):
            found.append(child)
            stack.append(child)
    return found


def resolve_server_pid(process: subprocess.Popen, wrapped: bool, binary: str) -> int:
    if not wrapped:
        return process.pid
    target = os.path.realpath(binary)
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        # The wrapper may exec the server itself or start it one or more levels down.
        for pid in [process.pid, *descendants(process.pid)]:
            if process_exe(pid) == target:
                return pid
        time.sleep(0.05)
    try:
        output = subprocess.check_output(
            ["pgrep", "-P", str(process.pid)], text=True
        ).split()
        if output:
            return int(output[0])
    except (subprocess.SubprocessError, ValueError):
        pass
    raise ProbeError("could not find the server process under the wrapper")


def read_rss_mib(pid: int) -> tuple[float, float]:
    rss_kib: int | None = None
    hwm_kib: int | None = None
    try:
        with open(f"/proc/{pid}/status", encoding="ascii") as handle:
            for line in handle:
                if line.startswith("VmRSS:"):
                    rss_kib = int(line.split()[1])
                elif line.startswith("VmHWM:"):
                    hwm_kib = int(line.split()[1])
    except OSError as exc:
        raise ProbeError(f"cannot read /proc/{pid}/status: {exc}") from None
    if rss_kib is None or hwm_kib is None:
        raise ProbeError(f"/proc/{pid}/status has no VmRSS/VmHWM")
    return rss_kib / 1024.0, hwm_kib / 1024.0


def dump_output(path: str, data: dict) -> None:
    directory = os.path.dirname(os.path.abspath(path))
    if directory:
        os.makedirs(directory, exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(data, handle, ensure_ascii=False, indent=1)


def probe(args: argparse.Namespace) -> int:
    document = os.path.abspath(args.document)
    if not os.path.isfile(document):
        raise ProbeError(f"file not found: {document}")
    if args.calls <= 0:
        raise ProbeError("--calls must be positive")

    out_path = os.path.abspath(args.out)
    out_dir = os.path.dirname(out_path) or "."
    os.makedirs(out_dir, exist_ok=True)
    scratch_root = os.path.join(os.getcwd(), ".tmp")
    os.makedirs(scratch_root, exist_ok=True)
    state_dir = tempfile.mkdtemp(prefix="leak-state-", dir=scratch_root)
    log_path = os.path.join(scratch_root, "long-read-server.log")

    wrapper_parts = shlex.split(args.wrapper) if args.wrapper else []
    binary = os.path.abspath(args.binary)
    if not os.path.isfile(binary):
        raise ProbeError(f"binary not found: {binary}")
    command = wrapper_parts + [binary] if wrapper_parts else [binary]
    env = server_environment(state_dir, os.path.dirname(document), args.env)

    server = Server(command, env, log_path, REQUEST_TIMEOUT)
    samples: list[dict] = []
    calls = 0
    try:
        server.initialize()
        opened = tool_payload(server.tool("open_document", {"source": document}))
        if not isinstance(opened, dict):
            raise ProbeError("open_document did not return an object")
        document_id = opened.get("document_id")
        if not isinstance(document_id, str):
            raise ProbeError("open_document did not return a document_id")
        sections = read_structure(server, document_id)
        if not sections:
            raise ProbeError("document has no sections")

        pid = resolve_server_pid(server.process, bool(wrapper_parts), binary)
        start_rss, start_hwm = read_rss_mib(pid)
        samples.append(
            {"calls": 0, "rss_mib": round(start_rss, 1), "hwm_mib": round(start_hwm, 1)}
        )

        section_index = 0
        cursor = None
        while calls < args.calls:
            section_id = sections[section_index]["section_id"]
            arguments = {
                "document_id": document_id,
                "section_id": section_id,
                **TEXT_UNIT_BASE,
                "max_items": 1,
            }
            if cursor is not None:
                arguments["cursor"] = cursor
            payload = tool_payload(server.tool("get_text_units", arguments))
            if not isinstance(payload, dict):
                raise ProbeError("get_text_units returned no JSON object")
            calls += 1
            cursor = payload.get("next_cursor")
            if calls % SAMPLE_EVERY == 0:
                rss, hwm = read_rss_mib(pid)
                samples.append(
                    {
                        "calls": calls,
                        "rss_mib": round(rss, 1),
                        "hwm_mib": round(hwm, 1),
                    }
                )
            if not cursor:
                section_index += 1
                if section_index >= len(sections):
                    section_index = 0
                cursor = None

        end_rss, end_hwm = read_rss_mib(pid)
        if samples[-1]["calls"] != calls:
            samples.append(
                {"calls": calls, "rss_mib": round(end_rss, 1), "hwm_mib": round(end_hwm, 1)}
            )
    finally:
        server.close()

    growth = (end_rss - start_rss) / (calls / 100_000.0) if calls else 0.0
    series = [sample for index, sample in enumerate(samples) if index % 10 == 0]
    if samples and samples[-1] not in series:
        series.append(samples[-1])

    print(
        f"calls={calls} rss_start_mib={start_rss:.1f} rss_end_mib={end_rss:.1f} "
        f"growth_mib_per_100k={growth:.1f}"
    )
    print(
        "rss_series_mib="
        + ",".join(f"{sample['calls']}:{sample['rss_mib']:.1f}" for sample in series)
    )

    dump_output(
        out_path,
        {
            "document": document,
            "calls": calls,
            "wrapper": args.wrapper,
            "env": args.env,
            "rss_start_mib": round(start_rss, 1),
            "rss_end_mib": round(end_rss, 1),
            "hwm_start_mib": round(start_hwm, 1),
            "hwm_end_mib": round(end_hwm, 1),
            "growth_mib_per_100k": round(growth, 1),
            "samples": samples,
        },
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, help="reading-mcp binary to run")
    parser.add_argument("--document", required=True, help="document to read")
    parser.add_argument("--calls", type=int, required=True, help="get_text_units calls to make")
    parser.add_argument(
        "--wrapper",
        default=None,
        help='optional command prefix, e.g. "heaptrack -o leak/en-novel"',
    )
    parser.add_argument(
        "--env",
        action="append",
        default=[],
        metavar="KEY=VALUE",
        help="extra server environment entry (repeatable)",
    )
    parser.add_argument("--out", required=True, help="JSON result path")
    args = parser.parse_args()
    try:
        return probe(args)
    except ProbeError as exc:
        print(f"long-read-probe: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
