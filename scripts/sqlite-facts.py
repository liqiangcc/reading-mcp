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

With `--layout-detail`, every input additionally reports the stored section layout (LAYOUT-1):
`dbstat` page bytes per table and per index, `SUM(length(col))` per `section_layout_v2` column, how
many rows repeat the same coverage value, how many sections have exactly one layout chunk, the
bytes of the chunk blobs against the sentences they hold, and the `EXPLAIN QUERY PLAN` of every
SQL statement the store runs against either layout table. Those lines start with `layout-` so a
reader can pick them out of the facts lines.

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


# ---------------------------------------------------------------------------------------------
# Layout detail (LAYOUT-1).
# ---------------------------------------------------------------------------------------------

LAYOUT_TABLES = ("section_layout_v2", "section_layout_chunk_v2")

# The `section_layout_v2` columns whose bytes are reported one by one.
HEADER_COLUMNS = (
    "section_id",
    "paragraph_coverage_json",
    "plan_error",
    "coverage_paragraph_preserve_json",
    "coverage_paragraph_eligible_json",
    "coverage_sentence_preserve_json",
    "coverage_sentence_eligible_json",
)

# Every statement `src/infrastructure/sqlite.rs` runs against either layout table, in the order the
# file defines them, with the placeholder values bound from the stored document. `?1` and `?2` of a
# lookup are the document key and a section key; `?3` is whatever the statement ranges over.
HEADER_HEADER_SELECT = (
    "SELECT section_key, char_len, paragraph_coverage_json, plan_error,"
    " paragraph_count, sentence_count, chunk_count,"
    " total_items_paragraph_preserve, total_items_paragraph_eligible,"
    " total_items_sentence_preserve, total_items_sentence_eligible,"
    " coverage_paragraph_preserve_json, coverage_paragraph_eligible_json,"
    " coverage_sentence_preserve_json, coverage_sentence_eligible_json"
    " FROM section_layout_v2 WHERE doc_key = ?1 AND section_id = ?2"
)
CHUNK_SELECT = (
    "SELECT chunk_no, first_paragraph_index, first_sentence_index, paragraph_slot,"
    " sentence_slot, paragraph_entry, sentence_entry, eligible_paragraph_entry,"
    " eligible_sentence_entry, paragraph_count, sentence_count, blob"
    " FROM section_layout_chunk_v2"
)

LAYOUT_QUERIES: tuple[tuple[str, str, tuple[str, ...]], ...] = (
    (
        "header_char_len_by_key",
        "SELECT char_len FROM section_layout_v2 WHERE doc_key = ?1 AND section_key = ?2",
        ("doc_key", "section_key"),
    ),
    (
        "header_char_lens_by_doc",
        "SELECT char_len FROM section_layout_v2 WHERE doc_key = ?1 ORDER BY section_key",
        ("doc_key",),
    ),
    (
        "header_section_keys",
        "SELECT l.section_id, l.section_key FROM section_layout_v2 l"
        " WHERE l.doc_key = ?1 ORDER BY l.section_key",
        ("doc_key",),
    ),
    (
        "header_sections",
        "SELECT section_key, section_id, char_len FROM section_layout_v2"
        " WHERE doc_key = ?1 ORDER BY section_key ASC",
        ("doc_key",),
    ),
    (
        "document_exists_with_layout",
        "SELECT doc_key FROM text_unit_documents_v2 WHERE document_id = ?1"
        " AND EXISTS (SELECT 1 FROM section_layout_v2"
        " WHERE section_layout_v2.doc_key = text_unit_documents_v2.doc_key)",
        ("document_id",),
    ),
    ("header_by_section_id", HEADER_HEADER_SELECT, ("doc_key", "section_id")),
    (
        "header_keys_and_ids",
        "SELECT section_key, section_id FROM section_layout_v2"
        " WHERE doc_key = ?1 ORDER BY section_key ASC",
        ("doc_key",),
    ),
    (
        "section_key_of_id",
        "SELECT l.section_key FROM section_layout_v2 l"
        " JOIN text_unit_documents_v2 d ON d.doc_key = l.doc_key"
        " WHERE d.document_id = ?1 AND l.section_id = ?2",
        ("document_id", "section_id"),
    ),
    (
        "section_id_of_key",
        "SELECT l.section_id FROM section_layout_v2 l"
        " JOIN text_unit_documents_v2 d ON d.doc_key = l.doc_key"
        " WHERE d.document_id = ?1 AND l.section_key = ?2",
        ("document_id", "section_key"),
    ),
    (
        "chunk_at_paragraph",
        CHUNK_SELECT
        + " WHERE doc_key = ?1 AND section_key = ?2"
        " AND first_paragraph_index <= ?3 AND paragraph_count > 0"
        " ORDER BY first_paragraph_index DESC, chunk_no DESC LIMIT 1",
        ("doc_key", "section_key", "paragraph_index"),
    ),
    (
        "chunk_at_sentence",
        CHUNK_SELECT
        + " WHERE doc_key = ?1 AND section_key = ?2 AND first_paragraph_index <= ?3"
        " AND (first_paragraph_index < ?3 OR first_sentence_index <= ?4)"
        " ORDER BY first_paragraph_index DESC, first_sentence_index DESC, chunk_no DESC LIMIT 1",
        ("doc_key", "section_key", "paragraph_index", "sentence_index"),
    ),
    (
        "chunks_from",
        CHUNK_SELECT
        + " WHERE doc_key = ?1 AND section_key = ?2 AND chunk_no >= ?3 ORDER BY chunk_no ASC",
        ("doc_key", "section_key", "chunk_no"),
    ),
    (
        "chunk_at_no",
        CHUNK_SELECT + " WHERE doc_key = ?1 AND section_key = ?2 AND chunk_no = ?3",
        ("doc_key", "section_key", "chunk_no"),
    ),
    (
        "stream_position_paragraph_preserve",
        CHUNK_SELECT
        + " WHERE doc_key = ?1 AND section_key = ?2 AND paragraph_entry <= ?3"
        " AND paragraph_count > 0 ORDER BY paragraph_entry DESC, chunk_no DESC LIMIT 1",
        ("doc_key", "section_key", "position"),
    ),
    (
        "stream_position_paragraph_eligible",
        CHUNK_SELECT
        + " WHERE doc_key = ?1 AND section_key = ?2 AND eligible_paragraph_entry <= ?3"
        " AND paragraph_count > 0"
        " ORDER BY eligible_paragraph_entry DESC, chunk_no DESC LIMIT 1",
        ("doc_key", "section_key", "position"),
    ),
    (
        "stream_position_sentence_preserve",
        CHUNK_SELECT
        + " WHERE doc_key = ?1 AND section_key = ?2 AND sentence_entry <= ?3"
        " AND (sentence_count > 0 OR paragraph_count > 0)"
        " ORDER BY sentence_entry DESC, chunk_no DESC LIMIT 1",
        ("doc_key", "section_key", "position"),
    ),
    (
        "stream_position_sentence_eligible",
        CHUNK_SELECT
        + " WHERE doc_key = ?1 AND section_key = ?2 AND eligible_sentence_entry <= ?3"
        " AND (sentence_count > 0 OR paragraph_count > 0)"
        " ORDER BY eligible_sentence_entry DESC, chunk_no DESC LIMIT 1",
        ("doc_key", "section_key", "position"),
    ),
)


