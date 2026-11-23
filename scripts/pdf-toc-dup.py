#!/usr/bin/env python3
"""TOC-dup (#255): measure outline-section duplication before and after heading cuts.

The harness exercises PDFs exactly as a user does over the v2 tools: `open`, paginated
`outline`, then a sequential `read` walk with `unit="paragraph"` following `next` to the end
of the document. No product code is imported; the only shared helpers are the probe/corpus
modules every PDF script already uses.

Phases:

- `--fetch`: download every corpus PDF (the committed sweep list plus `--extra-id`) into
  `--work-dir/pdfs`, then stop. Downloads are sequential with the usual arXiv delay.
- `--run`: measure one binary over the corpus and write one JSONL record per document to
  `--out`. Repeat counts come from the served paragraph stream; "lost" characters are the
  PyMuPDF reference page text inside the outline-covered page range (first outline entry's
  start page through the last page) that no served paragraph contains, compared as a
  normalised character multiset - never against the old binary's sections. Structure
  provenance and the heading-fallback count are read from the state database the open call
  itself wrote (`documents.document_json` metadata), so nothing user-invisible is guessed.
- `--report`: print the before/after tables for two `--run` JSONL files, plus the requested
  section-prefix dump for the extra paper.
"""

from __future__ import annotations

import argparse
import collections
import concurrent.futures
import importlib.util
import json
import math
import os
import re
import shutil
import sqlite3
import statistics
import sys
import time

ARXIV_PDF = "https://arxiv.org/pdf/{id}"
FETCH_DELAY_S = 2.0
READ_MAX_CHARS = 16000
READ_UNIT = "paragraph"
REPEAT_MIN_CHARS = 200
MAX_READ_CALLS = 200_000
NATIVE_TOC_PROVENANCE = "native_toc"
PAGE_RE = re.compile(r"pdf:page:(\d+)$")
PAGES_RE = re.compile(r"pdf:pages:(\d+)-(\d+)$")


