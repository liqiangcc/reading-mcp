#!/usr/bin/env python3
"""Prove two reading-mcp binaries ingest the same bytes into the same stored facts.

For every input and every binary the probe starts one server over stdio on a fresh `dir` state
directory, calls `open_document`, saves the tool payload, stops the server, and dumps the state
database into one text file per side:

- `documents`: `id`, `content_hash` and `document_json`, compared as parsed objects after the
  section text of a row that keeps it in `section_text_v1`, `section_text_v2` or
  `section_text_v3` (its `content_store` key) is put back in depth-first section order from that
  document's chunks (decompressing `section_text_v3` chunks), and after the metadata entries of a
  row that keeps its heavy ones in `document_meta_v1` or `document_meta_v2` (its `metadata_store`
  key) are merged back into its `metadata` object;
- layouts joined to `text_unit_documents_v2` and ordered by document then `section_key`:
  `section_id`, `char_len`, `paragraph_coverage_json` and the whole-section layout hex. The v3
  `section_layout_v1` blob is compared directly; the v4 chunk rows are decoded, rebased and
  re-encoded as that blob;
- section text: one sha256 per section over its chunks in order, plus `char_len`;
- sections: the section tree rebuilt from `document_sections_v1` plus `document_header_v1` (or, on
  a side without those rows, from `document_json`), compared field by field in depth-first order,
  and the body order rebuilt from `document_body_order_rows_v1` (or `document_body_order_v1`);
- reads: the v1 text-unit/context/read/search sequence and the v2 short-handle sequence, on one
  server per side, every payload compared byte for byte after wall-clock keys are dropped.

It then compares A against B per input, prints one markdown table and writes unified diffs (first
200 lines each) into the output directory for anything that differs. It exits 1 when any input
differs.

Standard library only. This script never builds or starts anything itself: pass existing release
binaries with `--binary-a` and `--binary-b`.
"""

from __future__ import annotations

import argparse
import difflib
from collections import defaultdict
import hashlib
import itertools
import json
import os
import queue
import sqlite3
import subprocess
import sys
import threading
import time
import zlib

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

DUMP_BLOCKS = ("documents", "sections", "layouts", "text")

# The `documents` block is not a byte comparison: it compares the parsed objects after the rebuild,
# and the `sections` block rebuilds the tree from the head's `document_sections_v1` rows (or, on a
# side without them, from `document_json`), so the columns say so.
BLOCK_COLUMNS = {
    "documents": "documents (rebuilt)",
    "sections": "sections (rebuilt)",
    "layouts": "layouts",
    "text": "text",
}

# The `content_store` values of a row that keeps no section text inline: v3 used
# `section_text_v1`, v4 used `section_text_v2`, v5 uses `section_text_v3` (compressed chunks).
SECTION_TEXT_STORES = ("section_text_v1", "section_text_v2", "section_text_v3")

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
    env["READING_MCP_TOOLS"] = "v1"
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


def nested_sections(section_rows) -> list[dict]:
    """The section tree inside `document_sections_v1`, nested back into the `document_json` shape."""
    nodes: dict[str, dict] = {}
    roots: list[dict] = []
    for _section_key, section_id, parent_id, level, title, location_json in section_rows:
        node = {
            "id": section_id,
            "parent_id": parent_id,
            "title": title,
            "level": level,
            "content": "",
            "location": json.loads(location_json),
            "children": [],
        }
        nodes[section_id] = node
        if parent_id is None:
            roots.append(node)
        else:
            nodes[parent_id]["children"].append(node)
    return roots


def rebuild_document_json(
    document_json: str,
    texts: dict[int, str],
    metadata: dict[str, str] | None = None,
    section_rows=None,
) -> str:
    """One `documents` row as a parsed object, with what it keeps elsewhere put back.

    A row whose `content_store` is `section_text_v1` holds an empty `content` for every section: the
    text lives in `section_text_v1`. Filling it back in depth-first section order — the order
    `section_key` numbers the sections in — and dropping the key leaves the JSON a row that keeps
    its text inline would have written, so both sides compare as the same parsed object.

    A row whose `section_tree_store` is `document_sections_v1` holds no tree at all: `root_sections`
    is empty and the tree lives in the section rows. Nesting those rows back into `root_sections`
    and dropping the key leaves the JSON a row that keeps its tree inline would have written, so
    the `documents` block compares the rebuilt document while the stored JSON shrinks.

    A row whose `metadata_store` is `document_meta_v1` or `document_meta_v2` holds only its light
    metadata entries: the heavy ones live in that table, keyed by document. Merging them back into `metadata` and
    dropping the key leaves the JSON a row that keeps all of its metadata inline would have
    written. `sort_keys` then writes both sides in the one key order the store writes a
    `BTreeMap` in.
    """
    stored = json.loads(document_json)
    if not isinstance(stored, dict):
        return json.dumps(stored, ensure_ascii=False, sort_keys=True)
    if stored.pop("section_tree_store", None) == "document_sections_v1":
        stored["root_sections"] = nested_sections(section_rows or [])
    if stored.pop("content_store", None) in SECTION_TEXT_STORES:
        fill_section_text(stored.get("root_sections") or [], texts, itertools.count())
    if stored.pop("metadata_store", None) in ("document_meta_v1", "document_meta_v2"):
        merged = dict(stored.get("metadata") or {})
        merged.update(metadata or {})
        stored["metadata"] = merged
    return json.dumps(stored, ensure_ascii=False, sort_keys=True)


def decode_metadata_value(encoding: str, value) -> str:
    """One `document_meta_v2` value as the string the save stored.

    `raw` is the UTF-8 bytes themselves; `zlib` is the UTF-8 bytes compressed in zlib format
    (RFC 1950), which `zlib.decompress` reads.
    """
    if encoding == "raw":
        data = bytes(value)
    elif encoding == "zlib":
        data = zlib.decompress(bytes(value))
    else:
        raise ValueError(f"unknown document_meta_v2 encoding {encoding!r}")
    return data.decode("utf-8")


def fill_section_text(sections: list, texts: dict[int, str], keys) -> None:
    """Assigns the stored texts in depth-first order, one text per section."""
    for section in sections:
        section["content"] = texts.get(next(keys), "")
        fill_section_text(section.get("children") or [], texts, keys)