def layout_bindings(connection) -> dict[str, object] | None:
    """Representative values for the placeholders of every layout statement, or None."""
    document = connection.execute(
        "SELECT doc_key, document_id FROM text_unit_documents_v2 ORDER BY doc_key LIMIT 1"
    ).fetchone()
    if document is None:
        return None
    doc_key, document_id = document
    section = connection.execute(
        "SELECT section_key, section_id FROM section_layout_v2"
        " WHERE doc_key = ? ORDER BY section_key LIMIT 1",
        (doc_key,),
    ).fetchone()
    if section is None:
        return None
    section_key, section_id = section
    chunk = connection.execute(
        "SELECT chunk_no, first_paragraph_index, first_sentence_index, paragraph_count"
        " FROM section_layout_chunk_v2 WHERE doc_key = ? AND section_key = ?"
        " ORDER BY chunk_no DESC LIMIT 1",
        (doc_key, section_key),
    ).fetchone()
    chunk_no, first_paragraph_index, first_sentence_index, paragraph_count = (
        chunk if chunk is not None else (0, 0, 0, 0)
    )
    return {
        "doc_key": doc_key,
        "document_id": document_id,
        "section_key": section_key,
        "section_id": section_id,
        "chunk_no": chunk_no,
        "paragraph_index": int(first_paragraph_index) + int(paragraph_count),
        "sentence_index": int(first_sentence_index) + 1,
        "position": 0,
    }


def table_exists(connection, table: str) -> bool:
    return (
        connection.execute(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?", (table,)
        ).fetchone()
        is not None
    )


