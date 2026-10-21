#!/usr/bin/env python3
"""PDF-1a: a real-paper corpus for the PDF backend measurement.

Three modes, all standard library plus PyMuPDF (and the pinned layout environment for the
layout backend):

- `--select`: query the arXiv API for CC-BY(-SA)/CC0 papers across seven groups (including
  five papers with substantial CJK text), download each candidate to check its page count and
  (for the CJK group) its CJK character count, and write the committed corpus list
  (`scripts/pdf-corpus-ids.txt`, JSON lines with a comment header). Downloads happen at CI
  time; only the list is committed.
- `--run`: for one shard of the corpus, download each PDF, measure a PyMuPDF reference text,
  then open/read it with the default and the layout backend over stdio MCP exactly as
  `scripts/pdf-probe.py` does. For every layout `OCR_REQUIRED`, reproduce the worker invocation
  from `src/parsing/layout_pdf.rs`, capture its failure output, and analyse each page with the
  worker's own `require_disabled_page_coverage` (imported, never modified). Writes JSONL.
- `--aggregate`: merge every shard's JSONL and print the tables used in
  `docs/design/pdf-backend-data.md` (per-paper outcomes, refusal rate and reasons, detector
  candidates, open-time comparison).

`--coverage` is the helper the run mode executes inside the layout interpreter for one PDF.
It changes no product code and never builds anything.
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import importlib.util
import itertools
import json
import math
import os
import re
import statistics
import subprocess
import sys
import time
import unicodedata
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

MIB = 1024 * 1024
ARXIV_API = "https://export.arxiv.org/api/query"
ARXIV_ABS = "https://arxiv.org/abs/{id}"
ARXIV_PDF = "https://arxiv.org/pdf/{id}"
ARXIV_DELAY_S = 3.0
USER_AGENT = "reading-mcp-bench-pdf1a"
CC_LICENSE_MARKERS = (
    "licenses/by/4.0",
    "licenses/by-sa/4.0",
    "publicdomain/zero/1.0",
)
MIN_PAGES, MAX_PAGES = 8, 30
CJK_MIN_CHARS = 200
READ_SENTENCES = 200
CONTAINMENT_PAGES = 20
GRAM = 5
# Mirrors the probe's server environment: the release server's own limits.
WORKER_MAX_PAGES = 2000
WORKER_MAX_BYTES = 134217728
WORKER_MAX_CHARS = 200000000
RASTER_PIXEL_CAP = 64_000_000

SLOTS = [
    {"group": "cs.CL", "count": 6, "query": "cat:cs.CL", "cjk": False},
    {"group": "cs.CV", "count": 5, "query": "cat:cs.CV", "cjk": False},
    {"group": "cs.LG/stat.ML", "count": 4, "query": "(cat:cs.LG OR cat:stat.ML)", "cjk": False},
    {"group": "math", "count": 4,
     "query": "(cat:math.NT OR cat:math.CO OR cat:math.PR OR cat:math.AG)", "cjk": False},
    {"group": "physics", "count": 3,
     "query": "(cat:hep-th OR cat:quant-ph OR cat:cond-mat.stat-mech)", "cjk": False},
    {"group": "q-bio/econ", "count": 3, "query": "(cat:q-bio.QM OR cat:econ.EM)", "cjk": False},
    {"group": "cjk", "count": 5,
     "query": "cat:cs.CL AND (abs:Chinese OR abs:Japanese)", "cjk": True},
]
MAX_CANDIDATES_PER_SLOT = 120


# --------------------------------------------------------------------------------------------
# Shared helpers
# --------------------------------------------------------------------------------------------

def load_probe():
    """Import `scripts/pdf-probe.py` (the file name has a dash) for its stdio MCP helpers."""
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "pdf-probe.py")
    spec = importlib.util.spec_from_file_location("pdf_probe", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def squash(text: str) -> str:
    return re.sub(r"\s+", "", unicodedata.normalize("NFKC", text))


def is_cjk(char: str) -> bool:
    code = ord(char)
    return (0x3400 <= code <= 0x4DBF or 0x4E00 <= code <= 0x9FFF
            or 0xF900 <= code <= 0xFAFF or 0x3040 <= code <= 0x30FF
            or 0x31F0 <= code <= 0x31FF or 0xAC00 <= code <= 0xD7AF)


def is_unprintable(char: str) -> bool:
    code = ord(char)
    if char == "\ufffd" or 0xE000 <= code <= 0xF8FF:
        return True
    if code < 0x20 and char not in "\n\r\t":
        return True
    return 0x80 <= code <= 0x9F


def text_stats(texts: list[str]) -> dict:
    """Statistics over the first sentences a backend returned."""
    joined = " ".join(texts)
    total = len(joined)
    if total == 0:
        return {"chars": 0, "raw_chars": 0, "cjk_chars": 0, "cjk_share": None,
                "nonprintable_share": None, "alnum_share": None}
    cjk = sum(1 for char in joined if is_cjk(char))
    bad = sum(1 for char in joined if is_unprintable(char))
    alnum = sum(1 for char in joined if char.isalnum())
    return {
        "chars": len(squash(joined)),
        "raw_chars": total,
        "cjk_chars": cjk,
        "cjk_share": round(cjk / total, 4),
        "nonprintable_share": round(bad / total, 4),
        "alnum_share": round(alnum / total, 4),
    }


def gram_containment(reference: str, got: str, n: int = GRAM) -> float | None:
    """Share of reference n-grams (multiset) that the backend text also contains."""
    reference = squash(reference)
    got = squash(got)
    if len(reference) < n:
        return None
    if len(got) < n:
        return 0.0
    counts = collections.Counter(got[index:index + n] for index in range(len(got) - n + 1))
    total = 0
    hit = 0
    for index in range(len(reference) - n + 1):
        gram = reference[index:index + n]
        total += 1
        if counts.get(gram, 0) > 0:
            counts[gram] -= 1
            hit += 1
    return round(hit / total, 4) if total else None


def percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, math.ceil(fraction * len(ordered)) - 1))
    return ordered[index]


def license_marker(url: str) -> str | None:
    for marker in CC_LICENSE_MARKERS:
        if marker in url:
            return marker
    return None


def fetch(url: str, timeout: float, attempts: int = 3) -> bytes:
    last: Exception | None = None
    for attempt in range(attempts):
        try:
            request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
            with urllib.request.urlopen(request, timeout=timeout) as response:
                return response.read()
        except Exception as exc:  # noqa: BLE001 - retried below
            last = exc
            time.sleep(5 * (attempt + 1))
    raise RuntimeError(f"{url}: {last}")


def write_json(path: str, value) -> None:
    os.makedirs(os.path.dirname(os.path.abspath(path)) or ".", exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(value, handle, ensure_ascii=False, indent=1, sort_keys=True)


def load_corpus(path: str) -> list[dict]:
    entries = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            entries.append(json.loads(line))
    return entries


# --------------------------------------------------------------------------------------------
# Selection
# --------------------------------------------------------------------------------------------

def arxiv_query(query: str, start: int, max_results: int) -> list[dict]:
    url = (f"{ARXIV_API}?search_query={urllib.parse.quote(query)}&start={start}"
           f"&max_results={max_results}&sortBy=submittedDate&sortOrder=descending")
    payload = fetch(url, 120)
    root = ET.fromstring(payload)
    namespace = {"atom": "http://www.w3.org/2005/Atom",
                 "arxiv": "http://arxiv.org/schemas/atom"}
    entries = []
    for entry in root.findall("atom:entry", namespace):
        identifier = entry.findtext("atom:id", default="", namespaces=namespace)
        match = re.search(r"abs/([^/]+?)(v\d+)?$", identifier)
        if not match:
            continue
        category = ""
        primary = entry.find("arxiv:primary_category", namespace)
        if primary is not None:
            category = primary.get("term", "")
        entries.append({
            "id": match.group(1),
            "title": re.sub(r"\s+", " ", entry.findtext("atom:title", default="",
                                                        namespaces=namespace)).strip(),
            "abstract": re.sub(r"\s+", " ", entry.findtext("atom:summary", default="",
                                                           namespaces=namespace)).strip(),
            "submitted": entry.findtext("atom:published", default="", namespaces=namespace),
            "category": category,
            "license": entry.findtext("arxiv:license", default="", namespaces=namespace),
        })
    return entries


def pdf_pages_and_cjk(path: str) -> tuple[int, int]:
    import pymupdf

    with pymupdf.open(path) as document:
        pages = document.page_count
        cjk = 0
        for page in document:
            cjk += sum(1 for char in page.get_text() if is_cjk(char))
    return pages, cjk


def select_corpus(args) -> int:
    work = os.path.join(args.work_dir, "select")
    os.makedirs(work, exist_ok=True)
    chosen: list[dict] = []
    seen: set[str] = set()
    skipped: list[dict] = []
    for slot in SLOTS:
        picked = 0
        start = 0
        scanned = 0
        while picked < slot["count"] and scanned < MAX_CANDIDATES_PER_SLOT:
            batch = arxiv_query(slot["query"], start, 25)
            time.sleep(ARXIV_DELAY_S)
            if not batch:
                break
            start += len(batch)
            for candidate in batch:
                if picked >= slot["count"] or scanned >= MAX_CANDIDATES_PER_SLOT:
                    break
                scanned += 1
                if candidate["id"] in seen:
                    continue
                seen.add(candidate["id"])
                # The licence line on the abs page is the authoritative check (the API
                # entry no longer always carries an <arxiv:license> element).
                try:
                    abs_page = fetch(ARXIV_ABS.format(id=candidate["id"]), 120).decode(
                        "utf-8", "ignore")
                    time.sleep(ARXIV_DELAY_S)
                except RuntimeError as exc:
                    skipped.append({"id": candidate["id"], "reason": f"abs_failed: {exc}"})
                    continue
                marker = None
                license_url = candidate["license"]
                for found in re.findall(r"https?://(?:creativecommons\.org|arxiv\.org)/"
                                        r"[a-z0-9./\-]+", abs_page):
                    marker = license_marker(found)
                    if marker is not None:
                        license_url = found
                        break
                if marker is None:
                    skipped.append({"id": candidate["id"], "reason": "licence_not_cc"})
                    continue
                pdf_path = os.path.join(work, f"{candidate['id']}.pdf")
                if not os.path.isfile(pdf_path):
                    try:
                        data = fetch(ARXIV_PDF.format(id=candidate["id"]), 300)
                    except RuntimeError as exc:
                        skipped.append({"id": candidate["id"], "reason": f"download_failed: {exc}"})
                        time.sleep(ARXIV_DELAY_S)
                        continue
                    time.sleep(ARXIV_DELAY_S)
                    if not data.startswith(b"%PDF"):
                        skipped.append({"id": candidate["id"], "reason": "not_a_pdf"})
                        continue
                    with open(pdf_path, "wb") as handle:
                        handle.write(data)
                try:
                    pages, cjk = pdf_pages_and_cjk(pdf_path)
                except Exception as exc:  # noqa: BLE001 - a broken PDF is a skip
                    skipped.append({"id": candidate["id"], "reason": f"unreadable: {exc}"})
                    continue
                if not MIN_PAGES <= pages <= MAX_PAGES:
                    skipped.append({"id": candidate["id"], "reason": f"pages={pages}"})
                    continue
                if slot["cjk"] and cjk < CJK_MIN_CHARS:
                    skipped.append({"id": candidate["id"], "reason": f"cjk={cjk}"})
                    continue
                chosen.append({
                    "id": candidate["id"],
                    "title": candidate["title"],
                    "group": slot["group"],
                    "category": candidate["category"],
                    "pages": pages,
                    "license": license_url,
                    "license_marker": marker,
                    "cjk_chars": cjk,
                    "submitted": candidate["submitted"][:10],
                    "bytes": os.path.getsize(pdf_path),
                    "sha256": hashlib.sha256(open(pdf_path, "rb").read()).hexdigest(),
                })
                picked += 1
                print(f"{slot['group']} {picked}/{slot['count']} {candidate['id']} "
                      f"pages={pages} cjk={cjk} {candidate['title'][:60]}", flush=True)
        if picked < slot["count"]:
            print(f"::warning::slot {slot['group']} found only {picked}/{slot['count']}",
                  flush=True)

    header = [
        "# PDF-1a real-paper corpus: 30 arXiv papers, CC-BY(-SA)/CC0 only.",
        f"# Selected {time.strftime('%Y-%m-%d')} by scripts/pdf-corpus.py --select; "
        "one JSON object per line.",
        "# id, title, group, category, pages, licence (abs-page link), cjk_chars.",
    ]
    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)
    with open(args.out, "w", encoding="utf-8") as handle:
        handle.write("\n".join(header) + "\n")
        for entry in chosen:
            handle.write(json.dumps(entry, ensure_ascii=False, sort_keys=True) + "\n")
    print(f"wrote {len(chosen)} entries to {args.out}")
    print(f"skipped {len(skipped)} candidates; reasons: "
          f"{collections.Counter(item['reason'].split(':')[0] for item in skipped).most_common()}")
    write_json(os.path.join(work, "select-report.json"),
               {"chosen": chosen, "skipped": skipped})
    return 0 if len(chosen) == sum(slot["count"] for slot in SLOTS) else 1


# --------------------------------------------------------------------------------------------
# Reference text (independent of both backends)
# --------------------------------------------------------------------------------------------

def reference_metrics(path: str) -> dict:
    import pymupdf

    pages: list[dict] = []
    first_pages: list[str] = []
    total_chars = 0
    total_cjk = 0
    with pymupdf.open(path) as document:
        page_count = document.page_count
        for page in document:
            text = page.get_text()
            chars = len(squash(text))
            cjk = sum(1 for char in text if is_cjk(char))
            total_chars += chars
            total_cjk += cjk
            images = page.get_image_info()
            page_area = page.rect.width * page.rect.height or 1.0
            largest = 0.0
            for image in images:
                bbox = image.get("bbox") or [0, 0, 0, 0]
                area = max(0.0, (bbox[2] - bbox[0]) * (bbox[3] - bbox[1]))
                largest = max(largest, area / page_area)
            pages.append({
                "page": page.number + 1,
                "chars": chars,
                "cjk": cjk,
                "images": len(images),
                "largest_raster_share": round(largest, 3),
                "has_text": chars > 0,
            })
            if page.number < CONTAINMENT_PAGES:
                first_pages.append(text)
    return {
        "pages": page_count,
        "chars": total_chars,
        "cjk_chars": total_cjk,
        "chars_per_page": round(total_chars / page_count, 1) if page_count else None,
        "page_text": pages,
        "first_page_texts": first_pages,
    }


def reference_prefix(page_texts: list[str], target_chars: int) -> tuple[str, int, int]:
    """Leading reference pages whose characters reach `target_chars`.

    A backend that reads a fixed number of sentences cannot be compared against a whole
    document: the ratio and the n-gram containment would only measure the sentence cap.
    The comparison window is therefore the leading pages the backend's own output volume
    corresponds to (at most `CONTAINMENT_PAGES`).
    """
    used: list[str] = []
    total = 0
    for text in page_texts[:CONTAINMENT_PAGES]:
        used.append(text)
        total += len(squash(text))
        if total >= target_chars:
            break
    return "\n".join(used), len(used), total


# --------------------------------------------------------------------------------------------
# Backend runs (stdio MCP, exactly as scripts/pdf-probe.py does)
# --------------------------------------------------------------------------------------------

def backend_run(probe, binary: str, pdf_path: str, backend: str, layout_python: str,
                state_root: str, label: str, deadline: float) -> tuple[dict, list[str]]:
    import shutil

    record: dict = {
        "outcome": "error", "error_code": None, "error": None, "open_s": None,
        "peak_rss_kib": 0, "sections": None, "sentences": None, "sentences_per_page": None,
        "read_sentences": 0, "read_partial": False, "read_error": None,
    }
    texts: list[str] = []
    state_dir = os.path.join(state_root, f"{label}-{backend}")
    shutil.rmtree(state_dir, ignore_errors=True)
    os.makedirs(state_dir, exist_ok=True)
    env = probe.server_env(state_dir, os.path.dirname(os.path.abspath(pdf_path)), backend,
                           layout_python)
    server = probe.Server([binary], env, os.path.join(state_dir, "server.log"),
                          probe.TOOL_TIMEOUT_S)
    document_id = None
    try:
        server.initialize()
        data, open_s, error = probe.open_document(server, pdf_path)
        record["open_s"] = round(open_s, 3)
        if error is not None:
            record["error"] = str(error)[:500]
            record["error_code"] = error.code
        else:
            record["outcome"] = "ok"
            document_id = data.get("document_id")
        if document_id:
            sections = probe.read_sections(server, document_id)
            totals, totals_error = probe.sentence_totals(server, document_id, sections, deadline)
            sentences = sum(totals) if totals and not totals_error else None
            record["sections"] = len(sections)
            record["sentences"] = sentences
            record["read_error"] = totals_error
            pages = record_pages(pdf_path)
            if sentences is not None and pages:
                record["sentences_per_page"] = round(sentences / pages, 2)
            if time.monotonic() < deadline:
                read = probe.read_sentences(server, document_id, sections, READ_SENTENCES,
                                            deadline)
                texts = read["texts"]
                record["read_sentences"] = len(texts)
                record["read_partial"] = bool(read["partial"])
                if read["errors"]:
                    record["read_error"] = read["errors"][0][:200]
            else:
                record["read_error"] = "deadline before the read"
    except probe.ProbeError as exc:
        if record["open_s"] is None:
            record["error"] = str(exc)[:500]
            record["error_code"] = exc.code
        else:
            record["read_error"] = str(exc)[:200]
    finally:
        server.close()
    record["peak_rss_kib"] = server.peak_kib()
    record["server_rc"] = server.rc
    return record, texts


_PAGE_CACHE: dict[str, int] = {}


def record_pages(path: str) -> int | None:
    if path in _PAGE_CACHE:
        return _PAGE_CACHE[path]
    import pymupdf

    try:
        with pymupdf.open(path) as document:
            _PAGE_CACHE[path] = document.page_count
    except Exception:  # noqa: BLE001
        return None
    return _PAGE_CACHE[path]


# --------------------------------------------------------------------------------------------
# Layout refusal analysis
# --------------------------------------------------------------------------------------------

def worker_source() -> str:
    return os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                        "src", "parsing", "pdf_layout_worker.py")


def worker_direct(layout_python: str, pdf_path: str, timeout: float) -> dict:
    """Reproduce src/parsing/layout_pdf.rs's worker invocation, OCR disabled."""
    path = worker_source()
    with open(path, encoding="utf-8") as handle:
        source = handle.read()
    # src/infrastructure OCR_INTERPRETER_ARGS is ["-I", "-B"]; the worker source is
    # passed with -c exactly as layout_pdf.rs does.
    command = [layout_python, "-I", "-B", "-c", source,
               str(WORKER_MAX_PAGES), str(WORKER_MAX_BYTES), str(WORKER_MAX_CHARS),
               "", "", path]
    with open(pdf_path, "rb") as handle:
        raw = handle.read()
    started = time.perf_counter()
    try:
        proc = subprocess.run(command, input=raw, capture_output=True, timeout=timeout)
        return {
            "returncode": proc.returncode,
            "seconds": round(time.perf_counter() - started, 2),
            "stdout": proc.stdout.decode("utf-8", "replace")[-1500:],
            "stderr": proc.stderr.decode("utf-8", "replace")[-1500:],
        }
    except subprocess.TimeoutExpired:
        return {"returncode": None, "seconds": round(time.perf_counter() - started, 2),
                "stdout": "", "stderr": f"worker timed out after {timeout:.0f}s"}