def load_sibling(name: str):
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), name)
    spec = importlib.util.spec_from_file_location(
        name.replace("-", "_").replace(".py", ""), path
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CORPUS = load_sibling("pdf-corpus.py")
PDFPROBE = load_sibling("pdf-probe.py")
PROBE = load_sibling("sequential-read-probe.py")
fetch = CORPUS.fetch
squash = PDFPROBE.squash
percentile = CORPUS.percentile
load_corpus = CORPUS.load_corpus


# --------------------------------------------------------------------------------------------
# Corpus and downloads
# --------------------------------------------------------------------------------------------

def corpus_entries(ids_path: str, extra_id: str | None) -> list[dict]:
    entries = {entry["id"]: entry for entry in load_corpus(ids_path)}
    if extra_id and extra_id not in entries:
        entries[extra_id] = {"id": extra_id, "group": "extra"}
    return list(entries.values())


def pdf_path(work_dir: str, identifier: str) -> str:
    return os.path.join(work_dir, "pdfs", f"{identifier.replace('/', '_')}.pdf")


def fetch_all(entries: list[dict], work_dir: str) -> None:
    pdf_dir = os.path.join(work_dir, "pdfs")
    os.makedirs(pdf_dir, exist_ok=True)
    for entry in entries:
        path = pdf_path(work_dir, entry["id"])
        if os.path.isfile(path) and os.path.getsize(path) > 0:
            continue
        last: str | None = None
        for attempt in range(4):
            try:
                data = fetch(ARXIV_PDF.format(id=entry["id"]), 300, attempts=1)
                if not data.startswith(b"%PDF"):
                    raise RuntimeError("response is not a PDF")
                with open(path, "wb") as handle:
                    handle.write(data)
                print(f"fetched {entry['id']} ({len(data)} bytes)", flush=True)
                time.sleep(FETCH_DELAY_S)
                break
            except Exception as exc:  # noqa: BLE001 - retried below
                last = f"{type(exc).__name__}: {exc}"
                time.sleep(5 * (attempt + 1))
        else:
            print(f"::warning::download failed for {entry['id']}: {last}", flush=True)


# --------------------------------------------------------------------------------------------
# PyMuPDF reference page texts
# --------------------------------------------------------------------------------------------

def reference_pages(path: str) -> list[str]:
    """Squashed page text per page, the independent reference every PDF gate uses."""
    import pymupdf

    pages: list[str] = []
    with pymupdf.open(path) as document:
        for page in document:
            pages.append(squash(page.get_text()))
    return pages


def reference_toc_pages(path: str) -> list[int]:
    """Outline start pages from PyMuPDF's own TOC read (1-based), for cross-checks."""
    import pymupdf

    with pymupdf.open(path) as document:
        return [row[2] for row in document.get_toc() if len(row) >= 3 and row[2] > 0]


# --------------------------------------------------------------------------------------------
# v2 MCP measurement
# --------------------------------------------------------------------------------------------

def tool_payload(result: dict):
    return PROBE.tool_payload(result)


def timed_tool(server, name: str, arguments: dict, timeout: float):
    return PROBE.timed_tool(server, name, arguments, timeout)


def outline_sections(server, doc_handle: str, deadline: float) -> list[tuple[str, str]]:
    """Every section as (handle, title) in outline order."""
    sections: list[tuple[str, str]] = []
    cursor = None
    for _ in range(10_000):
        arguments = {"at": doc_handle, "depth": 16, "limit": 500}
        if cursor is not None:
            arguments["cursor"] = cursor
        result = server.tool("outline", arguments, PROBE.deadline_timeout(deadline))
        payload = tool_payload(result)
        if not isinstance(payload, dict):
            raise PROBE.ProbeError("outline returned no JSON object")
        for item in payload.get("items") or []:
            if isinstance(item, (list, tuple)) and item and isinstance(item[0], str):
                sections.append((item[0], str(item[1]) if len(item) > 1 else ""))
        cursor = payload.get("next")
        if not cursor:
            return sections
    raise PROBE.ProbeError("outline pagination did not terminate")


def cite_pages(server, handles: list[str], deadline: float) -> dict[str, int | None]:
    """The page each section handle starts on, parsed from its citation's native_location."""
    pages: dict[str, int | None] = {}
    if not handles:
        return pages
    for chunk_start in range(0, len(handles), 64):
        chunk = handles[chunk_start:chunk_start + 64]
        result = server.tool("cite", {"at": chunk}, PROBE.deadline_timeout(deadline))
        payload = tool_payload(result)
        items = payload.get("items") if isinstance(payload, dict) else None
        if not isinstance(items, list):
            raise PROBE.ProbeError("cite returned no items")
        for citation in items:
            if not isinstance(citation, dict):
                continue
            handle = citation.get("at")
            page = citation.get("page") if isinstance(citation.get("page"), int) else None
            if page is None:
                locator = citation.get("locator")
                native = (
                    locator.get("native_location")
                    if isinstance(locator, dict)
                    else None
                )
                if isinstance(native, str):
                    match = PAGE_RE.match(native) or PAGES_RE.match(native)
                    if match:
                        page = int(match.group(1))
            if isinstance(handle, str):
                pages[handle] = page
    return pages


def read_walk(server, doc_handle: str, deadline: float) -> list[str]:
    """Every served item text of a sequential paragraph-unit read walk."""
    texts: list[str] = []
    handle = doc_handle
    seen: set[str] = set()
    for _ in range(MAX_READ_CALLS):
        if time.monotonic() >= deadline:
            raise PROBE.DeadlineExceeded("read walk deadline")
        result, _wall_ms = timed_tool(
            server,
            "read",
            {"from": handle, "unit": READ_UNIT, "max_chars": READ_MAX_CHARS},
            PROBE.deadline_timeout(deadline),
        )
        payload = tool_payload(result)
        if not isinstance(payload, dict):
            raise PROBE.ProbeError("read returned no JSON object")
        for item in payload.get("items") or []:
            if isinstance(item, (list, tuple)) and len(item) >= 2 and isinstance(item[1], str):
                texts.append(item[1])
        next_handle = payload.get("next")
        if not isinstance(next_handle, str) or not next_handle:
            return texts
        if next_handle in seen:
            raise PROBE.ProbeError("read `next` repeated a handle")
        seen.add(next_handle)
        handle = next_handle
    raise PROBE.ProbeError("read walk did not terminate")


def section_prefixes(server, handles: list[str], deadline: float) -> dict[str, str]:
    """The first 80 characters each section handle reads, for the before/after dump."""
    prefixes: dict[str, str] = {}
    for handle in handles:
        text = ""
        try:
            result, _wall_ms = timed_tool(
                server,
                "read",
                {"from": handle, "unit": READ_UNIT, "max_chars": READ_MAX_CHARS},
                PROBE.deadline_timeout(deadline),
            )
            payload = tool_payload(result)
            items = payload.get("items") if isinstance(payload, dict) else None
            if isinstance(items, list) and items:
                first = items[0]
                if isinstance(first, (list, tuple)) and len(first) >= 2:
                    text = first[1] if isinstance(first[1], str) else ""
        except PROBE.ProbeError:
            pass
        prefixes[handle] = text[:80]
    return prefixes


def stored_metadata(state_dir: str) -> dict:
    """The document metadata the open call persisted, read back from the state database."""
    database = os.path.join(state_dir, "reading-mcp.sqlite")
    if not os.path.isfile(database):
        return {}
    try:
        connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
        try:
            rows = connection.execute(
                "SELECT document_json FROM documents"
            ).fetchall()
        finally:
            connection.close()
    except sqlite3.Error:
        return {}
    for (document_json,) in rows:
        try:
            document = json.loads(document_json)
        except json.JSONDecodeError:
            continue
        metadata = document.get("metadata")
        if isinstance(metadata, dict):
            return metadata
    return {}


def measure_document(entry: dict, args, deadline: float) -> dict:
    identifier = entry["id"]
    record: dict = {"id": identifier, "group": entry.get("group"), "outcome": "ok"}
    path = pdf_path(args.work_dir, identifier)
    if not os.path.isfile(path):
        record.update({"outcome": "download_failed"})
        return record
    try:
        pages = reference_pages(path)
    except Exception as exc:  # noqa: BLE001 - a broken reference is a result, not a crash
        record.update({"outcome": "reference_failed", "error": str(exc)[:160]})
        return record
    record["page_count"] = len(pages)
    record["ref_toc_pages"] = reference_toc_pages(path)

    # The server resolves the state dir against its own cwd, so it must be absolute.
    state_dir = os.path.abspath(
        os.path.join(args.state_root, args.label, identifier.replace("/", "_"))
    )
    shutil.rmtree(state_dir, ignore_errors=True)
    os.makedirs(state_dir, exist_ok=True)
    server = None
    served_text = ""
    try:
        server = PROBE.Server(
            args.binary,
            PROBE.server_environment(os.path.dirname(os.path.abspath(path)), state_dir, "v2"),
            os.path.join(state_dir, "server.log"),
            request_timeout=max(30.0, args.per_document_seconds),
            cwd=os.path.dirname(os.path.abspath(path)),
        )
        server.initialize(timeout=max(30.0, args.per_document_seconds))
        open_result, open_wall_ms = timed_tool(
            server, "open", {"source": os.path.abspath(path)},
            max(30.0, args.per_document_seconds),
        )
        opened = tool_payload(open_result)
        if not isinstance(opened, dict) or not isinstance(opened.get("doc"), str):
            raise PROBE.ProbeError("open did not return a document handle")
        doc_handle = opened["doc"]
        record["open_seconds"] = round(open_wall_ms / 1000.0, 3)
        record["title"] = opened.get("title")
        pass_deadline = time.monotonic() + args.per_document_seconds
        sections = outline_sections(server, doc_handle, pass_deadline)
        record["sections"] = [[handle, title] for handle, title in sections]
        pages_of = cite_pages(server, [handle for handle, _ in sections], pass_deadline)
        if sections:
            record["outline_start_page"] = pages_of.get(sections[0][0])
        texts = read_walk(server, doc_handle, pass_deadline)
        record["served_chars"] = sum(len(text) for text in texts)
        record["paragraphs"] = len(texts)
        counter = collections.Counter(text for text in texts if len(text) > REPEAT_MIN_CHARS)
        record["repeat_chars"] = sum(
            (count - 1) * len(text) for text, count in counter.items() if count > 1
        )
        served_text = "".join(texts)
        record["served_counts"] = dict(collections.Counter(squash(served_text)))
        record["repeated_paragraphs"] = sorted(
            (len(text), count, text[:80])
            for text, count in counter.items()
            if count > 1
        )[::-1]
        wanted = args.prefix_sections or []
        prefixes = {}
        for wanted_index in wanted:
            if 0 <= wanted_index < len(sections):
                handle = sections[wanted_index][0]
                prefixes[f"s{wanted_index}"] = section_prefixes(
                    server, [handle], pass_deadline
                )[handle]
        record["prefixes"] = prefixes
    except PROBE.ProbeError as exc:
        record.update({"outcome": "probe_error", "error": str(exc)[:200]})
    except Exception as exc:  # noqa: BLE001 - one document must not kill the sweep
        record.update({"outcome": "error", "error": f"{type(exc).__name__}: {exc}"[:200]})
    finally:
        if server is not None:
            try:
                server.kill()
            except Exception:  # noqa: BLE001 - best effort
                pass

    metadata = stored_metadata(state_dir)
    record["provenance"] = metadata.get("pdf_structure_provenance")
    record["toc_entries"] = _int_or_none(metadata.get("pdf_toc_entries"))
    record["heading_fallbacks"] = _int_or_none(metadata.get("pdf_toc_heading_fallbacks"))

    # Lost characters: reference page text inside the outline-covered range (the first
    # outline entry's start page through the last page) that no served paragraph carries,
    # as a normalised character multiset. Repeats can never hide a loss this way.
    served_counts = record.get("served_counts")
    served = "".join(
        char * count for char, count in served_counts.items()
    ) if isinstance(served_counts, dict) else ""
    return finish_record(record, pages, served)


def finish_record(record: dict, ref_pages: list[str], served: str) -> dict:
    if record["outcome"] != "ok":
        return record
    start_page = record.get("outline_start_page")
    if record.get("provenance") == NATIVE_TOC_PROVENANCE and isinstance(start_page, int):
        served_counter = collections.Counter(served)
        lost = 0
        lost_pages = []
        for page_index in range(start_page - 1, len(ref_pages)):
            missing = collections.Counter(ref_pages[page_index]) - served_counter
            page_lost = sum(missing.values())
            if page_lost:
                lost_pages.append([page_index + 1, page_lost])
                lost += page_lost
        record["lost_chars"] = lost
        if lost_pages:
            record["lost_pages"] = lost_pages[:20]
    return record


def _int_or_none(value) -> int | None:
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def run_measure(args) -> int:
    entries = corpus_entries(args.ids, args.extra_id)
    if args.limit:
        entries = entries[: args.limit]
    pending = []
    for entry in entries:
        if os.path.isfile(pdf_path(args.work_dir, entry["id"])):
            pending.append(entry)
        else:
            print(f"::warning::no PDF for {entry['id']}; run --fetch first", flush=True)
    print(f"measuring {len(pending)} documents with {args.binary}", flush=True)
    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)
    os.makedirs(args.state_root, exist_ok=True)
    deadline = time.monotonic() + args.deadline_minutes * 60
    done = 0
    with open(args.out, "a", encoding="utf-8") as out:
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
            futures = {
                pool.submit(
                    measure_document,
                    entry,
                    args,
                    deadline,
                ): entry
                for entry in pending
            }
            for future in concurrent.futures.as_completed(futures):
                record = future.result()
                record["label"] = args.label
                out.write(json.dumps(record, ensure_ascii=False) + "\n")
                out.flush()
                done += 1
                print(
                    f"[{done}/{len(pending)}] {record['id']}: {record['outcome']} "
                    f"served={record.get('served_chars')} repeat={record.get('repeat_chars')} "
                    f"lost={record.get('lost_chars')}",
                    flush=True,
                )
    return 0


