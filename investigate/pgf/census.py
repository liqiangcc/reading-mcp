#!/usr/bin/env python3
"""pgf: page furniture (running headers, footers, page numbers, arXiv stamps, review line
numbers) inside the text a reader gets from a PDF.

For every input this census answers three questions with one run:

1. which furniture lines the PDF has (from a PyMuPDF page/line/position reference), which of
   them survive into the reader's paragraph and sentence stream, how they land there
   (`own_unit`, `prefix`, `suffix`, `inside`, `absent`), and how many sentences they cut in two;
2. what each detection option finds, measured against the positional reference: the
   positional band test, the repetition test, the page-number pattern test, and the cheap
   pass a fix can run today on the extractor's own per-page chunks (no positions at all);
3. what removing the furniture would change: the coarse-paragraph share before and after the
   furniture text is stripped out of each paragraph, judged by the same guard port the server
   runs (`scripts/pdf-guard-events.py`, `scripts/pdf6f-sentence-price.py`).

    python3 investigate/pgf/census.py --binary target/release/reading-mcp \
        --state-dir .tmp/pgf/state --sources .tmp/pgf/sources.txt \
        --cache-dir .tmp/pgf/inputs --out .tmp/pgf/census

`--pymupdf-python` names an interpreter that has PyMuPDF (this one is used if it imports it
already); without one the run falls back to text-only mode, where the furniture is detected from
the reader's own units by repetition and pattern only, and the positional columns are blank.

The sources file is one input per line: a path to a PDF, a bare arXiv id, `arxiv:<id>`, an
http(s) URL, or a JSON object line with an `id` (the shape of `scripts/pdf-sweep-ids.txt`).
Blank lines and `#` comments are skipped, downloads are cached under `--cache-dir`.

Standard library only, plus PyMuPDF for the reference.
"""

from __future__ import annotations

import argparse
import bisect
import collections
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.normpath(os.path.join(HERE, "..", "..", "scripts"))
ARXIV_PDF = "https://arxiv.org/pdf/{id}"
USER_AGENT = "reading-mcp-bench-pgf"
FETCH_DELAY_S = 2.0
MAX_PDF_BYTES = 134217728
OPEN_TIMEOUT_S = 300.0
DOC_DEADLINE_S = 3600.0
HEAD_CHARS = 120

# --- furniture thresholds (the PDF-7b-A design's numbers where one exists) -------------------
BAND_SHARE = 0.10        # a header/footer line's baseline sits in the top/bottom 10% of the text
REPEAT_PAGE_SHARE = 0.30  # a running header repeats on >= 30% of the pages (pdf7b-a §3.5)
PAGENUM_PAGE_SHARE = 0.50  # a page-number line on >= 50% of the pages (pdf6a)
MIN_REPEAT_PAGES = 3
MIN_FURNITURE_CHARS = 2   # a 1-character line matches inside every number on the page
MIN_REPEAT_CHARS = 8      # a repeated line shorter than this is a table cell or a heading word
ALIGN_BACKTRACK = 2000    # how far before the cursor a paragraph's start may still be found
EDGE_LINES = 2            # the cheap pass's band proxy: the first/last two chunks of a page
LINE_NUMBER_PAGE_MIN = 5  # a review copy's line numbers: >= 5 increasing digit lines per page
LINE_NUMBER_LEFT_SHARE = 0.20
LINE_NUMBER_X_TOLERANCE = 6.0  # pt: a line-number column has one x, a table's rows do not
TERMINAL = ".!?:;。！？"
ARXIV_STAMP_RE = re.compile(r"arXiv:\s*\d{4}\.\d{4,5}v\d+")
PAGE_NUM_RES = (
    re.compile(r"\A\d{1,4}\Z"),
    re.compile(r"\A(?:page\s*)?\d{1,4}\s*(?:of|/)\s*\d{1,4}\Z", re.I),
    re.compile(r"\A[-–—]\s*\d{1,4}\s*[-–—]\Z"),
)
DIGIT_RUN_RE = re.compile(r"\d+")
WS_RE = re.compile(r"\s+")
# A broken font encoding maps a space (or a period) to U+0000 in the reader's text
# (`S\x00Kotlyk\x00MODERN`), so both the normalised and the squashed comparison drop control
# characters: the reference has a space or a period where the reader's text has U+0000.
CONTROL_RE = re.compile(r"[\x00-\x08\x0b\x0c\x0e-\x1f]")
# A cheap-pass page number: digits at a unit edge, followed (or preceded) by a lower-case word.
EDGE_PAGENUM_RES = (
    re.compile(r"\A(\d{1,4})\s+(?=[a-z(\[])", re.U),
    re.compile(r"(?<=[a-z,;)])\s(\d{1,4})\Z", re.U),
    re.compile(r"\A(\d{1,4})\s*[|/]\s*\d{1,4}\s+"),
    re.compile(r"(?:page\s*)?\d{1,4}\s*(?:of|/)\s*\d{1,4}", re.I),
)
# A repeated edge token run that is furniture rather than a prose opener ("In this paper,").
NON_PROSE_TOKEN_RE = re.compile(r"[\d\[\]()|/§•·]|^[A-Z][A-Z&.\-]+$")

REASON_DOUBLED = "corruption_doubled_token"
REASON_BIBLIOGRAPHIC = "corruption_bibliographic_run"
REASON_GLUE = "glue_missing_space_sentence"
REASON_CLASS = "content_class"


# --------------------------------------------------------------------------------------------
# Text helpers
# --------------------------------------------------------------------------------------------

def squash(text: str) -> str:
    return WS_RE.sub("", CONTROL_RE.sub("", text))


def fold(text: str) -> tuple[str, list[int]]:
    """The letters and digits of an already squashed text, with the squash offset of each.

    A furniture line's punctuation can be missing from the reader's text (a font whose period
    glyph decodes to U+0000), so the comparison is on letters and digits only; the map keeps the
    position usable for the sentence test.
    """
    out: list[str] = []
    index: list[int] = []
    for offset, char in enumerate(text):
        if char.isalnum():
            out.append(char)
            index.append(offset)
    return "".join(out), index


def norm(text: str) -> str:
    return WS_RE.sub(" ", CONTROL_RE.sub(" ", text)).strip()


def mask_digits(text: str) -> str:
    return DIGIT_RUN_RE.sub("#", text)


def ends_terminal(text: str) -> bool:
    stripped = text.rstrip()
    return bool(stripped) and stripped[-1] in TERMINAL


def head(text: str, limit: int = HEAD_CHARS) -> str:
    return norm(text)[:limit]


# --------------------------------------------------------------------------------------------
# Sibling ports (never edited)
# --------------------------------------------------------------------------------------------