def coverage_analysis(layout_python: str, pdf_path: str, timeout: float) -> dict:
    """Run `--coverage` inside the layout interpreter (imports the worker module)."""
    script = os.path.abspath(__file__)
    command = [layout_python, script, "--coverage", "--pdf", pdf_path,
               "--worker", worker_source(), "--out", os.devnull]
    try:
        proc = subprocess.run(command, capture_output=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return {"error": f"coverage analysis timed out after {timeout:.0f}s"}
    if proc.returncode != 0:
        return {"error": proc.stderr.decode("utf-8", "replace")[-1500:]}
    try:
        return json.loads(proc.stdout.decode("utf-8", "replace"))
    except json.JSONDecodeError:
        return {"error": "coverage analysis returned no JSON",
                "stdout": proc.stdout.decode("utf-8", "replace")[-500:]}


def load_worker_module(path: str):
    spec = importlib.util.spec_from_file_location("pdf_layout_worker_analysis", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def box_classes(page_layout: dict) -> list[tuple[str, list[float]]]:
    boxes = []
    for box in page_layout.get("boxes", []):
        name = box.get("boxclass")
        if name in ("image", "picture", "figure", "table"):
            boxes.append((name, [box.get(key) for key in ("x0", "y0", "x1", "y1")]))
    return boxes


def contains(outer: list[float], inner: list[float]) -> bool:
    return (outer[0] <= inner[0] and outer[1] <= inner[1]
            and inner[2] <= outer[2] and inner[3] <= outer[3])


def coverage_mode(args) -> int:
    """Per-page analysis of `require_disabled_page_coverage` for one PDF (layout venv)."""
    import pymupdf
    import pymupdf4llm

    worker = load_worker_module(args.worker)
    worker.OCR_CONFIG = {"enabled": False}
    worker.RASTER_BUDGET = worker.OcrRasterBudget()
    worker.PAGE_DEADLINE = None
    worker.INVOCATION_DEADLINE = None
    pymupdf4llm.use_layout(True)
    with open(args.pdf, "rb") as handle:
        raw = handle.read()
    result: dict = {"schema": "pdf1a-coverage/v1", "pdf": os.path.basename(args.pdf),
                    "pages": [], "first_failing_page": None, "first_reason": None,
                    "cumulative_raster_pixels": 0}
    with pymupdf.open(stream=raw, filetype="pdf") as document:
        layout = json.loads(pymupdf4llm.to_json(document, use_ocr=False))
        cumulative = 0
        for page, page_layout in zip(document, layout["pages"]):
            info: dict = {
                "page": page.number + 1,
                "image_count": 0,
                "has_native_body": None,
                "text_regions": None,
                "rotation": page.rotation,
                "page_points": [round(page.rect.width, 1), round(page.rect.height, 1)],
                "images": [],
                "visual_boxes": [],
                "reason": None,
                "analysis_error": None,
            }
            try:
                images = page.get_image_info()
                info["image_count"] = len(images)
                if not images:
                    # `require_disabled_page_coverage` returns before any predicate on a page
                    # without raster images, so such a page cannot raise a refusal.
                    result["pages"].append(info)
                    continue
                # The worker's own predicate; it raises TypeError when a layout box carries
                # `textlines: null`, which the worker would hit on the same page.
                info["has_native_body"] = worker.has_native_body(page_layout)
                info["text_regions"] = len(worker.native_text_regions(page_layout))
                boxes = box_classes(page_layout)
                info["visual_boxes"] = [name for name, _ in boxes]
                page_area = page.rect.width * page.rect.height or 1.0
                for image in images:
                    bbox = [float(value) for value in (image.get("bbox") or [0, 0, 0, 0])]
                    area = max(0.0, (bbox[2] - bbox[0]) * (bbox[3] - bbox[1]))
                    inside = None
                    for name, box in boxes:
                        if contains(box, bbox):
                            inside = name
                            break
                    info["images"].append({
                        "bbox": [round(value, 1) for value in bbox],
                        "area_share": round(area / page_area, 3),
                        "pixels": [image.get("width"), image.get("height")],
                        "inside_box": inside,
                        "has_transform": bool(image.get("transform")),
                    })
                info["images"].sort(key=lambda item: item["area_share"], reverse=True)
                if info["image_count"]:
                    pixels = worker.raster_pixel_count(page, 300)
                    cumulative += pixels
                    info["raster_pixels_at_300dpi"] = pixels
                    info["cumulative_raster_pixels"] = cumulative
                    try:
                        pixmap = worker.prepare_page_raster(page, dpi=300)
                        coverage = worker.native_raster_coverage(
                            page, pixmap, worker.native_text_regions(page_layout), dpi=300)
                        if coverage is None:
                            info["coverage"] = None
                        else:
                            total_samples = len(pixmap.samples)
                            before = coverage["uncovered_samples"]
                            masked = bytearray(pixmap.samples)
                            rectangles = [mask["pixel_bbox"] for mask in coverage["masks"]]
                            scale = 300 / 72
                            for _, bbox in boxes:
                                rectangles.append([math.ceil(bbox[0] * scale),
                                                   math.ceil(bbox[1] * scale),
                                                   math.floor(bbox[2] * scale),
                                                   math.floor(bbox[3] * scale)])
                            for x0, y0, x1, y1 in rectangles:
                                for row in range(int(y0), int(y1)):
                                    start = (row * pixmap.width + int(x0)) * 3
                                    masked[start:start + max(0, int(x1) - int(x0)) * 3] = \
                                        b"\xff" * (max(0, int(x1) - int(x0)) * 3)
                            after = len(masked) - masked.count(255)
                            info["coverage"] = {
                                "total_samples": total_samples,
                                "uncovered_samples": before,
                                "uncovered_share": round(before / total_samples, 5),
                                "masks": len(coverage["masks"]),
                                "uncovered_after_visual_boxes": after,
                                "box_explained_share": (
                                    round((before - after) / before, 4) if before else None),
                                "residual_page_share": round(after / total_samples, 6),
                            }
                            # Where does the residue live? Mask every vector drawing too: a
                            # residue that disappears is page furniture (rules, tinted boxes,
                            # rendered math), not a raster the rule could ever cover.
                            drawings = page.get_drawings()
                            info["drawings"] = len(drawings)
                            masked = bytearray(pixmap.samples)
                            for x0, y0, x1, y1 in rectangles:
                                for row in range(int(y0), int(y1)):
                                    start = (row * pixmap.width + int(x0)) * 3
                                    masked[start:start + max(0, int(x1) - int(x0)) * 3] = \
                                        b"\xff" * (max(0, int(x1) - int(x0)) * 3)
                            for drawing in drawings:
                                rect = drawing.get("rect")
                                if rect is None:
                                    continue
                                x0 = max(0, math.floor(rect.x0 * scale))
                                y0 = max(0, math.floor(rect.y0 * scale))
                                x1 = min(pixmap.width, math.ceil(rect.x1 * scale))
                                y1 = min(pixmap.height, math.ceil(rect.y1 * scale))
                                for row in range(int(y0), int(y1)):
                                    start = (row * pixmap.width + int(x0)) * 3
                                    masked[start:start + max(0, int(x1) - int(x0)) * 3] = \
                                        b"\xff" * (max(0, int(x1) - int(x0)) * 3)
                            info["coverage"]["residual_after_drawings"] = (
                                len(masked) - masked.count(255))
                            info["coverage"]["residual_after_drawings_page_share"] = round(
                                (len(masked) - masked.count(255)) / total_samples, 6)
                    except worker.OcrStageFailure as exc:
                        info["coverage"] = {"error": f"{exc.code}: {exc}"}
                    except Exception as exc:  # noqa: BLE001 - recorded, not fatal
                        info["coverage"] = {"error": f"{type(exc).__name__}: {exc}"}
                    # Fresh per-page raster budget: each page gets an independent verdict.
                    worker.RASTER_BUDGET = worker.OcrRasterBudget()
                    worker.PAGE_DEADLINE = None
                    try:
                        worker.require_disabled_page_coverage(page, page_layout)
                    except worker.OcrRequired as exc:
                        info["reason"] = str(exc)
                    except worker.OcrStageFailure as exc:
                        info["reason"] = f"{exc.code}: {exc}"
                    except Exception as exc:  # noqa: BLE001
                        info["reason"] = f"{type(exc).__name__}: {exc}"
            except Exception as exc:  # noqa: BLE001 - a page that cannot be analysed is data
                info["analysis_error"] = f"{type(exc).__name__}: {exc}"
                if info["reason"] is None:
                    info["reason"] = info["analysis_error"]
            if info["reason"] and result["first_failing_page"] is None:
                result["first_failing_page"] = info["page"]
                result["first_reason"] = info["reason"]
            result["pages"].append(info)
    result["cumulative_raster_pixels"] = cumulative
    print(json.dumps(result, ensure_ascii=False, separators=(",", ":")))
    if args.out and args.out != os.devnull:
        write_json(args.out, result)
    return 0


# --------------------------------------------------------------------------------------------
# Run mode
# --------------------------------------------------------------------------------------------

def download_paper(entry: dict, work_dir: str) -> tuple[str | None, dict]:
    """Download at CI time, re-checking the licence line on the abs page."""
    pdf_dir = os.path.join(work_dir, "pdfs")
    os.makedirs(pdf_dir, exist_ok=True)
    path = os.path.join(pdf_dir, f"{entry['id']}.pdf")
    status: dict = {"download": "ok", "error": None}
    marker = entry.get("license_marker") or license_marker(entry.get("license", ""))
    try:
        abs_page = fetch(ARXIV_ABS.format(id=entry["id"]), 120).decode("utf-8", "ignore")
        time.sleep(ARXIV_DELAY_S)
    except RuntimeError as exc:
        status.update({"download": "download_failed", "error": f"abs: {exc}"})
        return None, status
    if marker is None or marker not in abs_page:
        status.update({"download": "licence_mismatch",
                       "error": f"marker {marker!r} not on the abs page"})
        return None, status
    if os.path.isfile(path) and os.path.getsize(path) > 0:
        return path, status
    last: str | None = None
    for attempt in range(3):
        try:
            data = fetch(ARXIV_PDF.format(id=entry["id"]), 300, attempts=1)
            if not data.startswith(b"%PDF"):
                raise RuntimeError("response is not a PDF")
            with open(path, "wb") as handle:
                handle.write(data)
            time.sleep(ARXIV_DELAY_S)
            return path, status
        except Exception as exc:  # noqa: BLE001 - retried below
            last = f"{type(exc).__name__}: {exc}"
            time.sleep(5 * (attempt + 1))
    status.update({"download": "download_failed", "error": last})
    return None, status


def run_shard(args) -> int:
    probe = load_probe()
    entries = load_corpus(args.corpus)
    if not entries:
        print("::error::corpus list is empty")
        return 1
    shard = [entry for index, entry in enumerate(entries) if index % args.shards == args.shard]
    print(f"shard {args.shard}/{args.shards}: {len(shard)} papers", flush=True)
    os.makedirs(args.state_root, exist_ok=True)
    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)
    shard_deadline = time.monotonic() + args.deadline_minutes * 60
    for entry in shard:
        if time.monotonic() > shard_deadline:
            record = {"kind_record": "paper", "id": entry["id"], "group": entry["group"],
                      "title": entry["title"], "pages": entry["pages"],
                      "outcome": "not_run", "error": "shard deadline"}
            append_jsonl(args.out, record)
            print(f"| {entry['id']} | - | not_run (shard deadline) |", flush=True)
            continue
        paper_deadline = min(shard_deadline, time.monotonic() + args.paper_deadline_minutes * 60)
        record = process_paper(probe, entry, args, paper_deadline)
        append_jsonl(args.out, record)
        for coverage in record.pop("_coverage_records", []):
            append_jsonl(args.out, coverage)
        print(paper_line(record), flush=True)
    return 0


def append_jsonl(path: str, record: dict) -> None:
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, ensure_ascii=False) + "\n")