# ---------------------------------------------------------------------------------------------
# Section layout codec.
#
# `section_layout_v1` stored one whole-section blob; `section_layout_v2` stores the same encoding
# cut into chunks. To compare the two schemas, decode every chunk (local paragraph indices and
# per-chunk delta bases) into absolute records and re-encode them as one v1 blob. The encoder is the
# one in `src/infrastructure/section_layout.rs`: version byte, three counts, unsigned LEB128 varints,
# and ZigZag deltas from the previous value in the same list (starting at zero).
# ---------------------------------------------------------------------------------------------


class BlobReader:
    def __init__(self, data: bytes) -> None:
        self.data = data
        self.pos = 0

    def byte(self) -> int:
        value = self.data[self.pos]
        self.pos += 1
        return value

    def uvarint(self) -> int:
        value = 0
        shift = 0
        while True:
            byte = self.byte()
            value |= (byte & 0x7F) << shift
            if byte < 0x80:
                return value
            shift += 7

    def delta(self) -> int:
        encoded = self.uvarint()
        return (encoded >> 1) ^ -(encoded & 1)


def put_uvarint(out: bytearray, value: int) -> None:
    while value >= 0x80:
        out.append((value & 0x7F) | 0x80)
        value >>= 7
    out.append(value)


def put_delta(out: bytearray, value: int, previous: int) -> int:
    delta = value - previous
    put_uvarint(out, (delta << 1) ^ (delta >> 63))
    return value


def decode_layout(data: bytes) -> dict:
    reader = BlobReader(data)
    version = reader.byte()
    if version != 1:
        raise ValueError(f"unsupported section layout version {version}")
    paragraphs = []
    source = start = end = 0
    for _ in range(reader.uvarint()):
        paragraph_index = reader.uvarint()
        source = source + reader.delta()
        start = start + reader.delta()
        end = end + reader.delta()
        paragraphs.append((paragraph_index, source, start, end))
    sentences = []
    source = start = end = parent_start = parent_end = 0
    for _ in range(reader.uvarint()):
        paragraph_index = reader.uvarint()
        sentence_index = reader.uvarint()
        source = source + reader.delta()
        start = start + reader.delta()
        end = end + reader.delta()
        parent_start = parent_start + reader.delta()
        parent_end = parent_end + reader.delta()
        sentences.append(
            (
                paragraph_index,
                sentence_index,
                source,
                start,
                end,
                parent_start,
                parent_end,
            )
        )
    coverage = []
    paragraph_start = paragraph_end = 0
    for _ in range(reader.uvarint()):
        paragraph_index = reader.uvarint()
        paragraph_start = paragraph_start + reader.delta()
        paragraph_end = paragraph_end + reader.delta()
        coverage.append(
            (
                paragraph_index,
                paragraph_start,
                paragraph_end,
                reader.uvarint(),
                reader.uvarint(),
                reader.uvarint(),
                reader.uvarint(),
                reader.uvarint(),
                reader.uvarint(),
                reader.uvarint(),
            )
        )
    if reader.pos != len(data):
        raise ValueError(f"section layout has {len(data) - reader.pos} trailing bytes")
    return {"paragraphs": paragraphs, "sentences": sentences, "coverage": coverage}


def encode_layout(layout: dict) -> bytes:
    out = bytearray([1])
    paragraphs = layout["paragraphs"]
    put_uvarint(out, len(paragraphs))
    source = start = end = 0
    for paragraph_index, item_source, item_start, item_end in paragraphs:
        put_uvarint(out, paragraph_index)
        source = put_delta(out, item_source, source)
        start = put_delta(out, item_start, start)
        end = put_delta(out, item_end, end)
    sentences = layout["sentences"]
    put_uvarint(out, len(sentences))
    source = start = end = parent_start = parent_end = 0
    for (
        paragraph_index,
        sentence_index,
        item_source,
        item_start,
        item_end,
        item_parent_start,
        item_parent_end,
    ) in sentences:
        put_uvarint(out, paragraph_index)
        put_uvarint(out, sentence_index)
        source = put_delta(out, item_source, source)
        start = put_delta(out, item_start, start)
        end = put_delta(out, item_end, end)
        parent_start = put_delta(out, item_parent_start, parent_start)
        parent_end = put_delta(out, item_parent_end, parent_end)
    coverage = layout["coverage"]
    put_uvarint(out, len(coverage))
    paragraph_start = paragraph_end = 0
    for record in coverage:
        paragraph_index, item_start, item_end = record[0], record[1], record[2]
        put_uvarint(out, paragraph_index)
        paragraph_start = put_delta(out, item_start, paragraph_start)
        paragraph_end = put_delta(out, item_end, paragraph_end)
        for value in record[3:]:
            put_uvarint(out, value)
    return bytes(out)


def rebase_chunk_layout(layout: dict, base: int) -> dict:
    return {
        "paragraphs": [
            (paragraph_index + base, source, start, end)
            for paragraph_index, source, start, end in layout["paragraphs"]
        ],
        "sentences": [
            (
                paragraph_index + base,
                sentence_index,
                source,
                start,
                end,
                parent_start,
                parent_end,
            )
            for (
                paragraph_index,
                sentence_index,
                source,
                start,
                end,
                parent_start,
                parent_end,
            ) in layout["sentences"]
        ],
        "coverage": [
            (record[0] + base, *record[1:]) for record in layout["coverage"]
        ],
    }


def merge_chunk_layouts(chunks: list[tuple[int, bool, bytes]]) -> bytes:
    """`chunks` are `(first_paragraph_index, paragraph_count == 0, blob)` in `chunk_no` order."""
    merged = {"paragraphs": [], "sentences": [], "coverage": []}
    seen_paragraphs = set()
    seen_coverage = set()
    for base, is_mid, blob in chunks:
        layout = rebase_chunk_layout(decode_layout(blob), base)
        for paragraph in layout["paragraphs"]:
            if is_mid and paragraph[0] in seen_paragraphs:
                continue
            seen_paragraphs.add(paragraph[0])
            merged["paragraphs"].append(paragraph)
        merged["sentences"].extend(layout["sentences"])
        for record in layout["coverage"]:
            if is_mid and record[0] in seen_coverage:
                continue
            seen_coverage.add(record[0])
            merged["coverage"].append(record)
    return encode_layout(merged)


