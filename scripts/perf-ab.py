#!/usr/bin/env python3
"""perf-ab: interleaved base/head performance A/B measurement on one CI runner.

Three modes:

- `corpus`: one shard of the 121-id PDF sweep corpus. Every id is opened by the base and the
  head binary side by side (base, head), three rounds over the shard's ids, every open in a
  fresh state directory. Per id, round and side the outcome class, open seconds, peak RSS
  (VmHWM right after the open answers), and the sha256 of the extracted text (sequential v2
  `read` batches, 1.5M char budget) are recorded as JSONL.
- `readpath`: on one runner, interleaved the same way: 1,000 one-item `read` calls and 50
  `search` calls on a large EPUB, plus `source_view` on 20 pages of a generated text PDF,
  three rounds base/head. Per-call latencies are recorded as JSONL.
- `analyze`: merge the shard and read-path results and compute the gates. `--phase initial`
  reads run1 only and says which jobs must be rechecked; `--phase final` reads run1 and run2,
  judges every metric on its recheck run when its feeding job was rerun, writes the Markdown
  summary and the JSON artifact, and exits non-zero when a metric is worse than base by more
  than the 5% bar on the recheck too, or when an outcome changed from ok to a failure,
  refusal or timeout. The bar is one-sided: positive deltas mean head is slower or uses more
  memory, only a positive delta beyond the bar fails, and a big negative delta (an
  improvement) passes and is reported as improved - never rechecked, never failed.

Paired-ratio method: for the corpus, each id contributes one ratio per round - head's
measurement of that round over base's measurement of the same round, measured back to back on
the same runner - and the id's ratio is the median of its round ratios; the metric's value is
the median (resp. p95) of those per-id ratios. For the read path, each round contributes one
ratio - head's p50 (resp. p95) of the call latencies in that round divided by base's - and the
metric's value is the median of those per-round ratios. All latencies of one side and round
come from one server process; base and head passes of one round run back to back on the same
runner.

Helpers come from `scripts/pdf-probe.py` (imported, never edited). This script never builds
anything.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
import os
import shutil
import statistics
import sys
import time

MIB = 1024 * 1024
OPEN_TIMEOUT_S = 120.0
TEXT_BUDGET_CHARS = 1500000
PER_DOCUMENT_MINUTES = 4.5
READ_CALLS = 1000
SEARCH_CALLS = 50
SOURCE_VIEW_PAGES = 20
BAR_PCT = 5.0
FAILURE_OUTCOMES = {"parse_failed", "timeout", "oom_killed", "unreadable", "too_large",
                    "ocr_required", "download_failed"}


def load_sibling(name: str):
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), name)
    spec = importlib.util.spec_from_file_location(name.replace("-", "_").replace(".py", ""), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


PROBE = load_sibling("pdf-probe.py")

percentile = PROBE.percentile

ARXIV_PDF = "https://arxiv.org/pdf/{id}"
FETCH_DELAY_S = 2.0


def is_cjk(char: str) -> bool:
    code = ord(char)
    return (0x4E00 <= code <= 0x9FFF or 0x3400 <= code <= 0x4DBF
            or 0xF900 <= code <= 0xFAFF)


def load_id_list(path: str) -> list[dict]:
    entries = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            entries.append(json.loads(line))
    return entries


def fetch_bytes(url: str, timeout: float, attempts: int = 3) -> bytes:
    """Bounded download (the same shape as pdf-corpus.fetch, inlined to stay importable)."""
    import urllib.request
    last: Exception | None = None
    for attempt in range(attempts):
        try:
            request = urllib.request.Request(url, headers={
                "User-Agent": "reading-mcp-bench-perfab"})
            with urllib.request.urlopen(request, timeout=timeout) as response:
                return response.read()
        except Exception as exc:  # noqa: BLE001 - retried below
            last = exc
            time.sleep(5 * (attempt + 1))
    raise RuntimeError(f"fetch failed: {last}")


def download_pdf(entry: dict, work_dir: str) -> tuple[str | None, dict]:
    """Download one corpus PDF at CI time, verified against the committed sha256.

    Unlike pdf-sweep's downloader this skips the abs-page licence re-check: the committed
    list already carries the licence and the content hash, and a performance run must not
    spend minutes on abs-page fetches. The sha256 check catches a changed document.
    """
    pdf_dir = os.path.join(work_dir, "pdfs")
    os.makedirs(pdf_dir, exist_ok=True)
    path = os.path.join(pdf_dir, f"{entry['id'].replace('/', '_')}.pdf")
    status: dict = {"download": "ok", "error": None}
    expected = entry.get("sha256")
    if os.path.isfile(path) and os.path.getsize(path) > 0:
        if expected:
            with open(path, "rb") as handle:
                if hashlib.sha256(handle.read()).hexdigest() == expected:
                    return path, status
            os.unlink(path)
        else:
            return path, status
    last: str | None = None
    for attempt in range(3):
        try:
            data = fetch_bytes(ARXIV_PDF.format(id=entry["id"]), 300, attempts=1)
            if not data.startswith(b"%PDF"):
                raise RuntimeError("response is not a PDF")
            if expected and hashlib.sha256(data).hexdigest() != expected:
                raise RuntimeError("sha256 mismatch with the committed corpus list")
            with open(path, "wb") as handle:
                handle.write(data)
            time.sleep(FETCH_DELAY_S)
            return path, status
        except Exception as exc:  # noqa: BLE001 - retried below
            last = f"{type(exc).__name__}: {str(exc)[:160]}"
            time.sleep(5 * (attempt + 1))
    status.update({"download": "download_failed", "error": last})
    return None, status


def classify_outcome(record: dict, timed_out: bool, message: str) -> str:
    code = record.get("error_code")
    if timed_out:
        return "timeout"
    if code in ("OCR_REQUIRED", "ocr_required"):
        return "ocr_required"
    if code in ("RESOURCE_LIMIT_EXCEEDED", "too_large"):
        return "too_large"
    if code == "unreadable":
        return "unreadable"
    if code == "PARSE_FAILED" and "no extractable text" in message:
        return "unreadable"
    return "parse_failed"


def write_json(path: str, data) -> None:
    os.makedirs(os.path.dirname(os.path.abspath(path)) or ".", exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(data, handle, ensure_ascii=False, indent=1, sort_keys=True)
        handle.write("\n")


def append_jsonl(path: str, record: dict) -> None:
    os.makedirs(os.path.dirname(os.path.abspath(path)) or ".", exist_ok=True)
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, ensure_ascii=False) + "\n")


def load_jsonl_dir(root: str) -> list[dict]:
    records: list[dict] = []
    for base, _dirs, names in os.walk(root):
        for name in sorted(names):
            if not name.endswith(".jsonl"):
                continue
            with open(os.path.join(base, name), encoding="utf-8") as handle:
                for line in handle:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        records.append(json.loads(line))
                    except json.JSONDecodeError:
                        continue
    return records


# --------------------------------------------------------------------------------------------
# Shared server measurement helpers
# --------------------------------------------------------------------------------------------

def start_server(binary: str, state_dir: str, source: str) -> PROBE.Server:
    shutil.rmtree(state_dir, ignore_errors=True)
    os.makedirs(state_dir, exist_ok=True)
    env = PROBE.server_env(state_dir, os.path.dirname(os.path.abspath(source)), "default", "")
    return PROBE.Server([binary], env, os.path.join(state_dir, "server.log"),
                        PROBE.TOOL_TIMEOUT_S)


def collect_text_sha(server: PROBE.Server, document_handle: str, budget: int,
                     deadline: float) -> dict:
    """sha256 of the extracted text through sequential v2 read batches."""
    digest = hashlib.sha256()
    chars = 0
    calls = 0
    truncated = False
    error = None
    from_handle = document_handle
    seen: set[str] = set()
    while True:
        if chars >= budget or time.monotonic() > deadline:
            truncated = True
            break
        try:
            data = PROBE.read_batch(server, from_handle)
        except PROBE.ProbeError as exc:
            error = str(exc)[:200]
            break
        calls += 1
        for _handle, text, _kind in PROBE.v2_items(data):
            digest.update(text.encode("utf-8", "ignore"))
            digest.update(b"\n")
            chars += len(text)
        next_handle = data.get("next")
        if not isinstance(next_handle, str):
            break
        if next_handle in seen:
            error = "v2 read next handle repeated"
            break
        seen.add(next_handle)
        from_handle = next_handle
    return {"sha256": digest.hexdigest(), "chars": chars, "calls": calls,
            "truncated": truncated, "error": error}


def measure_open(binary: str, source: str, state_dir: str, deadline: float) -> dict:
    """One open in a fresh state dir: outcome, open seconds, peak RSS, extracted-text sha256."""
    record: dict = {"outcome": None, "error": None, "error_code": None, "open_s": None,
                    "rss_after_open_kib": None, "rss_peak_kib": None,
                    "text_sha256": None, "text_chars": None, "text_truncated": None,
                    "killed": False, "rc": None}
    server = start_server(binary, state_dir, source)
    document_id = None
    timed_out = False
    try:
        server.initialize()
        began = time.perf_counter()
        try:
            result = server.tool("open", {"source": source}, timeout=OPEN_TIMEOUT_S)
            open_error = None
        except PROBE.ProbeError as exc:
            result = None
            open_error = exc
            timed_out = "timed out" in str(exc)
        record["open_s"] = round(time.perf_counter() - began, 3)
        record["rss_after_open_kib"] = PROBE.peak_rss_kib(server.pid)
        if open_error is not None:
            record["error"] = str(open_error)[:400]
            record["error_code"] = open_error.code
        else:
            data = PROBE.payload(result) or {}
            document_id = PROBE.document_handle(data)
        if document_id is not None and time.monotonic() < deadline:
            text = collect_text_sha(server, document_id, TEXT_BUDGET_CHARS, deadline)
            record["text_sha256"] = text["sha256"]
            record["text_chars"] = text["chars"]
            record["text_truncated"] = text["truncated"]
            if text["error"]:
                record["text_error"] = text["error"]
    except PROBE.ProbeError as exc:
        if record["open_s"] is None:
            record["error"] = str(exc)[:400]
            record["error_code"] = exc.code
    finally:
        record["rss_peak_kib"] = PROBE.peak_rss_kib(server.pid)
        server.close(grace=15.0)
    record["rc"] = server.rc
    if server.rc in PROBE.KILLED_CODES:
        record["killed"] = True
    if not record["rss_peak_kib"] and server.peak_kib():
        record["rss_peak_kib"] = server.peak_kib()
    if not record["rss_after_open_kib"]:
        record["rss_after_open_kib"] = record["rss_peak_kib"]

    if record["killed"]:
        record["outcome"] = "oom_killed"
    elif document_id is not None:
        record["outcome"] = "ok"
    elif timed_out:
        record["outcome"] = "timeout"
    elif record["error_code"] is not None:
        record["outcome"] = classify_outcome(record, timed_out,
                                             str(record.get("error") or ""))
    else:
        record["outcome"] = "parse_failed"
        record["error"] = record["error"] or "open returned no document handle"
    return record


# --------------------------------------------------------------------------------------------
# Corpus mode
# --------------------------------------------------------------------------------------------

def corpus_mode(args) -> int:
    entries = load_id_list(args.ids)
    if not entries:
        print("::error::the corpus list is empty")
        return 1
    shard = [entry for index, entry in enumerate(entries) if index % args.shards == args.shard]
    print(f"shard {args.shard}/{args.shards}: {len(shard)} documents, {args.rounds} rounds, "
          f"run tag {args.run_tag}", flush=True)
    os.makedirs(args.state_root, exist_ok=True)
    shard_deadline = time.monotonic() + args.deadline_minutes * 60
    for round_index in range(1, args.rounds + 1):
        for entry in shard:
            if time.monotonic() > shard_deadline:
                append_jsonl(args.out, {
                    "kind": "corpus", "run": args.run_tag, "id": entry["id"],
                    "round": round_index, "side": "both", "outcome": "not_run",
                    "error": "shard deadline",
                })
                continue
            for side, binary in (("base", args.base_bin), ("head", args.head_bin)):
                deadline = min(shard_deadline,
                               time.monotonic() + PER_DOCUMENT_MINUTES * 60)
                path, status = download_pdf(entry, args.work_dir)
                if path is None:
                    record = {"outcome": "download_failed", "error": status["error"],
                              "error_code": "download_failed", "open_s": None,
                              "rss_after_open_kib": None, "rss_peak_kib": None,
                              "text_sha256": None, "text_chars": None, "killed": False}
                else:
                    tag = f"{entry['id'].replace('/', '_')}-r{round_index}-{side}"
                    state_dir = os.path.join(args.state_root, tag)
                    record = measure_open(binary, path, state_dir, deadline)
                    shutil.rmtree(state_dir, ignore_errors=True)
                record.update({
                    "kind": "corpus", "run": args.run_tag, "id": entry["id"],
                    "group": entry.get("group"), "pages": entry.get("pages"),
                    "round": round_index, "side": side,
                })
                append_jsonl(args.out, record)
                print(f"r{round_index} {side:4s} {entry['id']:<16s} {record['outcome']:<14s} "
                      f"open={record['open_s']}s rss={record['rss_after_open_kib']}kib "
                      f"sha={str(record['text_sha256'])[:12]}", flush=True)
    return 0


# --------------------------------------------------------------------------------------------
# Read-path mode
# --------------------------------------------------------------------------------------------

def derive_queries(server: PROBE.Server, document_handle: str, cache_path: str) -> list[str]:
    """50 deterministic search queries: 45 tokens sampled from the book, 5 nonsense strings.

    Derived once per job with the base binary and cached in `cache_path`, so base and head run
    the identical query list in every round.
    """
    if os.path.isfile(cache_path):
        with open(cache_path, encoding="utf-8") as handle:
            return json.load(handle)
    data = PROBE.read_batch(server, document_handle, max_chars=PROBE.V2_READ_BATCH_CHARS)
    text = " ".join(item[1] for item in PROBE.v2_items(data))
    latin = sorted({w.lower() for w in __import__("re").findall(r"[A-Za-z]{5,}", text)})
    cjk = []
    run = ""
    for char in text:
        if is_cjk(char):
            run += char
        else:
            if len(run) >= 4:
                cjk.append(run[:2])
            run = ""
    cjk = sorted(set(cjk))
    tokens = latin if len(latin) >= 45 else cjk
    if len(tokens) < 45:
        tokens = (latin + cjk) or ["the"]
    step = max(1, len(tokens) // 45)
    queries = [tokens[i] for i in range(0, len(tokens), step)][:45]
    queries += ["xqzjvqq", "zzqwxjv", "qqqvwxz", "jxvqzww", "wqzxjvq"]
    queries = queries[:SEARCH_CALLS]
    with open(cache_path, "w", encoding="utf-8") as handle:
        json.dump(queries, handle)
    return queries


def bench_epub(binary: str, epub: str, state_dir: str, queries_cache: str) -> dict:
    """Open the EPUB once, then time 1,000 one-item reads and 50 searches in that session."""
    result: dict = {"epub_open_s": None, "read_ms": [], "search_ms": [], "error": None,
                    "queries": None}
    server = start_server(binary, state_dir, epub)
    try:
        server.initialize()
        began = time.perf_counter()
        open_result = server.tool("open", {"source": epub}, timeout=OPEN_TIMEOUT_S * 3)
        result["epub_open_s"] = round(time.perf_counter() - began, 3)
        document_id = PROBE.document_handle(PROBE.payload(open_result) or {})
        if document_id is None:
            result["error"] = "open returned no document handle"
            return result
        queries = derive_queries(server, document_id, queries_cache)
        result["queries"] = len(queries)

        from_handle = document_id
        seen: set[str] = set()
        for _ in range(READ_CALLS):
            began = time.perf_counter()
            try:
                data = PROBE.read_batch(server, from_handle, max_chars=None)
            except PROBE.ProbeError as exc:
                result["error"] = f"read: {str(exc)[:200]}"
                break
            result["read_ms"].append(round((time.perf_counter() - began) * 1000.0, 3))
            next_handle = data.get("next")
            if not isinstance(next_handle, str) or next_handle in seen:
                break
            seen.add(next_handle)
            from_handle = next_handle

        for query in queries:
            began = time.perf_counter()
            try:
                server.tool("search", {"doc": document_id, "query": query, "limit": 10})
            except PROBE.ProbeError as exc:
                result["error"] = f"search: {str(exc)[:200]}"
                break
            result["search_ms"].append(round((time.perf_counter() - began) * 1000.0, 3))
    except PROBE.ProbeError as exc:
        result["error"] = str(exc)[:300]
    finally:
        server.close(grace=15.0)
    return result


def bench_source_view(binary: str, pdf: str, state_dir: str) -> dict:
    """Open the generated text PDF and time `source_view` on 20 evenly spaced pages."""
    result: dict = {"pdf_open_s": None, "view_ms": [], "pages": 0, "error": None}
    server = start_server(binary, state_dir, pdf)
    try:
        server.initialize()
        began = time.perf_counter()
        open_result = server.tool("open", {"source": pdf}, timeout=OPEN_TIMEOUT_S * 3)
        result["pdf_open_s"] = round(time.perf_counter() - began, 3)
        document_id = PROBE.document_handle(PROBE.payload(open_result) or {})
        if document_id is None:
            result["error"] = "open returned no document handle"
            return result
        sections = PROBE.read_sections(server, document_id)
        handles = [section["section_id"] for section in sections]
        result["pages"] = len(handles)
        if not handles:
            result["error"] = "no sections"
            return result
        count = min(SOURCE_VIEW_PAGES, len(handles))
        step = max(1, len(handles) // count)
        picked = [handles[i] for i in range(0, len(handles), step)][:count]
        for handle in picked:
            began = time.perf_counter()
            try:
                server.tool("source_view", {"at": handle}, timeout=120.0)
            except PROBE.ProbeError as exc:
                result["error"] = f"source_view: {str(exc)[:200]}"
                break
            result["view_ms"].append(round((time.perf_counter() - began) * 1000.0, 3))
    except PROBE.ProbeError as exc:
        result["error"] = str(exc)[:300]
    finally:
        server.close(grace=15.0)
    return result


def readpath_mode(args) -> int:
    os.makedirs(args.state_root, exist_ok=True)
    os.makedirs(args.work_dir, exist_ok=True)
    queries_cache = os.path.join(args.work_dir, "queries.json")
    for round_index in range(1, args.rounds + 1):
        for side, binary in (("base", args.base_bin), ("head", args.head_bin)):
            epub = bench_epub(binary, args.epub,
                              os.path.join(args.state_root, f"epub-r{round_index}-{side}"),
                              queries_cache)
            views = bench_source_view(binary, args.pdf,
                                      os.path.join(args.state_root,
                                                   f"pdf-r{round_index}-{side}"))
            record = {
                "kind": "readpath", "run": args.run_tag, "round": round_index, "side": side,
                "epub_open_s": epub["epub_open_s"], "read_ms": epub["read_ms"],
                "search_ms": epub["search_ms"], "epub_error": epub["error"],
                "pdf_open_s": views["pdf_open_s"], "view_ms": views["view_ms"],
                "pdf_pages": views["pages"], "pdf_error": views["error"],
            }
            append_jsonl(args.out, record)
            print(f"r{round_index} {side:4s} epub_open={epub['epub_open_s']}s "
                  f"reads={len(epub['read_ms'])} searches={len(epub['search_ms'])} "
                  f"views={len(views['view_ms'])}", flush=True)
    shutil.rmtree(args.state_root, ignore_errors=True)
    return 0


# --------------------------------------------------------------------------------------------
# Analyze mode
# --------------------------------------------------------------------------------------------

def pct_delta(ratio: float | None) -> float | None:
    if ratio is None:
        return None
    return round((ratio - 1.0) * 100.0, 2)


def corpus_metrics(records: list[dict]) -> tuple[dict, list[dict], list[dict]]:
    """Per-id paired ratios for open seconds and peak RSS, plus outcome and text changes."""
    by_id: dict[str, dict[str, list[dict]]] = {}
    for record in records:
        if record.get("kind") != "corpus" or record.get("outcome") == "not_run":
            continue
        by_id.setdefault(record["id"], {}).setdefault(record["side"], []).append(record)

    open_ratios: list[float] = []
    rss_ratios: list[float] = []
    outcome_changes: list[dict] = []
    text_changes: list[dict] = []
    ids = sorted(by_id)
    used = 0

    def round_pairs(base: list[dict], head: list[dict], field: str) -> list[float]:
        """One ratio per shared round: the head value over the base value of that round."""
        base_by_round = {r["round"]: r.get(field) for r in base}
        head_by_round = {r["round"]: r.get(field) for r in head}
        ratios = []
        for round_index in sorted(set(base_by_round) & set(head_by_round)):
            denominator = base_by_round[round_index]
            numerator = head_by_round[round_index]
            if denominator and numerator:
                ratios.append(numerator / denominator)
        return ratios

    for identifier in ids:
        sides = by_id[identifier]
        base = sides.get("base") or []
        head = sides.get("head") or []
        open_pairs = round_pairs(base, head, "open_s")
        rss_pairs = round_pairs(base, head, "rss_after_open_kib")
        if open_pairs:
            open_ratios.append(statistics.median(open_pairs))
            used += 1
        if rss_pairs:
            rss_ratios.append(statistics.median(rss_pairs))
        base_outcome = statistics_mode([r["outcome"] for r in base])
        head_outcome = statistics_mode([r["outcome"] for r in head])
        if base_outcome != head_outcome:
            outcome_changes.append({"id": identifier, "base": base_outcome,
                                    "head": head_outcome,
                                    "regression": base_outcome == "ok"
                                    and head_outcome in FAILURE_OUTCOMES})
        base_sha = statistics_mode([r.get("text_sha256") for r in base
                                    if r.get("text_sha256")])
        head_sha = statistics_mode([r.get("text_sha256") for r in head
                                    if r.get("text_sha256")])
        if base_sha and head_sha and base_sha != head_sha:
            base_chars = statistics_mode([r.get("text_chars") for r in base
                                          if r.get("text_chars")])
            head_chars = statistics_mode([r.get("text_chars") for r in head
                                          if r.get("text_chars")])
            text_changes.append({"id": identifier, "base_sha": base_sha[:16],
                                 "head_sha": head_sha[:16],
                                 "base_chars": base_chars, "head_chars": head_chars})
    metrics = {
        "corpus_open_median": ratio_stats(open_ratios, 0.5),
        "corpus_open_p95": ratio_stats(open_ratios, 0.95),
        "corpus_rss_median": ratio_stats(rss_ratios, 0.5),
        "corpus_rss_p95": ratio_stats(rss_ratios, 0.95),
        "corpus_pairs": used,
        "corpus_ids": len(ids),
    }
    return metrics, outcome_changes, text_changes


def statistics_mode(values: list):
    counts: dict = {}
    for value in values:
        counts[value] = counts.get(value, 0) + 1
    if not counts:
        return None
    return max(counts, key=lambda v: (counts[v], str(v)))


def ratio_stats(ratios: list[float], fraction: float) -> float | None:
    if not ratios:
        return None
    value = percentile(ratios, fraction)
    return round(value, 4) if value is not None else None


def readpath_metrics(records: list[dict]) -> dict:
    """Per-round paired ratios of the read/search/source_view latency percentiles."""
    by_round: dict[int, dict[str, dict]] = {}
    for record in records:
        if record.get("kind") != "readpath":
            continue
        by_round.setdefault(record["round"], {})[record["side"]] = record

    series: dict[str, list[float]] = {"read_p50": [], "read_p95": [],
                                      "search_p50": [], "search_p95": [],
                                      "view_p50": [], "view_p95": []}
    for round_index in sorted(by_round):
        sides = by_round[round_index]
        base = sides.get("base")
        head = sides.get("head")
        if not base or not head:
            continue
        for key, field in (("read", "read_ms"), ("search", "search_ms"),
                           ("view", "view_ms")):
            for fraction, suffix in ((0.5, "p50"), (0.95, "p95")):
                base_value = percentile(base.get(field) or [], fraction)
                head_value = percentile(head.get(field) or [], fraction)
                if base_value and head_value and base_value > 0:
                    series[f"{key}_{suffix}"].append(head_value / base_value)
    metrics: dict = {}
    for key, ratios in series.items():
        metrics[f"readpath_{key}"] = (round(statistics.median(ratios), 4) if ratios else None)
        metrics[f"readpath_{key}_rounds"] = len(ratios)
    return metrics


GATES = [
    ("corpus_open_median", "corpus", "PDF open time, median of per-id ratios"),
    ("corpus_open_p95", "corpus", "PDF open time, p95 of per-id ratios"),
    ("corpus_rss_median", "corpus", "PDF peak RSS, median of per-id ratios"),
    ("corpus_rss_p95", "corpus", "PDF peak RSS, p95 of per-id ratios"),
    ("readpath_read_p50", "readpath", "EPUB sequential read, p50 round ratios"),
    ("readpath_read_p95", "readpath", "EPUB sequential read, p95 round ratios"),
    ("readpath_search_p50", "readpath", "EPUB search, p50 round ratios"),
    ("readpath_search_p95", "readpath", "EPUB search, p95 round ratios"),
    ("readpath_view_p50", "readpath", "source_view, p50 round ratios"),
    ("readpath_view_p95", "readpath", "source_view, p95 round ratios"),
]


def verdict_for_delta(delta_pct: float | None) -> str:
    """One-sided verdict: only a positive delta beyond the bar is a failure.

    Positive deltas mean head is slower or uses more memory. Any negative delta passes, and
    a delta below -BAR_PCT is an improvement worth reporting. The bar never applies to
    improvements: the head being much faster is a pass, not a regression.
    """
    if delta_pct is None:
        return "no data"
    if delta_pct > BAR_PCT:
        return "FAIL"
    if delta_pct < -BAR_PCT:
        return "improved"
    return "pass"


def verdict_fails(verdict: str) -> bool:
    return verdict in ("FAIL", "no data")


def evaluate(metrics: dict) -> dict:
    """Which feeding jobs must be rechecked: any of their metrics misses the 5% bar.

    A miss is a real slowdown or memory growth (positive delta beyond the bar) or missing
    data. Improvements never trigger a recheck.
    """
    misses = []
    for key, job, label in GATES:
        ratio = metrics.get(key)
        delta = pct_delta(ratio)
        verdict = verdict_for_delta(delta)
        if delta is None:
            misses.append({"key": key, "job": job, "label": label, "delta_pct": None,
                           "reason": "no data"})
        elif verdict == "FAIL":
            misses.append({"key": key, "job": job, "label": label, "delta_pct": delta,
                           "reason": f"delta > +{BAR_PCT}%"})
    return {
        "corpus": any(miss["job"] == "corpus" for miss in misses),
        "readpath": any(miss["job"] == "readpath" for miss in misses),
        "misses": misses,
    }


def metric_table(rows: list[dict]) -> str:
    lines = ["| Metric | Job | Run judged | head/base delta | Bar | Verdict |",
             "| --- | --- | --- | ---: | ---: | --- |"]
    for row in rows:
        delta = row.get("delta_pct")
        lines.append(
            f"| {row['label']} | {row['job']} | {row['run']} | "
            f"{'no data' if delta is None else f'{delta:+.2f}%'} | +{BAR_PCT:.0f}% | "
            f"{row['verdict']} |")
    return "\n".join(lines)


def split_runs(records: list[dict]) -> dict[str, list[dict]]:
    runs: dict[str, list[dict]] = {"run1": [], "run2": []}
    for record in records:
        tag = record.get("run") or "run1"
        runs.setdefault(tag, []).append(record)
    return runs


def analyze_mode(args) -> int:
    records = load_jsonl_dir(args.results_dir)
    runs = split_runs(records)
    print(f"loaded {len(records)} records: "
          + ", ".join(f"{tag}={len(rows)}" for tag, rows in sorted(runs.items())))

    per_run: dict[str, dict] = {}
    details: dict[str, dict] = {}
    for tag in sorted(runs):
        if not runs[tag]:
            continue
        corpus, outcome_changes, text_changes = corpus_metrics(runs[tag])
        readpath = readpath_metrics(runs[tag])
        per_run[tag] = {**corpus, **readpath}
        details[tag] = {"outcome_changes": outcome_changes, "text_changes": text_changes}

    run1 = per_run.get("run1", {})
    initial_needs = evaluate(run1)

    if args.phase == "initial":
        needs = {"corpus": initial_needs["corpus"], "readpath": initial_needs["readpath"]}
        write_json(args.needs, needs)
        write_json(args.out, {"phase": "initial", "run1": run1,
                              "misses": initial_needs["misses"], "needs": needs})
        print(f"needs recheck: {needs}")
        for miss in initial_needs["misses"]:
            print(f"  miss: {miss['key']} delta={miss['delta_pct']} ({miss['reason']})")
        return 0

    # Final phase: judge each metric on run2 when its feeding job was rechecked, else run1.
    run2 = per_run.get("run2", {})
    rechecked = {"corpus": False, "readpath": False}
    if run2:
        # The recheck only exists for jobs the initial analysis flagged; the needs file is
        # authoritative when present, otherwise any run2 data marks the job as rechecked.
        needs_path = args.needs
        if os.path.isfile(needs_path):
            with open(needs_path, encoding="utf-8") as handle:
                rechecked.update(json.load(handle))
        else:
            rechecked["corpus"] = any(r.get("kind") == "corpus" for r in runs["run2"])
            rechecked["readpath"] = any(r.get("kind") == "readpath" for r in runs["run2"])

    rows: list[dict] = []
    failed = False
    for key, job, label in GATES:
        judged_tag = "run2" if rechecked.get(job) and run2 else "run1"
        metrics = per_run.get(judged_tag, {})
        ratio = metrics.get(key)
        delta = pct_delta(ratio)
        first = run1.get(key)
        verdict = verdict_for_delta(delta)
        if verdict_fails(verdict):
            failed = True
        rows.append({"key": key, "job": job, "label": label, "run": judged_tag,
                     "ratio": ratio, "delta_pct": delta,
                     "run1_delta_pct": pct_delta(first),
                     "run2_delta_pct": pct_delta(run2.get(key)) if run2 else None,
                     "verdict": verdict})

    judged_tag = "run2" if rechecked.get("corpus") and run2 else "run1"
    outcome_changes = details.get(judged_tag, {}).get("outcome_changes", [])
    text_changes = details.get(judged_tag, {}).get("text_changes", [])
    regressions = [change for change in outcome_changes if change.get("regression")]
    if regressions:
        failed = True

    lines = ["# perf-ab: base vs head on the same runner", ""]
    if args.base or args.head:
        lines += [f"- base: `{args.base}`", f"- head: `{args.head}`", ""]
    lines += [
        "Paired ratios: every corpus id contributes one ratio per round (head's round over "
        "base's same round, measured back to back) and the id's ratio is the median of its "
        "round ratios; the metric is the median (p95) of those per-id ratios. Read path: "
        "every round contributes the ratio of head's p50 (p95) over base's; the metric is "
        "the median of those round ratios. Positive deltas mean head is slower or uses "
        "more memory. The bar is one-sided: only a positive delta beyond +5% fails; "
        "negative deltas pass and big ones are reported as improved.", "",
        "## Metrics", "",
        metric_table(rows), "",
    ]
    if run2:
        lines += ["### Both runs", "",
                  "| Metric | run1 delta | run2 delta |",
                  "| --- | ---: | ---: |"]
        for row in rows:
            first = row["run1_delta_pct"]
            second = row["run2_delta_pct"]
            lines.append(
                f"| {row['label']} | "
                f"{'no data' if first is None else f'{first:+.2f}%'} | "
                f"{'no data' if second is None else f'{second:+.2f}%'} |")
        lines += ["", f"Recheck ran for: "
                  + ", ".join(job for job in ("corpus", "readpath") if rechecked.get(job))
                  or "none", ""]
    lines += [
        f"Corpus pairs: {run1.get('corpus_pairs', 0)}/{run1.get('corpus_ids', 0)} ids with "
        f"paired open data in run1.", "",
        "## Text changes (reported, not gated)", "",
    ]
    if text_changes:
        lines += ["| id | base sha | head sha | base chars | head chars |",
                  "| --- | --- | --- | ---: | ---: |"]
        for change in text_changes:
            lines.append(f"| {change['id']} | {change['base_sha']} | {change['head_sha']} | "
                         f"{change.get('base_chars')} | {change.get('head_chars')} |")
    else:
        lines.append("No extracted-text changes.")
    lines += ["", "## Outcome changes", ""]
    if outcome_changes:
        lines += ["| id | base | head | regression |", "| --- | --- | --- | --- |"]
        for change in outcome_changes:
            lines.append(f"| {change['id']} | {change['base']} | {change['head']} | "
                         f"{'**YES**' if change.get('regression') else 'no'} |")
    else:
        lines.append("No outcome changes.")
    lines.append("")
    verdict = "FAIL" if failed else "PASS"
    lines += [f"## Verdict: {verdict}", ""]
    markdown = "\n".join(lines)

    if args.markdown:
        with open(args.markdown, "w", encoding="utf-8") as handle:
            handle.write(markdown)
    write_json(args.out, {
        "phase": "final", "base": args.base, "head": args.head, "bar_pct": BAR_PCT,
        "verdict": verdict, "metrics": rows, "per_run": per_run, "rechecked": rechecked,
        "text_changes": text_changes, "outcome_changes": outcome_changes,
    })
    print(markdown)
    return 1 if failed else 0


# --------------------------------------------------------------------------------------------

def main() -> int:
    parser = argparse.ArgumentParser(description="perf-ab base/head interleaved measurement.")
    sub = parser.add_subparsers(dest="mode", required=True)

    corpus = sub.add_parser("corpus")
    corpus.add_argument("--base-bin", required=True)
    corpus.add_argument("--head-bin", required=True)
    corpus.add_argument("--ids", default="scripts/pdf-sweep-ids.txt")
    corpus.add_argument("--shard", type=int, required=True)
    corpus.add_argument("--shards", type=int, default=4)
    corpus.add_argument("--rounds", type=int, default=5)
    corpus.add_argument("--run-tag", default="run1")
    corpus.add_argument("--work-dir", required=True)
    corpus.add_argument("--state-root", required=True)
    corpus.add_argument("--out", required=True)
    corpus.add_argument("--deadline-minutes", type=float, default=33.0)

    readpath = sub.add_parser("readpath")
    readpath.add_argument("--base-bin", required=True)
    readpath.add_argument("--head-bin", required=True)
    readpath.add_argument("--epub", required=True)
    readpath.add_argument("--pdf", required=True)
    readpath.add_argument("--rounds", type=int, default=5)
    readpath.add_argument("--run-tag", default="run1")
    readpath.add_argument("--work-dir", required=True)
    readpath.add_argument("--state-root", required=True)
    readpath.add_argument("--out", required=True)

    analyze = sub.add_parser("analyze")
    analyze.add_argument("--results-dir", required=True)
    analyze.add_argument("--phase", choices=["initial", "final"], required=True)
    analyze.add_argument("--out", required=True)
    analyze.add_argument("--markdown", default="")
    analyze.add_argument("--needs", default="needs.json")
    analyze.add_argument("--base", default="")
    analyze.add_argument("--head", default="")

    args = parser.parse_args()
    if args.mode == "corpus":
        return corpus_mode(args)
    if args.mode == "readpath":
        return readpath_mode(args)
    return analyze_mode(args)


if __name__ == "__main__":
    sys.exit(main())
