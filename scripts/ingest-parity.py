#!/usr/bin/env python3
"""Prove two reading-mcp binaries ingest the same bytes into the same stored facts.

For every input and every binary the probe starts one server over stdio on a fresh `dir` state
directory, calls `open_document`, saves the tool payload, stops the server, and dumps the state
database into one text file per side:

- `documents`: `id`, `content_hash` and `document_json`;
- `section_layout_v1` joined to `text_unit_documents_v2` and ordered by document then
  `section_key`: `section_id`, `char_len`, `paragraph_coverage_json` and `hex(layout)`;
- `section_text_v1`: one sha256 per section over its chunks in order, plus `char_len`.

It then compares A against B per input, prints one markdown table and writes unified diffs (first
200 lines each) into the output directory for anything that differs. It exits 1 when any input
differs.

Standard library only. This script never builds or starts anything itself: pass existing release
binaries with `--binary-a` and `--binary-b`.
"""

from __future__ import annotations

import argparse
import difflib
import hashlib
import json
import os
import queue
import sqlite3
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
        "clientInfo": {"name": "ingest-parity", "version": "0"},
    },
}

INITIALIZED_NOTIFICATION = {
    "jsonrpc": "2.0",
    "method": "notifications/initialized",
}

OPEN_MESSAGE = {
    "jsonrpc": "2.0",
    "id": 2,
    "method": "tools/call",
    "params": {"name": "open_document", "arguments": {}},
}

STATE_DATABASE = "reading-mcp.sqlite"

DUMP_BLOCKS = ("documents", "layouts", "text")

# Wall-clock fields that must never take part in a parity comparison.
WALL_CLOCK_KEYS = {
    "duration_ms",
    "elapsed_ms",
    "wall_ms",
    "wall_time_ms",
    "opened_at",
    "started_at",
    "finished_at",
    "timestamp",
}


class ProbeError(RuntimeError):
    """A transport or protocol failure: the run itself is broken, not the ingest."""


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


def read_response(responses: queue.Queue, awaited_id: int, timeout: float) -> dict:
    deadline = time.monotonic() + timeout
    while True:
        wait = deadline - time.monotonic()
        if wait <= 0:
            raise TimeoutError(f"timed out waiting for response to id {awaited_id}")
        try:
            item = responses.get(timeout=wait)
        except queue.Empty:
            raise TimeoutError(f"timed out waiting for response to id {awaited_id}") from None
        if item is None:
            raise RuntimeError(f"server closed stdout before responding to id {awaited_id}")
        if isinstance(item, dict) and item.get("id") == awaited_id:
            return item


def error_text(response: dict) -> str | None:
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
    """The tool's JSON payload, from structuredContent or content[0].text."""
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


def drop_wall_clock(value):
    """Removes wall-clock fields, recursively, so only ingest facts are compared."""
    if isinstance(value, dict):
        return {
            key: drop_wall_clock(item)
            for key, item in value.items()
            if key not in WALL_CLOCK_KEYS
        }
    if isinstance(value, list):
        return [drop_wall_clock(item) for item in value]
    return value


def server_environment(state_dir: str, roots: str) -> dict:
    env = dict(os.environ)
    env["READING_MCP_STATE_DIR"] = state_dir
    env["READING_MCP_LOCAL_ROOTS"] = roots
    env["READING_MCP_TELEMETRY"] = "false"
    env["READING_MCP_MAX_DOCUMENT_BYTES"] = "134217728"
    env["READING_MCP_MAX_NORMALIZED_CHARS"] = "200000000"
    env["READING_MCP_PARSE_TIMEOUT_SECS"] = "900"
    return env


def run_open(binary: str, source: str, state_dir: str, timeout: float, log_path: str):
    """Opens `source` once on a fresh state directory and returns the payload or its error."""
    os.makedirs(state_dir, exist_ok=True)
    roots = os.path.dirname(os.path.abspath(source))
    env = server_environment(state_dir, roots)
    with open(log_path, "wb") as log_handle:
        process = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=log_handle,
            env=env,
        )
        responses: queue.Queue = queue.Queue()
        reader = threading.Thread(
            target=pump_stdout, args=(process.stdout, responses), daemon=True
        )
        reader.start()
        payload = None
        try:
            send(process, INITIALIZE_MESSAGE)
            initialized = read_response(responses, 1, timeout)
            failure = error_text(initialized)
            if failure is not None:
                payload = {"error": failure}
            else:
                send(process, INITIALIZED_NOTIFICATION)
                message = json.loads(json.dumps(OPEN_MESSAGE))
                message["params"]["arguments"] = {"source": os.path.abspath(source)}
                send(process, message)
                response = read_response(responses, 2, timeout)
                failure = error_text(response)
                if failure is not None:
                    payload = {"error": failure}
                else:
                    result = response.get("result")
                    payload = tool_payload(result if isinstance(result, dict) else {})
                    if payload is None:
                        payload = {"error": "open_document returned no JSON payload"}
        except (TimeoutError, RuntimeError, ProbeError) as error:
            payload = {"error": f"{type(error).__name__}: {error}"}
            try:
                process.kill()
            except Exception:  # noqa: BLE001 - already gone
                pass
        finally:
            try:
                if process.stdin is not None:
                    process.stdin.close()
            except Exception:  # noqa: BLE001 - server may already be gone
                pass
            try:
                process.wait(timeout=60)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=30)
            reader.join(timeout=1.0)
    return drop_wall_clock(payload)