def table_exists(connection: sqlite3.Connection, name: str) -> bool:
    return (
        connection.execute(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?", (name,)
        ).fetchone()
        is not None
    )


def layout_rows(connection: sqlite3.Connection) -> list[str]:
    if table_exists(connection, "section_layout_chunk_v2"):
        chunks: dict[tuple[str, int], list[tuple[int, bool, bytes]]] = defaultdict(list)
        for (
            document_id,
            section_key,
            chunk_no,
            first_paragraph,
            paragraph_count,
            blob,
        ) in connection.execute(
            """
            SELECT d.document_id, c.section_key, c.chunk_no, c.first_paragraph_index,
                   c.paragraph_count, c.blob
              FROM section_layout_chunk_v2 c
              JOIN text_unit_documents_v2 d ON d.doc_key = c.doc_key
             ORDER BY d.document_id, c.section_key, c.chunk_no
            """
        ):
            chunks[(document_id, section_key)].append(
                (first_paragraph, paragraph_count == 0 and chunk_no > 0, bytes(blob))
            )
        rows = []
        for document_id, section_key, section_id, char_len, coverage_json in connection.execute(
            """
            SELECT d.document_id, l.section_key, l.section_id, l.char_len,
                   l.paragraph_coverage_json
              FROM section_layout_v2 l
              JOIN text_unit_documents_v2 d ON d.doc_key = l.doc_key
             ORDER BY d.document_id, l.section_key
            """
        ):
            layout_hex = merge_chunk_layouts(
                chunks.get((document_id, section_key), [])
            ).hex().upper()
            rows.append(
                "\t".join(
                    "" if value is None else str(value)
                    for value in (document_id, section_key, section_id, char_len, coverage_json)
                )
                + "\t"
                + layout_hex
            )
        return rows

    return [
        "\t".join("" if value is None else str(value) for value in row)
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
        )
    ]


def section_text_chunks(connection: sqlite3.Connection) -> list[tuple[str, int, str]]:
    """Every stored section text chunk as `(document_id, section_key, text)`, in chunk order.

    `section_text_v3` keeps each chunk as its UTF-8 bytes, compressed in zlib format when
    `encoding` is 1 (RFC 1950, which `zlib.decompress` reads) and raw when it is 0; the older
    `section_text_v2`/`section_text_v1` kept the text itself in a `text` column.
    """
    if table_exists(connection, "section_text_v3"):
        rows = connection.execute(
            """
            SELECT d.document_id, t.section_key, t.encoding, t.data
              FROM section_text_v3 t
              JOIN text_unit_documents_v2 d ON d.doc_key = t.doc_key
             ORDER BY d.document_id, t.section_key, t.chunk_no
            """
        )
        chunks = []
        for document_id, section_key, encoding, data in rows:
            blob = bytes(data)
            if encoding == 1:
                blob = zlib.decompress(blob)
            elif encoding != 0:
                raise ValueError(f"unknown section_text_v3 encoding {encoding!r}")
            chunks.append((document_id, section_key, blob.decode("utf-8")))
        return chunks
    table = (
        "section_text_v2"
        if table_exists(connection, "section_text_v2")
        else "section_text_v1"
    )
    return [
        (document_id, section_key, text)
        for document_id, section_key, text in connection.execute(
            f"""
            SELECT d.document_id, t.section_key, t.text
              FROM {table} t
              JOIN text_unit_documents_v2 d ON d.doc_key = t.doc_key
             ORDER BY d.document_id, t.section_key, t.chunk_no
            """
        )
    ]


def text_rows(connection: sqlite3.Connection) -> list[str]:
    sections: dict[tuple[str, int], list[str]] = {}
    for document_id, section_key, text in section_text_chunks(connection):
        sections.setdefault((document_id, section_key), []).append(text)
    rows = []
    for (document_id, section_key), chunks in sorted(sections.items()):
        joined = "".join(chunks)
        digest = hashlib.sha256(joined.encode("utf-8")).hexdigest()
        rows.append(f"{document_id}\t{section_key}\t{digest}\t{len(joined)}")
    return rows


def canonical_json(value) -> str:
    """One value as the single line every side of a comparison agrees on."""
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def json_section_rows(root_sections) -> list[tuple]:
    """The section tree inside `document_json`, in depth-first order."""
    rows = []

    def walk(sections) -> None:
        for section in sections:
            rows.append(
                (
                    section.get("id"),
                    section.get("parent_id"),
                    section.get("level"),
                    section.get("title"),
                    canonical_json(section.get("location")),
                )
            )
            walk(section.get("children") or [])

    walk(root_sections)
    return rows


def stored_section_rows(connection: sqlite3.Connection, document_id: str) -> list[tuple] | None:
    """The section tree rebuilt from `document_sections_v1`, or None when the side has no rows."""
    if not table_exists(connection, "document_sections_v1"):
        return None
    stored = list(
        connection.execute(
            "SELECT section_key, section_id, parent_key, level, title, location_json"
            " FROM document_sections_v1 WHERE document_id = ?1 ORDER BY section_key",
            (document_id,),
        )
    )
    if not stored:
        return None
    ids = {section_key: section_id for section_key, section_id, *_ in stored}
    return [
        (
            section_key,
            section_id,
            None if parent_key is None else ids.get(parent_key),
            level,
            title,
            canonical_json(json.loads(location_json)),
        )
        for section_key, section_id, parent_key, level, title, location_json in stored
    ]


def stored_header_row(connection: sqlite3.Connection, document_id: str) -> tuple | None:
    """The `document_header_v1` row of one document, or None when the side has no such row."""
    if not table_exists(connection, "document_header_v1"):
        return None
    return connection.execute(
        "SELECT source, title, media_type, content_hash, normalized_document_hash,"
        " section_count, root_count FROM document_header_v1 WHERE document_id = ?1",
        (document_id,),
    ).fetchone()


def stored_body_order_rows(
    connection: sqlite3.Connection, document_id: str
) -> list[tuple[int, int]] | None:
    """The `document_body_order_rows_v1` rows of one document, or None when the side has none."""
    if not table_exists(connection, "document_body_order_rows_v1"):
        return None
    rows = list(
        connection.execute(
            "SELECT ord, section_key FROM document_body_order_rows_v1"
            " WHERE document_id = ?1 ORDER BY ord",
            (document_id,),
        )
    )
    return rows or None