# --------------------------------------------------------------------------------------------
# Report
# --------------------------------------------------------------------------------------------

def load_records(path: str) -> dict[str, dict]:
    records: dict[str, dict] = {}
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line:
                record = json.loads(line)
                records[record["id"]] = record
    return records


def summarise(
    records: dict[str, dict], name: str, before: dict[str, dict] | None = None
) -> dict:
    ok = [record for record in records.values() if record["outcome"] == "ok"]
    toc = [record for record in ok if record.get("provenance") == NATIVE_TOC_PROVENANCE]
    dropped: dict[str, int] = {}
    if before is not None:
        for record in toc:
            old = before.get(record["id"])
            old_counts = (old or {}).get("served_counts")
            new_counts = record.get("served_counts")
            if not isinstance(old_counts, dict) or not isinstance(new_counts, dict):
                continue
            missing = sum(
                count for char, count in old_counts.items() if new_counts.get(char, 0) == 0
            )
            if missing:
                dropped[record["id"]] = missing
    opens = [record["open_seconds"] for record in ok if "open_seconds" in record]
    entries_total = sum(record.get("toc_entries") or 0 for record in toc)
    fallbacks_total = sum(record.get("heading_fallbacks") or 0 for record in toc)
    searched = entries_total - sum(1 for record in toc if (record.get("toc_entries") or 0) > 0)
    worst = sorted(
        (record for record in toc if record.get("heading_fallbacks")),
        key=lambda record: (-record["heading_fallbacks"], -(record.get("toc_entries") or 0)),
    )[:5]
    return {
        "name": name,
        "documents": len(records),
        "ok": len(ok),
        "outcomes": dict(collections.Counter(record["outcome"] for record in records.values())),
        "native_toc": len(toc),
        "native_toc_ids": sorted(record["id"] for record in toc),
        "served_chars": sum(record.get("served_chars") or 0 for record in ok),
        "served_chars_toc": sum(record.get("served_chars") or 0 for record in toc),
        "repeat_chars": sum(record.get("repeat_chars") or 0 for record in ok),
        "repeat_chars_toc": sum(record.get("repeat_chars") or 0 for record in toc),
        "papers_with_repeat": sum(1 for record in ok if (record.get("repeat_chars") or 0) > 0),
        "lost_chars": sum(record.get("lost_chars") or 0 for record in toc),
        "lost_papers": {
            record["id"]: record["lost_chars"]
            for record in toc
            if (record.get("lost_chars") or 0) > 0
        },
        "dropped_vs_before": dropped,
        "toc_entries": entries_total,
        "headings_searched": searched,
        "heading_fallbacks": fallbacks_total,
        "headings_located": searched - fallbacks_total,
        "worst_fallbacks": [
            {
                "id": record["id"],
                "fallbacks": record["heading_fallbacks"],
                "entries": record.get("toc_entries"),
            }
            for record in worst
        ],
        "open_median": percentile(opens, 0.5),
        "open_p95": percentile(opens, 0.95),
    }