def process_paper(probe, entry: dict, args, deadline: float) -> dict:
    label = entry["id"]
    record: dict = {
        "kind_record": "paper", "id": label, "title": entry["title"],
        "group": entry["group"], "category": entry.get("category"),
        "pages": entry.get("pages"), "license": entry.get("license"),
        "outcome": "ok", "download": None, "error": None,
        "reference": None, "default": None, "layout": None,
        "layout_refusal": None,
    }
    started = time.perf_counter()
    pdf_path, download_status = download_paper(entry, args.work_dir)
    record["download"] = download_status["download"]
    if pdf_path is None:
        record.update({"outcome": "download_failed", "error": download_status["error"]})
        return record
    reference = reference_metrics(pdf_path)
    first_page_texts = reference.pop("first_page_texts")
    record["pages"] = reference["pages"]
    record["reference"] = reference
    default_record, default_texts = backend_run(
        probe, args.binary, pdf_path, "default", "", args.state_root, label, deadline)
    layout_record, layout_texts = backend_run(
        probe, args.binary, pdf_path, "layout", args.layout_python, args.state_root, label,
        deadline)
    record["default"] = decorate(default_record, default_texts, reference, first_page_texts)
    record["layout"] = decorate(layout_record, layout_texts, reference, first_page_texts)
    record["seconds"] = round(time.perf_counter() - started, 1)

    coverage_records: list[dict] = []
    if layout_record["outcome"] != "ok" and layout_record["error_code"] == "OCR_REQUIRED":
        direct = worker_direct(args.layout_python, pdf_path, args.worker_timeout)
        coverage = coverage_analysis(args.layout_python, pdf_path, args.worker_timeout)
        record["layout_refusal"] = {
            "worker": direct,
            "first_failing_page": coverage.get("first_failing_page"),
            "first_reason": coverage.get("first_reason"),
            "cumulative_raster_pixels": coverage.get("cumulative_raster_pixels"),
            "error": coverage.get("error"),
        }
        coverage_records.append({
            "kind_record": "coverage", "id": label, "group": entry["group"],
            "pages": entry.get("pages"), "coverage": coverage,
        })
    record["_coverage_records"] = coverage_records
    return record