def stored_body_order_json(
    connection: sqlite3.Connection, document_id: str
) -> tuple[list[str] | None, str | None]:
    """The `document_body_order_v1` row of one document: its ids, or its error text."""
    if not table_exists(connection, "document_body_order_v1"):
        return (None, None)
    row = connection.execute(
        "SELECT order_json, error FROM document_body_order_v1 WHERE document_id = ?1",
        (document_id,),
    ).fetchone()
    if row is None:
        return (None, None)
    order_json, error = row
    if error is not None:
        return (None, str(error))
    if order_json is None:
        return (None, "<no order and no error>")
    return ([str(section_id) for section_id in json.loads(order_json)], None)


def stored_header_body_order_error(
    connection: sqlite3.Connection, document_id: str
) -> str | None:
    """The `document_header_v1.body_order_error` of one document, or None.

    The column exists only on a side that keeps the failed body order in the header (SEC-3). A side
    without it reads the error from `document_body_order_v1` instead.
    """
    if not table_exists(connection, "document_header_v1"):
        return None
    columns = {
        row[1] for row in connection.execute("PRAGMA table_info(document_header_v1)")
    }
    if "body_order_error" not in columns:
        return None
    row = connection.execute(
        "SELECT body_order_error FROM document_header_v1 WHERE document_id = ?1",
        (document_id,),
    ).fetchone()
    if row is None or row[0] is None:
        return None
    return str(row[0])


def section_block_lines(
    connection: sqlite3.Connection,
    document_id: str,
    content_hash: str,
    document_json: str,
    stored_hash: str | None,
) -> list[str]:
    """One document's section tree and body order, from rows when the side has them.

    The head stores the tree in `document_sections_v1` and the body order in
    `document_body_order_rows_v1`; a side without those tables (or without rows for this document)
    rebuilds both from `document_json` and `document_body_order_v1`, so the block compares the two
    storages byte for byte.
    """
    stored = json.loads(document_json)
    rows = stored_section_rows(connection, document_id)
    header = stored_header_row(connection, document_id)
    from_rows = rows is not None
    if rows is None:
        # A side without section rows reports the depth-first index as the key, which is the key the
        # rows of the other side must carry.
        rows = [
            (index, *row)
            for index, row in enumerate(json_section_rows(stored.get("root_sections") or []))
        ]

    lines = [f"document\t{document_id}"]
    if header is not None:
        source, title, media_type, header_hash, header_normalized, section_count, root_count = header
        lines.append(
            "header\t"
            + "\t".join(
                [
                    source,
                    canonical_json(title),
                    media_type,
                    header_hash,
                    header_normalized if header_normalized is not None else "-",
                    str(section_count),
                    str(root_count),
                ]
            )
        )
    else:
        roots = stored.get("root_sections") or []
        lines.append(
            "header\t"
            + "\t".join(
                [
                    str(stored.get("source")),
                    canonical_json(stored.get("title")),
                    str(stored.get("media_type")),
                    content_hash,
                    stored_hash if stored_hash is not None else "-",
                    str(len(rows)),
                    str(len(roots)),
                ]
            )
        )

    for section_key, section_id, parent_id, level, title, location in rows:
        lines.append(
            "section\t"
            + "\t".join(
                [
                    str(section_key),
                    str(section_id),
                    "-" if parent_id is None else str(parent_id),
                    str(level),
                    canonical_json(title),
                    location,
                ]
            )
        )

    order_rows = stored_body_order_rows(connection, document_id)
    if order_rows is not None and from_rows:
        keys = {section_key: section_id for section_key, section_id, *_rest in rows}
        for ord_value, section_key in order_rows:
            lines.append(f"body_order\t{ord_value}\t{keys.get(section_key, '<unknown key>')}")
    else:
        section_ids, error = stored_body_order_json(connection, document_id)
        if error is None:
            error = stored_header_body_order_error(connection, document_id)
        if error is not None:
            lines.append(f"body_order_error\t{canonical_json(error)}")
        elif section_ids is not None:
            for ord_value, section_id in enumerate(section_ids):
                lines.append(f"body_order\t{ord_value}\t{section_id}")
    return lines


def dump_state(database: str) -> dict:
    """The comparable blocks of one state database, as text."""
    blocks = {name: [] for name in DUMP_BLOCKS}
    if not os.path.isfile(database):
        blocks["documents"] = ["<missing state database>"]
        return {name: "\n".join(lines) + "\n" for name, lines in blocks.items()}

    connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
    try:
        # A row saved before the normalized hash column existed reports none, which the `sections`
        # header of that side writes as `-`.
        document_columns = {
            row[1] for row in connection.execute("PRAGMA table_info(documents)")
        }
        has_normalized_hash = "normalized_document_hash" in document_columns
        documents = list(
            connection.execute(
                "SELECT id, content_hash, document_json"
                + (", normalized_document_hash" if has_normalized_hash else ", NULL")
                + " FROM documents ORDER BY id"
            )
        )
        blocks["layouts"] = layout_rows(connection)
        blocks["text"] = text_rows(connection)
        texts_by_document: dict[str, dict[int, str]] = defaultdict(dict)
        for document_id, section_key, text in section_text_chunks(connection):
            texts_by_document[document_id].setdefault(section_key, "")
            texts_by_document[document_id][section_key] += text
        metadata_by_document: dict[str, dict[str, str]] = defaultdict(dict)
        if table_exists(connection, "document_meta_v1"):
            for document_id, key, value in connection.execute(
                "SELECT document_id, key, value FROM document_meta_v1 ORDER BY document_id, key"
            ):
                metadata_by_document[document_id][key] = value
        if table_exists(connection, "document_meta_v2"):
            for document_id, key, encoding, value in connection.execute(
                "SELECT document_id, key, encoding, value FROM document_meta_v2"
                " ORDER BY document_id, key"
            ):
                metadata_by_document[document_id][key] = decode_metadata_value(
                    encoding, value
                )
        for document_id, content_hash, document_json, normalized_hash in documents:
            rebuilt = rebuild_document_json(
                document_json,
                texts_by_document.get(document_id, {}),
                metadata_by_document.get(document_id, {}),
                stored_section_rows(connection, document_id),
            )
            blocks["documents"].append(f"{document_id}\t{content_hash}\t{rebuilt}")
            blocks["sections"].extend(
                section_block_lines(
                    connection,
                    document_id,
                    content_hash,
                    document_json,
                    normalized_hash,
                )
            )
    finally:
        connection.close()

    return {name: "\n".join(lines) + "\n" for name, lines in blocks.items()}


