#!/usr/bin/env python3
"""Report the SQLite facts behind the open's memory peak.

Opens one document per input with the release binary over newline-delimited JSON-RPC, each input
on a fresh state directory, and then reads the resulting database directly:

- the PRAGMAs that shape how much SQLite copies and caches: `page_size`, `journal_mode`,
  `cache_size`, `temp_store` and `mmap_size` (the last three are the connection defaults, which is
  what the server runs with because it sets none of them);
- the byte length of the `documents.document_json` row, the JSON that still carries the full text;
- the total byte length of that document's section text chunks (`section_text_v3`, or the older
  `section_text_v2`/`section_text_v1`), both as stored and after decompression.

Standard library only. Inputs are either a synthetic CJK document of a given size, generated the
way `scripts/memory-profile.py` generates them (`--cjk-mib`), or existing files given as
`--documents label=path` (repeatable). Every input is opened in a fresh process, and each run's
peak RSS is reported so the row can be tied back to the peak measured elsewhere.
"""

from __future__ import annotations

import argparse
import importlib.util
import os
import sqlite3
import sys
import tempfile
import zlib

MIB = 1024 * 1024

PRAGMAS = ("page_size", "journal_mode", "cache_size", "temp_store", "mmap_size")


def load_memory_profile():
    """Import `memory-profile.py` (the hyphen keeps it out of the module namespace) as a module."""
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "memory-profile.py")
    spec = importlib.util.spec_from_file_location("memory_profile", path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def parse_documents(values: list[str]) -> list[tuple[str, str]]:
    entries: list[tuple[str, str]] = []
    for item in values:
        if "=" not in item:
            raise ValueError(f"--documents entry must be label=path: {item}")
        label, path = item.split("=", 1)
        entries.append((label.strip(), os.path.abspath(path.strip())))
    return entries


def database_pragmas(database: str) -> dict[str, object]:
    connection = sqlite3.connect(database)
    try:
        return {
            name: connection.execute(f"PRAGMA {name}").fetchone()[0] for name in PRAGMAS
        }
    finally:
        connection.close()


def section_text_table(connection) -> str | None:
    """The section text table this database holds, newest first, or None."""
    for table in ("section_text_v3", "section_text_v2", "section_text_v1"):
        if connection.execute(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?", (table,)
        ).fetchone():
            return table
    return None


def section_text_facts(connection) -> tuple[int, int, int]:
    """The row count, the stored bytes and the UTF-8 text bytes of the section text chunks."""
    table = section_text_table(connection)
    if table is None:
        return (0, 0, 0)
    if table != "section_text_v3":
        # v1 and v2 kept the UTF-8 text itself: stored and text bytes are the same.
        rows, size = connection.execute(
            f"SELECT COUNT(*), COALESCE(SUM(length(CAST(text AS BLOB))), 0) FROM {table}"
        ).fetchone()
        return (int(rows), int(size), int(size))
    rows = 0
    stored = 0
    text_bytes = 0
    for encoding, data in connection.execute(
        "SELECT encoding, data FROM section_text_v3 ORDER BY doc_key, section_key, chunk_no"
    ):
        blob = bytes(data)
        rows += 1
        stored += len(blob)
        text_bytes += len(zlib.decompress(blob)) if encoding == 1 else len(blob)
    return (rows, stored, text_bytes)


def database_sizes(database: str) -> dict[str, int]:
    """Return the stored `document_json` and section-text byte totals of the one document."""
    connection = sqlite3.connect(database)
    try:
        # `length()` counts characters for TEXT; the CAST to BLOB makes it count UTF-8 bytes.
        documents = connection.execute(
            "SELECT COUNT(*), COALESCE(SUM(length(CAST(document_json AS BLOB))), 0)"
            " FROM documents"
        ).fetchone()
        rows, stored_bytes, text_bytes = section_text_facts(connection)
        meta_v1 = metadata_table_facts(connection, "document_meta_v1")
        meta_v2 = metadata_table_facts(connection, "document_meta_v2")
        header = metadata_table_facts(connection, "document_header_v1")
        sections = metadata_table_facts(connection, "document_sections_v1")
        body_order = metadata_table_facts(connection, "document_body_order_rows_v1")
        return {
            "documents": int(documents[0]),
            "document_json_bytes": int(documents[1]),
            "section_text_rows": rows,
            "section_text_stored_bytes": stored_bytes,
            "section_text_bytes": text_bytes,
            "document_meta_v1_rows": meta_v1[0],
            "document_meta_v1_bytes": meta_v1[1],
            "document_meta_v2_rows": meta_v2[0],
            "document_meta_v2_bytes": meta_v2[1],
            "document_header_v1_rows": header[0],
            "document_header_v1_bytes": header[1],
            "document_sections_v1_rows": sections[0],
            "document_sections_v1_bytes": sections[1],
            "document_body_order_rows_v1_rows": body_order[0],
            "document_body_order_rows_v1_bytes": body_order[1],
        }
    finally:
        connection.close()


# The variable-width payload of each document-scoped table, as a SQL expression over one row. The
# two fixed-width section-row tables keep their numbers in `INTEGER` columns, so only the payload
# counts; a page of the file holds both kinds the same way.
TABLE_PAYLOAD = {
    "document_meta_v1": "length(CAST(value AS BLOB))",
    "document_meta_v2": "length(CAST(value AS BLOB))",
    "document_header_v1": (
        "length(CAST(source AS BLOB)) + length(CAST(title AS BLOB))"
        " + length(CAST(media_type AS BLOB)) + length(CAST(content_hash AS BLOB))"
        " + COALESCE(length(CAST(normalized_document_hash AS BLOB)), 0)"
    ),
    "document_sections_v1": (
        "length(CAST(section_id AS BLOB)) + length(CAST(title AS BLOB))"
        " + length(CAST(location_json AS BLOB))"
    ),
    "document_body_order_rows_v1": "0",
}


def metadata_table_facts(connection, table: str) -> tuple[int, int]:
    """The row count and stored byte total of one document-scoped table, or zeros."""
    if not connection.execute(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?", (table,)
    ).fetchone():
        return (0, 0)
    payload = TABLE_PAYLOAD.get(table, "0")
    rows, size = connection.execute(
        f"SELECT COUNT(*), COALESCE(SUM({payload}), 0) FROM {table}"
    ).fetchone()
    return (int(rows), int(size))


def format_open_seconds(value: float | None) -> str:
    """The open duration with three decimals, or `-` when the open reported none."""
    return "-" if value is None else f"{value:.3f}"


def sqlite_file_bytes(database: str) -> int:
    total = 0
    for suffix in ("", "-wal", "-shm"):
        path = database + suffix
        if os.path.exists(path):
            total += os.path.getsize(path)
    return total


def main() -> int:
    parser = argparse.ArgumentParser(description="Report the SQLite facts behind one open.")
    parser.add_argument("--binary", required=True)
    parser.add_argument("--out-dir", required=True, help="where the generated inputs are written")
    parser.add_argument(
        "--cjk-mib",
        type=float,
        default=None,
        help="also generate and open a synthetic CJK document of this many MiB",
    )
    parser.add_argument(
        "--documents",
        action="append",
        default=[],
        metavar="LABEL=PATH",
        help="also open this existing file (repeatable)",
    )
    parser.add_argument("--run-timeout-seconds", type=float, default=900.0)
    parser.add_argument("--parse-timeout-seconds", type=float, default=900.0)
    args = parser.parse_args()

    binary = os.path.abspath(args.binary)
    if not os.path.exists(binary):
        print(f"binary not found: {binary}", file=sys.stderr)
        return 2
    try:
        document_entries = parse_documents(args.documents)
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 2

    profile = load_memory_profile()
    out_dir = os.path.abspath(args.out_dir)
    os.makedirs(out_dir, exist_ok=True)

    inputs: list[tuple[str, str, float]] = []
    if args.cjk_mib is not None:
        path = os.path.join(out_dir, f"cjk-{args.cjk_mib:g}mib.txt")
        profile.write_cjk_document(path, round(args.cjk_mib * MIB))
        inputs.append((f"cjk-{args.cjk_mib:g}mib", path, args.cjk_mib))
    for label, path in document_entries:
        inputs.append((label, path, round(os.path.getsize(path) / MIB, 2)))

    pragma_lines: list[str] = []
    failed = False
    for label, path, size_mb in inputs:
        state_dir = tempfile.mkdtemp(prefix=f"state-{label}-", dir=out_dir)
        roots = sorted({out_dir, os.path.dirname(path)})
        env = profile.server_environment(
            roots, state_dir, args.parse_timeout_seconds, telemetry=False
        )
        (
            peak_rss_kib,
            open_seconds,
            error,
            timed_out,
            _,
            _,
            _,
            _,
            _,
            _,
        ) = profile.run_one([binary], env, path, args.run_timeout_seconds)
        ok = error is None and not timed_out
        database = os.path.join(state_dir, "reading-mcp.sqlite")
        if not ok:
            failed = True
            print(
                f"input={label} size_mib={size_mb:g} ok=no error={error} "
                f"peak_rss_kib={peak_rss_kib}",
                flush=True,
            )
            continue
        sizes = database_sizes(database)
        pragmas = database_pragmas(database)
        if not pragma_lines:
            pragma_lines = [f"pragma {name}={value}" for name, value in pragmas.items()]
            pragma_lines.append(f"sqlite_file_bytes={sqlite_file_bytes(database)}")
        print(
            "input={label} size_mib={size_mb:g} ok=yes peak_rss_kib={peak} "
            "open_seconds={open_seconds} "
            "documents={documents} document_json_bytes={document_json_bytes} "
            "section_text_rows={section_text_rows} "
            "section_text_stored_bytes={section_text_stored_bytes} "
            "section_text_bytes={section_text_bytes} "
            "document_meta_v1_rows={document_meta_v1_rows} "
            "document_meta_v1_bytes={document_meta_v1_bytes} "
            "document_meta_v2_rows={document_meta_v2_rows} "
            "document_meta_v2_bytes={document_meta_v2_bytes} "
            "document_header_v1_rows={document_header_v1_rows} "
            "document_header_v1_bytes={document_header_v1_bytes} "
            "document_sections_v1_rows={document_sections_v1_rows} "
            "document_sections_v1_bytes={document_sections_v1_bytes} "
            "document_body_order_rows_v1_rows={document_body_order_rows_v1_rows} "
            "document_body_order_rows_v1_bytes={document_body_order_rows_v1_bytes} "
            "sqlite_file_bytes={file_bytes}".format(
                label=label,
                size_mb=size_mb,
                peak=peak_rss_kib,
                open_seconds=format_open_seconds(open_seconds),
                file_bytes=sqlite_file_bytes(database),
                **sizes,
            ),
            flush=True,
        )

    for line in pragma_lines:
        print(line, flush=True)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary and pragma_lines:
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write("\n".join(pragma_lines) + "\n")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
