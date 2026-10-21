#!/usr/bin/env python3
"""Measure the cost of sequential sentence reading over reading-mcp.

The probe speaks newline-delimited JSON-RPC MCP over stdio.  For every document it opens one
server process, enumerates the structure, walks every Section for the "page" pass, then reads
sentence by sentence ("one") and twenty at a time ("twenty").  Every tool call records wall
time, the size of both `content[0].text` and `structuredContent`, item text volume, cursor size,
locator size and a deliberately rough token estimate.

The same script also measures the token footprint of `tools/list` once per run and of one
representative call per tool per book.

Token estimates use `ascii_chars / 4 + non_ascii_chars / 1.5`.  That is a rough heuristic, not a
tokenizer.

Standard library only.  This script never builds or starts anything itself: pass an existing
release binary with `--binary`.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import queue
import shutil
import statistics
import subprocess
import sys
import threading
import time

INITIALIZE_MESSAGE = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "sequential-read-probe", "version": "0"},
    },
}

INITIALIZED_NOTIFICATION = {
    "jsonrpc": "2.0",
    "method": "notifications/initialized",
}

SEARCH_QUERIES = {
    "en-novel": "Pierre",
    "zh-classic": "寶玉",
    "zh-large": "皇帝",
}

TEXT_UNIT_BASE = {
    "requested_kind": "sentence",
    "direction": "forward",
    "coverage_policy": "preserve_source",
}


class ProbeError(RuntimeError):
    """A transport, protocol, tool or deadline failure."""


class DeadlineExceeded(ProbeError):
    """The per-pass (per-book) time budget ran out."""


def send(process: subprocess.Popen, message: dict) -> None:
    assert process.stdin is not None
    process.stdin.write((json.dumps(message) + "\n").encode("utf-8"))
    process.stdin.flush()


def pump_stdout(stream, responses: queue.Queue) -> None:
    """Read JSON-RPC responses on a background thread so callers never block past a deadline."""
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


def read_response(responses: queue.Queue, awaited_id: int, timeout: float | None) -> dict:
    deadline = None if timeout is None else time.monotonic() + timeout
    while True:
        if deadline is None:
            wait: float | None = None
        else:
            wait = deadline - time.monotonic()
            if wait <= 0:
                raise TimeoutError(f"timed out waiting for response to id {awaited_id}")
        try:
            item = responses.get(timeout=wait) if wait is not None else responses.get()
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


def compact_json(value) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def est_tokens(text: str) -> float:
    """Rough token estimate: ascii_chars / 4 + non_ascii_chars / 1.5."""
    if not text:
        return 0.0
    ascii_chars = sum(1 for char in text if ord(char) < 128)
    non_ascii_chars = len(text) - ascii_chars
    return ascii_chars / 4.0 + non_ascii_chars / 1.5


def measure_result(result: dict) -> dict:
    """Measure a tool result exactly as the client sees it."""
    text = result_text(result)
    structured = result_structured(result)
    structured_json = compact_json(structured) if structured is not None else None
    both = text is not None and structured is not None
    same_json: bool | None = None
    if both:
        try:
            same_json = json.loads(text) == structured
        except json.JSONDecodeError:
            same_json = None

    text_bytes = len(text.encode("utf-8")) if text is not None else 0
    text_chars = len(text) if text is not None else 0
    structured_bytes = len(structured_json.encode("utf-8")) if structured_json else 0
    structured_chars = len(structured_json) if structured_json else 0
    return {
        "text_present": text is not None,
        "structured_present": structured is not None,
        "both_present": both,
        "same_json": same_json,
        "text_bytes": text_bytes,
        "text_chars": text_chars,
        "structured_bytes": structured_bytes,
        "structured_chars": structured_chars,
        "resp_bytes": text_bytes + structured_bytes,
        "resp_chars": text_chars + structured_chars,
        "est_tokens": est_tokens(text or "") + est_tokens(structured_json or ""),
        "text": text,
    }


def item_stats(result: dict, measurement: dict) -> dict:
    items = result.get("items")
    if not isinstance(items, list):
        items = []
    item_text_bytes = 0
    item_text_chars = 0
    item_text_token_estimate = 0.0
    locator_chars_total = 0
    first_locator_chars = 0
    for index, item in enumerate(items):
        if not isinstance(item, dict):
            continue
        item_text = item.get("text")
        if isinstance(item_text, str):
            item_text_bytes += len(item_text.encode("utf-8"))
            item_text_chars += len(item_text)
            item_text_token_estimate += est_tokens(item_text)
        locator = item.get("locator")
        if isinstance(locator, dict):
            locator_chars = len(compact_json(locator))
            locator_chars_total += locator_chars
            if index == 0:
                first_locator_chars = locator_chars
    cursor = result.get("next_cursor")
    return {
        "items": len(items),
        "item_text_bytes": item_text_bytes,
        "item_text_chars": item_text_chars,
        "item_text_tokens": item_text_token_estimate,
        "first_locator_chars": first_locator_chars,
        "locator_chars": locator_chars_total,
        "has_cursor": bool(cursor),
        "cursor_chars": len(cursor) if isinstance(cursor, str) else 0,
    }


class Server:
    """One MCP server process over stdio."""

    def __init__(
        self,
        binary: str,
        env: dict,
        log_path: str,
        request_timeout: float,
        cwd: str | None = None,
    ) -> None:
        self.request_timeout = request_timeout
        self.log_handle = open(log_path, "wb")
        self.process = subprocess.Popen(
            [binary],
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

    def request(
        self, method: str, params: dict | None = None, timeout: float | None = None
    ) -> dict:
        request_id = self._new_id()
        message = {"jsonrpc": "2.0", "id": request_id, "method": method}
        if params is not None:
            message["params"] = params
        try:
            send(self.process, message)
            response = read_response(
                self.responses,
                request_id,
                self.request_timeout if timeout is None else timeout,
            )
        except TimeoutError as exc:
            self.kill()
            raise ProbeError(str(exc)) from None
        except Exception as exc:  # noqa: BLE001 - surfaced as a probe error
            self.kill()
            raise ProbeError(f"{type(exc).__name__}: {exc}") from None
        return response

    def notify(self, method: str) -> None:
        send(self.process, {"jsonrpc": "2.0", "method": method})

    def initialize(self, timeout: float | None = None) -> None:
        response = self.request("initialize", INITIALIZE_MESSAGE["params"], timeout)
        error = extract_error(response)
        if error is not None:
            raise ProbeError(error)
        self.notify("notifications/initialized")

    def tools_list(self, timeout: float | None = None) -> dict:
        response = self.request("tools/list", {}, timeout)
        error = extract_error(response)
        if error is not None:
            raise ProbeError(error)
        result = response.get("result")
        if not isinstance(result, dict):
            raise ProbeError("tools/list did not return an object")
        return result

    def tool(self, name: str, arguments: dict, timeout: float | None = None) -> dict:
        response = self.request(
            "tools/call", {"name": name, "arguments": arguments}, timeout
        )
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

    def close(self) -> None:
        try:
            if self.process.stdin is not None:
                self.process.stdin.close()
        except Exception:  # noqa: BLE001 - server may already be gone
            pass
        try:
            self.process.wait(timeout=10)
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


def server_environment(roots: str, state_dir: str) -> dict:
    env = dict(os.environ)
    env["READING_MCP_STATE_DIR"] = state_dir
    env["READING_MCP_LOCAL_ROOTS"] = roots
    env["READING_MCP_TELEMETRY"] = "false"
    env["READING_MCP_TOOLS"] = "v1"
    return env


def deadline_timeout(deadline: float) -> float:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise DeadlineExceeded("pass time budget exhausted")
    return max(1.0, remaining)


def timed_tool(
    server: Server, name: str, arguments: dict, timeout: float
) -> tuple[dict, float]:
    start = time.monotonic()
    result = server.tool(name, arguments, timeout)
    return result, (time.monotonic() - start) * 1000.0


def make_call_record(
    pass_name: str,
    section_id: str,
    wall_ms: float,
    result: dict,
    capture_sample: bool = False,
    **extra,
) -> dict:
    measurement = measure_result(result)
    items = item_stats(tool_payload(result), measurement)
    record = {
        "pass": pass_name,
        "section_id": section_id,
        "wall_ms": wall_ms,
        "resp_bytes": measurement["resp_bytes"],
        "resp_chars": measurement["resp_chars"],
        "text_bytes": measurement["text_bytes"],
        "text_chars": measurement["text_chars"],
        "structured_bytes": measurement["structured_bytes"],
        "structured_chars": measurement["structured_chars"],
        "both_present": measurement["both_present"],
        "same_json": measurement["same_json"],
        "est_tokens": measurement["est_tokens"],
        "items": items["items"],
        "item_text_bytes": items["item_text_bytes"],
        "item_text_chars": items["item_text_chars"],
        "item_text_tokens": items["item_text_tokens"],
        "cursor_chars": items["cursor_chars"],
        "has_cursor": items["has_cursor"],
        "first_locator_chars": items["first_locator_chars"],
        "locator_chars": items["locator_chars"],
    }
    if capture_sample:
        sample = measurement["text"]
        if sample is None:
            structured = result_structured(result)
            sample = compact_json(structured) if structured is not None else None
        record["sample_text"] = sample
    record.update(extra)
    return record


def p95(values: list[float]) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = max(0, math.ceil(0.95 * len(ordered)) - 1)
    return ordered[index]


def pass_summary(
    pass_name: str,
    records: list[dict],
    elapsed_seconds: float,
    partial: bool,
    error: str | None,
    section_switches: int = 0,
) -> dict:
    item_count = sum(record["items"] for record in records)
    total_resp_bytes = sum(record["resp_bytes"] for record in records)
    total_text_bytes = sum(record["item_text_bytes"] for record in records)
    total_est_tokens = sum(record["est_tokens"] for record in records)
    total_text_tokens = sum(record["item_text_tokens"] for record in records)
    total_locator_chars = sum(record["locator_chars"] for record in records)
    walls = [record["wall_ms"] for record in records]
    cursor_chars = sum(record["cursor_chars"] for record in records)
    denominator = item_count if item_count else 0
    return {
        "pass": pass_name,
        "calls": len(records),
        "items": item_count,
        "total_s": elapsed_seconds,
        "ms_mean": statistics.fmean(walls) if walls else None,
        "ms_p95": p95(walls) if walls else None,
        "resp_bytes_per_item": total_resp_bytes / denominator if denominator else None,
        "text_bytes_per_item": total_text_bytes / denominator if denominator else None,
        "text_share_pct": (
            100.0 * total_text_bytes / total_resp_bytes if total_resp_bytes else None
        ),
        "cursor_chars": cursor_chars / len(records) if records else None,
        "locator_chars_per_item": (
            total_locator_chars / denominator if denominator else None
        ),
        "est_tokens_per_item": total_est_tokens / denominator if denominator else None,
        "est_text_tokens_per_item": (
            total_text_tokens / denominator if denominator else None
        ),
        "partial": partial,
        "error": error,
        "section_switches": section_switches,
    }


def text_unit_arguments(
    document_id: str, section_id: str, max_items: int, cursor: str | None = None
) -> dict:
    arguments = {
        "document_id": document_id,
        "section_id": section_id,
        **TEXT_UNIT_BASE,
        "max_items": max_items,
    }
    if cursor is not None:
        arguments["cursor"] = cursor
    return arguments


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


def read_structure(server: Server, document_id: str, deadline: float) -> tuple[list[dict], int]:
    sections: dict[str, dict] = {}
    calls = 0
    cursor = None
    while True:
        arguments = {"document_id": document_id, "max_nodes": 500}
        if cursor is not None:
            arguments["cursor"] = cursor
        result = server.tool(
            "get_document_structure", arguments, deadline_timeout(deadline)
        )
        calls += 1
        payload = tool_payload(result)
        if not isinstance(payload, dict):
            raise ProbeError("get_document_structure returned no JSON object")
        for node in flatten_structure(payload):
            sections.setdefault(node["section_id"], node)
        cursor = payload.get("next_cursor")
        if not cursor or payload.get("complete") is True:
            break
        if calls > 10000:
            raise ProbeError("structure pagination did not terminate")
    ordered = sorted(sections.values(), key=lambda node: node.get("body_order", 0))
    return ordered, calls


def run_page_pass(
    server: Server,
    document_id: str,
    sections: list[dict],
    deadline: float,
) -> tuple[list[dict], float, bool, str | None, int]:
    records: list[dict] = []
    start = time.monotonic()
    partial = False
    error = None
    transitions = 0
    try:
        for section in sections:
            if time.monotonic() >= deadline:
                partial = True
                break
            section_id = section["section_id"]
            section_items = 0
            cursor = None
            seen_cursors: set[str] = set()
            for _ in range(100000):
                if time.monotonic() >= deadline:
                    partial = True
                    break
                arguments = text_unit_arguments(document_id, section_id, 256, cursor)
                result, wall_ms = timed_tool(
                    server, "get_text_units", arguments, deadline_timeout(deadline)
                )
                record = make_call_record(
                    "page",
                    section_id,
                    wall_ms,
                    result,
                    capture_sample=not records,
                )
                records.append(record)
                section_items += record["items"]
                cursor = (tool_payload(result) or {}).get("next_cursor")
                if not cursor:
                    break
                if cursor in seen_cursors:
                    raise ProbeError("get_text_units cursor repeated within a section")
                seen_cursors.add(cursor)
            else:
                raise ProbeError("get_text_units pagination did not terminate")
            if section_items > 0:
                transitions += 1
            if partial:
                break
    except DeadlineExceeded:
        partial = True
    except ProbeError as exc:
        error = str(exc)
    return records, time.monotonic() - start, partial, error, transitions


def start_section_index(sections: list[dict], page_section_items: dict[str, int]) -> int:
    for index, section in enumerate(sections):
        if page_section_items.get(section["section_id"], 0) > 0:
            return index
    return 0


def run_item_pass(
    server: Server,
    document_id: str,
    sections: list[dict],
    start_index: int,
    max_items: int,
    item_cap: int,
    deadline: float,
    pass_name: str,
) -> tuple[list[dict], float, bool, str | None, int]:
    records: list[dict] = []
    start = time.monotonic()
    partial = False
    error = None
    switches = 0
    total_items = 0
    cap_reached = False
    try:
        for index in range(start_index, len(sections)):
            if cap_reached:
                break
            if time.monotonic() >= deadline:
                partial = True
                break
            section_id = sections[index]["section_id"]
            cursor = None
            seen_cursors: set[str] = set()
            for _ in range(100000):
                if time.monotonic() >= deadline:
                    partial = True
                    break
                if total_items >= item_cap:
                    cap_reached = True
                    break
                arguments = text_unit_arguments(
                    document_id, section_id, max_items, cursor
                )
                result, wall_ms = timed_tool(
                    server, "get_text_units", arguments, deadline_timeout(deadline)
                )
                record = make_call_record(
                    pass_name,
                    section_id,
                    wall_ms,
                    result,
                    capture_sample=pass_name == "one" and not records,
                )
                records.append(record)
                total_items += record["items"]
                cursor = (tool_payload(result) or {}).get("next_cursor")
                if not cursor:
                    if total_items < item_cap and index + 1 < len(sections):
                        switches += 1
                    break
                if cursor in seen_cursors:
                    raise ProbeError("get_text_units cursor repeated within a section")
                seen_cursors.add(cursor)
            else:
                raise ProbeError("get_text_units pagination did not terminate")
            if total_items >= item_cap:
                cap_reached = True
    except DeadlineExceeded:
        partial = True
    except ProbeError as exc:
        error = str(exc)
    return records, time.monotonic() - start, partial, error, switches


def count_occurrences(haystack: str | None, needle: str | None) -> int:
    if not haystack or not needle:
        return 0
    return haystack.count(needle)


def representative_record(
    book_label: str,
    tool_name: str,
    result: dict,
    wall_ms: float,
    document_id: str | None,
    content_hash: str | None,
    normalized_hash: str | None,
) -> dict:
    measurement = measure_result(result)
    text_basis = measurement["text"]
    if text_basis is None:
        structured = result_structured(result)
        text_basis = compact_json(structured) if structured is not None else None
    structured = result_structured(result)
    structured_basis = compact_json(structured) if structured is not None else None
    doc_id_count = count_occurrences(text_basis, document_id)
    content_hash_count = count_occurrences(text_basis, content_hash)
    normalized_hash_count = count_occurrences(text_basis, normalized_hash)
    return {
        "book": book_label,
        "tool": tool_name,
        "wall_ms": wall_ms,
        "resp_bytes": measurement["resp_bytes"],
        "resp_chars": measurement["resp_chars"],
        "text_bytes": measurement["text_bytes"],
        "structured_bytes": measurement["structured_bytes"],
        "est_tokens": measurement["est_tokens"],
        "both_present": measurement["both_present"],
        "same_json": measurement["same_json"],
        "document_id_count": doc_id_count,
        "content_hash_count": content_hash_count,
        "normalized_hash_count": normalized_hash_count,
        "hashes_count": content_hash_count + normalized_hash_count,
        "structured_document_id_count": count_occurrences(
            structured_basis, document_id
        ),
        "structured_hashes_count": count_occurrences(
            structured_basis, content_hash
        )
        + count_occurrences(structured_basis, normalized_hash),
    }


def tools_list_footprint(result: dict) -> tuple[dict, list[dict]]:
    tools = result.get("tools")
    if not isinstance(tools, list):
        tools = []
    rows = []
    for tool in tools:
        if not isinstance(tool, dict):
            continue
        description = tool.get("description")
        description_chars = len(description) if isinstance(description, str) else 0
        input_schema = tool.get("inputSchema")
        input_chars = len(compact_json(input_schema)) if input_schema is not None else 0
        output_schema = tool.get("outputSchema")
        output_chars = len(compact_json(output_schema)) if output_schema is not None else 0
        full_chars = len(compact_json(tool))
        rows.append(
            {
                "tool": tool.get("name", "?"),
                "description_chars": description_chars,
                "input_schema_chars": input_chars,
                "output_schema_chars": output_chars,
                "est_tokens": est_tokens(compact_json(tool)),
                "full_chars": full_chars,
            }
        )
    response_json = compact_json(result)
    summary = {
        "tools": len(rows),
        "bytes": len(response_json.encode("utf-8")),
        "chars": len(response_json),
        "est_tokens": est_tokens(response_json),
        "rows": rows,
    }
    return summary, rows


def full_text_query(label: str) -> str:
    return SEARCH_QUERIES.get(label, "Pierre")


def representative_calls(
    server: Server,
    book_label: str,
    document_id: str,
    content_hash: str,
    normalized_hash: str,
    sections: list[dict],
    start_index: int,
    open_result: dict,
    open_wall_ms: float,
    deadline: float,
) -> list[dict]:
    records = [
        representative_record(
            book_label,
            "open_document",
            open_result,
            open_wall_ms,
            document_id,
            content_hash,
            normalized_hash,
        )
    ]

    def add(tool_name: str, result: dict, wall_ms: float) -> None:
        records.append(
            representative_record(
                book_label,
                tool_name,
                result,
                wall_ms,
                document_id,
                content_hash,
                normalized_hash,
            )
        )

    section_id = sections[start_index]["section_id"] if sections else None

    try:
        result, wall_ms = timed_tool(
            server,
            "get_document_structure",
            {"document_id": document_id, "max_nodes": 50},
            deadline_timeout(deadline),
        )
        add("get_document_structure", result, wall_ms)
    except ProbeError:
        pass

    search_result = None
    try:
        search_result, wall_ms = timed_tool(
            server,
            "search_document",
            {"document_id": document_id, "query": full_text_query(book_label)},
            deadline_timeout(deadline),
        )
        add("search_document", search_result, wall_ms)
    except ProbeError:
        search_result = None

    if search_result is not None:
        hits = (tool_payload(search_result) or {}).get("hits")
        if isinstance(hits, list) and hits:
            first = hits[0] if isinstance(hits[0], dict) else {}
            locator = first.get("text_locator")
            if isinstance(locator, dict):
                unit = "sentence" if locator.get("sentence_index") is not None else "paragraph"
                try:
                    context_result, wall_ms = timed_tool(
                        server,
                        "get_context",
                        {
                            "document_id": document_id,
                            "target_locator": locator,
                            "relation": {
                                "type": "neighbor",
                                "unit": unit,
                                "before": 2,
                                "after": 2,
                            },
                        },
                        deadline_timeout(deadline),
                    )
                    add("get_context", context_result, wall_ms)
                except ProbeError:
                    pass

    if section_id is not None:
        try:
            read_result, wall_ms = timed_tool(
                server,
                "read_document",
                {"document_id": document_id, "section_id": section_id},
                deadline_timeout(deadline),
            )
            add("read_document", read_result, wall_ms)
        except ProbeError:
            pass

        try:
            units_result, wall_ms = timed_tool(
                server,
                "get_text_units",
                text_unit_arguments(document_id, section_id, 20),
                deadline_timeout(deadline),
            )
            add("get_text_units", units_result, wall_ms)
        except ProbeError:
            pass

    return records


def format_number(value, digits: int = 1) -> str:
    if value is None:
        return "-"
    if isinstance(value, float):
        return f"{value:.{digits}f}"
    return str(value)


def format_int(value) -> str:
    if value is None:
        return "-"
    return str(int(round(value)))


def cell(value) -> str:
    return "-" if value is None else str(value)


def probe_table(passes: list[dict]) -> str:
    header = (
        "| Book | Pass | Calls | Items | Total s | ms/call mean | ms/call p95 | "
        "Resp bytes/item | Text bytes/item | Text share % | Cursor chars | "
        "Locator chars/item | Est. tokens/item | Est. text tokens/item |"
    )
    separator = (
        "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | "
        "---: | ---: | ---: | ---: |"
    )
    lines = [header, separator]
    for item in passes:
        lines.append(
            "| {book} | {pass_name} | {calls} | {items} | {total_s} | {ms_mean} | {ms_p95} | "
            "{resp} | {text} | {share} | {cursor} | {locator} | {tokens} | {text_tokens} |".format(
                book=cell(item.get("book")),
                pass_name=cell(item.get("pass")),
                calls=format_int(item.get("calls")),
                items=format_int(item.get("items")),
                total_s=format_number(item.get("total_s"), 2),
                ms_mean=format_number(item.get("ms_mean"), 1),
                ms_p95=format_number(item.get("ms_p95"), 1),
                resp=format_number(item.get("resp_bytes_per_item"), 0),
                text=format_number(item.get("text_bytes_per_item"), 0),
                share=format_number(item.get("text_share_pct"), 1),
                cursor=format_number(item.get("cursor_chars"), 0),
                locator=format_number(item.get("locator_chars_per_item"), 0),
                tokens=format_number(item.get("est_tokens_per_item"), 1),
                text_tokens=format_number(item.get("est_text_tokens_per_item"), 1),
            )
        )
    return "\n".join(lines)


def tools_table(summary: dict) -> str:
    lines = [
        "| Tool | Description chars | Input schema chars | Output schema chars | Est. tokens |",
        "| --- | ---: | ---: | ---: | ---: |",
    ]
    for row in summary.get("rows", []):
        lines.append(
            "| {name} | {desc} | {input} | {output} | {tokens} |".format(
                name=row["tool"],
                desc=row["description_chars"],
                input=row["input_schema_chars"],
                output=row["output_schema_chars"],
                tokens=format_number(row["est_tokens"], 1),
            )
        )
    return "\n".join(lines)


def footprint_table(records: list[dict]) -> str:
    lines = [
        "| Book | Tool | Resp bytes | Est. tokens | text+structured both | Duplicate | "
        "document_id ×N | hashes ×N |",
        "| --- | --- | ---: | ---: | :--: | :--: | ---: | ---: |",
    ]
    for record in records:
        duplicate = "-"
        if record["both_present"]:
            if record["same_json"] is True:
                duplicate = "same"
            elif record["same_json"] is False:
                duplicate = "differs"
        lines.append(
            "| {book} | {tool} | {bytes} | {tokens} | {both} | {duplicate} | {doc} | {hashes} |".format(
                book=record["book"],
                tool=record["tool"],
                bytes=record["resp_bytes"],
                tokens=format_number(record["est_tokens"], 1),
                both="yes" if record["both_present"] else "no",
                duplicate=duplicate,
                doc=record["document_id_count"],
                hashes=record["hashes_count"],
            )
        )
    return "\n".join(lines)


def book_summary_line(book: dict) -> str:
    page = next(
        (item for item in book["passes"] if item["pass"] == "page"), None
    )
    partial = ""
    if page is not None and page.get("partial"):
        partial = ", page pass partial"
    return (
        f"- **{book['label']}**: sections={book['section_count']}, "
        f"structure_calls={book['structure_calls']}, "
        f"page_section_transitions={book['page_section_transitions']}"
        f"{partial}"
    )


def fresh_directory(path: str) -> str:
    if os.path.isdir(path):
        shutil.rmtree(path)
    os.makedirs(path, exist_ok=True)
    return path


def safe_label(label: str) -> str:
    cleaned = "".join(
        char if char.isalnum() or char in "-_." else "_" for char in label
    )
    return cleaned or "book"


def parse_documents(spec: str) -> list[tuple[str, str]]:
    documents = []
    for part in spec.split(","):
        part = part.strip()
        if not part:
            continue
        if "=" not in part:
            raise ValueError(f"document must be label=path, got {part!r}")
        label, path = part.split("=", 1)
        label = label.strip()
        path = path.strip()
        if not label or not path:
            raise ValueError(f"document must be label=path, got {part!r}")
        documents.append((label, path))
    return documents


def dump_output(path: str, data: dict) -> None:
    directory = os.path.dirname(os.path.abspath(path))
    if directory:
        os.makedirs(directory, exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(data, handle, ensure_ascii=False, indent=1)


def probe_book(
    binary: str,
    label: str,
    path: str,
    state_root: str,
    max_seconds: float,
) -> dict:
    book: dict = {
        "label": label,
        "path": path,
        "error": None,
        "section_count": 0,
        "structure_calls": 0,
        "page_section_transitions": 0,
        "document_id": None,
        "content_hash": None,
        "normalized_document_hash": None,
        "title": None,
        "passes": [],
        "calls": [],
        "representative_calls": [],
        "sample_text_units": None,
    }
    if not os.path.isfile(path):
        book["error"] = f"file not found: {path}"
        return book

    state_dir = fresh_directory(os.path.join(state_root, safe_label(label)))
    parent = os.path.dirname(os.path.abspath(path))
    server: Server | None = None
    try:
        server = Server(
            binary,
            server_environment(parent, state_dir),
            os.path.join(state_dir, "server.log"),
            request_timeout=max(30.0, max_seconds),
            cwd=parent,
        )
        server.initialize(timeout=max(30.0, max_seconds))
        open_result, open_wall_ms = timed_tool(
            server,
            "open_document",
            {"source": os.path.abspath(path)},
            max(30.0, max_seconds),
        )
        opened = tool_payload(open_result)
        if not isinstance(opened, dict):
            raise ProbeError("open_document did not return an object")
        document_id = opened.get("document_id")
        if not isinstance(document_id, str):
            raise ProbeError("open_document did not return a document_id")
        book["document_id"] = document_id
        book["content_hash"] = opened.get("content_hash")
        book["normalized_document_hash"] = opened.get("normalized_document_hash")
        book["title"] = opened.get("title")
        book["section_count"] = opened.get("section_count") or 0

        structure_deadline = time.monotonic() + max_seconds
        sections, structure_calls = read_structure(
            server, document_id, structure_deadline
        )
        book["structure_calls"] = structure_calls
        book["sections"] = sections
        book["section_count"] = len(sections) or book["section_count"]

        representative_deadline = time.monotonic() + max_seconds
        book["representative_calls"] = representative_calls(
            server,
            label,
            document_id,
            book["content_hash"] or "",
            book["normalized_document_hash"] or "",
            sections,
            0,
            open_result,
            open_wall_ms,
            representative_deadline,
        )

        page_deadline = time.monotonic() + max_seconds
        page_records, page_elapsed, page_partial, page_error, transitions = (
            run_page_pass(server, document_id, sections, page_deadline)
        )
        book["page_section_transitions"] = transitions
        book["calls"].extend(page_records)
        book["passes"].append(
            pass_summary(
                "page", page_records, page_elapsed, page_partial, page_error
            )
        )

        page_section_items: dict[str, int] = {}
        for record in page_records:
            page_section_items[record["section_id"]] = (
                page_section_items.get(record["section_id"], 0) + record["items"]
            )
        start_index = start_section_index(sections, page_section_items)

        one_deadline = time.monotonic() + max_seconds
        one_records, one_elapsed, one_partial, one_error, one_switches = (
            run_item_pass(
                server,
                document_id,
                sections,
                start_index,
                1,
                500,
                one_deadline,
                "one",
            )
        )
        book["calls"].extend(one_records)
        book["passes"].append(
            pass_summary(
                "one",
                one_records,
                one_elapsed,
                one_partial,
                one_error,
                one_switches,
            )
        )

        twenty_deadline = time.monotonic() + max_seconds
        twenty_records, twenty_elapsed, twenty_partial, twenty_error, twenty_switches = (
            run_item_pass(
                server,
                document_id,
                sections,
                start_index,
                20,
                300,
                twenty_deadline,
                "twenty",
            )
        )
        book["calls"].extend(twenty_records)
        book["passes"].append(
            pass_summary(
                "twenty",
                twenty_records,
                twenty_elapsed,
                twenty_partial,
                twenty_error,
                twenty_switches,
            )
        )

        if one_records:
            book["sample_text_units"] = one_records[0].get("sample_text")
        if book["sample_text_units"] is None:
            for record in page_records:
                if record.get("sample_text"):
                    book["sample_text_units"] = record["sample_text"]
                    break

    except ProbeError as exc:
        book["error"] = str(exc)
    except Exception as exc:  # noqa: BLE001 - one book must not hide the others
        book["error"] = f"{type(exc).__name__}: {exc}"
    finally:
        if server is not None:
            server.close()
        book.pop("sections", None)
    return book


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Measure sequential sentence reading cost over MCP stdio."
    )
    parser.add_argument("--binary", required=True, help="reading-mcp binary to run")
    parser.add_argument(
        "--documents",
        default="",
        help="comma-separated label=path documents (en-novel, zh-classic, zh-large)",
    )
    parser.add_argument("--out", required=True, help="raw per-call JSON output path")
    parser.add_argument(
        "--max-seconds-per-book",
        type=float,
        default=900.0,
        help="time budget for each pass of a book (default 900)",
    )
    parser.add_argument(
        "--state-dir",
        default=None,
        help="base state directory; a fresh run subdirectory is created inside it",
    )
    args = parser.parse_args()

    try:
        documents = parse_documents(args.documents)
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    binary = os.path.abspath(args.binary)
    if not os.path.isfile(binary):
        print(f"error: binary not found: {binary}", file=sys.stderr)
        return 2

    stamp = time.strftime("%Y%m%d-%H%M%S", time.gmtime())
    if args.state_dir:
        base = args.state_dir
    else:
        base = os.path.join(os.getcwd(), ".tmp", "sequential-read-probe")
    state_root = os.path.join(base, f"run-{os.getpid()}-{stamp}")
    os.makedirs(state_root, exist_ok=True)

    data: dict = {
        "binary": binary,
        "documents": documents,
        "max_seconds_per_book": args.max_seconds_per_book,
        "state_dir": state_root,
        "tools_list": None,
        "books": [],
        "markdown": {},
    }

    tools_state = fresh_directory(os.path.join(state_root, "tools-list"))
    tools_server: Server | None = None
    try:
        tools_server = Server(
            binary,
            server_environment(state_root, tools_state),
            os.path.join(tools_state, "server.log"),
            request_timeout=max(30.0, args.max_seconds_per_book),
            cwd=state_root,
        )
        tools_server.initialize(timeout=max(30.0, args.max_seconds_per_book))
        tools_result = tools_server.tools_list(
            timeout=max(30.0, args.max_seconds_per_book)
        )
        summary, _rows = tools_list_footprint(tools_result)
        response_json = compact_json(tools_result)
        summary["response_bytes"] = len(response_json.encode("utf-8"))
        summary["response_chars"] = len(response_json)
        summary["response_est_tokens"] = est_tokens(response_json)
        data["tools_list"] = summary
    except ProbeError as exc:
        data["tools_list"] = {"error": str(exc), "rows": []}
    except Exception as exc:  # noqa: BLE001 - still measure the books
        data["tools_list"] = {"error": f"{type(exc).__name__}: {exc}", "rows": []}
    finally:
        if tools_server is not None:
            tools_server.close()

    for label, path in documents:
        print(f"probing {label}: {path}", file=sys.stderr)
        book = probe_book(
            binary, label, path, state_root, args.max_seconds_per_book
        )
        data["books"].append(book)
        book["passes"] = [
            {"book": label, **item} for item in book.get("passes", [])
        ]
        dump_output(args.out, data)

    table = probe_table(
        [item for book in data["books"] for item in book.get("passes", [])]
    )
    book_lines = "\n".join(
        book_summary_line(book) for book in data["books"] if book.get("error") is None
    )
    error_lines = "\n".join(
        f"- **{book['label']}**: error: {book['error']}"
        for book in data["books"]
        if book.get("error")
    )
    tools_list_table = tools_table(data["tools_list"] or {"rows": []})
    footprint_records = [
        record
        for book in data["books"]
        for record in book.get("representative_calls", [])
    ]
    footprint = footprint_table(footprint_records)

    tools_summary_line = ""
    if data["tools_list"] and "error" in data["tools_list"]:
        tools_summary_line = f"tools/list error: {data['tools_list']['error']}"
    elif data["tools_list"]:
        tools_summary_line = (
            f"tools/list: {data['tools_list']['tools']} tools, "
            f"{data['tools_list']['response_bytes']} bytes, "
            f"~{data['tools_list']['response_est_tokens']:.0f} est. tokens "
            "(rough heuristic)"
        )

    print("## Sequential reading probe")
    print()
    print("Token estimates are a rough heuristic: ascii_chars / 4 + non_ascii_chars / 1.5.")
    print()
    print(table)
    print()
    if book_lines:
        print("### Books")
        print(book_lines)
        print()
    if error_lines:
        print("### Errors")
        print(error_lines)
        print()
    print("### tools/list footprint (re-sent to the model every turn)")
    print()
    if tools_summary_line:
        print(tools_summary_line)
        print()
    print(tools_list_table)
    print()
    print("### Representative tool calls (per book)")
    print()
    print(footprint)
    print()

    data["markdown"] = {
        "probe_table": table,
        "book_lines": book_lines,
        "errors": error_lines,
        "tools_summary": tools_summary_line,
        "tools_table": tools_list_table,
        "footprint_table": footprint,
    }
    dump_output(args.out, data)

    failed = any(book.get("error") for book in data["books"])
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