def print_summary(summary: dict) -> None:
    print(f"== {summary['name']} ==")
    print(f"documents measured ok: {summary['ok']} of {summary['documents']}")
    print(f"outcomes: {summary['outcomes']}")
    print(f"papers with native outline: {summary['native_toc']}")
    print(f"total characters served (all / native-outline): "
          f"{summary['served_chars']} / {summary['served_chars_toc']}")
    print(f"repeated characters (all / native-outline): "
          f"{summary['repeat_chars']} / {summary['repeat_chars_toc']}")
    print(f"papers with any repeat: {summary['papers_with_repeat']}")
    print(f"lost characters inside the outline range: {summary['lost_chars']}")
    if summary["lost_papers"]:
        print(f"  lost by paper: {summary['lost_papers']}")
    dropped = summary.get("dropped_vs_before") or {}
    print(f"characters the before binary served that are absent now: {sum(dropped.values())}")
    if dropped:
        print(f"  dropped by paper: {dropped}")
    print(f"headings located vs fallback: {summary['headings_located']} located / "
          f"{summary['heading_fallbacks']} fallback of {summary['headings_searched']} searched")
    if summary["worst_fallbacks"]:
        print("  five worst fallback papers:")
        for row in summary["worst_fallbacks"]:
            print(f"    {row['id']}: {row['fallbacks']} of {row['entries']} entries")
    print(f"open seconds: median {summary['open_median']}  p95 {summary['open_p95']}")
    print()