def dump_state(database: str) -> dict:
    """The three comparable blocks of one state database, as text."""
    blocks = {name: [] for name in DUMP_BLOCKS}
    if not os.path.isfile(database):
        blocks["documents"] = ["<missing state database>"]
        return {name: "\n".join(lines) + "\n" for name, lines in blocks.items()}

    connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
    try:
        for document_id, content_hash, document_json in connection.execute(
            "SELECT id, content_hash, document_json FROM documents ORDER BY id"
        ):
            blocks["documents"].append(f"{document_id}\t{content_hash}\t{document_json}")

        for row in connection.execute(
            """
            SELECT d.document_id,
                   l.section_key,
                   l.section_id,
                   l.char_len,
                   l.paragraph_coverage_json,
                   hex(l.layout)
              FROM section_layout_v1 l
              JOIN text_unit_documents_v2 d ON d.doc_key = l.doc_key
             ORDER BY d.document_id, l.section_key
            """
        ):
            blocks["layouts"].append("\t".join("" if value is None else str(value) for value in row))

        sections: dict[tuple[str, int], list[str]] = {}
        for document_id, section_key, text in connection.execute(
            """
            SELECT d.document_id, t.section_key, t.text
              FROM section_text_v1 t
              JOIN text_unit_documents_v2 d ON d.doc_key = t.doc_key
             ORDER BY d.document_id, t.section_key, t.chunk_no
            """
        ):
            sections.setdefault((document_id, section_key), []).append(text)
        for (document_id, section_key), chunks in sorted(sections.items()):
            joined = "".join(chunks)
            digest = hashlib.sha256(joined.encode("utf-8")).hexdigest()
            blocks["text"].append(f"{document_id}\t{section_key}\t{digest}\t{len(joined)}")
    finally:
        connection.close()

    return {name: "\n".join(lines) + "\n" for name, lines in blocks.items()}


def payload_text(payload) -> str:
    return json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def safe_label(label: str) -> str:
    return "".join(
        ch if ch.isalnum() or ch in "-._" else "_" for ch in label
    )


def run_side(binary: str, side: str, label: str, path: str, out_dir: str, timeout: float) -> dict:
    """Opens `path` with `binary` on a fresh state directory and returns its comparable facts."""
    side_dir = os.path.join(out_dir, "runs", safe_label(label), side)
    os.makedirs(side_dir, exist_ok=True)
    state_dir = os.path.join(side_dir, "state")
    payload = run_open(
        binary,
        path,
        state_dir,
        timeout,
        os.path.join(side_dir, "server.log"),
    )
    dump = dump_state(os.path.join(state_dir, STATE_DATABASE))
    return {"payload": payload, "dump": dump}


def compare(label: str, left: dict, right: dict, out_dir: str) -> dict:
    """Compares one input's A and B facts, writes diffs for differences, and returns the columns."""
    columns = {"payload": "same", "documents": "same", "layouts": "same", "text": "same"}
    diffs = []

    left_payload = payload_text(left["payload"])
    right_payload = payload_text(right["payload"])
    if left_payload != right_payload:
        columns["payload"] = "DIFF"
        diffs.append(difflib_text(label, "open payload", left_payload, right_payload))

    for block in DUMP_BLOCKS:
        if left["dump"][block] != right["dump"][block]:
            columns[block] = "DIFF"
            diffs.append(difflib_text(label, block, left["dump"][block], right["dump"][block]))

    if diffs:
        with open(
            os.path.join(out_dir, f"{safe_label(label)}.diff.txt"), "w", encoding="utf-8"
        ) as handle:
            handle.write("\n".join(diffs))
    return columns


def difflib_text(label: str, block: str, left: str, right: str, limit: int = 200) -> str:
    diff = list(
        difflib.unified_diff(
            left.splitlines(keepends=True),
            right.splitlines(keepends=True),
            fromfile=f"A/{label}/{block}",
            tofile=f"B/{label}/{block}",
            n=3,
        )
    )
    return "".join(diff[:limit])


def parse_inputs(spec: str) -> list[tuple[str, str]]:
    pairs = []
    for item in spec.split(","):
        item = item.strip()
        if not item:
            continue
        label, _, path = item.partition("=")
        if not path:
            raise SystemExit(f"--inputs entry {item!r} is not label=path")
        pairs.append((label.strip(), path.strip()))
    return pairs


def main() -> int:
    parser = argparse.ArgumentParser(description="Base-vs-head ingest parity probe.")
    parser.add_argument("--binary-a", required=True)
    parser.add_argument("--binary-b", required=True)
    parser.add_argument("--inputs", required=True, help="label=path pairs, comma separated")
    parser.add_argument("--out", required=True)
    parser.add_argument("--timeout-seconds", type=float, default=900.0)
    args = parser.parse_args()

    os.makedirs(args.out, exist_ok=True)
    inputs = parse_inputs(args.inputs)
    if not inputs:
        raise SystemExit("--inputs is empty")

    rows = []
    failed = False
    for label, path in inputs:
        if not os.path.isfile(path):
            rows.append((label, "missing", "missing", "missing", "missing", "DIFF"))
            failed = True
            continue
        left = run_side(args.binary_a, "a", label, path, args.out, args.timeout_seconds)
        right = run_side(args.binary_b, "b", label, path, args.out, args.timeout_seconds)
        columns = compare(label, left, right, args.out)
        result = "same" if all(value == "same" for value in columns.values()) else "DIFF"
        failed = failed or result == "DIFF"
        rows.append(
            (
                label,
                columns["payload"],
                columns["documents"],
                columns["layouts"],
                columns["text"],
                result,
            )
        )
        print(f"{label}: {result}", flush=True)

    print()
    print("| Input | open payload | documents | layouts | text | result |")
    print("| --- | --- | --- | --- | --- | --- |")
    for row in rows:
        print("| " + " | ".join(row) + " |")

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