def payload_text(payload) -> str:
    return json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def safe_label(label: str) -> str:
    return "".join(
        ch if ch.isalnum() or ch in "-._" else "_" for ch in label
    )


CURSOR_CHECKSUM_DOMAIN = b"reading-mcp/text-unit-cursor-checksum/v1\0"


def pick_read_section(structure) -> str | None:
    """The top-level section covering the most body order, ties broken by document order."""
    if not isinstance(structure, dict):
        return None
    sections = structure.get("sections")
    if not isinstance(sections, list) or not sections:
        return None
    stream = structure.get("stream")
    total_nodes = stream.get("total_nodes") if isinstance(stream, dict) else None
    best = None
    for index, node in enumerate(sections):
        if not isinstance(node, dict):
            continue
        start = node.get("body_order")
        end = total_nodes
        if index + 1 < len(sections) and isinstance(sections[index + 1], dict):
            end = sections[index + 1].get("body_order")
        if not isinstance(start, int) or not isinstance(end, int):
            continue
        if best is None or end - start > best[0]:
            best = (end - start, start, node.get("section_id"))
    if best is None:
        first = sections[0]
        return first.get("section_id") if isinstance(first, dict) else None
    return best[2]


def text_unit_cursor(
    opened: dict,
    structure: dict,
    section_id: str,
    kind: str,
    direction: str,
    policy: str,
    next_index: int,
    total_items: int,
) -> str:
    """Rebuilds a resume cursor at `next_index` with the encoding `text_unit_cursor.rs` uses.

    The claims are serialized in declaration order and checksummed, so the server accepts this
    exactly like a cursor it issued itself; that is what makes a middle-of-stream read one call.
    """
    claims = {
        "schema_version": "text-unit-cursor/v1",
        "document_id": opened["document_id"],
        "content_hash": opened["content_hash"],
        "normalized_document_hash": opened["normalized_document_hash"],
        "section_id": section_id,
        "segmentation_version": structure["segmentation_version"],
        "requested_kind": kind,
        "direction": direction,
        "coverage_policy": policy,
        "next_index": next_index,
        "total_items": total_items,
    }
    claims_bytes = json.dumps(claims, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    checksum = "sha256:" + hashlib.sha256(
        CURSOR_CHECKSUM_DOMAIN + claims_bytes
    ).hexdigest()
    envelope = json.dumps(
        {"claims": claims, "checksum": checksum},
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")
    return "tuc1." + envelope.hex()


def run_v1_reads(call, source: str, records: list[dict]) -> None:
    """Records the design note's §4 read sequence: text units, context, reads, search.

    Runs on the same server as the v2 sequence, after `open_document`, on the largest top-level
    section so the page boundaries are non-trivial.
    """
    opened = call("open_document", {"source": os.path.abspath(source)})
    records.append({"step": "v1 open_document", "payload": opened})
    document_id = opened.get("document_id") if isinstance(opened, dict) else None
    if not isinstance(document_id, str):
        return

    structure = call("get_document_structure", {"document_id": document_id, "max_depth": 1})
    records.append({"step": "v1 get_document_structure", "payload": structure})
    section = pick_read_section(structure)
    if not isinstance(section, str):
        return

    # A one-node first page forces a structure cursor; its continuation must resume identically.
    first_page = call(
        "get_document_structure",
        {"document_id": document_id, "max_depth": 1, "max_nodes": 1},
    )
    records.append({"step": "v1 get_document_structure first page", "payload": first_page})
    structure_cursor = first_page.get("next_cursor") if isinstance(first_page, dict) else None
    if isinstance(structure_cursor, str):
        records.append(
            {
                "step": "v1 get_document_structure continue",
                "payload": call(
                    "get_document_structure",
                    {"document_id": document_id, "cursor": structure_cursor, "max_nodes": 1},
                ),
            }
        )

    # A named-section request, when the input has a titled section. A page-only PDF answers
    # `Unavailable`; that is a result too, and both binaries must agree on it.
    named_title = None
    structure_sections = structure.get("sections") if isinstance(structure, dict) else None
    if isinstance(structure_sections, list):
        for node in structure_sections:
            if (
                isinstance(node, dict)
                and isinstance(node.get("title"), str)
                and node["title"].strip()
            ):
                named_title = node["title"].strip()
                break
    if named_title is not None:
        records.append(
            {
                "step": "v1 get_document_structure named section",
                "payload": call(
                    "get_document_structure",
                    {
                        "document_id": document_id,
                        "named_section_query": named_title,
                        "expected_content_hash": opened.get("content_hash"),
                        "expected_normalized_document_hash": opened.get(
                            "normalized_document_hash"
                        ),
                        "max_depth": 1,
                    },
                ),
            }
        )

    first_locators: dict[str, dict] = {}
    first_texts: dict[str, str] = {}
    section_locator: dict | None = None
    for kind in ("paragraph", "sentence"):
        for policy in ("preserve_source", "eligible_only"):
            label = f"{kind}/{policy}"
            base = {
                "document_id": document_id,
                "section_id": section,
                "requested_kind": kind,
                "coverage_policy": policy,
            }

            def text_units(**extra):
                return call("get_text_units", {**base, **extra})

            start = text_units(direction="forward", max_items=1)
            records.append({"step": f"v1 {label} forward start", "payload": start})
            if isinstance(start, dict) and isinstance(
                start.get("target_section_locator"), dict
            ):
                section_locator = start["target_section_locator"]
            items = start.get("items") if isinstance(start, dict) else None
            if isinstance(items, list) and items and isinstance(items[0], dict):
                locator = items[0].get("locator")
                text = items[0].get("text")
                if isinstance(locator, dict):
                    first_locators.setdefault(kind, locator)
                if isinstance(text, str):
                    first_texts.setdefault(kind, text)
            stream = start.get("stream") if isinstance(start, dict) else None
            total = stream.get("total_items") if isinstance(stream, dict) else None
            if not isinstance(total, int) or total <= 0:
                continue

            batch = text_units(direction="forward", max_items=256)
            records.append({"step": f"v1 {label} forward 256", "payload": batch})
            follow = batch.get("next_cursor") if isinstance(batch, dict) else None
            if isinstance(follow, str):
                records.append(
                    {
                        "step": f"v1 {label} forward continue",
                        "payload": text_units(
                            direction="forward", max_items=256, cursor=follow
                        ),
                    }
                )

            anchor = None
            middle = total // 2
            if 0 < middle < total:
                middle_page = text_units(
                    direction="forward",
                    max_items=256,
                    cursor=text_unit_cursor(
                        opened, structure, section, kind, "forward", policy, middle, total
                    ),
                )
                records.append({"step": f"v1 {label} forward middle", "payload": middle_page})
                middle_items = middle_page.get("items")
                if (
                    isinstance(middle_items, list)
                    and middle_items
                    and isinstance(middle_items[0], dict)
                ):
                    anchor = middle_items[0].get("locator")
            end_page = text_units(
                direction="forward",
                max_items=1,
                cursor=text_unit_cursor(
                    opened, structure, section, kind, "forward", policy, total - 1, total
                ),
            )
            records.append({"step": f"v1 {label} forward end", "payload": end_page})
            records.append(
                {
                    "step": f"v1 {label} backward end 1",
                    "payload": text_units(direction="backward", max_items=1),
                }
            )
            records.append(
                {
                    "step": f"v1 {label} backward end 256",
                    "payload": text_units(direction="backward", max_items=256),
                }
            )

            if not isinstance(anchor, dict):
                anchor = first_locators.get(kind)
            if isinstance(anchor, dict):
                anchored = text_units(
                    direction="forward", max_items=1, anchor_locator=anchor
                )
                records.append({"step": f"v1 {label} anchored", "payload": anchored})
                follow = anchored.get("next_cursor") if isinstance(anchored, dict) else None
                if isinstance(follow, str):
                    records.append(
                        {
                            "step": f"v1 {label} anchored continue",
                            "payload": text_units(
                                direction="forward", max_items=256, cursor=follow
                            ),
                        }
                    )

    paragraph_locator = first_locators.get("paragraph")
    sentence_locator = first_locators.get("sentence")
    if isinstance(paragraph_locator, dict):
        records.append(
            {
                "step": "v1 get_context paragraph",
                "payload": call(
                    "get_context",
                    {
                        "document_id": document_id,
                        "target_locator": paragraph_locator,
                        "relation": {
                            "type": "neighbor",
                            "unit": "paragraph",
                            "before": 1,
                            "after": 1,
                        },
                    },
                ),
            }
        )
    if isinstance(sentence_locator, dict):
        records.append(
            {
                "step": "v1 get_context sentence",
                "payload": call(
                    "get_context",
                    {
                        "document_id": document_id,
                        "target_locator": sentence_locator,
                        "relation": {
                            "type": "neighbor",
                            "unit": "sentence",
                            "before": 2,
                            "after": 2,
                        },
                    },
                ),
            }
        )
        records.append(
            {
                "step": "v1 get_context container",
                "payload": call(
                    "get_context",
                    {
                        "document_id": document_id,
                        "target_locator": sentence_locator,
                        "relation": {"type": "container", "kind": "paragraph"},
                    },
                ),
            }
        )
    records.append(
        {
            "step": "v1 get_context section container",
            "payload": call(
                "get_context",
                {
                    "document_id": document_id,
                    "section_id": section,
                    "relation": {"type": "container", "kind": "section"},
                },
            ),
        }
    )

    records.append(
        {
            "step": "v1 read_document section",
            "payload": call(
                "read_document",
                {"document_id": document_id, "section_id": section, "max_chars": 512},
            ),
        }
    )
    if isinstance(paragraph_locator, dict):
        records.append(
            {
                "step": "v1 read_document locator",
                "payload": call(
                    "read_document",
                    {
                        "document_id": document_id,
                        "target_locator": paragraph_locator,
                        "max_chars": 512,
                    },
                ),
            }
        )

    query = first_texts.get("sentence", "").strip()[:24] or "reading"
    records.append(
        {
            "step": "v1 search_document",
            "payload": call(
                "search_document",
                {"document_id": document_id, "query": query, "limit": 5},
            ),
        }
    )

    # `source_view` only renders a PDF. A precise Paragraph locator maps to a page; a Section
    # locator is the fallback when the input has no paragraph units. A refusal is also a result:
    # both binaries must refuse identically.
    media_type = opened.get("media_type") if isinstance(opened, dict) else None
    if (
        isinstance(media_type, str)
        and media_type.lower() == "application/pdf"
        and isinstance(paragraph_locator or section_locator, dict)
    ):
        records.append(
            {
                "step": "v1 get_source_view",
                "payload": call(
                    "get_source_view",
                    {
                        "document_id": document_id,
                        "target_locator": paragraph_locator or section_locator,
                    },
                ),
            }
        )

    run_light_reader_shapes(
        call,
        document_id,
        structure,
        section,
        section_locator,
        paragraph_locator,
        records,
    )


def run_light_reader_shapes(
    call,
    document_id: str,
    structure: dict,
    section: str,
    section_locator: dict | None,
    paragraph_locator: dict | None,
    records: list[dict],
) -> None:
    """The shapes R1c-3c adds risk for: every text use the light readers serve per section.

    A section neighbour renders `before + after + 1` sections, a section container one section, and
    a structural relation none; a root section's tree read renders its whole subtree; a section-kind
    anchor asks for that section's length and then refuses the anchor. Every payload is kept so two
    binaries are compared byte for byte on all of them.
    """
    records.append(
        {
            "step": "v1 get_context section neighbour 2/2",
            "payload": call(
                "get_context",
                {
                    "document_id": document_id,
                    "section_id": section,
                    "relation": {
                        "type": "neighbor",
                        "unit": "section",
                        "before": 2,
                        "after": 2,
                    },
                },
            ),
        }
    )
    records.append(
        {
            "step": "v1 get_context structured section container",
            "payload": call(
                "get_context",
                {
                    "document_id": document_id,
                    "section_id": section,
                    "relation": {"type": "container", "kind": "section"},
                },
            ),
        }
    )
    for kind in ("children", "ancestors"):
        records.append(
            {
                "step": f"v1 get_context structural {kind}",
                "payload": call(
                    "get_context",
                    {
                        "document_id": document_id,
                        "section_id": section,
                        "relation": {"type": "structural", "kind": kind},
                    },
                ),
            }
        )

    if isinstance(section_locator, dict):
        # A section-kind anchor is not part of any text-unit stream; the answer is an error, and it
        # must be the same error on both binaries.
        records.append(
            {
                "step": "v1 get_text_units section anchor",
                "payload": call(
                    "get_text_units",
                    {
                        "document_id": document_id,
                        "section_id": section,
                        "requested_kind": "sentence",
                        "anchor_locator": section_locator,
                        "max_items": 1,
                    },
                ),
            }
        )

    # The whole subtree of a top-level section, with a budget small enough to need a cursor.
    sections = structure.get("sections") if isinstance(structure, dict) else None
    root = None
    if isinstance(sections, list):
        for node in sections:
            if (
                isinstance(node, dict)
                and node.get("parent_id") is None
                and isinstance(node.get("section_id"), str)
            ):
                root = node["section_id"]
                break
    if isinstance(root, str):
        tree = call(
            "read_document",
            {"document_id": document_id, "section_id": root, "max_chars": 200},
        )
        records.append({"step": "v1 read_document root 200", "payload": tree})
        cursor = tree.get("next_cursor") if isinstance(tree, dict) else None
        if isinstance(cursor, str):
            records.append(
                {
                    "step": "v1 read_document root continue",
                    "payload": call(
                        "read_document",
                        {
                            "document_id": document_id,
                            "section_id": root,
                            "cursor": cursor,
                            "max_chars": 200,
                        },
                    ),
                }
            )

    if isinstance(paragraph_locator, dict):
        exact = call(
            "read_document",
            {
                "document_id": document_id,
                "target_locator": paragraph_locator,
                "max_chars": 120,
            },
        )
        records.append({"step": "v1 read_document exact 120", "payload": exact})
        cursor = exact.get("next_cursor") if isinstance(exact, dict) else None
        if isinstance(cursor, str):
            records.append(
                {
                    "step": "v1 read_document exact continue",
                    "payload": call(
                        "read_document",
                        {
                            "document_id": document_id,
                            "target_locator": paragraph_locator,
                            "cursor": cursor,
                            "max_chars": 120,
                        },
                    ),
                }
            )

    # A query built from the section title, so the scan can produce a section-kind hit. If the
    # input's titles do not allow one, the payload says so and both binaries must say it alike.
    title = None
    if isinstance(sections, list):
        for node in sections:
            if isinstance(node, dict) and node.get("section_id") == section:
                title = node.get("title")
                break
    query = title.strip()[:24] if isinstance(title, str) and title.strip() else None
    if query is not None:
        records.append(
            {
                "step": "v1 search_document section title",
                "payload": call(
                    "search_document",
                    {"document_id": document_id, "query": query, "limit": 5},
                ),
            }
        )


def run_read_probe(
    binary: str, source: str, state_dir: str, timeout: float, log_path: str
) -> list[dict]:
    """Reopens the stored state and records the v1 and v2 read sequences, both tools registered.

    The v1 sequence (`run_v1_reads`) covers text units, context, reads and search; then `open`
    returns the short document handle, five `read` calls follow `next`, the next call uses `back=2`
    and the last one asks for a 2000-char batch from the section start. Every response is kept so
    two binaries can be compared call by call.
    """
    env = server_environment(state_dir, os.path.dirname(os.path.abspath(source)))
    env["READING_MCP_TOOLS"] = "both"
    records: list[dict] = []
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
        request_id = 0

        def call(name: str, arguments: dict) -> dict:
            nonlocal request_id
            request_id += 1
            send(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": "tools/call",
                    "params": {"name": name, "arguments": arguments},
                },
            )
            response = read_response(responses, request_id, timeout)
            failure = error_text(response)
            if failure is not None:
                return {"error": failure}
            result = response.get("result")
            payload = tool_payload(result if isinstance(result, dict) else {})
            return payload if payload is not None else {"error": "no JSON payload"}

        try:
            send(process, INITIALIZE_MESSAGE)
            initialized = read_response(responses, 1, timeout)
            request_id = 1
            failure = error_text(initialized)
            if failure is not None:
                return [{"step": "initialize", "payload": {"error": failure}}]
            send(process, INITIALIZED_NOTIFICATION)

            run_v1_reads(call, source, records)

            opened = call("open", {"source": os.path.abspath(source)})
            records.append({"step": "v2 open", "payload": opened})
            document = opened.get("doc") if isinstance(opened, dict) else None
            top = opened.get("top") if isinstance(opened, dict) else None
            section = None
            if isinstance(top, list) and top:
                first = top[0]
                if isinstance(first, (list, tuple)) and first:
                    section = first[0]
                elif isinstance(first, str):
                    section = first
            if not isinstance(document, str) or not isinstance(section, str):
                return records

            payload = call("read", {"from": section})
            records.append({"step": "read from section", "payload": payload})
            cursor = payload.get("next") if isinstance(payload, dict) else None
            for index in range(5):
                if not isinstance(cursor, str):
                    break
                payload = call("read", {"from": cursor})
                records.append({"step": f"read next {index + 1}", "payload": payload})
                cursor = payload.get("next") if isinstance(payload, dict) else None

            back_from = cursor if isinstance(cursor, str) else section
            records.append(
                {
                    "step": "read back 2",
                    "payload": call("read", {"from": back_from, "back": 2}),
                }
            )
            records.append(
                {
                    "step": "read batch 2000",
                    "payload": call(
                        "read", {"from": section, "max_chars": 2000}
                    ),
                }
            )

            # A backward read returns a `b:` cursor in `next`; the cursor itself must resume
            # identically, so it is one more recorded shape.
            backward = call(
                "read", {"from": section, "direction": "backward", "max_chars": 64}
            )
            records.append({"step": "read backward 64", "payload": backward})
            backward_cursor = backward.get("next") if isinstance(backward, dict) else None
            if isinstance(backward_cursor, str):
                records.append(
                    {
                        "step": "read backward cursor",
                        "payload": call("read", {"from": backward_cursor, "max_chars": 64}),
                    }
                )

            # The preorder listing at two depths, and scoped to one section, so the point-query
            # preorder and its `next` cursor are compared with the tree walk it replaced.
            for depth in (1, 2):
                records.append(
                    {
                        "step": f"outline depth {depth}",
                        "payload": call("outline", {"at": document, "depth": depth}),
                    }
                )
            records.append(
                {
                    "step": "outline section depth 1",
                    "payload": call("outline", {"at": section, "depth": 1}),
                }
            )
            paged = call("outline", {"at": document, "depth": 2, "limit": 3})
            records.append({"step": "outline paged 3", "payload": paged})
            outline_cursor = paged.get("next") if isinstance(paged, dict) else None
            if isinstance(outline_cursor, str):
                records.append(
                    {
                        "step": "outline paged continue",
                        "payload": call(
                            "outline",
                            {"at": document, "depth": 2, "limit": 3, "cursor": outline_cursor},
                        ),
                    }
                )

            # `search` resolves its document handle from the header; the query is the section
            # title, which the scan answers with a section-kind hit on inputs that have one.
            title = None
            if isinstance(top, list) and top:
                first = top[0]
                if isinstance(first, (list, tuple)) and len(first) > 1:
                    title = first[1]
            if isinstance(title, str) and title.strip():
                records.append(
                    {
                        "step": "search title",
                        "payload": call(
                            "search", {"doc": document, "query": title.strip()[:24], "limit": 5}
                        ),
                    }
                )

            # `cite` returns the canonical locators of the handles it is given.
            records.append(
                {"step": "cite section", "payload": call("cite", {"at": [section]})}
            )
            records.append(
                {"step": "cite document", "payload": call("cite", {"at": [document]})}
            )
        except (TimeoutError, RuntimeError, ProbeError) as error:
            records.append(
                {"step": "probe", "payload": {"error": f"{type(error).__name__}: {error}"}}
            )
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
    return drop_wall_clock(records)


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
    reads = run_read_probe(
        binary,
        path,
        state_dir,
        timeout,
        os.path.join(side_dir, "read-server.log"),
    )
    return {"payload": payload, "dump": dump, "reads": reads}


def compare(label: str, left: dict, right: dict, out_dir: str) -> dict:
    """Compares one input's A and B facts, writes diffs for differences, and returns the columns."""
    columns = {column: "same" for column in BLOCK_COLUMNS.values()}
    columns = {"payload": "same", "reads": "same", **columns}
    diffs = []

    left_payload = payload_text(left["payload"])
    right_payload = payload_text(right["payload"])
    if left_payload != right_payload:
        columns["payload"] = "DIFF"
        diffs.append(difflib_text(label, "open payload", left_payload, right_payload))

    left_reads = payload_text(left["reads"])
    right_reads = payload_text(right["reads"])
    if left_reads != right_reads:
        columns["reads"] = "DIFF"
        diffs.append(difflib_text(label, "reads", left_reads, right_reads))

    for block in DUMP_BLOCKS:
        column = BLOCK_COLUMNS[block]
        if left["dump"][block] != right["dump"][block]:
            columns[column] = "DIFF"
            diffs.append(difflib_text(label, column, left["dump"][block], right["dump"][block]))
            if block == "layouts":
                detail = layout_block_diff(left["dump"][block], right["dump"][block])
                if detail:
                    print(f"{label}: {detail}", flush=True)
                    diffs.append(detail + "\n")

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


def layout_block_diff(
    left_text: str, right_text: str, limit: int = 4
) -> str | None:
    """Returns a compact first-mismatch summary for the layouts block, if it can be decoded."""
    left_rows = left_text.splitlines()
    right_rows = right_text.splitlines()
    if len(left_rows) != len(right_rows):
        return f"layouts row count A={len(left_rows)} B={len(right_rows)}"
    for row_index, (left_row, right_row) in enumerate(zip(left_rows, right_rows)):
        if left_row == right_row:
            continue
        left_fields = left_row.split("\t")
        right_fields = right_row.split("\t")
        if len(left_fields) != len(right_fields):
            return f"layouts row {row_index} field count A={len(left_fields)} B={len(right_fields)}"
        for field_index, (left_field, right_field) in enumerate(
            zip(left_fields[:-1], right_fields[:-1])
        ):
            if left_field != right_field:
                return (
                    f"layouts row {row_index} field {field_index}: "
                    f"A={left_field!r} B={right_field!r}"
                )
        try:
            left_layout = decode_layout(bytes.fromhex(left_fields[-1]))
            right_layout = decode_layout(bytes.fromhex(right_fields[-1]))
        except (ValueError, KeyError) as error:
            return f"layouts row {row_index} blob decode failed: {error}"
        for part in ("paragraphs", "sentences", "coverage"):
            left_records = left_layout[part]
            right_records = right_layout[part]
            if left_records == right_records:
                continue
            if len(left_records) != len(right_records):
                return (
                    f"section {left_fields[2]} {part} count "
                    f"A={len(left_records)} B={len(right_records)}"
                )
            for record_index, (left_record, right_record) in enumerate(
                zip(left_records, right_records)
            ):
                if left_record != right_record:
                    return (
                        f"section {left_fields[2]} {part}[{record_index}]: "
                        f"A={left_record} B={right_record}"
                    )
        return f"section {left_fields[2]} blob length A={len(left_fields[-1])} B={len(right_fields[-1])}"
    return None


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
    # The comparison columns, in the order the table prints them.
    column_names = [BLOCK_COLUMNS[block] for block in DUMP_BLOCKS]
    for label, path in inputs:
        if not os.path.isfile(path):
            rows.append(
                (label, "missing", "missing", *(["missing"] * len(column_names)), "DIFF")
            )
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
                columns["reads"],
                *[columns[column] for column in column_names],
                result,
            )
        )
        print(f"{label}: {result}", flush=True)

    print()
    print("| Input | open payload | reads | " + " | ".join(column_names) + " | result |")
    print("| --- | --- | --- | " + " | ".join("---" for _ in column_names) + " | --- |")
    for row in rows:
        print("| " + " | ".join(row) + " |")

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