def report(args) -> int:
    before = load_records(args.before)
    after = load_records(args.after)
    print_summary(summarise(before, args.before_label))
    print_summary(summarise(after, args.after_label, before))

    extra = args.extra_id
    if extra and extra in before and extra in after:
        old = before[extra]
        new = after[extra]
        print(f"== {extra} section prefixes (first 80 chars) ==")
        keys = sorted(set(old.get("prefixes", {})) | set(new.get("prefixes", {})),
                      key=lambda key: int(key[1:]))
        for key in keys:
            print(f"  {key}")
            print(f"    before: {old.get('prefixes', {}).get(key)!r}")
            print(f"    after:  {new.get('prefixes', {}).get(key)!r}")
        print()
        for side, record in (("before", old), ("after", new)):
            repeats = record.get("repeated_paragraphs") or []
            print(f"  {side} repeated paragraphs: {len(repeats)}")
            for length, count, sample in repeats[:10]:
                print(f"    {length} chars x{count}: {sample!r}")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ids", default="scripts/pdf-sweep-ids.txt")
    parser.add_argument("--extra-id", default="2403.19012")
    parser.add_argument("--work-dir", default=".tmp/pdf-toc-dup")
    parser.add_argument("--state-root", default=".tmp/pdf-toc-dup/state")
    parser.add_argument("--fetch", action="store_true")
    parser.add_argument("--run", action="store_true")
    parser.add_argument("--report", action="store_true")
    parser.add_argument("--binary")
    parser.add_argument("--label", default="run")
    parser.add_argument("--out")
    parser.add_argument("--before")
    parser.add_argument("--after")
    parser.add_argument("--before-label", default="before")
    parser.add_argument("--after-label", default="after")
    parser.add_argument("--workers", type=int, default=4)
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--deadline-minutes", type=float, default=240)
    parser.add_argument("--per-document-seconds", type=float, default=240)
    parser.add_argument("--prefix-sections", type=int, nargs="*", default=None)
    args = parser.parse_args()

    if args.fetch:
        fetch_all(corpus_entries(args.ids, args.extra_id), args.work_dir)
        return 0
    if args.run:
        if not args.binary or not args.out:
            parser.error("--run needs --binary and --out")
        return run_measure(args)
    if args.report:
        if not args.before or not args.after:
            parser.error("--report needs --before and --after")
        return report(args)
    parser.error("one of --fetch, --run or --report is required")
    return 2


if __name__ == "__main__":
    sys.exit(main())