def decorate(backend: dict, texts: list[str], reference: dict,
             first_page_texts: list[str]) -> dict:
    stats = text_stats(texts)
    result = dict(backend)
    result.update(stats)
    joined = " ".join(texts)
    prefix, pages_used, prefix_chars = reference_prefix(first_page_texts, stats["chars"])
    result["reference_pages_used"] = pages_used
    result["reference_chars_used"] = prefix_chars
    result["char_ratio"] = (round(stats["chars"] / prefix_chars, 3) if prefix_chars else None)
    result["containment"] = gram_containment(prefix, joined)
    result["containment_first20"] = gram_containment("\n".join(first_page_texts), joined)
    # CJK recall against the same leading-page window: the sentence cap must not be
    # mistaken for lost CJK text.
    window_cjk = sum((page.get("cjk") or 0) for page in reference["page_text"][:pages_used])
    result["reference_cjk_window"] = window_cjk
    result["cjk_recall"] = (round(stats["cjk_chars"] / window_cjk, 3) if window_cjk else None)
    return result


def paper_line(record: dict) -> str:
    def cell(backend):
        if not backend:
            return "-"
        if backend["outcome"] != "ok":
            return f"{backend['error_code'] or backend['outcome']}"
        return (f"{backend['open_s']}s sp={backend['sentences_per_page']} "
                f"cr={backend['char_ratio']} c={backend['containment']}")
    return (f"| {record['id']} | {record.get('pages')} | {record.get('download')} | "
            f"{cell(record.get('default'))} | {cell(record.get('layout'))} |")