def layout_detail_lines(connection, label: str, store_bytes: int) -> list[str]:
    """The `layout-` lines of one input: sizes, columns, chunk shape and query plans."""
    if not all(table_exists(connection, table) for table in LAYOUT_TABLES):
        return [f"layout-missing input={label} tables={'+'.join(LAYOUT_TABLES)}"]
    lines: list[str] = []

    objects = {}
    try:
        objects = {
            str(name): int(size or 0)
            for name, size in connection.execute(
                "SELECT name, SUM(pgsize) FROM dbstat GROUP BY name"
            )
        }
    except sqlite3.Error:
        lines.append(f"layout-dbstat input={label} unavailable")
    for name in sorted(objects, key=lambda name: objects[name], reverse=True):
        lines.append(f"layout-dbstat input={label} object={name} bytes={objects[name]}")

    layout_bytes = 0
    for table in LAYOUT_TABLES:
        for name, size in objects.items():
            if name == table or name.startswith(f"idx_{table}_") or (
                name.startswith("sqlite_autoindex_" + table)
            ):
                layout_bytes += size
    lines.append(
        f"layout-share input={label} layout_bytes={layout_bytes}"
        f" store_bytes={store_bytes}"
        f" layout_share_pct={100.0 * layout_bytes / store_bytes if store_bytes else 0.0:.2f}"
    )

    header_rows = connection.execute("SELECT COUNT(*) FROM section_layout_v2").fetchone()[0]
    for column in HEADER_COLUMNS:
        total, present = connection.execute(
            f"SELECT COALESCE(SUM(length(CAST({column} AS BLOB))), 0),"
            f" COALESCE(SUM({column} IS NOT NULL), 0) FROM section_layout_v2"
        ).fetchone()
        lines.append(
            f"layout-column input={label} table=section_layout_v2 column={column}"
            f" bytes={int(total)} present_rows={int(present)} rows={int(header_rows)}"
        )

    coverage_columns = (
        "coverage_paragraph_preserve_json",
        "coverage_paragraph_eligible_json",
        "coverage_sentence_preserve_json",
        "coverage_sentence_eligible_json",
    )
    all_equal, preserve_equal, sentence_equal, empty = connection.execute(
        "SELECT"
        f" COALESCE(SUM({coverage_columns[0]} = {coverage_columns[1]}"
        f" AND {coverage_columns[0]} = {coverage_columns[2]}"
        f" AND {coverage_columns[0]} = {coverage_columns[3]}), 0),"
        f" COALESCE(SUM({coverage_columns[0]} = {coverage_columns[1]}), 0),"
        f" COALESCE(SUM({coverage_columns[2]} = {coverage_columns[3]}), 0),"
        f" COALESCE(SUM(length({coverage_columns[0]}) = 0), 0)"
        " FROM section_layout_v2"
    ).fetchone()
    null_paragraph_coverage = connection.execute(
        "SELECT COUNT(*) FROM section_layout_v2 WHERE paragraph_coverage_json IS NULL"
    ).fetchone()[0]
    lines.append(
        f"layout-coverage input={label} rows={int(header_rows)}"
        f" all_four_equal_rows={int(all_equal)}"
        f" paragraph_pair_equal_rows={int(preserve_equal)}"
        f" sentence_pair_equal_rows={int(sentence_equal)}"
        f" empty_rows={int(empty)}"
        f" paragraph_coverage_null_rows={int(null_paragraph_coverage)}"
    )

    chunk_rows, sections, blob_bytes, sentences, paragraphs = connection.execute(
        "SELECT COUNT(*), COUNT(DISTINCT doc_key || '/' || section_key),"
        " COALESCE(SUM(length(blob)), 0), COALESCE(SUM(sentence_count), 0),"
        " COALESCE(SUM(paragraph_count), 0) FROM section_layout_chunk_v2"
    ).fetchone()
    single_chunk_sections = connection.execute(
        "SELECT COUNT(*) FROM (SELECT doc_key, section_key FROM section_layout_chunk_v2"
        " GROUP BY doc_key, section_key HAVING COUNT(*) = 1)"
    ).fetchone()[0]
    max_chunks = connection.execute(
        "SELECT COALESCE(MAX(count), 0) FROM (SELECT COUNT(*) AS count"
        " FROM section_layout_chunk_v2 GROUP BY doc_key, section_key)"
    ).fetchone()[0]
    blob_bytes = int(blob_bytes)
    sentences = int(sentences)
    lines.append(
        f"layout-chunks input={label} chunk_rows={int(chunk_rows)}"
        f" sections_with_chunks={int(sections)} single_chunk_sections={int(single_chunk_sections)}"
        f" max_chunks_per_section={int(max_chunks)} blob_bytes={blob_bytes}"
        f" sentence_count={sentences} paragraph_count={int(paragraphs)}"
        f" blob_bytes_per_sentence={blob_bytes / sentences if sentences else 0.0:.2f}"
    )

    bindings = layout_bindings(connection)
    if bindings is None:
        return lines
    for name, sql, names in LAYOUT_QUERIES:
        values = tuple(bindings[placeholder] for placeholder in names)
        try:
            plan = connection.execute("EXPLAIN QUERY PLAN " + sql, values).fetchall()
        except sqlite3.Error as error:
            lines.append(f"layout-plan input={label} name={name} error={error}")
            continue
        for row in plan:
            lines.append(
                f"layout-plan input={label} name={name} detail={str(row[-1]).replace(' ', '_')}"
            )
    return lines


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
    parser.add_argument(
        "--layout-detail",
        action="store_true",
        help="also report the stored section layout's bytes, shape and query plans",
    )
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
        if args.layout_detail:
            connection = sqlite3.connect(database)
            try:
                detail = layout_detail_lines(
                    connection, label, sqlite_file_bytes(database)
                )
            finally:
                connection.close()
            for line in detail:
                print(line, flush=True)
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