def load_sibling(name: str):
    path = os.path.join(SCRIPTS, name)
    spec = importlib.util.spec_from_file_location(name.replace("-", "_")[:-3], path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


GE = load_sibling("pdf-guard-events.py")        # the junction, doubled and bibliographic rules
SP = load_sibling("pdf6f-sentence-price.py")    # the PDF-profile verdict and sentence_ranges
PROBE = load_sibling("pdf-probe.py")            # the stdio MCP client


def pdf_exempt(event: dict, context=None) -> bool:
    return GE.BASELINE(event) or GE.IMPLEMENTED(event, context)


def kinds_of(text: str, exempt=None, context=None) -> tuple[dict, int]:
    verdict = GE.guard_verdict(text, exempt, context)
    return dict(verdict["kinds"]), verdict["glue_events"]


def repeated_context(text: str) -> dict:
    return {"tokens": GE.token_counts(text)}


# --- the bibliographic rule, ported from `bibliographic_marker_counts` ------------------------
MONTH_PREFIXES = ("jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct",
                  "nov", "dec")
VOLUME_WORDS = ("vol", "volume", "no", "number", "issue", "series", "pp", "p", "pages")
RIGHTS_WORDS = ("copyright", "isbn", "issn")


def alphanumeric_words(text: str) -> list[str]:
    words: list[str] = []
    current: list[str] = []
    for ch in text:
        if ch.isalnum():
            current.append(ch)
        elif current:
            words.append("".join(current))
            current = []
    if current:
        words.append("".join(current))
    return words


def ascii_digit_first(word: str) -> bool:
    return bool(word) and "0" <= word[0] <= "9"


def is_month_word(word: str) -> bool:
    lowered = word.lower()
    return len(lowered) >= 3 and lowered[:3] in MONTH_PREFIXES


def nearby_number(words: list[str], index: int) -> bool:
    for offset in (-1, 1):
        other = index + offset
        if 0 <= other < len(words) and ascii_digit_first(words[other]):
            return True
    return False


def bibliographic_markers(text: str) -> int:
    words = alphanumeric_words(text)
    counts = []
    for index, word in enumerate(words):
        lowered = word.lower()
        if is_month_word(word) and nearby_number(words, index):
            counts.append(1)
        elif lowered in VOLUME_WORDS and nearby_number(words, index):
            counts.append(1)
        elif lowered in RIGHTS_WORDS:
            counts.append(1)
        elif ascii_digit_first(word) and len(word) == 4 and word.isdigit():
            counts.append(1)
        else:
            counts.append(0)
    window = min(GE.BIBLIOGRAPHIC_WINDOW_WORDS, len(counts))
    best = sum(counts[:window])
    current = best
    for index in range(window, len(counts)):
        current += counts[index] - counts[index - window]
        best = max(best, current)
    return best


def paragraph_reason(text: str) -> dict:
    """The guard's verdict on one paragraph: doubled, bibliographic, then the glue count."""
    doubled = GE.doubled_token(text)
    if doubled is not None:
        return {"reason": REASON_DOUBLED, "evidence": {"token": doubled[:60]}}
    markers = bibliographic_markers(text)
    if markers >= GE.MIN_BIBLIOGRAPHIC_MARKERS:
        return {"reason": REASON_BIBLIOGRAPHIC, "evidence": {"window_markers": markers}}
    bare_kinds, bare_events = kinds_of(text)
    if bare_events < GE.MIN_MISSING_SPACE_GLUE_EVENTS:
        return {"reason": "unexplained", "evidence": {"bare_events": bare_events}}
    context = repeated_context(text)
    kinds, events = kinds_of(text, pdf_exempt, context)
    if events < GE.MIN_MISSING_SPACE_GLUE_EVENTS:
        return {"reason": "unexplained", "evidence": {"bare_events": bare_events,
                                                      "exempted_events": events}}
    pieces = SP.sentence_ranges(text)
    for start, end in pieces:
        piece = text[start:end]
        sentence_kinds, sentence_events = kinds_of(piece, pdf_exempt, context)
        if sentence_events >= GE.MIN_MISSING_SPACE_GLUE_EVENTS:
            return {"reason": REASON_GLUE,
                    "evidence": {"kinds": kinds, "events": events, "bare_events": bare_events,
                                 "sentences": len(pieces)}}
    return {"reason": "unexplained", "evidence": {"paragraph_events": events,
                                                  "sentences": len(pieces)}}


# --------------------------------------------------------------------------------------------
# Inputs
# --------------------------------------------------------------------------------------------

ARXIV_ID_RE = re.compile(r"\A\d{4}\.\d{4,5}(v\d+)?\Z")


def parse_sources(path: str) -> list[dict]:
    entries: list[dict] = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            entry: dict = {}
            if line.startswith("{"):
                entry = json.loads(line)
                source = entry.get("source") or entry.get("id")
                if not source:
                    continue
                entry["source"] = source
            else:
                source = line
                if line.startswith("arxiv:"):
                    source = line.split(":", 1)[1]
                entry = {"source": source}
            entries.append(entry)
    return entries


def resolve(entry: dict, cache_dir: str) -> tuple[str | None, str | None]:
    """The local path of one input, downloading arXiv ids and URLs into the cache."""
    source = str(entry["source"])
    if os.path.isfile(source):
        return os.path.abspath(source), None
    label = entry.get("id") or os.path.basename(source)
    safe = re.sub(r"[^A-Za-z0-9._-]+", "_", label)
    path = os.path.join(cache_dir, safe + ".pdf")
    if os.path.isfile(path) and os.path.getsize(path) > 1024:
        return path, None
    if source.startswith("http://") or source.startswith("https://"):
        url = source
    elif ARXIV_ID_RE.match(source):
        url = ARXIV_PDF.format(id=source)
    else:
        return None, f"cannot resolve source {source!r}"
    try:
        request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
        with urllib.request.urlopen(request, timeout=180) as response:
            data = response.read(MAX_PDF_BYTES)
    except Exception as exc:  # noqa: BLE001 - the record keeps the reason
        return None, f"download failed: {exc}"
    if not data.startswith(b"%PDF"):
        return None, "download is not a PDF"
    os.makedirs(cache_dir, exist_ok=True)
    with open(path, "wb") as handle:
        handle.write(data)
    time.sleep(FETCH_DELAY_S)
    return path, None


# --------------------------------------------------------------------------------------------
# The reference: page, line, position (PyMuPDF)
# --------------------------------------------------------------------------------------------

# The worker `--pymupdf-python` runs when this interpreter has no PyMuPDF: the same page/line
# extraction, JSON on stdout.
PYMUPDF_WORKER = r'''
import json, sys
import pymupdf
pages = []
with pymupdf.open(sys.argv[1]) as document:
    for page in document:
        lines = []
        for block in page.get_text("dict").get("blocks", []):
            if block.get("type") != 0:
                continue
            for line in block.get("lines", []):
                text = "".join(span.get("text", "") for span in line.get("spans", []))
                if not text.strip():
                    continue
                x0, y0, x1, y1 = line["bbox"]
                size = max((span.get("size", 0.0) for span in line.get("spans", [])),
                           default=0.0)
                lines.append({"text": text, "x0": x0, "y0": y0, "x1": x1, "y1": y1,
                              "size": size,
                              "rotated": list(line.get("dir", (1, 0))) != [1, 0]})
        pages.append({"number": len(pages) + 1, "width": page.rect.width,
                      "height": page.rect.height, "lines": lines})
print(json.dumps({"pages": pages}))
'''


def pymupdf_pages_local(path: str) -> list[dict]:
    import pymupdf

    pages: list[dict] = []
    with pymupdf.open(path) as document:
        for page in document:
            lines: list[dict] = []
            for block in page.get_text("dict").get("blocks", []):
                if block.get("type") != 0:
                    continue
                for line in block.get("lines", []):
                    text = "".join(span.get("text", "") for span in line.get("spans", []))
                    if not text.strip():
                        continue
                    x0, y0, x1, y1 = line["bbox"]
                    size = max((span.get("size", 0.0) for span in line.get("spans", [])),
                               default=0.0)
                    lines.append({"text": text, "x0": x0, "y0": y0, "x1": x1, "y1": y1,
                                  "size": size,
                                  "rotated": tuple(line.get("dir", (1, 0))) != (1, 0)})
            pages.append({"number": len(pages) + 1, "width": page.rect.width,
                          "height": page.rect.height, "lines": lines})
    return pages


def pymupdf_pages(path: str, python: str | None) -> tuple[list[dict] | None, str]:
    """The per-page lines of one PDF, or (None, why). Uses PyMuPDF in this interpreter when it
    imports, else the same worker under `--pymupdf-python`."""
    try:
        import pymupdf  # noqa: F401
    except Exception:  # noqa: BLE001
        if not python:
            return None, "no pymupdf"
        try:
            result = subprocess.run([python, "-c", PYMUPDF_WORKER, path],
                                    capture_output=True, timeout=900)
        except Exception as exc:  # noqa: BLE001
            return None, f"pymupdf worker failed: {exc}"
        if result.returncode != 0:
            return None, ("pymupdf worker failed: "
                          + result.stderr.decode("utf-8", "replace")[:200])
        try:
            return json.loads(result.stdout.decode("utf-8", "replace"))["pages"], "ok"
        except Exception as exc:  # noqa: BLE001
            return None, f"pymupdf worker output unreadable: {exc}"
    try:
        return pymupdf_pages_local(path), "ok"
    except Exception as exc:  # noqa: BLE001
        return None, f"pymupdf failed: {exc}"


# --------------------------------------------------------------------------------------------
# Furniture detection (positional reference)
# --------------------------------------------------------------------------------------------

def page_number_map(pages: list[dict], band_edges) -> dict:
    """Which lines are a page's own page number: `(page, line_index)` -> the line's text.

    A digit-only line in the margin band is the page number when the same offset between the
    printed value and the page's position holds across the document: a working paper whose
    front matter is unnumbered prints `1` on the third page, so "within ±1 of the page number"
    alone would miss every page of it.
    """
    found: dict[tuple[int, int], str] = {}
    digit_hits: list[tuple[int, int, int]] = []
    labelled = collections.Counter()
    for page in pages:
        for index, line in enumerate(page["lines"]):
            text = norm(line["text"])
            if not text or not band_edges(line):
                continue
            if PAGE_NUM_RES[0].match(text):
                digit_hits.append((page["number"], index, int(text)))
                continue
            for pattern in PAGE_NUM_RES[1:]:
                if pattern.match(text):
                    found[(page["number"], index)] = text
                    labelled[text] += 1
                    break
    for key, text in list(found.items()):
        if labelled[text] < 2:
            del found[key]
    deltas = collections.Counter(page - value for page, _, value in digit_hits)
    share = max(MIN_REPEAT_PAGES, REPEAT_PAGE_SHARE * len(pages))
    best = next((delta for delta, count in deltas.most_common(1) if count >= share), None)
    if best is not None:
        for page, index, value in digit_hits:
            if abs((page - value) - best) <= 1:
                found[(page, index)] = str(value)
    return found


def review_line_numbers(pages: list[dict], min_y: float, max_y: float) -> set[tuple[int, int]]:
    """The lines of a review copy's line-number column: `(page, line_index)`.

    The signature is a column, not a number: at least `LINE_NUMBER_PAGE_MIN` digit-only lines at
    one x in the left margin, in increasing order down the page, their values consecutive, the
    column spanning most of the text height, and the column present on at least half the pages.
    A table's row numbers are the trap: they satisfy every clause but the height and the page
    share (the corpus's two hits are both a table on one page).
    """
    height = (max_y - min_y) or 1.0
    columns: dict[int, list[tuple[int, int]]] = collections.defaultdict(list)
    for page in pages:
        width = page["width"] or 1.0
        candidates = [line for line in page["lines"]
                      if PAGE_NUM_RES[0].match(norm(line["text"]))
                      and line["x0"] <= LINE_NUMBER_LEFT_SHARE * width]
        if len(candidates) < LINE_NUMBER_PAGE_MIN:
            continue
        xs = [line["x0"] for line in candidates]
        ys = [line["y0"] for line in candidates]
        values = [int(norm(line["text"])) for line in candidates]
        if max(xs) - min(xs) > LINE_NUMBER_X_TOLERANCE:
            continue
        if any(later <= earlier for earlier, later in zip(ys, ys[1:])):
            continue
        steps = sum(1 for left, right in zip(values, values[1:]) if right == left + 1)
        if steps < 0.9 * (len(values) - 1):
            continue
        if max(ys) - min(ys) < 0.7 * height:
            continue
        for line in candidates:
            columns[page["number"]].append((page["number"], page["lines"].index(line)))
    found: set[tuple[int, int]] = set()
    if len(columns) >= max(MIN_REPEAT_PAGES, 0.5 * len(pages)):
        for entries in columns.values():
            found.update(entries)
    return found


def detect_furniture(pages: list[dict]) -> dict:
    """Every furniture occurrence of one document, with the tests that found it.

    The reference standard is positional: a line's baseline in the top/bottom 10% band of the
    text area, plus one of the four tests (repeated text, page-number pattern, arXiv stamp,
    review line numbers). `mid_page` marks a candidate whose text also occurs away from the
    band, which is the string-removal risk of every non-positional option.
    """
    page_count = len(pages)
    all_lines = [line for page in pages for line in page["lines"]]
    if not all_lines:
        return {"page_count": page_count, "occurrences": [], "candidates": [],
                "band": None, "text_area": None}
    min_y = min(line["y0"] for line in all_lines)
    max_y = max(line["y1"] for line in all_lines)
    band = BAND_SHARE * (max_y - min_y)

    band_pages: dict[str, set] = collections.defaultdict(set)
    band_mask_pages: dict[str, set] = collections.defaultdict(set)
    mid_pages: dict[str, set] = collections.defaultdict(set)
    for page in pages:
        for line in page["lines"]:
            text = norm(line["text"])
            if not text:
                continue
            in_header = line["y1"] <= min_y + band
            in_footer = line["y0"] >= max_y - band
            if in_header or in_footer or line.get("rotated"):
                # A running head set sideways spans the middle of the page, so a rotated line
                # counts as margin: the band alone would miss every landscape/rotated head.
                band_pages[text].add(page["number"])
                band_mask_pages[mask_digits(text)].add(page["number"])
            elif not ARXIV_STAMP_RE.search(text):
                mid_pages[text].add(page["number"])

    repeated_share = max(MIN_REPEAT_PAGES, REPEAT_PAGE_SHARE * page_count)
    repeated = {text for text, pages_seen in band_pages.items()
                if len(pages_seen) >= repeated_share and len(text) >= MIN_REPEAT_CHARS}
    # A digit-masked repeat is only furniture when enough of it survives the masking: `(1)`,
    # `(2)`, `(3)` mask to `(#)` and repeat on every page of a maths paper, and they are
    # equation numbers, not page numbers.
    repeated_masked = {masked for masked, pages_seen in band_mask_pages.items()
                       if len(pages_seen) >= repeated_share and "#" in masked
                       and len(masked) >= MIN_REPEAT_CHARS}

    def in_band(line) -> bool:
        return line["y1"] <= min_y + band or line["y0"] >= max_y - band

    page_numbers = page_number_map(pages, in_band)
    line_number_lines = review_line_numbers(pages, min_y, max_y)
    occurrences: list[dict] = []
    candidates: dict[str, dict] = {}
    for page in pages:
        number = page["number"]
        width = page["width"] or 1.0
        for line in page["lines"]:
            text = norm(line["text"])
            if not text:
                continue
            in_header = line["y1"] <= min_y + band
            in_footer = line["y0"] >= max_y - band
            tests = []
            if (number, page["lines"].index(line)) in line_number_lines:
                tests.append("line_number")
            if ARXIV_STAMP_RE.search(text):
                tests.append("arxiv_stamp")
            if (number, page["lines"].index(line)) in page_numbers:
                tests.append("page_number")
            if text in repeated:
                tests.append("repeated")
            if mask_digits(text) in repeated_masked and "#" in mask_digits(text):
                tests.append("repeated_masked")
            if not tests:
                continue
            for test in tests:
                candidates.setdefault(test, {})
                key = text if test != "repeated_masked" else mask_digits(text)
                entry = candidates[test].setdefault(key, {"text": key, "pages": 0, "kind": test})
                entry["pages"] += 1
            occurrences.append({
                "page": number,
                "line_index": page["lines"].index(line),
                "line": text,
                "tests": tests,
                "in_band": in_header or in_footer,
                "rotated": bool(line.get("rotated")),
                "x0": round(line["x0"], 1),
                "y0": round(line["y0"], 1),
                "size": round(line["size"], 1),
                "mid_page_elsewhere": bool(mid_pages.get(text)),
            })

    for by_text in candidates.values():
        for entry in by_text.values():
            entry["mid_page_elsewhere"] = any(
                occurrence["mid_page_elsewhere"]
                for occurrence in occurrences
                if (occurrence["line"] == entry["text"]
                    if entry["kind"] != "repeated_masked"
                    else mask_digits(occurrence["line"]) == entry["text"]))
    flat = [entry for by_text in candidates.values() for entry in by_text.values()]
    return {"page_count": page_count, "occurrences": occurrences,
            "candidates": flat,
            "band": band, "text_area": [round(min_y, 1), round(max_y, 1)],
            "repeated": sorted(repeated)[:200], "repeated_masked": sorted(repeated_masked)[:200],
            "cheap_flags": option_flags(pages)["cheap_chunks"]}


def boundary_lines(pages: list[dict], furniture_lines: set[str]) -> list[dict]:
    """The page boundaries where a sentence runs from one page into the next.

    A boundary is a candidate when the last body line of the left page has no terminal mark and
    the first body line of the right page starts a continuation (lower-case, a bracket or a
    digit). Furniture lines on either side of the boundary are recorded with it.
    """
    boundaries: list[dict] = []
    for left, right in zip(pages, pages[1:]):
        left_body = [(index, line) for index, line in enumerate(left["lines"])
                     if norm(line["text"]) not in furniture_lines]
        right_body = [(index, line) for index, line in enumerate(right["lines"])
                      if norm(line["text"]) not in furniture_lines]
        if not left_body or not right_body:
            continue
        last = norm(left_body[-1][1]["text"])
        first = norm(right_body[0][1]["text"])
        if not last or ends_terminal(last):
            continue
        if not (first[:1].islower() or first[:1] in "([{" or first[:1].isdigit()):
            continue
        if last.endswith("-") or last.endswith("‐") or last.endswith("–"):
            pass
        boundaries.append({
            "page": left["number"],
            "last": last,
            "first": first,
            "last_index": left_body[-1][0],
            "first_index": right_body[0][0],
            "furniture_left": [norm(line["text"]) for line in left["lines"]
                               if norm(line["text"]) in furniture_lines],
            "furniture_right": [norm(line["text"]) for line in right["lines"]
                                if norm(line["text"]) in furniture_lines],
        })
    return boundaries


# --------------------------------------------------------------------------------------------
# One document through stdio MCP
# --------------------------------------------------------------------------------------------

def read_stream(server, handle: str, unit: str | None, deadline: float) -> dict:
    items: list[tuple[str, str, str | None]] = []
    cursor = handle
    seen: set[str] = set()
    calls = 0
    while True:
        if time.monotonic() > deadline:
            return {"items": items, "truncated": True, "calls": calls}
        try:
            data = PROBE.read_batch(server, cursor, unit=unit)
        except PROBE.ProbeError as exc:
            return {"items": items, "truncated": True, "calls": calls, "error": str(exc)[:200]}
        calls += 1
        items.extend(PROBE.v2_items(data))
        following = data.get("next")
        if not isinstance(following, str):
            return {"items": items, "truncated": False, "calls": calls}
        if following in seen:
            return {"items": items, "truncated": True, "calls": calls,
                    "error": "v2 read next handle repeated"}
        seen.add(following)
        cursor = following


def paragraph_key(handle: str) -> tuple[str, int | None]:
    return PROBE.handle_section(handle), PROBE.handle_paragraph_index(handle)


def reference_stream(pages: list[dict]) -> dict:
    """The reference's squashed text and the squash offset of every line, in reading order."""
    pieces: list[str] = []
    offsets: list[int] = []
    total = 0
    for page in pages:
        for line in page["lines"]:
            squashed = squash(line["text"])
            if not squashed:
                offsets.append(total)
                continue
            offsets.append(total)
            pieces.append(squashed)
            total += len(squashed)
    return {"squash": "".join(pieces), "offsets": offsets}


def align_paragraphs(paragraphs: list[tuple[str, str]], reference: dict) -> list[dict]:
    """Where each paragraph's text starts and ends in the reference's squashed stream."""
    stream = reference["squash"]
    spans: list[dict] = []
    cursor = 0
    for handle, text in paragraphs:
        squashed = squash(text)
        if not squashed:
            spans.append({"handle": handle, "start": None, "end": None, "chars": 0})
            continue
        # The search window starts a little before the cursor: our paragraphs and the
        # reference's lines split the same text at different places, so a paragraph can begin
        # inside the one before it.
        search_from = max(0, cursor - ALIGN_BACKTRACK)
        found = -1
        for offset in (0, len(squashed) // 4, len(squashed) // 2):
            for width in (32, 24, 16, 8):
                window = squashed[offset:offset + width]
                if len(window) < width:
                    continue
                index = stream.find(window, search_from)
                if index >= 0:
                    found = index - offset
                    break
            if found >= 0:
                break
        if found < 0 or found < search_from:
            spans.append({"handle": handle, "start": None, "end": None,
                          "chars": len(squashed)})
            continue
        found = max(found, 0)
        end = found + len(squashed)
        spans.append({"handle": handle, "start": found, "end": max(end, found + 1),
                      "chars": len(squashed)})
        cursor = max(end, found + 1)
    return spans


def sentence_spans(paragraphs: list[tuple[str, str]], sentences: list[tuple[str, str, str | None]]
                   ) -> list[dict]:
    """The squash offset of every sentence inside its paragraph's own squashed text."""
    by_paragraph: dict[tuple[str, int | None], list[tuple[str, str, str | None]]] = (
        collections.defaultdict(list))
    for item in sentences:
        by_paragraph[paragraph_key(item[0])].append(item)
    spans: list[dict] = []
    for handle, text in paragraphs:
        squashed = squash(text)
        cursor = 0
        for sentence_handle, sentence_text, kind in by_paragraph.get(paragraph_key(handle), []):
            piece = squash(sentence_text)
            if not piece:
                spans.append({"handle": sentence_handle, "paragraph": handle, "start": None,
                              "end": None, "kind": kind})
                continue
            index = squashed.find(piece, cursor)
            if index < 0:
                index = squashed.find(piece)
            if index < 0:
                spans.append({"handle": sentence_handle, "paragraph": handle, "start": None,
                              "end": None, "kind": kind})
                continue
            spans.append({"handle": sentence_handle, "paragraph": handle, "start": index,
                          "end": index + len(piece), "kind": kind})
            cursor = index + len(piece)
    return spans


def locate(span_list: list[dict], offset: int) -> dict | None:
    """The span that holds one reference squash offset, by monotone binary search."""
    starts = [span["start"] for span in span_list if span["start"] is not None]
    if not starts:
        return None
    index = bisect.bisect_right(starts, offset) - 1
    if index < 0:
        return None
    # The list may have unaligned entries; walk back to the same index in the full list.
    candidates = [span for span in span_list if span["start"] is not None]
    span = candidates[index]
    if span["start"] <= offset < span["end"]:
        return span
    return None


def paragraph_index(paragraphs: list[tuple[str, str]]) -> dict:
    """One entry per paragraph: its squashed text and the folded form used for matching.

    Folding once per document keeps the search cheap: a furniture line is looked for in every
    paragraph, and re-folding 1,170 paragraphs for each of 400 occurrences dominated the run.
    """
    index: dict[str, dict] = {}
    for handle, text in paragraphs:
        squashed = squash(text)
        folded, offsets = fold(squashed)
        index[handle] = {"squash": squashed, "folded": folded, "offsets": offsets}
    return index


def find_in_paragraph(entry: dict, needle: str) -> int | None:
    """The squash offset of `needle` in one paragraph, folded for comparison."""
    folded = entry["folded"]
    at = folded.find(needle)
    text = entry["squash"]
    while at >= 0:
        position = entry["offsets"][at]
        end = entry["offsets"][at + len(needle) - 1] + 1
        if not needle.isdigit():
            return position
        # A page number must not match inside a longer number.
        if (position == 0 or not text[position - 1].isdigit()) and (
                end >= len(text) or not text[end].isdigit()):
            return position
        at = folded.find(needle, at + 1)
    return None


def classify_occurrence(occurrence: dict, spans: list[dict], s_spans: list[dict],
                        paragraph_order: list[str], index: dict[str, dict],
                        sentences_by_handle: dict[str, str]) -> dict:
    """How one furniture line lands in the reader's text.

    The reference offset locates the paragraph first, and the hit is verified by the text; when
    that fails (our paragraphs and the reference's lines drift apart on a page whose tables and
    dot leaders are not extracted the same way) every paragraph is searched, and the hit whose
    offset is nearest the reference's wins. So a line is called absent only when its letters and
    digits are nowhere in the reader's text.

    `glued` means the line's characters are in the reader's sentence but its spaces are not, so
    the line cannot be removed by a string match and the sentence reads as one long word run.
    """
    line = squash(occurrence["line"])
    needle = fold(line)[0]
    if not needle:
        return {"where": "absent", "unit": None, "glued": False}
    offset = occurrence.get("squash_offset") or 0
    tries: list[tuple[str, int]] = []
    span = locate(spans, offset)
    if span is not None:
        index_of = spans.index(span)
        for delta in (0, 1, -1, 2, -2):
            neighbour = index_of + delta
            if not 0 <= neighbour < len(spans):
                continue
            handle = spans[neighbour]["handle"]
            position = find_in_paragraph(index[handle], needle) if handle in index else None
            if position is not None:
                tries.append((handle, position))
                break
    if not tries:
        # Every paragraph, aligned or not: an unaligned paragraph is still the reader's text.
        starts = {span["handle"]: span["start"] for span in spans}
        best: tuple[float, str, int] | None = None
        for handle in paragraph_order:
            position = find_in_paragraph(index[handle], needle)
            if position is None:
                continue
            start = starts.get(handle)
            distance = abs(start - offset) if start is not None else float("inf")
            if best is None or distance < best[0]:
                best = (distance, handle, position)
        if best is not None:
            tries.append((best[1], best[2]))
    if not tries:
        return {"where": "absent", "unit": None, "glued": False}
    handle, position = tries[0]
    sentence = None
    for item in s_spans:
        if item["paragraph"] != handle or item["start"] is None:
            continue
        if item["start"] <= position < item["end"]:
            sentence = item
            break
    if sentence is None:
        return {"where": "inside", "unit": handle, "glued": False}
    sentence_text = squash(sentences_by_handle.get(sentence["handle"], ""))
    inner = position - sentence["start"]
    if fold(sentence_text)[0] == needle:
        where = "own_unit"
    elif inner <= 1:
        where = "prefix"
    elif inner + len(line) >= len(sentence_text) - 2:
        where = "suffix"
    else:
        where = "inside"
    original = sentences_by_handle.get(sentence["handle"], "")
    return {"where": where, "unit": sentence["handle"], "kind": sentence.get("kind"),
            "before": sentence_text[:max(inner, 0)][-40:],
            "after": sentence_text[max(inner, 0) + len(line):][:40],
            "sentence_head": sentence_text[:HEAD_CHARS],
            "glued": norm(occurrence["line"]) not in norm(original)}


def locate_unit(offset: int, spans: list[dict], s_spans: list[dict]) -> dict | None:
    """The sentence unit that holds one reference-global squash offset."""
    paragraph = locate(spans, offset)
    if paragraph is None:
        return None
    position = offset - paragraph["start"]
    for item in s_spans:
        if item["paragraph"] != paragraph["handle"] or item["start"] is None:
            continue
        if item["start"] <= position < item["end"]:
            return item
    return None


def cheap_pass_pages(pages: list[dict]) -> list[dict]:
    """The pass a fix can run on the extractor's own per-page chunks: no positions.

    What `PageText` already carries is the page number and the page's chunk order, so the band
    test becomes "the first/last `EDGE_LINES` chunks of the page", and the repetition, page
    number, `Page N of M` and arXiv-stamp tests are the same as the positional ones.
    """
    page_count = len(pages)
    edge_pages: dict[str, set] = collections.defaultdict(set)
    edge_masks: dict[str, set] = collections.defaultdict(set)
    for page in pages:
        for line in page["lines"][:EDGE_LINES] + page["lines"][-EDGE_LINES:]:
            text = norm(line["text"])
            if text:
                edge_pages[text].add(page["number"])
                edge_masks[mask_digits(text)].add(page["number"])
    repeated_share = max(MIN_REPEAT_PAGES, REPEAT_PAGE_SHARE * page_count)
    repeated = {text for text, seen in edge_pages.items()
                if len(seen) >= repeated_share and len(text) >= MIN_REPEAT_CHARS}
    repeated_masked = {masked for masked, seen in edge_masks.items()
                       if len(seen) >= repeated_share and "#" in masked
                       and len(masked) >= MIN_REPEAT_CHARS}
    def at_edge(page: dict, index: int) -> bool:
        return index < EDGE_LINES or index >= len(page["lines"]) - EDGE_LINES

    page_numbers = page_number_map(pages, lambda line: True)
    flags: list[dict] = []
    for page in pages:
        for index, line in enumerate(page["lines"]):
            text = norm(line["text"])
            if not text:
                continue
            tests = []
            if ARXIV_STAMP_RE.search(text):
                tests.append("arxiv_stamp")
            if at_edge(page, index) and (page["number"], index) in page_numbers:
                tests.append("page_number")
            if at_edge(page, index) and (text in repeated
                                         or mask_digits(text) in repeated_masked):
                tests.append("repeated")
            for test in tests:
                flags.append({"page": page["number"], "line_index": index, "text": text,
                              "test": test})
    return flags


def cheap_pass_units(sentences: list[tuple[str, str, str | None]]) -> list[dict]:
    """The same idea with no page information at all: the reader's units are the only input.

    This is what a pass outside the extractor can do (a filter on the read stream), and the
    corpus shows why it is not enough: repetition of a unit-edge token run cannot tell a running
    header from a prose opener or a table cell.
    """
    flagged: list[dict] = []
    for handle, text, kind in sentences:
        if not text.strip():
            continue
        stripped = text.strip()
        for match in ARXIV_STAMP_RE.finditer(stripped):
            flagged.append({"handle": handle, "test": "arxiv_stamp",
                            "start": match.start(), "end": match.end(),
                            "text": match.group(0)})
        for pattern in EDGE_PAGENUM_RES:
            match = pattern.search(stripped)
            if match:
                flagged.append({"handle": handle, "test": "page_number",
                                "start": match.start(), "end": match.end(),
                                "text": match.group(0)})
                break
    counts: collections.Counter = collections.Counter()
    for _, text, _ in sentences:
        tokens = text.split()
        for size in range(1, min(10, len(tokens)) + 1):
            for run in (" ".join(tokens[:size]), " ".join(tokens[-size:])):
                if len(run) >= MIN_REPEAT_CHARS:
                    counts[run] += 1
    repeated = set()
    for run, count in counts.items():
        if count < MIN_REPEAT_PAGES:
            continue
        tokens = run.split()
        if not any(NON_PROSE_TOKEN_RE.search(token) for token in tokens):
            continue
        if ends_terminal(run):
            continue
        repeated.add(run)
    for handle, text, kind in sentences:
        stripped = text.strip()
        for run in repeated:
            if stripped.startswith(run):
                flagged.append({"handle": handle, "test": "repeated",
                                "start": 0, "end": len(run), "text": run})
            elif stripped.endswith(run):
                flagged.append({"handle": handle, "test": "repeated",
                                "start": len(stripped) - len(run), "end": len(stripped),
                                "text": run})
    return flagged


def option_flags(pages: list[dict]) -> dict:
    """The flag set of every detection option, each scored against the positional standard.

    `band` alone is the positional test without a repetition or pattern condition (it is what
    the margin band catches on its own, table cells included); `repetition` and `page_number`
    and `stamp` are the non-positional tests alone; `cheap_chunks` is the pass a fix can run
    today on `PageText` (page number + chunk order, no coordinates).
    """
    page_count = len(pages)
    all_lines = [line for page in pages for line in page["lines"]]
    if not all_lines:
        return {}
    min_y = min(line["y0"] for line in all_lines)
    max_y = max(line["y1"] for line in all_lines)
    band = BAND_SHARE * (max_y - min_y)
    anywhere_pages: dict[str, set] = collections.defaultdict(set)
    for page in pages:
        for line in page["lines"]:
            text = norm(line["text"])
            if text:
                anywhere_pages[text].add(page["number"])
    repeated_share = max(MIN_REPEAT_PAGES, REPEAT_PAGE_SHARE * page_count)
    repeated = {text for text, seen in anywhere_pages.items()
                if len(seen) >= repeated_share and len(text) >= MIN_REPEAT_CHARS}
    def in_band(line) -> bool:
        return line["y1"] <= min_y + band or line["y0"] >= max_y - band

    page_numbers = page_number_map(pages, in_band)
    options: dict[str, list[dict]] = {"band": [], "repetition": [], "page_number": [],
                                      "stamp": [], "cheap_chunks": cheap_pass_pages(pages)}
    for page in pages:
        for index, line in enumerate(page["lines"]):
            text = norm(line["text"])
            if not text:
                continue
            base = {"page": page["number"], "line_index": index, "text": text}
            if in_band(line):
                options["band"].append(dict(base, test="band"))
            if text in repeated:
                options["repetition"].append(dict(base, test="repetition"))
            if (page["number"], index) in page_numbers:
                options["page_number"].append(dict(base, test="page_number"))
            if ARXIV_STAMP_RE.search(text):
                options["stamp"].append(dict(base, test="stamp"))
    return options


def score_flags(flags: list[dict], occurrences: list[dict]) -> dict:
    """Precision and recall of one option against the positional reference standard.

    An occurrence and a flag match when they name the same page and the same normalised line
    text; a flag on a line that is not furniture is a false positive, and a furniture occurrence
    with no flag is a miss.
    """
    reference = {(item["page"], norm(item["line"])) for item in occurrences}
    flagged = {(item["page"], norm(item["text"])) for item in flags}
    true_hits = len(flagged & reference)
    return {"flagged": len(flagged), "reference": len(reference), "true_hits": true_hits,
            "precision": round(true_hits / len(flagged), 4) if flagged else None,
            "recall": round(true_hits / len(reference), 4) if reference else None}


def strip_furniture(text: str, lines: list[str]) -> str:
    """The paragraph with the furniture removed the way a line-level fix would remove it.

    The extractor drops a furniture *line* from the page's text before the paragraphs are built,
    so this drops a whole line whose text is a furniture line, and trims a furniture line off a
    line's start or end. A furniture line in the *middle* of a line (the `glued` case, where the
    missing spaces cannot be cut apart) is left alone: removing it by string match would also
    delete real text elsewhere on the page.
    """
    keys = sorted({norm(line) for line in lines if norm(line)}, key=len, reverse=True)
    if not keys:
        return norm(text)
    out: list[str] = []
    for raw in text.split("\n"):
        body = raw.strip()
        if not body:
            out.append(raw)
            continue
        key = norm(body)
        if key in keys:
            continue
        changed = True
        while changed and key:
            changed = False
            for line in keys:
                if len(key) <= len(line):
                    continue
                if key.startswith(line):
                    body = body[len(line):].lstrip()
                elif key.endswith(line):
                    body = body[: len(body) - len(line)].rstrip()
                else:
                    continue
                key = norm(body)
                changed = True
                break
        out.append(body)
    return norm("\n".join(out))


def census_document(entry: dict, path: str, args) -> dict:
    label = entry.get("id") or os.path.basename(path)
    record: dict = {
        "id": label, "source": entry["source"], "path": path, "title": entry.get("title"),
        "outcome": None, "error": None,
        "pages": None, "furniture_mode": "text_only",
        "furniture": {"occurrences": 0, "by_test": {}, "candidates": [], "where": {},
                      "units": 0, "broken": 0, "polluted": 0, "boundary_broken": 0,
                      "boundary_sentences": 0, "absent": 0},
        "cheap": {"flagged": 0, "precision": None, "recall": None},
        "cheap_units": {"flagged": 0, "precision": None, "recall": None},
        "options": {},
        "coarse": {"paragraphs": 0, "chars": 0, "coarse_paragraphs": 0, "coarse_chars": 0,
                   "share": None, "after_paragraphs": 0, "after_chars": 0,
                   "after_coarse_paragraphs": 0, "after_coarse_chars": 0, "after_share": None},
        "examples": [], "sentences": 0,
    }
    pages, why = (None, "no pymupdf")
    if not args.text_only:
        pages, why = pymupdf_pages(path, args.pymupdf_python)
    furniture = None
    reference = None
    if pages:
        record["furniture_mode"] = "positional"
        record["pages"] = len(pages)
        furniture = detect_furniture(pages)
        reference = reference_stream(pages)
        record["furniture"]["by_test"] = dict(collections.Counter(
            test for item in furniture["occurrences"] for test in item["tests"]))
        record["furniture"]["candidates"] = furniture["candidates"]
        record["furniture"]["band"] = furniture["band"]
        record["furniture"]["text_area"] = furniture["text_area"]
    else:
        record["furniture"]["error"] = why

    state_dir = os.path.join(args.state_dir, re.sub(r"[^A-Za-z0-9._-]+", "_", label))
    shutil.rmtree(state_dir, ignore_errors=True)
    os.makedirs(state_dir, exist_ok=True)
    env = PROBE.server_env(state_dir, os.path.dirname(os.path.abspath(path)), "default", "")
    server = PROBE.Server([args.binary], env, os.path.join(state_dir, "server.log"),
                          PROBE.TOOL_TIMEOUT_S)
    deadline = time.monotonic() + DOC_DEADLINE_S
    try:
        server.initialize()
        try:
            opened = PROBE.payload(server.tool("open", {"source": path},
                                              timeout=OPEN_TIMEOUT_S)) or {}
        except PROBE.ProbeError as exc:
            record["outcome"] = "open_failed"
            record["error"] = f"{exc.code}: {str(exc)[:300]}"
            return record
        handle = PROBE.document_handle(opened)
        if not handle:
            record["outcome"] = "open_failed"
            record["error"] = "open returned no document handle"
            return record
        paragraphs_stream = read_stream(server, handle, "paragraph", deadline)
        sentences_stream = read_stream(server, handle, None, deadline)
        record["paragraph_stream"] = {"items": len(paragraphs_stream["items"]),
                                      "truncated": paragraphs_stream["truncated"]}
        record["sentence_stream"] = {"items": len(sentences_stream["items"]),
                                     "truncated": sentences_stream["truncated"]}
        paragraphs = [(item[0], item[1]) for item in paragraphs_stream["items"]]
        sentences = list(sentences_stream["items"])
        record["sentences"] = len(sentences)
        record["outcome"] = "ok"
    finally:
        server.close(grace=30.0)

    # --- coarse share, before and after the furniture is stripped out ---------------------
    paragraph_text = dict(paragraphs)
    coarse_items = [item for item in sentences if item[2] is not None]
    record["coarse"]["paragraphs"] = len(paragraphs)
    record["control_chars"] = sum(1 for _, text in paragraphs if CONTROL_RE.search(text))
    record["coarse"]["chars"] = sum(len(text) for _, text in paragraphs)
    record["coarse"]["coarse_paragraphs"] = len(coarse_items)
    record["coarse"]["coarse_chars"] = sum(len(paragraph_text.get(paragraph_key(item[0]),
                                                                  item[1]))
                                           for item in coarse_items)
    if record["coarse"]["chars"]:
        record["coarse"]["share"] = round(
            record["coarse"]["coarse_chars"] / record["coarse"]["chars"], 4)

    furniture_lines: list[str] = []
    if furniture is not None:
        furniture_lines = sorted({norm(item["line"]) for item in furniture["occurrences"]},
                                 key=len, reverse=True)
    if furniture_lines:
        after_chars = 0
        after_coarse_chars = 0
        after_coarse = 0
        for item in coarse_items:
            key = paragraph_key(item[0])
            body = paragraph_text.get(key, item[1])
            stripped = strip_furniture(body, furniture_lines)
            if paragraph_reason(stripped)["reason"] in ("unexplained", REASON_CLASS):
                continue
            after_coarse += 1
            after_coarse_chars += len(stripped)
        for _, text in paragraphs:
            after_chars += len(strip_furniture(text, furniture_lines))
        record["coarse"]["after_paragraphs"] = len(paragraphs)
        record["coarse"]["after_chars"] = after_chars
        record["coarse"]["after_coarse_paragraphs"] = after_coarse
        record["coarse"]["after_coarse_chars"] = after_coarse_chars
        if after_chars:
            record["coarse"]["after_share"] = round(after_coarse_chars / after_chars, 4)

    # --- where the furniture lands in the reader's text -----------------------------------
    if furniture is not None and reference is not None:
        spans = align_paragraphs(paragraphs, reference)
        record["aligned_paragraphs"] = sum(1 for span in spans if span["start"] is not None)
        record["paragraphs"] = len(paragraphs)
        s_spans = sentence_spans(paragraphs, sentences)
        paragraphs_by_handle = dict(paragraphs)
        paragraph_order = [handle for handle, _ in paragraphs]
        index = paragraph_index(paragraphs)
        sentences_by_handle = {item[0]: item[1] for item in sentences}
        where: collections.Counter = collections.Counter()
        by_test_where: collections.Counter = collections.Counter()
        units: set[str] = set()
        broken = 0
        polluted = 0
        absent = 0
        glued = 0
        # Every line's squash offset in the reference stream, in reading order: the
        # occurrence's own offset is what locates it in a paragraph and in a sentence.
        by_page_line: dict[tuple[int, int], int] = {}
        running = 0
        for page in pages:
            for line_index, line in enumerate(page["lines"]):
                by_page_line[(page["number"], line_index)] = running
                running += len(squash(line["text"]))
        cheap_keys = {(flag["page"], norm(flag["text"]))
                      for flag in furniture.get("cheap_flags", [])}
        for occurrence in furniture["occurrences"]:
            occurrence["cheap"] = (occurrence["page"], norm(occurrence["line"])) in cheap_keys
            page_lines = next((item["lines"] for item in pages
                               if item["number"] == occurrence["page"]), [])
            line_index = next((index for index, line in enumerate(page_lines)
                               if norm(line["text"]) == occurrence["line"]), 0)
            occurrence["squash_offset"] = by_page_line.get(
                (occurrence["page"], occurrence["line_index"]), 0)
            verdict = classify_occurrence(occurrence, spans, s_spans, paragraph_order,
                                          index, sentences_by_handle)
            occurrence.update(verdict)
            where[verdict["where"]] += 1
            for test in occurrence["tests"]:
                by_test_where[(test, verdict["where"])] += 1
            if verdict.get("glued"):
                glued += 1
            if verdict["where"] == "absent":
                absent += 1
                continue
            unit = verdict["unit"]
            if unit:
                units.add(unit)
            cut = False
            if verdict["where"] == "own_unit":
                cut = not ends_terminal(occurrence["line"])
            elif verdict["where"] == "suffix":
                cut = not ends_terminal(verdict["before"])
            elif verdict["where"] == "prefix":
                # Leading furniture only cuts a sentence when what follows it is the rest of a
                # sentence that started on the previous page; a capital after it starts a new
                # sentence and the furniture is leading pollution, not a split.
                cut = verdict["after"][:1].islower() or verdict["after"][:1] in ",;)]"
            verdict["broken"] = cut
            occurrence["broken"] = cut
            if cut:
                broken += 1
            else:
                polluted += 1
            if len(record["examples"]) < 12:
                record["examples"].append({
                    "line": occurrence["line"][:HEAD_CHARS],
                    "tests": occurrence["tests"], "where": verdict["where"],
                    "unit": unit, "broken": cut,
                    "sentence": head(sentences_by_handle.get(unit, ""), HEAD_CHARS)})
        record["furniture"]["occurrences"] = len(furniture["occurrences"])
        record["furniture"]["where"] = dict(where)
        record["furniture"]["occurrence_list"] = [
            {key: occurrence[key] for key in
             ("page", "line", "tests", "where", "broken", "unit", "in_band", "rotated",
              "mid_page_elsewhere", "glued", "cheap")
             if key in occurrence}
            for occurrence in furniture["occurrences"]]
        record["furniture"]["where_by_test"] = {
            f"{test}:{place}": count for (test, place), count in by_test_where.items()}
        record["furniture"]["units"] = len(units)
        record["furniture"]["broken"] = broken
        record["furniture"]["polluted"] = polluted
        record["furniture"]["absent"] = absent
        record["furniture"]["glued"] = glued

        # --- page boundaries ---------------------------------------------------------
        furniture_set = set(furniture_lines)
        boundaries = boundary_lines(pages, furniture_set)
        boundary_broken = 0
        boundary_with_furniture = 0
        for boundary in boundaries:
            left_offset = by_page_line.get((boundary["page"], boundary["last_index"]))
            right_offset = by_page_line.get((boundary["page"] + 1, boundary["first_index"]))
            if left_offset is None or right_offset is None:
                continue
            last = squash(boundary["last"])
            left_offset += max(0, len(last) - 24)
            left_unit = locate_unit(left_offset, spans, s_spans)
            right_unit = locate_unit(right_offset, spans, s_spans)
            joined = (left_unit is not None and right_unit is not None
                      and left_unit["handle"] == right_unit["handle"])
            has_furniture = bool(boundary["furniture_left"] or boundary["furniture_right"])
            boundary["joined"] = joined
            boundary["furniture"] = has_furniture
            if not joined:
                boundary_broken += 1
                if has_furniture:
                    boundary_with_furniture += 1
        record["furniture"]["boundary_sentences"] = len(boundaries)
        record["furniture"]["boundary_broken"] = boundary_broken
        record["furniture"]["boundary_broken_with_furniture"] = boundary_with_furniture
        record["boundaries"] = boundaries[:200]

    # --- every detection option, scored against the positional occurrences ---------------
    if furniture is not None:
        options = option_flags(pages)
        record["options"] = {name: score_flags(flags, furniture["occurrences"])
                             for name, flags in options.items()}
        record["options"]["positional"] = {"flagged": len(furniture["occurrences"]),
                                           "reference": len(furniture["occurrences"]),
                                           "true_hits": len(furniture["occurrences"]),
                                           "precision": 1.0, "recall": 1.0}
        record["cheap"] = dict(record["options"]["cheap_chunks"])
    units_flags = cheap_pass_units(sentences)
    unit_texts = {norm(item["text"]) for item in units_flags}
    reference_texts = {norm(item["line"]) for item in furniture["occurrences"]} \
        if furniture is not None else set()
    record["cheap_units"] = {
        "flagged": len(units_flags),
        "by_test": dict(collections.Counter(item["test"] for item in units_flags)),
        "samples": [{"test": item["test"], "text": item["text"][:HEAD_CHARS]}
                    for item in units_flags[:20]],
        "reference_texts": len(reference_texts),
        "true_texts": len(unit_texts & reference_texts),
        "precision": (round(len(unit_texts & reference_texts) / len(unit_texts), 4)
                      if unit_texts else None),
        "recall": (round(len(unit_texts & reference_texts) / len(reference_texts), 4)
                   if reference_texts else None),
    }
    if args.keep_text:
        record["paragraph_text"] = paragraphs
    return record


# --------------------------------------------------------------------------------------------
# Report
# --------------------------------------------------------------------------------------------

def pct(value) -> str:
    return "—" if value is None else f"{100 * value:.1f}%"


def report(records: list[dict]) -> str:
    ok = [record for record in records if record["outcome"] == "ok"]
    positional = [record for record in ok if record["furniture_mode"] == "positional"]
    lines: list[str] = []
    lines.append("# pgf census")
    lines.append("")
    lines.append(f"- inputs measured: {len(ok)} of {len(records)} "
                 f"({len(positional)} with the positional reference)")
    total_occurrences = sum(record["furniture"]["occurrences"] for record in positional)
    total_units = sum(record["furniture"]["units"] for record in positional)
    total_broken = sum(record["furniture"]["broken"] for record in positional)
    lines.append(f"- furniture occurrences: {total_occurrences}; units carrying furniture: "
                 f"{total_units}; sentences cut by furniture: {total_broken}")
    lines.append("")
    lines.append("| input | pages | occurrences | units | broken | polluted | absent | "
                 "boundary broken | cheap P | cheap R | coarse | after |")
    lines.append("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |")
    for record in sorted(positional, key=lambda item: -item["furniture"]["units"]):
        furniture = record["furniture"]
        lines.append(
            f"| {record['id']} | {record['pages']} | {furniture['occurrences']} | "
            f"{furniture['units']} | {furniture['broken']} | {furniture['polluted']} | "
            f"{furniture['absent']} | {furniture.get('boundary_broken', 0)} | "
            f"{pct(record['cheap']['precision'])} | {pct(record['cheap']['recall'])} | "
            f"{pct(record['coarse']['share'])} | {pct(record['coarse']['after_share'])} |")
    lines.append("")
    where: collections.Counter = collections.Counter()
    for record in positional:
        where.update(record["furniture"]["where"])
    lines.append("## Where the furniture lands in the reader's text")
    lines.append("")
    lines.append("| own_unit | prefix | suffix | inside | absent |")
    lines.append("| --- | --- | --- | --- | --- |")
    lines.append(f"| {where.get('own_unit', 0)} | {where.get('prefix', 0)} | "
                 f"{where.get('suffix', 0)} | {where.get('inside', 0)} | "
                 f"{where.get('absent', 0)} |")
    lines.append("")
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--state-dir", required=True)
    parser.add_argument("--sources", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--cache-dir")
    parser.add_argument("--pymupdf-python")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--text-only", action="store_true",
                        help="skip the PyMuPDF reference (repetition and pattern tests only)")
    parser.add_argument("--keep-text", action="store_true")
    args = parser.parse_args(argv)

    os.makedirs(args.out, exist_ok=True)
    os.makedirs(args.state_dir, exist_ok=True)
    cache_dir = args.cache_dir or os.path.join(args.out, "inputs")
    os.makedirs(cache_dir, exist_ok=True)
    entries = parse_sources(args.sources)
    if args.limit:
        entries = entries[: args.limit]

    records: list[dict] = []
    jsonl = open(os.path.join(args.out, "census.jsonl"), "w", encoding="utf-8")
    for index, entry in enumerate(entries, 1):
        path, error = resolve(entry, cache_dir)
        if path is None:
            record = {"id": entry.get("id") or entry.get("source"), "outcome": "resolve_failed",
                      "error": error, "furniture": {"occurrences": 0, "units": 0,
                                                    "broken": 0, "polluted": 0, "absent": 0,
                                                    "where": {}},
                      "cheap": {"precision": None, "recall": None},
                      "coarse": {"share": None, "after_share": None},
                      "furniture_mode": "none", "pages": None}
            records.append(record)
            jsonl.write(json.dumps(record, ensure_ascii=False) + "\n")
            print(f"[{index}/{len(entries)}] {record['id']}: {error}", file=sys.stderr)
            continue
        record = census_document(entry, path, args)
        records.append(record)
        jsonl.write(json.dumps(record, ensure_ascii=False) + "\n")
        jsonl.flush()
        furniture = record["furniture"]
        print(f"[{index}/{len(entries)}] {record['id']}: {record['outcome']} "
              f"occ={furniture.get('occurrences')} units={furniture.get('units')} "
              f"broken={furniture.get('broken')} share={pct(record['coarse']['share'])} -> "
              f"{pct(record['coarse']['after_share'])}", file=sys.stderr)
    jsonl.close()
    with open(os.path.join(args.out, "report.md"), "w", encoding="utf-8") as handle:
        handle.write(report(records))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