# --------------------------------------------------------------------------------------------
# Aggregate
# --------------------------------------------------------------------------------------------

def load_records(jsonl_dir: str) -> list[dict]:
    records: list[dict] = []
    for name in sorted(os.listdir(jsonl_dir)):
        if not name.endswith(".jsonl"):
            continue
        with open(os.path.join(jsonl_dir, name), encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if line:
                    try:
                        records.append(json.loads(line))
                    except json.JSONDecodeError:
                        continue
    return records


def fmt(value, digits: int = 2) -> str:
    if value is None:
        return "-"
    if isinstance(value, bool):
        return "yes" if value else "no"
    if isinstance(value, float):
        return f"{value:.{digits}f}"
    return str(value)


def detector_rules(paper: dict) -> dict:
    reference = paper.get("reference") or {}
    default = paper.get("default") or {}
    ref_chars = reference.get("chars") or 0
    ref_cjk = reference.get("cjk_chars") or 0
    ref_per_page = reference.get("chars_per_page") or 0
    chars = default.get("chars") or 0
    nonprintable = default.get("nonprintable_share")
    cjk_share = default.get("cjk_share")
    sentences_per_page = default.get("sentences_per_page")
    # Rule (a) uses the aligned window ratio (`char_ratio`): a fixed 200-sentence read
    # compared against a whole document would only measure the sentence cap.
    ratio = default.get("char_ratio")
    return {
        "a_char_ratio": (ratio is not None and ratio < 0.5),
        "b_sentences_per_page": (sentences_per_page is not None and sentences_per_page < 3
                                 and ref_per_page > 500),
        "c_nonprintable": (nonprintable is not None and nonprintable > 0.01),
        "d_cjk_share": (cjk_share is not None and cjk_share < 0.05 and ref_cjk >= 200),
    }


def evaluate(rule: dict[str, bool], label: bool) -> str:
    positive = any(rule.values())
    if positive and label:
        return "tp"
    if positive and not label:
        return "fp"
    if not positive and label:
        return "fn"
    return "tn"


def detector_evaluation(papers: list[dict], threshold: float) -> tuple[list[str], dict]:
    """TP/FP/FN for every single rule and every OR-pair, against one containment label."""
    labelled = [paper for paper in papers if (paper.get("reference") or {}).get("chars")]
    names = ["a_char_ratio", "b_sentences_per_page", "c_nonprintable", "d_cjk_share"]
    combos = [(name,) for name in names] + list(itertools.combinations(names, 2))
    rows = ["| Rule | TP | FP | FN | TN | Errors |", "| --- | --- | --- | --- | --- | --- |"]
    result: dict = {"threshold": threshold, "papers": len(labelled), "rules": {}}
    for combo in combos:
        counts: collections.Counter = collections.Counter()
        for paper in labelled:
            rules = detector_rules(paper)
            label = ((paper.get("default") or {}).get("containment") or 0.0) < threshold
            counts[evaluate({"x": any(rules[name] for name in combo)}, label)] += 1
        key = "+".join(combo)
        entry = {"tp": counts.get("tp", 0), "fp": counts.get("fp", 0),
                 "fn": counts.get("fn", 0), "tn": counts.get("tn", 0)}
        entry["errors"] = entry["fp"] + entry["fn"]
        result["rules"][key] = entry
        rows.append(f"| {key} | {entry['tp']} | {entry['fp']} | {entry['fn']} | "
                    f"{entry['tn']} | {entry['errors']} |")
    return rows, result


def aggregate_mode(args) -> int:
    if not os.path.isdir(args.jsonl_dir):
        print(f"::error::no shard results in {args.jsonl_dir}")
        return 1
    records = load_records(args.jsonl_dir)
    papers = [record for record in records if record.get("kind_record") == "paper"]
    if not papers:
        print(f"::error::no paper records in {args.jsonl_dir}")
        return 1
    coverages = {record["id"]: record.get("coverage") or {} for record in records
                 if record.get("kind_record") == "coverage"}
    papers.sort(key=lambda item: (item.get("group") or "", item.get("id") or ""))

    # 1. per-paper table
    table = ["| Paper | Group | Pages | Backend | Outcome | Open s | Peak RSS MiB | Sections | "
             "Sentences | Sent/page | Chars | Ref window (pages/chars) | Char ratio vs ref | "
             "5-gram containment | Non-printable | CJK share |",
             "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | "
             "--- | --- | --- |"]
    for paper in papers:
        for backend in ("default", "layout"):
            result = paper.get(backend) or {}
            if not result:
                table.append(f"| {paper['id']} | {paper.get('group')} | {paper.get('pages')} | "
                             f"{backend} | not run | - | - | - | - | - | - | - | - | - | - | - |")
                continue
            table.append(
                f"| {paper['id']} | {paper.get('group')} | {paper.get('pages')} | {backend} | "
                f"{result.get('error_code') or result.get('outcome')} | "
                f"{fmt(result.get('open_s'))} | "
                f"{fmt((result.get('peak_rss_kib') or 0) / 1024, 1)} | "
                f"{fmt(result.get('sections'))} | {fmt(result.get('sentences'))} | "
                f"{fmt(result.get('sentences_per_page'))} | {fmt(result.get('chars'))} | "
                f"{fmt(result.get('reference_pages_used'))}/{fmt(result.get('reference_chars_used'))} | "
                f"{fmt(result.get('char_ratio'), 3)} | {fmt(result.get('containment'), 3)} | "
                f"{fmt(result.get('nonprintable_share'), 3)} | "
                f"{fmt(result.get('cjk_share'), 3)} |")

    # 2. refusals
    refused = [paper for paper in papers
               if (paper.get("layout") or {}).get("error_code") == "OCR_REQUIRED"]
    other_fail = [paper for paper in papers if paper.get("layout")
                  and (paper["layout"].get("outcome") != "ok")
                  and paper["layout"].get("error_code") != "OCR_REQUIRED"]
    by_group: dict[str, list[int]] = {}
    by_reason: collections.Counter = collections.Counter()
    refusal_rows = ["| Paper | Group | Pages | Failing page(s) | First reason | Raster on the first "
                    "failing page | Uncovered share | Box explained | Residual share | After "
                    "drawings | Drawings | Verdict |",
                    "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    inside_box = 0
    outside_box = 0
    edges_only = 0
    refusal_details = []
    for paper in refused:
        coverage = coverages.get(paper["id"]) or {}
        pages_info = coverage.get("pages") or []
        failing = [info for info in pages_info if info.get("reason")]
        first = failing[0] if failing else None
        reason = (first or {}).get("reason") or coverage.get("first_reason") \
            or (paper.get("layout_refusal") or {}).get("first_reason") or "unknown"
        by_reason[reason] += 1
        group = paper.get("group") or "?"
        counts = by_group.setdefault(group, [0, 0])
        counts[0] += 1
        top = (first or {}).get("images") or []
        top = top[0] if top else None
        description = "-"
        if top:
            description = (f"{first['image_count']} raster(s), largest "
                           f"{top['area_share'] * 100:.0f}% of page"
                           + (f" inside {top['inside_box']} box" if top.get("inside_box") else "")
                           + (" (transformed)" if top.get("has_transform") else ""))
        coverage_info = (first or {}).get("coverage") or {}
        explained = coverage_info.get("box_explained_share")
        residual = coverage_info.get("residual_page_share")
        verdict = "not explained by any box"
        if top and top.get("inside_box") and residual is not None and residual <= 0.005:
            verdict = "raster inside a retained picture/figure box"
            inside_box += 1
        elif (explained or 0) >= 0.99 and residual is not None and residual <= 0.005:
            verdict = "residue only at box edges (no box contains the raster)"
            edges_only += 1
        else:
            outside_box += 1
        refusal_details.append({
            "id": paper["id"], "group": group, "pages": paper.get("pages"),
            "failing_pages": [info["page"] for info in failing],
            "reason": reason, "top_raster": top,
            "uncovered_share": coverage_info.get("uncovered_share"),
            "box_explained_share": explained, "verdict": verdict,
            "residual_page_share": residual,
            "residual_after_drawings_page_share":
                coverage_info.get("residual_after_drawings_page_share"),
            "drawings": (first or {}).get("drawings"),
            "has_native_body": (first or {}).get("has_native_body"),
            "visual_boxes": (first or {}).get("visual_boxes"),
        })
        refusal_rows.append(
            f"| {paper['id']} | {group} | {paper.get('pages')} | "
            f"{','.join(str(info['page']) for info in failing) or '-'} | {reason} | "
            f"{description} | {fmt(coverage_info.get('uncovered_share'), 3)} | "
            f"{fmt(explained, 3)} | {fmt(residual, 4)} | "
            f"{fmt(coverage_info.get('residual_after_drawings_page_share'), 4)} | "
            f"{fmt((first or {}).get('drawings'))} | {verdict} |")
    for group in by_group:
        by_group[group][1] = len([paper for paper in papers if paper.get("group") == group])
    group_rows = ["| Group | Papers | Layout refused | Rate |",
                  "| --- | --- | --- | --- |"]
    for group in sorted(by_group):
        refused_count, total = by_group[group]
        rate = f"{refused_count / total:.2f}" if total else "-"
        group_rows.append(f"| {group} | {total} | {refused_count} | {rate} |")
    reason_rows = ["| Reason | Papers |", "| --- | --- |"]
    for reason, count in by_reason.most_common():
        reason_rows.append(f"| {reason} | {count} |")

    # 3. detectors
    detector_rows, primary = detector_evaluation(papers, 0.5)
    sensitivity_rows, sensitivity = detector_evaluation(papers, 0.8)
    per_paper_rules = ["| Paper | Ref CJK | Default CJK | Ref chars/page | Default "
                       "containment | Default char ratio | a | b | c | d |",
                       "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for paper in papers:
        reference = paper.get("reference") or {}
        if not reference.get("chars"):
            continue
        rules = detector_rules(paper)
        default = paper.get("default") or {}
        per_paper_rules.append(
            f"| {paper['id']} | {fmt(reference.get('cjk_chars'))} | "
            f"{fmt(default.get('cjk_chars'))} | "
            f"{fmt(reference.get('chars_per_page'), 0)} | "
            f"{fmt(default.get('containment'), 3)} | "
            f"{fmt(default.get('char_ratio'), 3)} | "
            f"{fmt(rules['a_char_ratio'])} | {fmt(rules['b_sentences_per_page'])} | "
            f"{fmt(rules['c_nonprintable'])} | {fmt(rules['d_cjk_share'])} |")

    # 3b. CJK papers: how much of the reference CJK text each backend returns
    cjk_rows = ["| Paper | Reference CJK chars | Default CJK chars | Default recall | "
                "Layout CJK chars | Layout recall |",
                "| --- | --- | --- | --- | --- | --- |"]
    for paper in papers:
        reference = paper.get("reference") or {}
        if (reference.get("cjk_chars") or 0) < 200:
            continue
        default = paper.get("default") or {}
        layout = paper.get("layout") or {}
        default_cjk = default.get("cjk_chars")
        layout_cjk = layout.get("cjk_chars")
        cjk_rows.append(
            f"| {paper['id']} | {reference['cjk_chars']} | {fmt(default_cjk)} | "
            f"{fmt(default.get('cjk_recall'), 3)} | {fmt(layout_cjk)} | "
            f"{fmt(layout.get('cjk_recall'), 3)} |")

    # 4. open time and RSS
    def times(backend: str) -> list[float]:
        return [paper[backend]["open_s"] for paper in papers
                if paper.get(backend) and paper[backend].get("open_s") is not None]

    def rss(backend: str) -> list[float]:
        return [(paper[backend].get("peak_rss_kib") or 0) / 1024 for paper in papers
                if paper.get(backend) and paper[backend].get("peak_rss_kib")]

    timing_rows = ["| Backend | Opens | Median s | p95 s | Worst s | Median peak RSS MiB | "
                   "Worst peak RSS MiB |",
                   "| --- | --- | --- | --- | --- | --- | --- |"]
    timing = {}
    for backend in ("default", "layout"):
        open_times = times(backend)
        memory = rss(backend)
        timing[backend] = {
            "opens": len(open_times),
            "median_s": round(statistics.median(open_times), 2) if open_times else None,
            "p95_s": percentile(open_times, 0.95),
            "worst_s": max(open_times) if open_times else None,
            "median_rss_mib": round(statistics.median(memory), 1) if memory else None,
            "worst_rss_mib": round(max(memory), 1) if memory else None,
        }
        row = timing[backend]
        timing_rows.append(
            f"| {backend} | {row['opens']} | {fmt(row['median_s'])} | {fmt(row['p95_s'])} | "
            f"{fmt(row['worst_s'])} | {fmt(row['median_rss_mib'], 1)} | "
            f"{fmt(row['worst_rss_mib'], 1)} |")

    summary = {
        "papers": len(papers),
        "papers_ok": len([paper for paper in papers if paper.get("outcome") == "ok"]),
        "download_failed": [paper["id"] for paper in papers
                            if paper.get("outcome") == "download_failed"],
        "not_run": [paper["id"] for paper in papers if paper.get("outcome") == "not_run"],
        "layout_refused": len(refused),
        "layout_ok": len([paper for paper in papers
                          if (paper.get("layout") or {}).get("outcome") == "ok"]),
        "layout_other_failures": [(paper["id"], paper["layout"].get("error_code"))
                                  for paper in other_fail],
        "refusal_reasons": dict(by_reason),
        "refused_inside_picture_box": inside_box,
        "refused_box_edges_only": edges_only,
        "refused_outside_any_box": outside_box,
        "refusal_details": refusal_details,
        "detectors": {"primary": primary, "sensitivity": sensitivity},
        "timing": timing,
        "corpus": [{"id": paper["id"], "group": paper.get("group"),
                    "pages": paper.get("pages")} for paper in papers],
    }
    text = "\n".join([
        "# PDF-1a corpus results", "",
        "## Per-paper outcome and text quality", "",
        "The reference window is the leading reference pages whose characters reach the "
        "backend's own output volume (at most 20 pages); comparing a 200-sentence read against "
        "a whole document would only measure the sentence cap.", "",
        *table, "",
        "## Layout refusals", "", *group_rows, "", *reason_rows, "", *refusal_rows, "",
        "## Detector candidates (label: default 5-gram containment < 0.5)", "",
        *detector_rows, "",
        "## Detector candidates (sensitivity: containment < 0.8)", "",
        *sensitivity_rows, "",
        "## Rule flags per paper", "", *per_paper_rules, "",
        "## CJK papers", "", *cjk_rows, "",
        "## Open time and peak RSS", "", *timing_rows, "",
    ])
    print(text)
    if args.markdown:
        with open(args.markdown, "w", encoding="utf-8") as handle:
            handle.write(text + "\n")
    if args.out:
        write_json(args.out, summary)
    return 0


# --------------------------------------------------------------------------------------------
# Entry point
# --------------------------------------------------------------------------------------------

def main() -> int:
    parser = argparse.ArgumentParser(description="PDF-1a real-paper corpus.")
    parser.add_argument("--select", action="store_true", help="build the corpus list")
    parser.add_argument("--coverage", action="store_true",
                        help="per-page coverage analysis (layout interpreter)")
    parser.add_argument("--aggregate", action="store_true", help="merge shards into tables")
    parser.add_argument("--corpus", default="scripts/pdf-corpus-ids.txt")
    parser.add_argument("--out", default="")
    parser.add_argument("--markdown", default="")
    parser.add_argument("--jsonl-dir", default="")
    parser.add_argument("--work-dir", default=".tmp/pdf-corpus")
    parser.add_argument("--binary", default="")
    parser.add_argument("--layout-python", default="")
    parser.add_argument("--state-root", default=".tmp/pdf-corpus/state")
    parser.add_argument("--shard", type=int, default=0)
    parser.add_argument("--shards", type=int, default=1)
    parser.add_argument("--deadline-minutes", type=float, default=75.0)
    parser.add_argument("--paper-deadline-minutes", type=float, default=6.0)
    parser.add_argument("--worker-timeout", type=float, default=900.0)
    parser.add_argument("--pdf", default="")
    parser.add_argument("--worker", default="")
    args = parser.parse_args()
    if args.coverage:
        if not args.pdf or not args.worker:
            parser.error("--coverage needs --pdf and --worker")
        return coverage_mode(args)
    if args.select:
        if not args.out:
            parser.error("--select needs --out")
        return select_corpus(args)
    if args.aggregate:
        if not args.jsonl_dir:
            parser.error("--aggregate needs --jsonl-dir")
        return aggregate_mode(args)
    if not args.binary or not args.out:
        parser.error("run mode needs --binary and --out")
    if not args.layout_python:
        parser.error("run mode needs --layout-python")
    return run_shard(args)


if __name__ == "__main__":
    sys.exit(main())
