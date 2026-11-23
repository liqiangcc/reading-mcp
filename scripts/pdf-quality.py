#!/usr/bin/env python3
"""PDF-6a: why PDF sentence units come out too long, too short, or full of page furniture.

Measurement only: this script changes no product code, builds nothing, and reuses the PDF-5a
sweep's corpus, downloader, capped-server helper and PyMuPDF reference text
(`scripts/pdf-sweep.py`, `scripts/pdf-probe.py`, `scripts/pdf-corpus.py`, all imported and
never edited).

Two modes:

- run mode (default; needs `--binary` and `--out`): for one shard of
  `scripts/pdf-sweep-ids.txt`, download the PDF, measure a PyMuPDF reference (per-page text,
  per-page lines, fonts), open the document through stdio MCP with the **default** backend
  inside the same 600 MB cap the sweep uses, read every sentence unit with
  `read`, and record the measurements of PDF-6a:

  1. whitespace (H1): the whitespace share of the backend text against PyMuPDF's
     `page.get_text()` on the same pages, per document;
  2. long units (> 800 characters) classified by the first matching rule of
     `glued_terminal` / `glued_words` / `bibliography` / `math_table` / `missed_boundary` /
     `genuine`, with the approximate what-if of splitting each unit at the boundaries that
     rule finds;
  3. short units (< 5 characters) classified as `page_number` / `marker` / `heading_number` /
     `math` / `hyphen_fragment` / `other`;
  4. running headers and footers (H5): where every line repeated on >= 80% of the pages, and
     every page-number line on >= 50%, lands in the reader's units;
  5. silent loss and bad characters (H6): for the documents with 5-gram containment below 0.8
     or more than 1% U+FFFD/private-use characters, backend against reference characters per
     page, and for the worst pages the PDF's own text-showing operators, `Do`-reached Form
     XObjects, whether those forms carry a CJK font (so whether PDF-1c inlines them), font
     subtypes and `/ToUnicode` presence.

  One JSONL record per document plus one record per long and per short unit (the full
  per-unit data goes into the run artifact, never into git).
- `--aggregate --jsonl-dir DIR`: merge the shards into the tables of
  `docs/design/pdf6a-text-quality.md` and print them.
- `--paired --baseline-jsonl-dir DIR --compare LABEL=DIR`: pair two or more runs by arXiv id
  against the baseline run and print the same measures side by side (PDF-7a).

PDF-7a adds a second backend and the cost of running either one:

- `--backend default|layout` selects the backend the way the server selects it
  (`READING_MCP_PDF_LAYOUT_PYTHON` set to `--layout-python` for `layout`, unset for `default`),
  reusing `server_env`/`cap_command` from `scripts/pdf-probe.py` rather than a copy of them.
  `default` is the only setting that existed before PDF-7a and it is unchanged.
- Every document records the **peak resident set of the whole process tree** (the server and the
  Python worker it spawns) and the number of cgroup OOM kills, not only the server's
  `ru_maxrss`: PDF-1a measured the server alone and said so. A document the 600 MB cap kills is
  its own outcome (`cap_killed`) whether the kill hit the server or the worker.
- A `layout` refusal is reported by the server as a bare `OCR_REQUIRED`, so for those documents
  the harness runs the same worker source the binary embeds (`--worker-source`, default
  `src/parsing/pdf_layout_worker.py`) under the pinned interpreter with the arguments
  `layout_pdf.rs` passes, and records which of the four refusal reasons fired and on which
  pages. That probe is outside the cap: it is measurement, not the server.

Standard library only, plus PyMuPDF for the reference text and the PDF-level facts.
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
import statistics
import subprocess
import sys
import threading
import time

MIB = 1024 * 1024
PROC = "/proc"
CGROUP_ROOT = "/sys/fs/cgroup"
TREE_SAMPLE_SECONDS = 0.05   # how often the process tree's resident set is sampled

# --- measurement thresholds (also written into docs/design/pdf6a-text-quality.md) -----------
LONG_CHARS = 800          # a unit longer than this is "long" (PDF-5a's threshold)
SHORT_CHARS = 5           # a unit shorter than this is "short" (PDF-5a's threshold)
GLUED_TERMINAL_MIN = 2    # `glued_terminal` needs at least 2 glued boundaries
GLUED_TOKEN_LETTERS = 30  # a 30+ letter token that is not a reference token is glued text
BIB_MARKER_MIN = 6        # `bibliography` needs at least 6 bibliographic markers
LETTER_SHARE_MAX = 0.5    # `math_table` when letters are under 50% of the non-space chars
MISSED_BOUNDARY_MIN = 2   # `missed_boundary` needs at least 2 space-separated boundaries
HEADER_PAGE_SHARE = 0.8   # a line on >= 80% of the pages is a running header/footer
PAGENUM_PAGE_SHARE = 0.5  # a page-number line on >= 50% of the pages is page furniture
MIN_REPEAT_PAGES = 3
# A repeated line shorter than this is a table cell or a heading word, not page furniture, and
# a one- or two-character line would match inside every number on the page.
MIN_HEADER_PLACEMENT_CHARS = 8
MIN_LINE_CHARS = 4
WS_REFERENCE_FLOOR = 0.7  # "backend whitespace below 70% of the reference"
LOSS_CONTAINMENT = 0.8    # H6 population: containment below this
BAD_CHAR_SHARE = 0.01     # H6 population: over 1% U+FFFD + private-use
WORST_PAGES = 5
SAMPLE_CHARS = 300        # stored per long/short unit in the artifact
CONTAINMENT_PAGES = 20
GRAM = 5

QUALITY_BATCH_ITEMS = 256
TEXT_BUDGET_CHARS = 1500000
OPEN_TIMEOUT_S = 120.0

# The arguments `layout_pdf.rs` passes to the embedded worker with OCR disabled: page budget,
# byte budget, normalized-character budget, then an empty OCR config and identity, then the
# worker path (which the probe passes as `sys.argv[6]`, the worker's own `WORKER_SOURCE`).
# PDF-1a reproduced the refusals with the same triple.
WORKER_PAGE_BUDGET = 2000
WORKER_BYTE_BUDGET = 134217728
WORKER_CHAR_BUDGET = 200000000
REFUSAL_TIMEOUT_S = 900.0

# The outcome PDF-7a adds: the 600 MB cap, whether it hit the server or the worker. The
# sweep's own vocabulary (`ocr_required`, `parse_failed`, `timeout`, ...) is kept.
CAP_KILLED = "cap_killed"

LONG_CAUSES = ("glued_terminal", "glued_words", "bibliography", "math_table",
               "missed_boundary", "genuine")
SHORT_CAUSES = ("page_number", "marker", "heading_number", "math", "hyphen_fragment", "other")
HEADER_CASES = ("own_unit", "prefix", "suffix", "inside", "glued", "not_found",
                "skipped_short")
PAGE_POSITIONS = ("start", "end", "both", "middle")

# `glued_terminal`: a terminal mark directly followed by a capitalised word. The lookahead form
# finds exactly the same boundaries as the task's `[a-z]{2,}[.?!]["')\]]?[A-Z][a-z]` and also
# yields the cut position, which the what-if needs.
GLUED_TERMINAL_RE = re.compile(r"[a-z]{2,}[.?!][\"')\]]?(?=[A-Z][a-z])")
# `missed_boundary`: a terminal mark, whitespace, then an uppercase letter.
MISSED_BOUNDARY_RE = re.compile(r"[.?!][\"')\]]?\s+(?=[A-Z])")
BIB_MARKER_RES = (
    re.compile(r"\b(?:1[89]|20)\d{2}\b"),                      # years
    re.compile(r"\bet\s+al\b\.?"),                             # et al.
    re.compile(r"\[\d{1,3}\]"),                                # [3]
    re.compile(r"\bpp?\.\s*\d"),                               # pp. 12
    re.compile(r"\bvol\.\s*\d", re.I),                         # vol. 3
    re.compile(r"\bdoi\b", re.I),                              # doi
    re.compile(r"arXiv:\s*\d{4}\.\d{4,5}"),                    # arXiv:2401.00001
    re.compile(r"\b(?:eds?|edited\s+by)\b", re.I),             # eds.
)
LETTER_RE = re.compile(r"[^\W\d_]", re.U)
WORD_RE = re.compile(r"[^\W\d_]{3,}", re.U)
LONG_TOKEN_RE = re.compile(r"[^\W\d_]+", re.U)
PAGE_NUMBER_LINE_RES = (
    re.compile(r"^[-–—]?\s*\d{1,4}\s*[-–—]?$"),
    re.compile(r"^\d{1,4}\s*/\s*\d{1,4}$"),
    re.compile(r"^(?:page|p\.)\s*\d{1,4}$", re.I),
    re.compile(r"^\d{1,4}\s+of\s+\d{1,4}$", re.I),
)
HEADING_NUMBER_RE = re.compile(r"^(?:\d+(?:\.\d+)+\.?|\d+\.|[A-Z]\.|\d+\))$")
MARKER_RE = re.compile(r"^(?:[\[\(]\d{1,3}[\]\)]|\d{1,3}[\)\]]|[*†‡§¶#]+|[⁰¹²³⁴⁵⁶⁷⁸⁹]+)$")
MATH_ONLY_RE = re.compile(r"^[\s=<>≤≥≈≠+\-−–—/|\\^_(){}\[\]·×÷±√∑∫∞∂∇]+$")
SINGLE_LETTER_RE = re.compile(r"^[\[\(]?[A-Za-z][\]\)]?[,;.]?$")
HYPHEN_END_RE = re.compile(r"[-\u2010\u00ad]$")
LEADING_WORD_RE = re.compile(r"^([^\W\d_]{2,})", re.U)
TRAILING_WORD_RE = re.compile(r"([^\W\d_]{2,})$", re.U)
CONTROL_LINE_RE = re.compile(r"\s+")


def load_sibling(name: str):
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), name)
    spec = importlib.util.spec_from_file_location(name.replace("-", "_").replace(".py", ""), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


SWEEP = load_sibling("pdf-sweep.py")
CORPUS = SWEEP.CORPUS
PROBE = SWEEP.PROBE
MATH_CHARS = SWEEP.MATH_CHARS
write_json = CORPUS.write_json
append_jsonl = CORPUS.append_jsonl
percentile = CORPUS.percentile
fmt = CORPUS.fmt
gram_containment = CORPUS.gram_containment


def normalize(text: str) -> str:
    """The probe's comparison normal form: NFKC, glyph fold, whitespace collapsed."""
    return PROBE.normalize_text(text)


# --------------------------------------------------------------------------------------------
# Small measures
# --------------------------------------------------------------------------------------------

def whitespace_share(text: str) -> float | None:
    if not text:
        return None
    return sum(1 for char in text if char.isspace()) / len(text)


def is_private_use(char: str) -> bool:
    return SWEEP.is_private_use(char)


def bad_char_shares(text: str) -> dict:
    length = len(text) or 1
    private = sum(1 for char in text if is_private_use(char))
    replacement = text.count("\ufffd")
    return {"private_use_share": round(private / length, 5),
            "replacement_share": round(replacement / length, 5)}


def reference_tokens(text: str) -> set[str]:
    return {match.group(0).lower() for match in WORD_RE.finditer(text)}


def clipped(text: str, limit: int = SAMPLE_CHARS) -> str:
    text = CONTROL_LINE_RE.sub(" ", text).strip()
    return text[:limit]


# --------------------------------------------------------------------------------------------
# Reference text (PyMuPDF): per-page text, lines, fonts
# --------------------------------------------------------------------------------------------

def reference_pages(path: str) -> dict:
    """Per-page reference text and lines, the font inventory and the first-20-page text."""
    import pymupdf

    texts: list[str] = []
    lines_per_page: list[list[str]] = []
    chars: list[int] = []
    fonts: dict[tuple, dict] = {}
    with pymupdf.open(path) as document:
        metadata = document.metadata or {}
        for page in document:
            text = page.get_text()
            texts.append(text)
            chars.append(len(CORPUS.squash(text)))
            lines = []
            for line in text.splitlines():
                line = normalize(line)
                if len(line) >= MIN_LINE_CHARS or any(
                        pattern.match(line) for pattern in PAGE_NUMBER_LINE_RES):
                    lines.append(line)
            lines_per_page.append(lines)
            for font in page.get_fonts(full=True):
                basefont = font[3] if len(font) > 3 else "?"
                subtype = font[2] if len(font) > 2 else "?"
                ext = font[1] if len(font) > 1 else "n/a"
                key = (basefont, subtype, ext)
                entry = fonts.setdefault(key, {"basefont": basefont, "subtype": subtype,
                                               "embedded": ext not in ("n/a", None),
                                               "pages": 0})
                entry["pages"] += 1
        pages = document.page_count
    inventory = sorted(fonts.values(), key=lambda item: item["pages"], reverse=True)[:8]
    return {
        "pages": pages,
        "texts": texts,
        "lines": lines_per_page,
        "chars": chars,
        "chars_total": sum(chars),
        "fonts": inventory,
        "producer": (metadata.get("producer") or "")[:120],
        "first20_text": "\n".join(texts[:CONTAINMENT_PAGES]),
        "first20_chars": sum(chars[:CONTAINMENT_PAGES]),
    }


class PageIndex:
    """Maps a unit's opening characters back to the reference page that contains them."""

    PREFIX_LENGTHS = (60, 40, 24, 16, 10)

    def __init__(self, texts: list[str]) -> None:
        parts = [normalize(text) for text in texts]
        self.starts: list[int] = []
        offset = 0
        for part in parts:
            self.starts.append(offset)
            offset += len(part) + 1
        self.text = "\n".join(parts)

    def page_of(self, unit_text: str) -> int | None:
        head = normalize(unit_text)
        if not head:
            return None
        for length in self.PREFIX_LENGTHS:
            if len(head) < length:
                continue
            position = self.text.find(head[:length])
            if position >= 0:
                return bisect.bisect_right(self.starts, position)
        return None


# --------------------------------------------------------------------------------------------
# Long-unit classification
# --------------------------------------------------------------------------------------------

def glued_terminal_cuts(text: str) -> list[int]:
    return [match.end() for match in GLUED_TERMINAL_RE.finditer(text)]


def missed_boundary_cuts(text: str) -> list[int]:
    return [match.end() for match in MISSED_BOUNDARY_RE.finditer(text)]


def missed_boundary_tokens(text: str) -> list[str]:
    tokens = []
    for match in MISSED_BOUNDARY_RE.finditer(text):
        before = text[:match.start()]
        found = re.search(r"([\w.\-/]+)$", before, re.U)
        tokens.append(found.group(1) if found else "")
    return tokens


def bibliographic_markers(text: str) -> int:
    return sum(len(pattern.findall(text)) for pattern in BIB_MARKER_RES)


def letter_share(text: str) -> float | None:
    non_space = [char for char in text if not char.isspace()]
    if not non_space:
        return None
    letters = sum(1 for char in non_space if char.isalpha())
    return letters / len(non_space)


def glued_long_tokens(text: str, reference_words: set[str]) -> list[str]:
    found = []
    for match in LONG_TOKEN_RE.finditer(text):
        token = match.group(0)
        if len(token) >= GLUED_TOKEN_LETTERS and token.lower() not in reference_words:
            found.append(token)
    return found


def line_breaks(text: str) -> list[int]:
    return [index + 1 for index, char in enumerate(text) if char == "\n"]


def citation_cuts(text: str) -> list[int]:
    cuts = line_breaks(text)
    for match in re.finditer(r"(?<=\s)\[\d{1,3}\]", text):
        cuts.append(match.start())
    return cuts


def classify_long(unit: str, unit_share: float | None, reference_share: float | None,
                  reference_words: set[str]) -> dict:
    """The first matching rule wins, in the order the task fixes."""
    text = unit.strip()
    terminal = glued_terminal_cuts(text)
    long_tokens = glued_long_tokens(text, reference_words)
    share = whitespace_share(text)
    glued_share = bool(unit_share is not None and reference_share
                       and unit_share < 0.5 * reference_share)
    evidence = {
        "glued_terminal_matches": len(terminal),
        "long_tokens_not_in_reference": long_tokens[:5],
        "whitespace_share": round(share, 4) if share is not None else None,
        "page_whitespace_share": round(reference_share, 4)
        if reference_share is not None else None,
    }
    if len(terminal) >= GLUED_TERMINAL_MIN:
        return {"cause": "glued_terminal", "cuts": terminal, "evidence": evidence}
    if glued_share or long_tokens:
        evidence["glued_by"] = ("whitespace_share" if glued_share
                                else "long_token_not_in_reference")
        return {"cause": "glued_words", "cuts": [], "evidence": evidence}
    markers = bibliographic_markers(text)
    evidence["bibliographic_markers"] = markers
    if markers >= BIB_MARKER_MIN:
        return {"cause": "bibliography", "cuts": citation_cuts(text), "evidence": evidence}
    letters = letter_share(text)
    evidence["letter_share"] = round(letters, 4) if letters is not None else None
    if letters is not None and letters < LETTER_SHARE_MAX:
        return {"cause": "math_table", "cuts": line_breaks(text), "evidence": evidence}
    missed = missed_boundary_cuts(text)
    evidence["missed_boundary_matches"] = len(missed)
    evidence["missed_boundary_tokens"] = missed_boundary_tokens(text)[:6]
    if len(missed) >= MISSED_BOUNDARY_MIN:
        return {"cause": "missed_boundary", "cuts": missed, "evidence": evidence}
    return {"cause": "genuine", "cuts": [], "evidence": evidence}


def split_pieces(text: str, cuts: list[int]) -> list[str]:
    """Split at the cut positions the cause rule found, dropping empty and tiny pieces."""
    if not cuts:
        return [text]
    points = sorted({cut for cut in cuts if 0 < cut < len(text)})
    pieces = []
    start = 0
    for point in points:
        piece = text[start:point].strip()
        if piece:
            pieces.append(piece)
        start = point
    tail = text[start:].strip()
    if tail:
        pieces.append(tail)
    return pieces or [text]


def what_if(text: str, cuts: list[int]) -> dict:
    pieces = split_pieces(text, cuts)
    return {"pieces": len(pieces),
            "pieces_long": sum(1 for piece in pieces if len(piece) > LONG_CHARS)}


# --------------------------------------------------------------------------------------------
# Short-unit classification
# --------------------------------------------------------------------------------------------

def neighbour_word(text: str, direction: int) -> str | None:
    match = (LEADING_WORD_RE.search(text) if direction > 0 else TRAILING_WORD_RE.search(text))
    return match.group(1).lower() if match else None


def edge_digits(lines_per_page: list[list[str]]) -> collections.Counter:
    """Digit lines that sit at the start or the end of a page: page numbers and equation
    numbers. A unit whose text is one of these is page furniture, not prose."""
    counts: collections.Counter = collections.Counter()
    for lines in lines_per_page:
        for line in set(lines[:2] + lines[-2:]):
            stripped = line.strip()
            if stripped.isdigit() and len(stripped) <= 4:
                counts[stripped] += 1
    return counts


def classify_short(unit: str, page: int | None, page_count: int,
                   before: str, after: str, reference_words: set[str],
                   edges: collections.Counter) -> dict:
    """The first matching rule wins: page_number, heading_number, marker, hyphen_fragment,
    math, other."""
    text = unit.strip()
    digits = text.strip("[](){}.-–— ")
    if text.isdigit() and len(text) <= 4:
        value = int(text)
        if 1 <= value <= page_count:
            if page is not None and abs(value - page) <= 1:
                return {"cause": "page_number", "page_matched": True}
            if edges.get(text):
                return {"cause": "page_number",
                        "page_matched": page is not None and abs(value - page) <= 1}
            if page is None:
                return {"cause": "page_number", "page_matched": False}
    if HEADING_NUMBER_RE.match(text):
        return {"cause": "heading_number"}
    if MARKER_RE.match(text) or (digits.isdigit() and len(digits) <= 3):
        return {"cause": "marker"}
    if HYPHEN_END_RE.search(text):
        stem = re.search(r"([^\W\d_]+)[-\u2010\u00ad]$", text, re.U)
        head = neighbour_word(after, +1)
        if stem and head and stem.group(1).lower() + head in reference_words:
            return {"cause": "hyphen_fragment", "joined": stem.group(1).lower() + head}
        return {"cause": "hyphen_fragment"}
    if before:
        head = LEADING_WORD_RE.match(text)
        tail = neighbour_word(before, -1)
        if head and tail and tail + head.group(1).lower() in reference_words:
            return {"cause": "hyphen_fragment", "joined": tail + head.group(1).lower()}
    if text and (MATH_ONLY_RE.match(text) or SINGLE_LETTER_RE.match(text)
                 or any(char in MATH_CHARS for char in text)):
        return {"cause": "math"}
    return {"cause": "other"}


# --------------------------------------------------------------------------------------------
# Running headers and footers (H5)
# --------------------------------------------------------------------------------------------

def header_candidates(lines_per_page: list[list[str]], page_count: int,
                      line_share: float = HEADER_PAGE_SHARE,
                      page_share: float = PAGENUM_PAGE_SHARE) -> list[dict]:
    """Lines repeated on >= `line_share` of the pages, and page-number lines on >= `page_share`."""
    counts: collections.Counter = collections.Counter()
    for lines in lines_per_page:
        for line in set(lines):
            counts[line] += 1
    found: list[dict] = []
    seen = set()
    # A page-number line is its own class, so it is tested first: at a loose line threshold a
    # digit-only line would otherwise be counted as a running header.
    for line, count in counts.items():
        if count < MIN_REPEAT_PAGES or count < page_share * page_count:
            continue
        if any(pattern.match(line) for pattern in PAGE_NUMBER_LINE_RES):
            found.append({"line": line, "pages": count, "kind": "page_number"})
            seen.add(line)
    for line, count in counts.items():
        if count < MIN_REPEAT_PAGES or line in seen:
            continue
        if count >= line_share * page_count:
            found.append({"line": line, "pages": count, "kind": "repeated_line"})
    found.sort(key=lambda item: (-item["pages"], -len(item["line"])))
    return found


def header_placement(candidate: dict, lines_per_page: list[list[str]],
                     unit_texts: list[str], squashed_units: list[str]) -> dict:
    """Where the line lands in the reader's units, and where it sits on its page.

    `glued` means the line's characters are in the reader's text but without their spaces, so
    the line cannot be removed until the missing whitespace is fixed."""
    line = candidate["line"]
    cases: collections.Counter = collections.Counter()
    positions: collections.Counter = collections.Counter()
    if candidate["kind"] == "page_number":
        # A page number is only interesting as a unit of its own: "1" occurs inside every
        # number on the page, so a substring test would say nothing.
        own = sum(1 for text in unit_texts if text.strip() == line)
        cases["own_unit" if own else "not_found"] = own or 1
        return {"cases": dict(cases), "positions": {}}
    if len(line) < MIN_HEADER_PLACEMENT_CHARS:
        cases["skipped_short"] = 1
        return {"cases": dict(cases), "positions": {}}
    for lines in lines_per_page:
        if line not in lines:
            continue
        index = lines.index(line)
        at_start = index < 2
        at_end = index >= len(lines) - 2
        if at_start and at_end:
            positions["both"] += 1
        elif at_start:
            positions["start"] += 1
        elif at_end:
            positions["end"] += 1
        else:
            positions["middle"] += 1
    found = False
    for text in unit_texts:
        if line not in text:
            continue
        found = True
        stripped = text.strip()
        if stripped == line:
            cases["own_unit"] += 1
        elif stripped.startswith(line):
            cases["prefix"] += 1
        elif stripped.endswith(line):
            cases["suffix"] += 1
        else:
            cases["inside"] += 1
    if not found:
        squashed = CORPUS.squash(line)
        glued = sum(1 for text in squashed_units if squashed and squashed in text)
        if glued:
            cases["glued"] = glued
        else:
            cases["not_found"] = 1
    return {"cases": dict(cases), "positions": dict(positions)}


# --------------------------------------------------------------------------------------------
# PDF-level facts for the silent-loss documents (H6)
# --------------------------------------------------------------------------------------------

INLINE_IMAGE_RE = re.compile(rb"(?<![A-Za-z0-9])BI\b.*?\bEI\b", re.S)
WS_BYTES = b" \t\r\n\f\x00"
DELIM_BYTES = b"()<>[]{}/%"


def content_tokens(data: bytes) -> list[bytes]:
    """A small content-stream tokenizer: enough to count operators and read `Do` names."""
    data = INLINE_IMAGE_RE.sub(b" ", data)
    tokens: list[bytes] = []
    index = 0
    length = len(data)
    while index < length:
        byte = data[index:index + 1]
        if byte in WS_BYTES:
            index += 1
            continue
        if byte == b"%":
            end = data.find(b"\n", index)
            index = length if end < 0 else end + 1
            continue
        if byte == b"(":
            depth = 1
            index += 1
            while index < length and depth:
                char = data[index:index + 1]
                if char == b"\\":
                    index += 2
                    continue
                if char == b"(":
                    depth += 1
                elif char == b")":
                    depth -= 1
                index += 1
            continue
        if byte == b"<":
            if data[index:index + 2] == b"<<":
                depth = 0
                while index < length:
                    if data[index:index + 2] == b"<<":
                        depth += 1
                        index += 2
                        continue
                    if data[index:index + 2] == b">>":
                        depth -= 1
                        index += 2
                        if depth == 0:
                            break
                        continue
                    if data[index:index + 1] == b"(":
                        inner = 1
                        index += 1
                        while index < length and inner:
                            char = data[index:index + 1]
                            if char == b"\\":
                                index += 2
                                continue
                            if char == b"(":
                                inner += 1
                            elif char == b")":
                                inner -= 1
                            index += 1
                        continue
                    index += 1
                continue
            end = data.find(b">", index)
            index = length if end < 0 else end + 1
            continue
        if byte in b"[]{} ":
            index += 1
            continue
        if byte == b"/":
            start = index
            index += 1
            while index < length and data[index:index + 1] not in WS_BYTES \
                    and data[index:index + 1] not in DELIM_BYTES:
                index += 1
            tokens.append(data[start:index])
            continue
        start = index
        while index < length and data[index:index + 1] not in WS_BYTES \
                and data[index:index + 1] not in DELIM_BYTES:
            index += 1
        if index == start:
            index += 1
            continue
        tokens.append(data[start:index])
    return tokens


TEXT_OPERATORS = (b"Tj", b"TJ", b"'", b'"')


def operator_facts(data: bytes) -> dict:
    tokens = content_tokens(data)
    counts = {b"Tj": 0, b"TJ": 0, b"'": 0, b'"': 0}
    do_names: list[str] = []
    font_names: list[str] = []
    for position, token in enumerate(tokens):
        if token in counts:
            counts[token] += 1
        elif token == b"Do" and position:
            name = tokens[position - 1]
            if name.startswith(b"/"):
                do_names.append(name[1:].decode("latin-1"))
        elif token == b"Tf" and position:
            name = tokens[position - 1]
            if name.startswith(b"/"):
                font_names.append(name[1:].decode("latin-1"))
    return {
        "Tj": counts[b"Tj"],
        "TJ": counts[b"TJ"],
        "quote": counts[b"'"],
        "dquote": counts[b'"'],
        "text_ops": sum(counts.values()),
        "do": len(do_names),
        "do_names": do_names,
        "font_names": sorted(set(font_names)),
    }


def is_cjk_char(value: int) -> bool:
    return (0x3400 <= value <= 0x4DBF or 0x4E00 <= value <= 0x9FFF
            or 0xF900 <= value <= 0xFAFF or 0x3040 <= value <= 0x30FF
            or 0x31F0 <= value <= 0x31FF or 0xAC00 <= value <= 0xD7AF)


def cmap_has_cjk(data: bytes) -> bool:
    """The PDF-1c rule: 4 or more `<hex>` targets that are CJK."""
    found = 0
    index = 0
    while index < len(data):
        if data[index:index + 1] != b"<":
            index += 1
            continue
        end = data.find(b">", index + 1)
        if end < 0:
            break
        hex_digits = data[index + 1:end]
        if 4 <= len(hex_digits) <= 6 and all(
                chr(byte) in "0123456789abcdefABCDEF" for byte in hex_digits):
            try:
                if is_cjk_char(int(hex_digits.decode("ascii"), 16)):
                    found += 1
                    if found >= 4:
                        return True
            except ValueError:
                pass
        index = end + 1
    return False


def xref_target(doc, xref: int, path: str) -> int | None:
    """The object number a dictionary entry points at, or None."""
    try:
        kind, value = doc.xref_get_key(xref, path)
    except Exception:  # noqa: BLE001 - a missing entry is data, not a crash
        return None
    if kind == "xref":
        return int(value.split()[0])
    return None


def cmap_facts(doc, xref: int) -> dict:
    """What a `/ToUnicode` CMap stream looks like to a decoder: does it decompress at all, and
    which sections does it use."""
    try:
        data = doc.xref_stream(xref)
    except Exception:  # noqa: BLE001
        return {"decompressed": False}
    if not data:
        return {"decompressed": False}
    return {"decompressed": True, "bytes": len(data),
            "bfchar": data.count(b"beginbfchar"), "bfrange": data.count(b"beginbfrange"),
            "cidrange": data.count(b"begincidrange"), "usecmap": data.count(b"usecmap"),
            "cjk": cmap_has_cjk(data)}


def font_facts(doc, xref: int, resources_xref: int | None) -> dict:
    """Subtype, `/Encoding`, `/ToUnicode` and the PDF-1c CJK hint of one font."""
    subtype = ""
    try:
        _kind, value = doc.xref_get_key(xref, "Subtype")
        subtype = value.lstrip("/") if isinstance(value, str) else ""
    except Exception:  # noqa: BLE001
        pass
    encoding = ""
    try:
        kind, value = doc.xref_get_key(xref, "Encoding")
        encoding = (value if isinstance(value, str) else "")[:60]
        if kind == "xref":
            encoding = f"indirect {value}"
    except Exception:  # noqa: BLE001
        pass
    to_unicode = xref_target(doc, xref, "ToUnicode")
    cmap = cmap_facts(doc, to_unicode) if to_unicode is not None else {"decompressed": False}
    return {"xref": xref, "subtype": subtype, "encoding": encoding,
            "to_unicode": to_unicode is not None, "cjk_cmap": bool(cmap.get("cjk")),
            "cmap": cmap}


def resource_fonts(doc, owner_xref: int) -> list[dict]:
    fonts: list[dict] = []
    try:
        kind, value = doc.xref_get_key(owner_xref, "Resources/Font")
    except Exception:  # noqa: BLE001
        return fonts
    if kind == "xref":
        font_dict = int(value.split()[0])
        try:
            names = doc.xref_get_keys(font_dict)
        except Exception:  # noqa: BLE001
            return fonts
        for name in names:
            target = xref_target(doc, font_dict, name)
            if target is not None:
                facts = font_facts(doc, target, font_dict)
                facts["name"] = name
                fonts.append(facts)
    elif kind == "dict":
        for name, target in re.findall(r"/(\w+)\s+(\d+)\s+\d+\s+R", value):
            facts = font_facts(doc, int(target), None)
            facts["name"] = name
            fonts.append(facts)
    return fonts


def xobject_target(doc, owner_xref: int, name: str) -> int | None:
    return xref_target(doc, owner_xref, f"Resources/XObject/{name}")


def form_facts(doc, xref: int, depth: int = 0) -> dict:
    """Text-showing operators and fonts of a Form XObject, recursing into nested forms."""
    try:
        data = doc.xref_stream(xref) or b""
    except Exception:  # noqa: BLE001
        data = b""
    facts = operator_facts(data)
    fonts = resource_fonts(doc, xref)
    facts["fonts"] = [{"name": font["name"], "subtype": font["subtype"],
                       "to_unicode": font["to_unicode"], "cjk_cmap": font["cjk_cmap"]}
                      for font in fonts]
    facts["has_cjk_font"] = any(font["cjk_cmap"] for font in fonts)
    facts["nested"] = []
    if depth < 4:
        for name in facts["do_names"]:
            target = xobject_target(doc, xref, name)
            if target is None:
                continue
            try:
                subtype = doc.xref_get_key(target, "Subtype")[1]
            except Exception:  # noqa: BLE001
                subtype = ""
            if subtype == "/Form":
                facts["nested"].append(form_facts(doc, target, depth + 1))
    return facts


def spans_by_font(page) -> dict:
    """Reference characters per font on one page, keyed by the page's own resource name (so the
    same names the `/Resources/Font` facts use), to attribute a lost page to a font."""
    counts: collections.Counter = collections.Counter()
    try:
        data = page.get_text("dict")
    except Exception:  # noqa: BLE001
        return {}
    resource_name = {}
    try:
        for font in page.get_fonts(full=True):
            basefont = font[3] if len(font) > 3 else "?"
            name = font[4] if len(font) > 4 else basefont
            resource_name[basefont.split("+")[-1]] = name
    except Exception:  # noqa: BLE001
        pass
    for block in data.get("blocks") or []:
        for line in block.get("lines") or []:
            for span in line.get("spans") or []:
                basefont = (span.get("font") or "?").split("+")[-1]
                counts[resource_name.get(basefont, basefont)] += len(span.get("text") or "")
    return dict(counts.most_common(8))


def font_census(path: str) -> dict:
    """Every font object in the file: subtype, `/ToUnicode` presence, and the CJK hint."""
    import pymupdf

    subtypes: collections.Counter = collections.Counter()
    no_to_unicode: collections.Counter = collections.Counter()
    type3 = []
    with pymupdf.open(path) as document:
        for xref in range(1, document.xref_length()):
            try:
                if document.xref_get_key(xref, "Type")[1] != "/Font":
                    continue
                subtype = document.xref_get_key(xref, "Subtype")[1].lstrip("/")
            except Exception:  # noqa: BLE001
                continue
            subtypes[subtype] += 1
            has_to_unicode = xref_target(document, xref, "ToUnicode") is not None
            if not has_to_unicode:
                no_to_unicode[subtype] += 1
            if subtype == "Type3":
                type3.append({"xref": xref,
                              "to_unicode": has_to_unicode,
                              "encoding": document.xref_get_key(xref, "Encoding")[1][:40]})
    return {"subtypes": dict(subtypes), "no_to_unicode": dict(no_to_unicode),
            "type3": type3[:10], "type3_count": len(type3)}


def form_census(path: str, sample: int = 30) -> dict:
    """Text drawn inside Form XObjects over a page sample: the text PDF-1c only reaches when the
    form carries a CJK font."""
    import pymupdf

    pages_with_text_forms = 0
    text_ops_in_forms = 0
    text_ops_in_cjk_forms = 0
    text_ops_in_other_forms = 0
    sampled = 0
    with pymupdf.open(path) as document:
        total = document.page_count
        step = max(1, total // sample)
        seen: set[int] = set()
        for page_number in range(1, total + 1, step):
            sampled += 1
            page = document.load_page(page_number - 1)
            own_do: list[str] = []
            for xref in page.get_contents() or []:
                try:
                    own_do.extend(operator_facts(document.xref_stream(xref) or b"")["do_names"])
                except Exception:  # noqa: BLE001
                    continue
            found_text = False
            for name in own_do:
                target = xobject_target(document, page.xref, name)
                if target is None or target in seen:
                    continue
                seen.add(target)
                try:
                    if document.xref_get_key(target, "Subtype")[1] != "/Form":
                        continue
                except Exception:  # noqa: BLE001
                    continue
                facts = form_facts(document, target)
                if not facts["text_ops"]:
                    continue
                found_text = True
                text_ops_in_forms += facts["text_ops"]
                if facts["has_cjk_font"]:
                    text_ops_in_cjk_forms += facts["text_ops"]
                else:
                    text_ops_in_other_forms += facts["text_ops"]
            if found_text:
                pages_with_text_forms += 1
    return {"pages_sampled": sampled, "pages_with_text_forms": pages_with_text_forms,
            "forms_seen": len(seen), "text_ops_in_forms": text_ops_in_forms,
            "text_ops_in_cjk_forms": text_ops_in_cjk_forms,
            "text_ops_in_other_forms": text_ops_in_other_forms}


def page_pdf_facts(path: str, page_numbers: list[int]) -> list[dict]:
    """Per page: own content text operators, the forms `Do` reaches, and the fonts."""
    import pymupdf

    results: list[dict] = []
    with pymupdf.open(path) as document:
        for page_number in page_numbers:
            if not 1 <= page_number <= document.page_count:
                continue
            page = document.load_page(page_number - 1)
            own = {"Tj": 0, "TJ": 0, "quote": 0, "dquote": 0, "text_ops": 0, "do": 0,
                   "do_names": [], "font_names": []}
            for xref in page.get_contents() or []:
                try:
                    facts = operator_facts(document.xref_stream(xref) or b"")
                except Exception:  # noqa: BLE001
                    continue
                for key in ("Tj", "TJ", "quote", "dquote", "text_ops", "do"):
                    own[key] += facts[key]
                own["do_names"].extend(facts["do_names"])
                own["font_names"].extend(facts["font_names"])
            own["font_names"] = sorted(set(own["font_names"]))
            forms = []
            for name in own["do_names"]:
                target = xobject_target(document, page.xref, name)
                if target is None:
                    continue
                try:
                    subtype = document.xref_get_key(target, "Subtype")[1]
                except Exception:  # noqa: BLE001
                    subtype = ""
                if subtype != "/Form":
                    continue
                facts = form_facts(document, target)
                facts["name"] = name
                forms.append(facts)
            fonts = resource_fonts(document, page.xref)
            resource_names = {font["name"] for font in fonts}
            content_bytes = 0
            for xref in page.get_contents() or []:
                try:
                    content_bytes += len(document.xref_stream(xref) or b"")
                except Exception:  # noqa: BLE001
                    continue
            spans = spans_by_font(page)
            font_by_name = {font["name"]: font for font in fonts}
            used_fonts = []
            for name, count in list(spans.items())[:4]:
                facts = font_by_name.get(name) or {}
                used_fonts.append({
                    "name": name, "chars": count,
                    "subtype": facts.get("subtype"),
                    "encoding": facts.get("encoding"),
                    "to_unicode": facts.get("to_unicode"),
                    "cmap": facts.get("cmap"),
                    "in_resources": bool(facts),
                })
            results.append({
                "page": page_number,
                "own": own,
                "content_bytes": content_bytes,
                "spans_by_font": spans,
                "used_fonts": used_fonts,
                "tf_fonts_missing_from_resources": [name for name in own["font_names"]
                                                   if name not in resource_names],
                "forms": [{"name": form["name"], "text_ops": form["text_ops"],
                           "has_cjk_font": form["has_cjk_font"],
                           "fonts": form["fonts"],
                           "nested_forms": len(form["nested"])} for form in forms],
                "form_text_ops": sum(form["text_ops"] for form in forms),
                "form_cjk_fonts": sum(1 for form in forms if form["has_cjk_font"]),
                "fonts": [{"name": font["name"], "subtype": font["subtype"],
                           "to_unicode": font["to_unicode"], "cjk_cmap": font["cjk_cmap"]}
                          for font in fonts],
            })
    return results


# --------------------------------------------------------------------------------------------
# Peak resident set of the whole process tree (PDF-7a)
# --------------------------------------------------------------------------------------------

def children_of(pid: int) -> list[int] | None:
    """The direct children of one process, or None when this kernel does not expose them."""
    try:
        with open(os.path.join(PROC, str(pid), "task", str(pid), "children"), "rb") as handle:
            return [int(value) for value in handle.read().split()]
    except (OSError, ValueError):
        return None


def process_tree_by_scan(root_pid: int) -> list[int]:
    """Every live process below `root_pid` by reading every `/proc/<pid>/stat`."""
    children: dict[int, list[int]] = {}
    try:
        names = os.listdir(PROC)
    except OSError:
        return [root_pid]
    for name in names:
        if not name.isdigit():
            continue
        try:
            with open(os.path.join(PROC, name, "stat"), "rb") as handle:
                data = handle.read()
        except OSError:
            continue
        # `comm` may contain spaces and parentheses, so split after the last `)`.
        end = data.rfind(b")")
        if end < 0:
            continue
        fields = data[end + 2:].split()
        if len(fields) < 2:
            continue
        try:
            parent = int(fields[1])
        except ValueError:
            continue
        children.setdefault(parent, []).append(int(name))
    found: list[int] = []
    seen: set[int] = set()
    stack = [root_pid]
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        found.append(pid)
        stack.extend(children.get(pid, ()))
    return found


def process_tree(root_pid: int) -> list[int]:
    """Every live process below `root_pid`, the root included.

    `ru_maxrss` of the direct child is the server alone. The layout backend's text lives in a
    Python worker child, so the cost of an open is a property of the tree, not of one process.

    `/proc/<pid>/task/<pid>/children` costs microseconds, while reading every `/proc/<pid>/stat`
    costs 115 ms on a machine that lists 9,000 of them - enough that a sampler built on the scan
    takes one sample per document instead of forty. The scan stays as the fallback for a kernel
    that does not expose `children` (`CONFIG_PROC_CHILDREN`).
    """
    if children_of(root_pid) is None:
        return process_tree_by_scan(root_pid)
    found: list[int] = []
    seen: set[int] = set()
    stack = [root_pid]
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        found.append(pid)
        stack.extend(children_of(pid) or ())
    return found


def resident_kib(pid: int) -> int:
    try:
        with open(os.path.join(PROC, str(pid), "statm"), "rb") as handle:
            fields = handle.read().split()
    except OSError:
        return 0
    try:
        return int(fields[1]) * (os.sysconf("SC_PAGE_SIZE") // 1024)
    except (IndexError, ValueError, OSError):
        return 0


def cgroup_dir(pid: int) -> str | None:
    """The cgroup v2 path of one process, or None."""
    try:
        with open(os.path.join(PROC, str(pid), "cgroup"), encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("0::"):
                    return CGROUP_ROOT + line[3:].strip()
    except OSError:
        return None
    return None


def read_int_file(path: str) -> int | None:
    try:
        with open(path, encoding="utf-8") as handle:
            return int(handle.read().split()[0])
    except (OSError, ValueError, IndexError):
        return None


def cgroup_events(path: str, key: str) -> int | None:
    """One counter of a cgroup's `memory.events` (for example `oom_kill`), or None."""
    try:
        with open(os.path.join(path, "memory.events"), encoding="utf-8") as handle:
            for line in handle:
                name, _, value = line.partition(" ")
                if name == key:
                    return int(value)
    except (OSError, ValueError):
        return None
    return None


def cgroup_procs(path: str) -> set[int] | None:
    try:
        with open(os.path.join(path, "cgroup.procs"), encoding="utf-8") as handle:
            return {int(line) for line in handle if line.strip().isdigit()}
    except (OSError, ValueError):
        return None


class TreePeak(threading.Thread):
    """The peak resident set of the whole server tree, sampled while the server runs.

    Two sources. The sampler sums `VmRSS` over the tree every 50 ms, which can miss a worker
    that lives between two samples. `systemd-run --scope` also puts the command in a cgroup
    whose `memory.peak` is exact and includes processes that already exited, and whose
    `memory.events` counts the OOM kills the 600 MB cap makes; the cgroup is only accepted when
    its `cgroup.procs` is a subset of this tree, so the shell's own session scope (which holds
    the `sudo` wrapper) is never mistaken for the server's. The cgroup wins when it could be
    identified: it is exactly what the cap governs, while the sampled sum also carries the
    wrapper the harness starts the server under.
    """

    def __init__(self, root_pid: int, interval: float = TREE_SAMPLE_SECONDS) -> None:
        super().__init__(daemon=True)
        self.root_pid = root_pid
        self.interval = interval
        self.halt = threading.Event()
        self.sampled_kib = 0
        self.single_kib = 0
        self.cgroup_kib = 0
        self.oom_kill = 0
        self.samples = 0
        self.cgroup_path: str | None = None
        self._root_cgroup = cgroup_dir(root_pid)

    def run(self) -> None:
        while not self.halt.wait(self.interval):
            try:
                self.sample()
            except Exception:  # noqa: BLE001 - sampling must never end a document
                pass

    def sample(self) -> None:
        self.samples += 1
        self.measure()

    def snapshot(self) -> None:
        """Measure now, without counting a sample.

        `memory.peak` is monotonic for the life of the scope, so one read at the end of the open
        is exact even when the peak happened 10 ms into it and the sampler never saw it.
        """
        try:
            self.measure()
        except Exception:  # noqa: BLE001 - measuring must never end a document
            pass

    def measure(self) -> None:
        pids = process_tree(self.root_pid)
        tree = set(pids)
        sizes = [(resident_kib(pid), pid) for pid in pids]
        self.sampled_kib = max(self.sampled_kib, sum(size for size, _ in sizes))
        if sizes:
            self.single_kib = max(self.single_kib, max(size for size, _ in sizes))
        if self.cgroup_path is None:
            best: tuple[str, int] | None = None
            for pid in pids:
                path = cgroup_dir(pid)
                if path is None or path == self._root_cgroup:
                    continue
                if read_int_file(os.path.join(path, "memory.peak")) is None:
                    continue
                members = cgroup_procs(path)
                if not members or not members <= tree:
                    continue
                if best is None or len(members) < best[1]:
                    best = (path, len(members))
            if best is not None:
                self.cgroup_path = best[0]
        if self.cgroup_path is not None:
            # `memory.peak` is bytes; the sampled sum is KiB.
            peak_bytes = read_int_file(os.path.join(self.cgroup_path, "memory.peak"))
            if peak_bytes is not None:
                self.cgroup_kib = max(self.cgroup_kib, peak_bytes // 1024)
            kills = cgroup_events(self.cgroup_path, "oom_kill")
            if kills is not None:
                self.oom_kill = max(self.oom_kill, kills)

    def stop(self) -> dict:
        self.halt.set()
        self.join(timeout=10.0)
        self.snapshot()
        # The scope is exactly the server and the worker the cap governs; the sampled sum also
        # carries the `sudo`/`systemd-run` wrapper the harness starts the server under, so the
        # cgroup is preferred when it could be identified.
        if self.cgroup_kib:
            peak, source = self.cgroup_kib, "cgroup"
        else:
            peak, source = self.sampled_kib, "sampled"
        return {
            "peak_tree_kib": peak,
            "peak_sampled_kib": self.sampled_kib,
            "peak_single_kib": self.single_kib,
            "peak_cgroup_kib": self.cgroup_kib or None,
            "peak_cgroup_path": self.cgroup_path,
            "peak_source": source,
            "tree_samples": self.samples,
            "oom_kill": self.oom_kill,
        }


def peak_kib(record: dict) -> int:
    """The peak resident set of one open, in KiB.

    The larger of what the sampler saw over the tree and the kernel's `ru_maxrss` for the process
    the harness started. The second is the peak of the largest single process in that tree over
    its whole life, so the tree's peak is never below it, and a transient shorter than the 50 ms
    sampling interval shows up only there. Neither source can be trusted alone on every runner:
    the scope's `memory.peak` missed a startup transient in every one of the 121 default-backend
    documents, while `ru_maxrss` cannot see two processes peaking at once.
    """
    resources = record.get("resources") or {}
    return max(resources.get("peak_tree_kib") or 0, record.get("peak_server_kib") or 0)


def peak_source(record: dict) -> str:
    """Which of the two sources `peak_kib` took."""
    resources = record.get("resources") or {}
    if (record.get("peak_server_kib") or 0) > (resources.get("peak_tree_kib") or 0):
        return "ru_maxrss"
    return resources.get("peak_source") or "?"


# --------------------------------------------------------------------------------------------
# Why the layout worker refused (PDF-7a)
# --------------------------------------------------------------------------------------------

# `layout_pdf.rs` maps the worker's `OCR_REQUIRED` to a message that does not carry the worker's
# own reason (`OcrRequiredDetail::unmeasured`, kept by PDF-4a because the layout path measures
# neither the page nor the character count). The reason decides whether the refusal is about a
# text page's figure or about a scan, so the harness runs the worker source itself, unmodified,
# with its coverage predicate wrapped: every `OcrRequired` records the page and the message.
REFUSAL_PROBE = '''
import json, os, sys
worker_path = sys.argv[6]
with open(worker_path, encoding="utf-8") as handle:
    source = handle.read()
namespace = {"__name__": "pdf_layout_worker", "__file__": worker_path}
exec(compile(source, worker_path, "exec"), namespace)
OcrRequired = namespace["OcrRequired"]
original = namespace["require_disabled_page_coverage"]
found = []
checked = []
def spy(page, page_layout):
    try:
        result = original(page, page_layout)
    except OcrRequired as error:
        found.append({"page": page.number + 1, "reason": str(error)})
        raise
    checked.append(page.number + 1)
    return result
namespace["require_disabled_page_coverage"] = spy
saved = sys.stdout
code = 0
sys.stdout = open(os.devnull, "w")
try:
    namespace["main"]()
except OcrRequired:
    code = 1
except Exception as error:
    found.append({"page": None, "reason": "%s: %s" % (type(error).__name__, str(error)[:200])})
    code = 2
finally:
    sys.stdout.close()
    sys.stdout = saved
print(json.dumps({"schema": "pdf7a-refusal/v1", "code": code,
                  "pages_checked": len(checked), "refusals": found}))
'''


def refusal_reason(layout_python: str, worker_source: str, path: str) -> dict:
    """Run the worker source the binary embeds and report why it refused this PDF."""
    if not worker_source or not os.path.isfile(worker_source):
        return {"error": f"worker source not found: {worker_source}"}
    command = [layout_python, "-I", "-B", "-c", REFUSAL_PROBE,
               str(WORKER_PAGE_BUDGET), str(WORKER_BYTE_BUDGET), str(WORKER_CHAR_BUDGET),
               "", "", os.path.abspath(worker_source)]
    try:
        with open(path, "rb") as handle:
            completed = subprocess.run(command, stdin=handle, capture_output=True,
                                       timeout=REFUSAL_TIMEOUT_S)
    except (OSError, subprocess.SubprocessError) as error:
        return {"error": f"{type(error).__name__}: {str(error)[:200]}"}
    stderr = completed.stderr.decode("utf-8", "replace").strip().splitlines()
    for line in reversed(completed.stdout.decode("utf-8", "replace").splitlines()):
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            data = json.loads(line)
        except json.JSONDecodeError:
            continue
        if data.get("schema") == "pdf7a-refusal/v1":
            data["stderr_tail"] = stderr[-1][:200] if stderr else None
            return data
    return {"error": "no refusal record", "rc": completed.returncode,
            "stderr_tail": stderr[-1][:200] if stderr else None}


# --------------------------------------------------------------------------------------------
# Reading the document through the server
# --------------------------------------------------------------------------------------------

def collect_units(server, document_id: str, sections: list[dict], deadline: float,
                  requested_kind: str = "sentence") -> dict:
    """Every unit of `requested_kind`, with the unit kind and the degradation the server
    reports. `sentence` is the only kind a run without --keep-pdf-sentence-text asks for,
    and its stream is unchanged."""
    return collect_units_v2(server, document_id, deadline, requested_kind)


# The v2 `read` does not carry the v1 coverage block. The degradation and content-class strings
# below are the ones the v1 stream writes for the same paragraph class (text_unit_stream.rs
# `coarse_degradation` / `content_class`), so the v2 records stay comparable.
V2_NON_PROSE_KINDS = ("code_block", "preformatted", "table")
V2_CONTAINER_KINDS = ("block_quote", "list_item")


def v2_unit_evidence(kind: str | None, requested_kind: str) -> tuple[str | None, str | None,
                                                                   str | None, str | None]:
    """(degradation, effective_kind, content_class, content_class_detail) for one v2 item."""
    effective = "paragraph" if kind or requested_kind == "paragraph" else "sentence"
    if kind is None:
        return None, effective, None, None
    if kind in V2_NON_PROSE_KINDS:
        return ("requested_sentence_but_non_prose_is_paragraph_only", effective,
                "non_prose", kind)
    if kind in V2_CONTAINER_KINDS:
        return ("flat_native_container_no_nested_textunit_evidence", effective, "unknown", kind)
    return "requested_sentence_but_paragraph_is_coarse_only", effective, "unknown", kind


def collect_units_v2(server, document_handle: str, deadline: float,
                     requested_kind: str = "sentence") -> dict:
    """Every unit of `requested_kind` through sequential v2 `read` batches to the end.

    The section and paragraph of every item come from its handle (`s<N>.p<M>…`). There is no
    v1 coverage block: `coverage` stays empty and the document record derives the paragraph
    count from the distinct `(section, paragraph)` pairs in the handles.
    """
    units: list[dict] = []
    calls = 0
    chars = 0
    truncated = False
    error = None
    started = time.monotonic()
    unit = "paragraph" if requested_kind == "paragraph" else None
    from_handle = document_handle
    seen: set[str] = set()
    while True:
        if chars >= TEXT_BUDGET_CHARS or time.monotonic() > deadline:
            truncated = True
            break
        try:
            data = PROBE.read_batch(server, from_handle, unit=unit)
        except PROBE.ProbeError as exc:
            error = str(exc)[:200]
            break
        calls += 1
        for handle, text, kind in PROBE.v2_items(data):
            degradation, effective, content_class, detail = v2_unit_evidence(
                kind, requested_kind)
            units.append({
                "text": text,
                "degradation": degradation,
                "effective_kind": effective,
                "content_class": content_class,
                "content_class_detail": detail,
                "section_id": PROBE.handle_section(handle),
                "paragraph_index": PROBE.handle_paragraph_index(handle),
            })
            chars += len(text)
        next_handle = data.get("next")
        if not isinstance(next_handle, str):
            break
        if next_handle in seen:
            error = "v2 read next handle repeated"
            break
        seen.add(next_handle)
        from_handle = next_handle
    return {"units": units, "coverage": [], "calls": calls, "chars": chars,
            "truncated": truncated, "error": error,
            "seconds": round(time.monotonic() - started, 2)}


# --------------------------------------------------------------------------------------------
# Per-document measurement
# --------------------------------------------------------------------------------------------

def unit_pages(units: list[dict], page_index: PageIndex) -> list[int | None]:
    """The reference page of every unit, filled in from a neighbour when the unit is too short
    to search for (a one- or two-character unit cannot be located by its opening characters)."""
    pages: list[int | None] = []
    for unit in units:
        text = unit["text"].strip()
        pages.append(page_index.page_of(text) if len(text) >= 10 else None)
    filled = list(pages)
    for index, page in enumerate(pages):
        if page is not None:
            continue
        for offset in (1, -1, 2, -2, 3, -3):
            other = index + offset
            if 0 <= other < len(pages) and pages[other] is not None:
                filled[index] = pages[other]
                break
    return filled


def measure_units(units: list[dict], reference: dict, pages: list[int | None],
                  page_count: int, keep_coarse_text: bool = False) -> dict:
    """Classify the long and the short units and the what-if of every long cause.

    With `keep_coarse_text`, every unit the server returned as a coarse paragraph - at any
    length, not only the long ones - is also recorded with its text exactly as returned. The
    long and short records are unchanged either way, so a run without the flag writes the same
    bytes it always did."""
    reference_words = reference_tokens("\n".join(reference["texts"]))
    edges = edge_digits(reference["lines"])
    page_reference_shares: dict[int, float | None] = {
        number + 1: whitespace_share(text) for number, text in enumerate(reference["texts"])
    }
    texts = [unit["text"] for unit in units]
    long_units: list[dict] = []
    short_units: list[dict] = []
    coarse_units: list[dict] = []
    long_by_cause: collections.Counter = collections.Counter()
    long_by_kind: collections.Counter = collections.Counter()
    short_by_cause: collections.Counter = collections.Counter()
    what_if_totals = {cause: {"fixed_units": 0, "added_units": 0, "remaining_long": 0}
                      for cause in LONG_CAUSES}

    for index, unit in enumerate(units):
        text = texts[index]
        stripped = text.strip()
        length = len(stripped)
        page = pages[index] if index < len(pages) else None
        if keep_coarse_text and unit.get("degradation"):
            coarse_units.append({
                "kind_record": "coarse_unit", "unit": index, "chars": length,
                "page": page, "degradation": unit.get("degradation"),
                "effective_kind": unit.get("effective_kind"),
                "content_class": unit.get("content_class"),
                "content_class_detail": unit.get("content_class_detail"),
                "text": text,
            })
        if length > LONG_CHARS:
            verdict = classify_long(stripped, whitespace_share(stripped),
                                    page_reference_shares.get(page) if page else None,
                                    reference_words)
            cause = verdict["cause"]
            long_by_cause[cause] += 1
            long_by_kind["coarse_paragraph" if unit.get("degradation") else "sentence"] += 1
            split = what_if(stripped, verdict["cuts"])
            what_if_totals[cause]["fixed_units"] += 1
            what_if_totals[cause]["added_units"] += split["pieces"] - 1
            what_if_totals[cause]["remaining_long"] += split["pieces_long"]
            long_units.append({
                "kind_record": "long_unit", "unit": index, "chars": length,
                "cause": cause, "page": page, "degradation": unit.get("degradation"),
                "effective_kind": unit.get("effective_kind"),
                "evidence": verdict["evidence"],
                "pieces": split["pieces"], "pieces_long": split["pieces_long"],
                "head": clipped(stripped), "tail": clipped(stripped[-120:]),
            })
        elif length < SHORT_CHARS:
            verdict = classify_short(stripped, page, page_count,
                                     texts[index - 1] if index else "",
                                     texts[index + 1] if index + 1 < len(texts) else "",
                                     reference_words, edges)
            short_by_cause[verdict["cause"]] += 1
            short_units.append({
                "kind_record": "short_unit", "unit": index, "chars": length,
                "cause": verdict["cause"], "page": page,
                "degradation": unit.get("degradation"),
                "effective_kind": unit.get("effective_kind"),
                "joined": verdict.get("joined"),
                "page_matched": verdict.get("page_matched"),
                "text": clipped(stripped, 60),
                "before": (clipped(texts[index - 1], 40)[-40:] if index else ""),
                "after": (clipped(texts[index + 1], 40)[:40]
                          if index + 1 < len(texts) else ""),
            })
    return {
        "long": {"units": len(long_units), "by_cause": dict(long_by_cause),
                 "by_kind": dict(long_by_kind)},
        "short": {"units": len(short_units), "by_cause": dict(short_by_cause)},
        "what_if": what_if_totals,
        "long_units": long_units,
        "short_units": short_units,
        "coarse_units": coarse_units,
        "total_units": len(units),
    }


HEADER_TIERS = (("spec", HEADER_PAGE_SHARE, PAGENUM_PAGE_SHARE),
                ("loose", 0.3, 0.3))

# A broken sentence (PDF-7b-1): a prose unit that does not end with a
# terminal, followed in the same section by a unit that starts lowercase.
# Headings (short paragraph units without a terminal) and non-prose units
# are not sentences and cannot be broken.
SENTENCE_TERMINALS = ".?!\u3002\uff1f\uff01"
TERMINAL_CLOSERS = "\u201d\u2019\u00bb)]}" + "\u3009\u300b\u3011\u3010\u300f\u300d"


def ends_with_terminal(text: str) -> bool:
    """The unit's last non-whitespace character is a terminal, or a closing
    quote/bracket after one."""
    text = text.rstrip()
    while text and text[-1] in TERMINAL_CLOSERS:
        text = text[:-1].rstrip()
    return bool(text) and text[-1] in SENTENCE_TERMINALS


def broken_sentence_count(units: list[dict]) -> int:
    """Units that are half of a sentence: not a heading (a short paragraph
    unit is the heading shape) and not non-prose, no terminal at the end,
    and the next unit of the same section opens lowercase."""
    broken = 0
    for index, unit in enumerate(units):
        if unit.get("content_class") == "non_prose":
            continue
        text = (unit.get("text") or "").strip()
        if not text:
            continue
        if unit.get("effective_kind") == "paragraph" and len(text) <= 120:
            continue
        if ends_with_terminal(text):
            continue
        for follow in units[index + 1:]:
            if follow.get("section_id") != unit.get("section_id"):
                break
            follow_text = (follow.get("text") or "").lstrip()
            if not follow_text:
                continue
            if follow_text[0].islower():
                broken += 1
            break
    return broken


def header_measurement(reference: dict, units: list[dict]) -> dict:
    """Both the thresholds the task fixes (80% / 50%) and PDF-5a's 30% proxy, because the
    interesting question is which tier the real running headers land in."""
    counts: collections.Counter = collections.Counter()
    for lines in reference["lines"]:
        for line in set(lines):
            counts[line] += 1
    top_lines = [{"line": clipped(line, 120), "pages": count,
                  "share": round(count / reference["pages"], 3) if reference["pages"] else None}
                 for line, count in counts.most_common(5)]
    unit_texts = [normalize(unit["text"]) for unit in units]
    squashed_units = [CORPUS.squash(unit["text"]) for unit in units]
    tiers = {}
    for name, line_share, page_share in HEADER_TIERS:
        candidates = header_candidates(reference["lines"], reference["pages"],
                                       line_share, page_share)
        results = []
        totals: collections.Counter = collections.Counter()
        positions: collections.Counter = collections.Counter()
        for candidate in candidates:
            placement = header_placement(candidate, reference["lines"], unit_texts,
                                        squashed_units)
            totals.update(placement["cases"])
            positions.update(placement["positions"])
            results.append({"line": clipped(candidate["line"], 120),
                            "kind": candidate["kind"], "pages": candidate["pages"],
                            "cases": placement["cases"],
                            "positions": placement["positions"]})
        tiers[name] = {"candidates": results, "cases": dict(totals),
                       "positions": dict(positions)}
    return {"tiers": tiers, "top_lines": top_lines,
            "lines_total": sum(len(lines) for lines in reference["lines"])}


def loss_measurement(path: str, reference: dict, units: list[dict],
                     pages: list[int | None], quality: dict) -> dict:
    """H6: backend against reference characters per page, plus the worst pages' PDF facts."""
    page_chars = [0] * (reference["pages"] + 1)
    for unit, page in zip(units, pages):
        if page is not None and 1 <= page <= reference["pages"]:
            page_chars[page] += len(unit["text"])
    deficits = []
    for number in range(1, reference["pages"] + 1):
        reference_chars = reference["chars"][number - 1]
        backend_chars = page_chars[number]
        deficits.append({
            "page": number,
            "reference_chars": reference_chars,
            "backend_chars": backend_chars,
            "ratio": round(backend_chars / reference_chars, 3) if reference_chars else None,
        })
    for item in deficits:
        item["deficit"] = item["reference_chars"] - item["backend_chars"]
    worst = sorted(deficits, key=lambda item: -item["deficit"])[:WORST_PAGES]
    pdf_facts = page_pdf_facts(path, [item["page"] for item in worst])
    for item, facts in zip(worst, pdf_facts):
        item["pdf"] = facts
    return {
        "containment_first20": quality.get("containment_first20"),
        "reference_chars": reference["chars_total"],
        "backend_chars": sum(len(unit["text"]) for unit in units),
        "pages": deficits,
        "worst_pages": worst,
        "fonts": reference["fonts"],
        "font_census": font_census(path),
        "form_census": form_census(path),
    }


def document_record(entry: dict, args, deadline: float) -> tuple[dict, list[dict]]:
    record: dict = {
        "kind_record": "document", "id": entry["id"], "group": entry["group"],
        "title": entry.get("title"), "pages": entry.get("pages"),
        "producer": entry.get("producer"), "creator": entry.get("creator"),
        "outcome": None, "error": None, "phase": "download",
        "sections": None, "structure_mode": None, "units": None,
        "backend": args.backend,
    }
    started = time.perf_counter()
    path, status = SWEEP.download_pdf(entry, args.work_dir)
    if path is None:
        record.update({"outcome": "download_failed", "error": status.get("error")})
        record["seconds"] = round(time.perf_counter() - started, 1)
        return record, []

    record["phase"] = "reference"
    reference = reference_pages(path)
    record["pages"] = reference["pages"]
    page_index = PageIndex(reference["texts"])

    state_dir = os.path.join(args.state_root, f"open-{entry['id'].replace('/', '_')}")
    shutil.rmtree(state_dir, ignore_errors=True)
    os.makedirs(state_dir, exist_ok=True)
    env = PROBE.server_env(state_dir, os.path.dirname(os.path.abspath(path)), args.backend,
                           args.layout_python)
    cap = SWEEP.CAP if args.cap_available else None
    command = PROBE.cap_command(args.binary, cap, env)
    server = PROBE.Server(command, env, os.path.join(state_dir, "server.log"),
                          PROBE.TOOL_TIMEOUT_S)
    peak = TreePeak(server.pid)
    peak.start()
    document_id = None
    units: list[dict] = []
    coverage: list[dict] = []
    read: dict = {}
    paragraph_records: list[dict] = []
    try:
        server.initialize()
        peak.snapshot()
        record["phase"] = "open"
        open_started = time.perf_counter()
        try:
            result = server.tool("open",
                                 {"source": path}, timeout=OPEN_TIMEOUT_S)
        except PROBE.ProbeError as exc:
            # A worker that dies prints its whole traceback into the server's message, and the
            # Python layout worker's onnxruntime warnings come first, so the reason a document
            # failed is at the end of the string, not at the start.
            text = str(exc)
            record["error"] = text if len(text) <= 800 else f"{text[:400]} ... {text[-400:]}"
            record["error_code"] = exc.code
            result = None
        record["open_s"] = round(time.perf_counter() - open_started, 3)
        peak.snapshot()
        if result is not None:
            document_id = PROBE.document_handle(PROBE.payload(result) or {})
        if document_id:
            record["phase"] = "structure"
            sections = PROBE.read_sections(server, document_id)
            record["sections"] = len(sections)
            record["structure_mode"] = (
                "page_fallback" if PROBE.is_page_fallback(sections) else "headings")
            record["phase"] = "read"
            read = collect_units(server, document_id, sections, deadline)
            units = read["units"]
            coverage = read["coverage"]
            record["read"] = {key: read[key] for key in
                              ("calls", "chars", "truncated", "error", "seconds")}
            # PDF-6g: with --keep-pdf-sentence-text, also read the paragraph stream so every
            # paragraph's full text can be re-segmented offline by the Python port of
            # `sentence_ranges`. The read is a second enumeration inside the same open; only
            # the new record kind carries it. The paragraph stream does not carry
            # `degradation` (a sentence-mode verdict), so a refused prose paragraph is marked
            # through the sentence stream's degraded units, keyed by the paragraph index the
            # locator carries.
            if args.keep_pdf_sentence_text:
                paragraph_read = collect_units(server, document_id, sections, deadline,
                                               "paragraph")
                record["paragraph_read"] = {key: paragraph_read[key] for key in
                                            ("calls", "chars", "truncated", "error",
                                             "seconds")}
                degraded_keys = collections.Counter(
                    (unit["section_id"], unit.get("paragraph_index"))
                    for unit in units if unit.get("degradation"))
                paragraph_records = []
                for index, unit in enumerate(paragraph_read["units"]):
                    key = (unit["section_id"], unit.get("paragraph_index"))
                    coarse = degraded_keys.get(key, 0) > 0
                    if coarse:
                        degraded_keys[key] -= 1
                    paragraph_records.append({
                        "kind_record": "pdf_paragraph",
                        "id": entry["id"],
                        "unit": index,
                        "section_id": unit["section_id"],
                        "paragraph_index": unit.get("paragraph_index"),
                        "content_class": unit.get("content_class"),
                        "content_class_detail": unit.get("content_class_detail"),
                        "degradation": (unit.get("degradation")
                                        or ("paragraph_is_coarse_only" if coarse else None)),
                        "chars": len(unit["text"]),
                        "text": unit["text"],
                    })
                paragraph_records.append({
                    "kind_record": "pdf_stream_totals",
                    "id": entry["id"],
                    "sentence_total_items": len(read["units"]),
                    "paragraph_total_items": len(paragraph_read["units"]),
                    "sentence_truncated": read["truncated"],
                    "paragraph_truncated": paragraph_read["truncated"],
                })
            record["outcome"] = "ok"
            peak.snapshot()
    finally:
        server.close(grace=30.0)
        # Stop the sampler after the server is gone so a worker outliving the server is still
        # counted, but before the scope's cgroup disappears.
        record["resources"] = peak.stop()
    record["server_rc"] = server.rc
    record["killed"] = server.rc in PROBE.KILLED_CODES
    record["peak_server_kib"] = server.peak_kib()
    if record["outcome"] is None:
        record["outcome"] = (CAP_KILLED if record["killed"]
                             else SWEEP.classify(record, "timed out" in str(record.get("error")),
                                                 str(record.get("error") or "")))
    # The cap is on the whole scope, so it can kill the worker while the server survives and
    # reports a parse failure. The cgroup's own OOM counter is the only exact witness.
    if record["outcome"] != "ok" and (record["resources"].get("oom_kill") or 0) > 0:
        record["outcome"] = CAP_KILLED
    if (record["outcome"] == "ocr_required" and args.backend == "layout"):
        record["refusal"] = refusal_reason(args.layout_python, args.worker_source, path)
    if not units:
        record["seconds"] = round(time.perf_counter() - started, 1)
        return record, []

    record["phase"] = "measure"
    backend_text = "\n".join(unit["text"] for unit in units)
    reference_text = "\n".join(reference["texts"])
    backend_share = whitespace_share(backend_text)
    reference_share = whitespace_share(reference_text)
    paragraphs = sum(int((item["coverage"] or {}).get("paragraph_count") or 0)
                     for item in coverage)
    # No v1 coverage block: the paragraph count is the distinct (section, paragraph) pairs
    # the sentence handles name. `sections_with_coverage` is the same set of sections.
    paragraph_keys = {(unit["section_id"], unit["paragraph_index"])
                      for unit in units if unit.get("paragraph_index") is not None}
    paragraphs = len(paragraph_keys)
    sections_with_coverage = len({unit["section_id"] for unit in units})
    quality = {
        "containment_first20": gram_containment(reference["first20_text"], backend_text),
        "backend_chars": len(backend_text),
        "reference_chars": reference["chars_total"],
        "char_ratio": (round(len(backend_text) / reference["chars_total"], 3)
                       if reference["chars_total"] else None),
    }
    quality.update(bad_char_shares(backend_text))
    record["whitespace"] = {
        "backend_share": round(backend_share, 5) if backend_share is not None else None,
        "reference_share": round(reference_share, 5) if reference_share is not None else None,
        "ratio": (round(backend_share / reference_share, 3)
                  if backend_share is not None and reference_share else None),
        "below_70pct": (backend_share is not None and reference_share is not None
                        and backend_share < WS_REFERENCE_FLOOR * reference_share),
        "backend_newlines": backend_text.count("\n"),
        "reference_newlines": reference_text.count("\n"),
        "backend_newlines_per_page": round(backend_text.count("\n") / reference["pages"], 2)
        if reference["pages"] else None,
        "reference_newlines_per_page": round(reference_text.count("\n") / reference["pages"], 2)
        if reference["pages"] else None,
    }
    record["paragraphs"] = {
        "count": paragraphs,
        "per_page": round(paragraphs / reference["pages"], 2) if reference["pages"] else None,
        "sections_with_coverage": sections_with_coverage,
    }
    record["quality"] = quality
    pages = unit_pages(units, page_index)
    record["pages_matched"] = sum(1 for page in pages if page is not None)
    measured = measure_units(units, reference, pages, reference["pages"],
                             args.keep_coarse_text)
    long_units = measured.pop("long_units")
    short_units = measured.pop("short_units")
    coarse_units = measured.pop("coarse_units")
    record["long"] = measured["long"]
    record["long"]["share"] = (round(measured["long"]["units"] / measured["total_units"], 4)
                               if measured["total_units"] else None)
    record["short"] = measured["short"]
    record["short"]["share"] = (round(measured["short"]["units"] / measured["total_units"], 4)
                                if measured["total_units"] else None)
    record["what_if"] = measured["what_if"]
    record["units"] = measured["total_units"]
    record["headers"] = header_measurement(reference, units)
    record["broken_sentences"] = broken_sentence_count(units)

    suspect_loss = ((quality["containment_first20"] is not None
                     and quality["containment_first20"] < LOSS_CONTAINMENT)
                    or quality["private_use_share"] + quality["replacement_share"]
                    > BAD_CHAR_SHARE)
    if suspect_loss:
        record["loss"] = loss_measurement(path, reference, units, pages, quality)

    detail = []
    if args.keep_pdf_sentence_text:
        for unit in paragraph_records:
            unit.setdefault("doc_units", len(units))
            detail.append(unit)
    for unit in long_units:
        unit["id"] = entry["id"]
        unit["doc_units"] = measured["total_units"]
        detail.append(unit)
    for unit in short_units:
        unit["id"] = entry["id"]
        unit["doc_units"] = measured["total_units"]
        detail.append(unit)
    for unit in coarse_units:
        unit["id"] = entry["id"]
        unit["doc_units"] = measured["total_units"]
        detail.append(unit)
    record["seconds"] = round(time.perf_counter() - started, 1)
    return record, detail


def venv_facts(path: str) -> dict:
    """The bytes of the pinned layout environment, as `du -sh` and `du -sb` report them."""
    if not path:
        return {"venv_path": None}
    facts: dict = {"venv_path": path}
    for flag, key in (("-sb", "venv_bytes"), ("-sh", "venv_du")):
        try:
            completed = subprocess.run(["du", flag, path], capture_output=True, timeout=300)
            facts[key] = completed.stdout.decode("utf-8", "replace").split()[0] or None
        except (OSError, subprocess.SubprocessError, IndexError):
            facts[key] = None
    try:
        facts["venv_bytes"] = int(facts.get("venv_bytes") or 0) or None
    except (TypeError, ValueError):
        facts["venv_bytes"] = None
    return facts


def run_shard(args) -> int:
    entries = SWEEP.load_list(args.ids)
    if not entries:
        print("::error::the corpus list is empty")
        return 1
    shard = [entry for index, entry in enumerate(entries) if index % args.shards == args.shard]
    print(f"shard {args.shard}/{args.shards}: {len(shard)} documents, backend {args.backend}",
          flush=True)
    print("| id | pages | outcome | units | ws share | ref ws | ws ratio | long | long % | "
          "short | short % | own unit | open s | peak tree MiB | oom kills | s |", flush=True)
    os.makedirs(args.state_root, exist_ok=True)
    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)
    environment = {"kind_record": "environment", "backend": args.backend,
                   "layout_python": args.layout_python or None,
                   "cap": SWEEP.CAP if args.cap_available else None}
    environment.update(venv_facts(args.venv_path) if args.backend == "layout" else
                       {"venv_path": None})
    append_jsonl(args.out, environment)
    shard_deadline = time.monotonic() + args.deadline_minutes * 60
    for entry in shard:
        if time.monotonic() > shard_deadline:
            append_jsonl(args.out, {"kind_record": "document", "id": entry["id"],
                                    "group": entry["group"], "pages": entry.get("pages"),
                                    "outcome": "not_run", "error": "shard deadline",
                                    "backend": args.backend})
            print(f"| {entry['id']} | {entry.get('pages')} | not_run | - | - | - | - | - | - | "
                  f"- | - | - | - | - | - | - |", flush=True)
            continue
        deadline = min(shard_deadline, time.monotonic() + args.per_document_minutes * 60)
        try:
            record, detail = document_record(entry, args, deadline)
        except Exception as exc:  # noqa: BLE001 - one document must not end the shard
            record = {"kind_record": "document", "id": entry["id"],
                      "group": entry["group"], "pages": entry.get("pages"),
                      "outcome": "measure_failed", "backend": args.backend,
                      "error": f"{type(exc).__name__}: {str(exc)[:300]}"}
            detail = []
        append_jsonl(args.out, record)
        for unit in detail:
            append_jsonl(args.out, unit)
        whitespace = record.get("whitespace") or {}
        long_stats = record.get("long") or {}
        short_stats = record.get("short") or {}
        headers = record.get("headers") or {}
        own = (headers.get("cases") or {}).get("own_unit", 0)
        resources = record.get("resources") or {}
        print(f"| {entry['id']} | {record.get('pages')} | {record.get('outcome')} | "
              f"{fmt(record.get('units'))} | {fmt(whitespace.get('backend_share'), 4)} | "
              f"{fmt(whitespace.get('reference_share'), 4)} | {fmt(whitespace.get('ratio'), 3)} "
              f"| {fmt(long_stats.get('units'))} | {fmt(long_stats.get('share'), 4)} | "
              f"{fmt(short_stats.get('units'))} | {fmt(short_stats.get('share'), 4)} | "
              f"{own} | {fmt(record.get('open_s'), 2)} | "
              f"{fmt(peak_kib(record) / 1024, 1)} | "
              f"{fmt(resources.get('oom_kill'))} | {fmt(record.get('seconds'), 1)} |", flush=True)
    return 0


# --------------------------------------------------------------------------------------------
# Aggregate
# --------------------------------------------------------------------------------------------

def load_records(root: str) -> tuple[list[dict], list[dict], list[dict], list[dict]]:
    documents: list[dict] = []
    long_units: list[dict] = []
    short_units: list[dict] = []
    environment: list[dict] = []
    for base, _, names in os.walk(root):
        for name in sorted(names):
            if not name.endswith(".jsonl"):
                continue
            with open(os.path.join(base, name), encoding="utf-8") as handle:
                for line in handle:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        record = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    kind = record.get("kind_record")
                    if kind == "document":
                        documents.append(record)
                    elif kind == "long_unit":
                        long_units.append(record)
                    elif kind == "short_unit":
                        short_units.append(record)
                    elif kind == "environment":
                        environment.append(record)
    return documents, long_units, short_units, environment


def sample_of(units: list[dict], key: str = "head", limit: int = 3) -> list[str]:
    seen = set()
    samples = []
    for unit in units:
        text = clipped(unit.get(key) or unit.get("text") or "", 200)
        if not text or text in seen:
            continue
        seen.add(text)
        samples.append(f"`{unit['id']}` {text}")
        if len(samples) >= limit:
            break
    return samples


def aggregate_mode(args) -> int:
    documents, long_units, short_units, environment = load_records(args.jsonl_dir)
    if not documents:
        print(f"::error::no document records under {args.jsonl_dir}")
        return 1
    docs = [doc for doc in documents if doc.get("outcome") == "ok"]
    not_run = [doc for doc in documents if doc.get("outcome") == "not_run"]
    failed = [doc for doc in documents
              if doc.get("outcome") not in ("ok", "not_run")]
    total_units = sum(doc.get("units") or 0 for doc in docs)
    long_total = sum((doc.get("long") or {}).get("units") or 0 for doc in docs)
    short_total = sum((doc.get("short") or {}).get("units") or 0 for doc in docs)
    lines: list[str] = [
        "# PDF-6a text-quality measurement", "",
        f"Documents measured: {len(docs)} of {len(documents)} "
        f"({len(not_run)} not run, {len(failed)} failed). "
        f"Units: {total_units}, long (>800): {long_total}, short (<5): {short_total}.", "",
    ]

    # 1. Whitespace (H1)
    ratios = [doc["whitespace"]["ratio"] for doc in docs
              if (doc.get("whitespace") or {}).get("ratio") is not None]
    backend = [doc["whitespace"]["backend_share"] for doc in docs
               if (doc.get("whitespace") or {}).get("backend_share") is not None]
    reference = [doc["whitespace"]["reference_share"] for doc in docs
                 if (doc.get("whitespace") or {}).get("reference_share") is not None]
    below = [doc for doc in docs if (doc.get("whitespace") or {}).get("below_70pct")]
    worst_ratio = sorted([doc for doc in docs
                          if (doc.get("whitespace") or {}).get("ratio") is not None],
                         key=lambda doc: doc["whitespace"]["ratio"])[:10]
    lines += [
        "## 1. Whitespace (H1)", "",
        "Whitespace share = whitespace characters / all characters, backend text against "
        "PyMuPDF `page.get_text()` over the same pages.", "",
        "| Measure | Median | p05 | p25 | p75 | p95 | Min | Max |",
        "| --- | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for label, values in (("backend whitespace share", backend),
                          ("reference whitespace share", reference),
                          ("backend / reference", ratios)):
        if not values:
            continue
        lines.append(
            f"| {label} | {fmt(statistics.median(values), 4)} | {fmt(percentile(values, 0.05), 4)} "
            f"| {fmt(percentile(values, 0.25), 4)} | {fmt(percentile(values, 0.75), 4)} | "
            f"{fmt(percentile(values, 0.95), 4)} | {fmt(min(values), 4)} | "
            f"{fmt(max(values), 4)} |")
    lines += ["", f"Documents whose backend whitespace is below 70% of the reference: "
                  f"**{len(below)}** of {len(docs)}.", "",
              "| Id | Backend | Reference | Ratio | Backend newlines/page | "
              "Reference newlines/page |", "| --- | --- | --- | --- | --- | --- |"]
    for doc in worst_ratio:
        whitespace = doc["whitespace"]
        lines.append(f"| {doc['id']} | {fmt(whitespace['backend_share'], 4)} | "
                     f"{fmt(whitespace['reference_share'], 4)} | {fmt(whitespace['ratio'], 3)} | "
                     f"{fmt(whitespace.get('backend_newlines_per_page'))} | "
                     f"{fmt(whitespace.get('reference_newlines_per_page'))} |")
    paragraph_ratios = [(doc["paragraphs"]["count"], doc["pages"], doc["id"]) for doc in docs
                        if (doc.get("paragraphs") or {}).get("count") is not None]
    fewer = [item for item in paragraph_ratios if item[0] < item[1]]
    modes: collections.Counter = collections.Counter()
    mode_paragraphs: dict[str, list[float]] = collections.defaultdict(list)
    for doc in docs:
        mode = doc.get("structure_mode") or "unknown"
        modes[mode] += 1
        if (doc.get("paragraphs") or {}).get("per_page") is not None:
            mode_paragraphs[mode].append(doc["paragraphs"]["per_page"])
    lines += ["", "| Structure | Documents | Median paragraphs/page |", "| --- | --- | --- |"]
    for mode, count in modes.most_common():
        values = mode_paragraphs.get(mode) or []
        lines.append(f"| `{mode}` | {count} | "
                     f"{fmt(statistics.median(values) if values else None, 2)} |")
    lines += ["", f"Paragraphs against pages (H2): {len(fewer)} of {len(paragraph_ratios)} "
                  f"documents have fewer paragraphs than pages "
                  f"({sum(1 for item in paragraph_ratios if item[0] <= item[1] / 2)} have at "
                  f"most half).", "",
              "| Id | Paragraphs | Pages | Paragraphs/page |", "| --- | --- | --- | --- |"]
    for item in sorted(paragraph_ratios, key=lambda entry: entry[0] / max(entry[1], 1))[:10]:
        lines.append(f"| {item[2]} | {item[0]} | {item[1]} | "
                     f"{fmt(item[0] / max(item[1], 1), 2)} |")

    # 2. Long units
    long_docs = [doc for doc in docs if ((doc.get("long") or {}).get("units") or 0) > 0]
    over_five = [doc for doc in docs if ((doc.get("long") or {}).get("share") or 0) > 0.05]
    lines += ["", "## 2. Long units (> 800 characters)", "",
              f"Long units: {long_total} in {len(long_docs)} documents; "
              f"{len(over_five)} documents have more than 5% long units.", "",
              "| Cause | Units | Documents | Share of long units | What-if: units split away | "
              "What-if: documents below 5% |", "| --- | --- | --- | --- | --- | --- |"]
    what_if_docs = {cause: 0 for cause in LONG_CAUSES}
    for cause in LONG_CAUSES:
        units = [unit for unit in long_units if unit.get("cause") == cause]
        documents_of_cause = {unit["id"] for unit in units}
        share = round(len(units) / long_total, 4) if long_total else None
        fixed = 0
        for doc in docs:
            stats = (doc.get("what_if") or {}).get(cause) or {}
            fixed += stats.get("fixed_units") or 0
        # Approximate what-if: split only the units of this cause, keep every other unit, and
        # count only the documents that are over the 5% line to begin with.
        for doc in over_five:
            stats = (doc.get("what_if") or {}).get(cause) or {}
            fixed_units = stats.get("fixed_units") or 0
            if not fixed_units:
                continue
            doc_units = doc.get("units") or 0
            doc_long = (doc.get("long") or {}).get("units") or 0
            new_long = doc_long - fixed_units + (stats.get("remaining_long") or 0)
            new_total = doc_units + (stats.get("added_units") or 0)
            if new_total and new_long / new_total <= 0.05:
                what_if_docs[cause] += 1
        lines.append(f"| `{cause}` | {len(units)} | {len(documents_of_cause)} | "
                     f"{fmt(share, 4)} | {fixed} | {what_if_docs[cause]} of "
                     f"{len(over_five)} |")
    lines += ["", "| Cause | 3 samples (arXiv id, first 200 characters) |", "| --- | --- |"]
    for cause in LONG_CAUSES:
        units = [unit for unit in long_units if unit.get("cause") == cause]
        for sample in sample_of(units):
            lines.append(f"| `{cause}` | {sample} |")
    kind_rows = ["| Unit kind | Long units | Documents |", "| --- | --- | --- |"]
    for kind in ("sentence", "coarse_paragraph"):
        count = 0
        ids = set()
        for doc in docs:
            by_kind = (doc.get("long") or {}).get("by_kind") or {}
            value = by_kind.get(kind) or 0
            count += value
            if value:
                ids.add(doc["id"])
        kind_rows.append(f"| {kind} | {count} | {len(ids)} |")
    lines += ["", "Long units by the kind the server reports (a coarse paragraph is a "
                  "paragraph the fallback guard refused to split):", "", *kind_rows]
    missed_tokens: collections.Counter = collections.Counter()
    for unit in long_units:
        if unit.get("cause") != "missed_boundary":
            continue
        for token in (unit.get("evidence") or {}).get("missed_boundary_tokens") or []:
            missed_tokens[token.lower()] += 1
    lines += ["", "Most common token before a missed boundary: "
                  + ", ".join(f"`{token}` {count}"
                              for token, count in missed_tokens.most_common(10)) + "."]

    # 3. Short units
    short_docs = [doc for doc in docs if ((doc.get("short") or {}).get("units") or 0) > 0]
    over_ten = [doc for doc in docs if ((doc.get("short") or {}).get("share") or 0) > 0.10]
    lines += ["", "## 3. Short units (< 5 characters)", "",
              f"Short units: {short_total} in {len(short_docs)} documents; "
              f"{len(over_ten)} documents have more than 10% short units.", "",
              "| Cause | Units | Documents | Share of short units |", "| --- | --- | --- | --- |"]
    for cause in SHORT_CAUSES:
        units = [unit for unit in short_units if unit.get("cause") == cause]
        ids = {unit["id"] for unit in units}
        lines.append(f"| `{cause}` | {len(units)} | {len(ids)} | "
                     f"{fmt(round(len(units) / short_total, 4) if short_total else None, 4)} |")
    lines += ["", "| Cause | 3 samples (arXiv id, text) |", "| --- | --- |"]
    for cause in SHORT_CAUSES:
        units = [unit for unit in short_units if unit.get("cause") == cause]
        for sample in sample_of(units, "text"):
            lines.append(f"| `{cause}` | {sample} |")
    other_texts = collections.Counter(unit.get("text") or ""
                                      for unit in short_units
                                      if unit.get("cause") == "other")
    lines += ["", "Most common `other` short units: "
                  + ", ".join(f"`{text}` {count}"
                              for text, count in other_texts.most_common(10)) + ".", "",
              "| Id | Unit | Before | After |", "| --- | --- | --- | --- |"]
    shown = 0
    for text, _count in other_texts.most_common(4):
        unit = next((unit for unit in short_units
                     if unit.get("cause") == "other" and unit.get("text") == text), None)
        if unit is None:
            continue
        shown += 1
        lines.append(f"| {unit['id']} | `{text}` | `{clipped(unit.get('before') or '', 40)}` | "
                     f"`{clipped(unit.get('after') or '', 40)}` |")
    if not shown:
        lines.append("| - | - | - | - |")
    unmatched = sum(1 for unit in short_units
                    if unit.get("cause") == "page_number" and unit.get("page_matched") is False)
    lines += ["", f"Page-number units whose page could not be inferred from the reference: "
                  f"{unmatched}."]

    # 4. Running headers and footers
    header_docs: dict[str, set] = {name: set() for name, _, _ in HEADER_TIERS}
    tier_cases: dict[str, collections.Counter] = {name: collections.Counter()
                                                 for name, _, _ in HEADER_TIERS}
    tier_positions: dict[str, collections.Counter] = {name: collections.Counter()
                                                      for name, _, _ in HEADER_TIERS}
    tier_rows: dict[str, list] = {name: [] for name, _, _ in HEADER_TIERS}
    for doc in docs:
        headers = doc.get("headers") or {}
        for name, _line_share, _page_share in HEADER_TIERS:
            tier = (headers.get("tiers") or {}).get(name) or {}
            for candidate in tier.get("candidates") or []:
                tier_rows[name].append((doc["id"], candidate))
                tier_cases[name].update(candidate.get("cases") or {})
                tier_positions[name].update(candidate.get("positions") or {})
                header_docs[name].add(doc["id"])
    lines += ["", "## 4. Running headers and footers (H5)", "",
              "Two populations, because the threshold decides the answer: the task's tier "
              "(a line on >= 80% of the pages, a page-number line on >= 50%) and PDF-5a's "
              "looser 30% proxy, which is where most real running headers sit.", ""]
    for name, line_share, page_share in HEADER_TIERS:
        label = (f">= {int(line_share * 100)}% / >= {int(page_share * 100)}%"
                 if name == "spec" else f">= {int(line_share * 100)}% (PDF-5a's proxy)")
        lines += [f"### Tier `{name}` ({label})", "",
                  f"Documents: {len(header_docs[name])}; candidate lines: "
                  f"{len(tier_rows[name])}.", "",
                  "| Where the line lands | Occurrences |", "| --- | --- |"]
        for case in HEADER_CASES:
            lines.append(f"| `{case}` | {tier_cases[name].get(case, 0)} |")
        lines += ["", "| Where the line sits on its page | Occurrences |", "| --- | --- |"]
        for position in PAGE_POSITIONS:
            lines.append(f"| `{position}` | {tier_positions[name].get(position, 0)} |")
        lines += ["", "| Id | Line | Kind | Pages | Cases | Position |",
                  "| --- | --- | --- | --- | --- | --- |"]
        for identifier, candidate in sorted(tier_rows[name],
                                            key=lambda item: -item[1]["pages"])[:15]:
            cases = ", ".join(f"{key}={value}" for key, value in sorted(
                (candidate.get("cases") or {}).items()))
            positions = ", ".join(f"{key}={value}" for key, value in sorted(
                (candidate.get("positions") or {}).items()))
            lines.append(f"| {identifier} | `{(candidate.get('line') or '')[:60]}` | "
                         f"{candidate.get('kind')} | {candidate.get('pages')} | {cases} | "
                         f"{positions} |")
        lines.append("")
    lines += ["### The most repeated line of every document", "",
              "| Id | Lines | Most repeated line (pages, share of pages) |",
              "| --- | --- | --- |"]
    for doc in docs:
        headers = doc.get("headers") or {}
        top = (headers.get("top_lines") or [{}])[0]
        lines.append(f"| {doc['id']} | {headers.get('lines_total')} | "
                     f"`{(top.get('line') or '')[:60]}` ({top.get('pages')}, "
                     f"{fmt(top.get('share'), 3)}) |")

    # 5. Silent loss and bad characters
    loss_docs = [doc for doc in docs if doc.get("loss")]
    containments = [value for value in
                    ((doc.get("quality") or {}).get("containment_first20") for doc in docs)
                    if value is not None]
    bad = [(doc.get("quality") or {}).get("replacement_share", 0)
           + (doc.get("quality") or {}).get("private_use_share", 0) for doc in docs]
    lines += ["", "## 5. Silent loss and bad characters (H6)", "",
              f"Documents measured in detail here: {len(loss_docs)} (containment below "
              f"{LOSS_CONTAINMENT} or over 1% U+FFFD/private-use). Over all {len(docs)} "
              f"documents:", "",
              "| Measure | Median | p05 | p25 | p75 | p95 | Min | Max | Documents below 0.9 |",
              "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for label, values, threshold in (("5-gram containment of the first 20 sentences "
                                      "(whitespace-insensitive)", containments, 0.9),
                                     ("U+FFFD + private-use share", bad, None)):
        if not values:
            continue
        below_threshold = (sum(1 for value in values if value is not None and value < threshold)
                           if threshold is not None else "-")
        lines.append(f"| {label} | {fmt(statistics.median(values), 4)} | "
                     f"{fmt(percentile(values, 0.05), 4)} | {fmt(percentile(values, 0.25), 4)} | "
                     f"{fmt(percentile(values, 0.75), 4)} | {fmt(percentile(values, 0.95), 4)} | "
                     f"{fmt(min(values), 4)} | {fmt(max(values), 4)} | "
                     f"{below_threshold} |")
    lines += ["", "| Id | Pages | Containment | Backend chars | Reference chars | Char ratio | "
              "U+FFFD | Private use | Worst page | Page reference chars | Page backend chars | "
              "Page content KiB | Page text ops | Form text ops on the page |",
              "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | "
              "--- |"]
    for doc in loss_docs:
        loss = doc["loss"]
        worst = (loss.get("worst_pages") or [{}])[0]
        pdf = worst.get("pdf") or {}
        quality = doc.get("quality") or {}
        lines.append(f"| {doc['id']} | {doc.get('pages')} | "
                     f"{fmt(loss.get('containment_first20'), 3)} | "
                     f"{loss.get('backend_chars')} | {loss.get('reference_chars')} | "
                     f"{fmt(quality.get('char_ratio'), 3)} | "
                     f"{fmt(quality.get('replacement_share'), 4)} | "
                     f"{fmt(quality.get('private_use_share'), 4)} | "
                     f"{worst.get('page')} | {worst.get('reference_chars')} | "
                     f"{worst.get('backend_chars')} | "
                     f"{fmt((pdf.get('content_bytes') or 0) / 1024, 1)} | "
                     f"{(pdf.get('own') or {}).get('text_ops')} | "
                     f"{pdf.get('form_text_ops')} |")
    lines += ["", "Font objects in the file, and the forms a page sample reaches:", "",
              "| Id | Fonts by subtype | Fonts without /ToUnicode | Type3 fonts | Pages sampled | "
              "Forms with text | Text ops in forms | ... with a CJK font (PDF-1c inlines those) |",
              "| --- | --- | --- | --- | --- | --- | --- | --- |"]
    for doc in loss_docs:
        loss = doc["loss"]
        census = loss.get("font_census") or {}
        forms = loss.get("form_census") or {}
        subtypes = ", ".join(f"{name} {count}"
                             for name, count in (census.get("subtypes") or {}).items())
        missing = ", ".join(f"{name} {count}"
                            for name, count in (census.get("no_to_unicode") or {}).items())
        lines.append(f"| {doc['id']} | {subtypes} | {missing or '-'} | "
                     f"{census.get('type3_count')} | {forms.get('pages_sampled')} | "
                     f"{forms.get('pages_with_text_forms')} | "
                     f"{forms.get('text_ops_in_forms')} | "
                     f"{forms.get('text_ops_in_cjk_forms')} |")
    lines += ["", "| Id | Page | Text by font (characters) | The font the page's text uses "
                  "(subtype, /Encoding, /ToUnicode CMap) | Forms reached by Do |",
              "| --- | --- | --- | --- | --- |"]
    for doc in loss_docs:
        for worst in (doc["loss"].get("worst_pages") or [])[:2]:
            pdf = worst.get("pdf") or {}
            spans = ", ".join(f"{name} {count}"
                              for name, count in list((pdf.get("spans_by_font") or {}).items())[:3])
            used = []
            for font in (pdf.get("used_fonts") or [])[:2]:
                cmap = font.get("cmap") or {}
                used.append(
                    f"{font['name']} ({font.get('subtype') or 'not in /Resources/Font'}, "
                    f"enc `{(font.get('encoding') or 'none')[:20]}`, "
                    f"{'no ToUnicode' if not font.get('to_unicode') else 'ToUnicode'}"
                    f"{' not decompressible' if not cmap.get('decompressed') else ''}"
                    f"{', bfchar %d bfrange %d' % (cmap.get('bfchar') or 0, cmap.get('bfrange') or 0) if cmap.get('decompressed') else ''})")
            forms = "; ".join(f"{form['name']} ({form['text_ops']} ops, "
                              f"{'CJK font' if form['has_cjk_font'] else 'no CJK font'})"
                              for form in (pdf.get("forms") or [])[:3])
            lines.append(f"| {doc['id']} | {worst.get('page')} | {spans[:80] or '-'} | "
                         f"{'; '.join(used)[:200]} | {forms[:80] or '-'} |")

    # 6. Backend, cost and outcomes (PDF-7a)
    #
    # Everything above measures the text of the documents that opened. This section measures what
    # opening them cost and what happened to the rest, which is the question PDF-7a asks and
    # PDF-6a could not answer. It is the only section PDF-6a did not have, so sections 1-5 stay
    # byte-identical to the PDF-6a run they are compared against.
    environments = {entry.get("backend") or "?": entry for entry in environment}
    backend_names = sorted(environments) or sorted({doc.get("backend") or "?" for doc in documents})
    outcome_counts: collections.Counter = collections.Counter(
        (doc.get("backend") or "?", doc.get("outcome") or "?", doc.get("error_code") or "-")
        for doc in documents)
    reasons: collections.Counter = collections.Counter()
    reason_pages: dict[str, set] = collections.defaultdict(set)
    for doc in documents:
        refusal = doc.get("refusal") or {}
        for item in refusal.get("refusals") or []:
            reason = item.get("reason") or "?"
            reasons[reason] += 1
            if item.get("page"):
                reason_pages[reason].add((doc["id"], item["page"]))
    lines += ["", "## 6. Backend, cost and outcomes (PDF-7a)", "",
              "Peak resident set is the **whole process tree** (the server and the Python worker it "
              "spawns), not the server's `ru_maxrss`: PDF-1a reported the server alone and said so. "
              "The number is the larger of a 50 ms sampler's sum over the tree and the kernel's "
              "`ru_maxrss` for the process the harness starts, which is the peak of the largest "
              "single process in that tree over its whole life: a transient shorter than the "
              "interval shows up only in the second, and two processes peaking at once only in the "
              "first. Every open runs inside the same `MemoryMax=600M` scope, so a document whose "
              "tree does not fit is killed by the cap and reported as its own outcome.", "",
              "| Backend | Documents | ok | cap_killed | OCR_REQUIRED | PARSE_FAILED | timeout | "
              "other |", "| --- | --- | --- | --- | --- | --- | --- | --- |"]
    for name in backend_names:
        rows = {key: value for (backend, key, _code), value in outcome_counts.items()
                if backend == name}
        total = sum(rows.values())
        other = total - sum(rows.get(key, 0) for key in
                            ("ok", CAP_KILLED, "ocr_required", "parse_failed", "timeout"))
        lines.append(f"| `{name}` | {total} | {rows.get('ok', 0)} | {rows.get(CAP_KILLED, 0)} | "
                     f"{rows.get('ocr_required', 0)} | {rows.get('parse_failed', 0)} | "
                     f"{rows.get('timeout', 0)} | {other} |")
    failures = [((backend, outcome, code), count)
                for (backend, outcome, code), count in sorted(outcome_counts.items())
                if outcome != "ok"]
    if failures:
        lines += ["", "| Backend | Outcome | Error code | Documents |",
                  "| --- | --- | --- | --- |"]
        for (backend, outcome, code), count in failures:
            lines.append(f"| `{backend}` | `{outcome}` | `{code}` | {count} |")
    if reasons:
        lines += ["", "Why the layout worker refused, read from the worker source the binary embeds "
                  "(the server's own `OCR_REQUIRED` message does not carry it):", "",
                  "| Reason | Refusing pages | Papers |", "| --- | --- | --- |"]
        for reason, count in reasons.most_common():
            ids = {identifier for identifier, _page in reason_pages.get(reason, set())}
            lines.append(f"| `{reason}` | {count} | {len(ids)} |")
    lines += ["", "| Backend | Opens | Open s median | Open s p95 | Open s max | Peak tree RSS "
              "MiB median | p95 | max | Peak tree RSS source | cgroup OOM kills |",
              "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for name in backend_names:
        rows = [doc for doc in documents if (doc.get("backend") or "?") == name]
        opens = [doc["open_s"] for doc in rows if doc.get("open_s") is not None]
        peaks = [peak_kib(doc) for doc in rows]
        peaks = [value / 1024 for value in peaks if value]
        sources = collections.Counter(peak_source(doc) for doc in rows)
        kills = sum((doc.get("resources") or {}).get("oom_kill") or 0 for doc in rows)
        lines.append(
            f"| `{name}` | {len(opens)} | {fmt(statistics.median(opens), 2) if opens else '-'} | "
            f"{fmt(percentile(opens, 0.95), 2)} | {fmt(max(opens), 2) if opens else '-'} | "
            f"{fmt(statistics.median(peaks), 1) if peaks else '-'} | "
            f"{fmt(percentile(peaks, 0.95), 1)} | {fmt(max(peaks), 1) if peaks else '-'} | "
            f"{', '.join(f'{key} {value}' for key, value in sources.most_common()) or '-'} | "
            f"{kills} |")
    venv_sizes = [entry for entry in environment if entry.get("venv_bytes")]
    if venv_sizes:
        biggest = max(venv_sizes, key=lambda entry: entry["venv_bytes"])
        lines += ["", f"Pinned layout environment: `{biggest.get('venv_du') or '-'}` "
                  f"(`du -sh`), {biggest['venv_bytes']} bytes (`du -sb`), from "
                  f"`{biggest.get('venv_path')}`."]

    # 7. Broken sentences (PDF-7b-1)
    #
    # A broken sentence is a prose unit that ends without a terminal and is
    # followed in the same section by a unit that starts lowercase - the
    # half-sentence unit the positional layer must not create more of.
    broken_total = sum(doc.get("broken_sentences") or 0 for doc in docs)
    broken_docs = sorted(
        ((doc.get("broken_sentences") or 0, doc["id"]) for doc in docs
         if doc.get("broken_sentences")),
        reverse=True)
    lines += ["", "## 7. Broken sentences (PDF-7b-1)", "",
              "A broken sentence is a prose unit (not a heading, not "
              "`non_prose`) that does not end with a terminal and is followed, "
              "in the same section, by a unit that starts with a lowercase "
              "letter - a unit that is half of a sentence.", "",
              f"Broken sentences: **{broken_total}** in "
              f"{len(broken_docs)} documents.", "",
              "| Id | Broken sentences |", "| --- | --- |"]
    for count, identifier in broken_docs[:30]:
        lines.append(f"| {identifier} | {count} |")

    text = "\n".join(lines)
    print(text)
    if args.markdown:
        os.makedirs(os.path.dirname(os.path.abspath(args.markdown)) or ".", exist_ok=True)
        with open(args.markdown, "w", encoding="utf-8") as handle:
            handle.write(text + "\n")
    if args.out:
        write_json(args.out, {
            "documents": len(docs),
            "not_run": [doc["id"] for doc in not_run],
            "failed": [{"id": doc["id"], "outcome": doc.get("outcome"),
                        "error": doc.get("error")} for doc in failed],
            "units": total_units,
            "long_units": long_total,
            "short_units": short_total,
            "documents_over_5pct_long": [doc["id"] for doc in over_five],
            "documents_over_10pct_short": [doc["id"] for doc in over_ten],
            "whitespace_below_70pct": [doc["id"] for doc in below],
            "what_if_documents_below_5pct": what_if_docs,
            "long_by_cause": {cause: len([unit for unit in long_units
                                          if unit.get("cause") == cause])
                              for cause in LONG_CAUSES},
            "short_by_cause": {cause: len([unit for unit in short_units
                                           if unit.get("cause") == cause])
                               for cause in SHORT_CAUSES},
            "header_cases": {name: dict(tier_cases[name]) for name, _, _ in HEADER_TIERS},
            "header_positions": {name: dict(tier_positions[name])
                                 for name, _, _ in HEADER_TIERS},
            "header_documents": {name: sorted(header_docs[name])
                                 for name, _, _ in HEADER_TIERS},
            "missed_boundary_tokens": missed_tokens.most_common(10),
            "loss_documents": [doc["id"] for doc in loss_docs],
            "broken_sentences": broken_total,
            "broken_sentences_documents": {identifier: count
                                           for count, identifier in broken_docs},
            "outcomes": {f"{backend}/{outcome}/{code}": count
                         for (backend, outcome, code), count in sorted(outcome_counts.items())},
            "refusal_reasons": reasons.most_common(),
            "environment": environment,
        })
    return 0


# --------------------------------------------------------------------------------------------
# Paired comparison (PDF-7a)
# --------------------------------------------------------------------------------------------

PAIRED_MEASURES = (
    ("documents", "Documents opened `ok` on both sides"),
    ("units", "Units"),
    ("long_units", "Units > 800 chars"),
    ("papers_over_5pct_long", "Papers above 5% long units"),
    ("long_share", "Long units / all units"),
    ("short_units", "Units < 5 chars"),
    ("papers_over_10pct_short", "Papers above 10% short units"),
    ("dot_units", "One-character `.` units"),
    ("heading_number", "`heading_number` units"),
    ("page_number", "`page_number` units"),
    ("paragraphs_per_page", "Paragraphs per page (median)"),
    ("header_occurrences", "Header/footer occurrences (80% / 50% tier)"),
    ("header_own_unit", "Header/footer lines that are their own unit"),
    ("header_own_unit_share", "Share of those that are their own unit"),
    ("header_prefix", "Header/footer lines that prefix a body unit"),
    ("header_inside", "Header/footer lines inside a body unit"),
    ("header_glued", "Header/footer lines present only without spaces"),
    ("whitespace_ratio", "Whitespace ratio, backend / reference (median)"),
    ("papers_below_70pct_ws", "Papers below 0.7 of the reference whitespace"),
    ("containment_median", "5-gram containment (median)"),
    ("containment_p05", "5-gram containment (p05)"),
    ("papers_below_0p9_containment", "Papers with containment below 0.9"),
    ("broken_sentences", "Broken sentence units"),
    ("open_s_median", "Open seconds (median)"),
    ("open_s_p95", "Open seconds (p95)"),
    ("peak_tree_mib_median", "Peak tree RSS MiB (median)"),
    ("peak_tree_mib_p95", "Peak tree RSS MiB (p95)"),
    ("peak_tree_mib_max", "Peak tree RSS MiB (max)"),
)


def measures_for(documents: list[dict], long_units: list[dict], short_units: list[dict],
                 ids: set[str]) -> dict:
    """The PDF-6a measures plus the PDF-7a resources, over one population of ids."""
    by_id = {doc["id"]: doc for doc in documents if doc.get("id") in ids}
    docs = [by_id[identifier] for identifier in sorted(ids) if by_id.get(identifier)]
    opened = [doc for doc in docs if doc.get("outcome") == "ok"]
    kept = {doc["id"] for doc in opened}
    longs = [unit for unit in long_units if unit.get("id") in kept]
    shorts = [unit for unit in short_units if unit.get("id") in kept]
    total_units = sum(doc.get("units") or 0 for doc in opened)
    long_total = sum((doc.get("long") or {}).get("units") or 0 for doc in opened)
    short_total = sum((doc.get("short") or {}).get("units") or 0 for doc in opened)
    cases: collections.Counter = collections.Counter()
    for doc in opened:
        tier = ((doc.get("headers") or {}).get("tiers") or {}).get("spec") or {}
        cases.update(tier.get("cases") or {})
    occurrences = sum(cases.values())
    paragraphs = [(doc.get("paragraphs") or {}).get("per_page") for doc in opened]
    paragraphs = [value for value in paragraphs if value is not None]
    ratios = [(doc.get("whitespace") or {}).get("ratio") for doc in opened]
    ratios = [value for value in ratios if value is not None]
    containments = [(doc.get("quality") or {}).get("containment_first20") for doc in opened]
    containments = [value for value in containments if value is not None]
    opens = [doc["open_s"] for doc in opened if doc.get("open_s") is not None]
    peaks = [peak_kib(doc) / 1024 for doc in opened]
    peaks = [value for value in peaks if value]
    short_causes = collections.Counter(unit.get("cause") for unit in shorts)
    return {
        "documents": len(opened),
        "units": total_units,
        "long_units": long_total,
        "papers_over_5pct_long": sum(1 for doc in opened
                                      if ((doc.get("long") or {}).get("share") or 0) > 0.05),
        "long_share": round(long_total / total_units, 4) if total_units else None,
        "short_units": short_total,
        "papers_over_10pct_short": sum(1 for doc in opened
                                        if ((doc.get("short") or {}).get("share") or 0) > 0.10),
        "dot_units": sum(1 for unit in shorts if (unit.get("text") or "").strip() == "."),
        "heading_number": short_causes.get("heading_number", 0),
        "page_number": short_causes.get("page_number", 0),
        "paragraphs_per_page": statistics.median(paragraphs) if paragraphs else None,
        "header_occurrences": occurrences,
        "header_own_unit": cases.get("own_unit", 0),
        "header_own_unit_share": (round(cases.get("own_unit", 0) / occurrences, 4)
                                  if occurrences else None),
        "header_prefix": cases.get("prefix", 0),
        "header_inside": cases.get("inside", 0),
        "header_glued": cases.get("glued", 0),
        "whitespace_ratio": statistics.median(ratios) if ratios else None,
        "papers_below_70pct_ws": sum(1 for doc in opened
                                     if (doc.get("whitespace") or {}).get("below_70pct")),
        "containment_median": statistics.median(containments) if containments else None,
        "containment_p05": percentile(containments, 0.05),
        "papers_below_0p9_containment": sum(1 for value in containments if value < 0.9),
        "broken_sentences": sum(doc.get("broken_sentences") or 0 for doc in opened),
        "open_s_median": statistics.median(opens) if opens else None,
        "open_s_p95": percentile(opens, 0.95),
        "peak_tree_mib_median": statistics.median(peaks) if peaks else None,
        "peak_tree_mib_p95": percentile(peaks, 0.95),
        "peak_tree_mib_max": max(peaks) if peaks else None,
        "open_ok": len(opened),
        "outcomes": dict(collections.Counter(doc.get("outcome") for doc in docs)),
    }


def paired_measure_table(base: dict, run: dict, label: str) -> list[str]:
    """One measure table, both columns measured on the same papers."""
    lines = ["| Measure | default | " + label + " |", "| --- | --- | --- |"]
    for key, name in PAIRED_MEASURES:
        digits = 4 if key in ("long_share", "header_own_unit_share") else (
            3 if key in ("whitespace_ratio", "containment_median", "containment_p05")
            else 2)
        lines.append(f"| {name} | {fmt(base.get(key), digits)} | {fmt(run.get(key), digits)} |")
    return lines


def outcome_table(loaded: dict, labels: list[str]) -> list[str]:
    every = sorted({doc.get("outcome") or "?" for label in labels for doc in loaded[label]})
    lines = ["| Outcome | " + " | ".join(labels) + " |",
             "| --- | " + " | ".join("---" for _ in labels) + " |"]
    for outcome in every:
        cells = [str(sum(1 for doc in loaded[label] if (doc.get("outcome") or "?") == outcome))
                 for label in labels]
        lines.append(f"| `{outcome}` | " + " | ".join(cells) + " |")
    return lines


def paired_mode(args) -> int:
    """Pair runs by arXiv id against the baseline and print the same measures side by side.

    Each run is paired with the baseline on its own population - the papers that run opened `ok`
    and the baseline opened `ok` - so the two columns of a table always describe the same papers.
    The last table repeats the measures on the papers that every run opened, which is the only
    population where all the columns are comparable to each other.
    """
    if not args.baseline_jsonl_dir or not args.compare:
        print("::error::--paired needs --baseline-jsonl-dir and at least one --compare")
        return 1
    base_documents, base_long, base_short, _ = load_records(args.baseline_jsonl_dir)
    base_ok = {doc["id"] for doc in base_documents if doc.get("outcome") == "ok"}
    loaded = {"default": base_documents}
    runs = []
    common: set[str] | None = None
    for item in args.compare:
        label, _, directory = item.partition("=")
        if not directory:
            print(f"::error::--compare {item} needs LABEL=DIR")
            return 1
        if label in loaded:
            print(f"::error::--compare label {label} is used twice")
            return 1
        documents, longs, shorts, _ = load_records(directory)
        loaded[label] = documents
        ok = {doc["id"] for doc in documents if doc.get("outcome") == "ok"}
        population = base_ok & ok
        common = population if common is None else common & population
        runs.append((label, directory, population, longs, shorts, ok))

    lines = ["# PDF-7a paired comparison", "",
             "Paired by arXiv id. The default side is "
             f"`{args.baseline_jsonl_dir}`, {len(base_ok)} papers opened `ok`. A paper enters a "
             "table only when the default side and that run both opened it `ok`, so the two "
             "columns of a table always measure the same text.", ""]
    for label, directory, population, longs, shorts, ok in runs:
        lines += [f"## `{label}`", "",
                  f"`{directory}` opened {len(ok)} of the corpus `ok`; paired population "
                  f"**{len(population)}** papers.", ""]
        base_measures = measures_for(base_documents, base_long, base_short, population)
        run_measures = measures_for(loaded[label], longs, shorts, population)
        lines += paired_measure_table(base_measures, run_measures, label)
        base_broken = {doc["id"]: doc.get("broken_sentences") or 0
                       for doc in base_documents}
        run_broken = {doc["id"]: doc.get("broken_sentences") or 0
                      for doc in loaded[label]}
        risen = sorted(
            (run_broken.get(identifier, 0) - base_broken.get(identifier, 0),
             identifier)
            for identifier in population
            if run_broken.get(identifier, 0) > base_broken.get(identifier, 0))
        risen.reverse()
        lines += ["", f"Papers whose broken-sentence count rose under `{label}`: "
                      f"{len(risen)} of {len(population)} paired papers.", ""]
        if risen:
            lines += ["| Id | Broken sentences added |", "| --- | --- |"]
            for added, identifier in risen[:20]:
                lines.append(f"| {identifier} | {added} |")
        lines += ["", f"Outcomes over `{label}`'s whole population:", ""]
        lines += outcome_table(loaded, ["default", label])
        lines.append("")
    if len(runs) > 1 and common:
        labels = ["default"] + [label for label, _d, _p, _l, _s, _o in runs]
        lines += ["## The papers every run opened", "",
                  f"Population: {len(common)} papers.", ""]
        header = ["| Measure | " + " | ".join(labels) + " |",
                  "| --- | " + " | ".join("---" for _ in labels) + " |"]
        lines += header
        every = {"default": measures_for(base_documents, base_long, base_short, common)}
        for label, _directory, _population, longs, shorts, _ok in runs:
            every[label] = measures_for(loaded[label], longs, shorts, common)
        for key, name in PAIRED_MEASURES:
            digits = 4 if key in ("long_share", "header_own_unit_share") else (
                3 if key in ("whitespace_ratio", "containment_median", "containment_p05")
                else 2)
            cells = [fmt(every[label].get(key), digits) for label in labels]
            lines.append(f"| {name} | " + " | ".join(cells) + " |")
        lines.append("")
    text = "\n".join(lines)
    print(text)
    if args.markdown:
        os.makedirs(os.path.dirname(os.path.abspath(args.markdown)) or ".", exist_ok=True)
        with open(args.markdown, "w", encoding="utf-8") as handle:
            handle.write(text + "\n")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description="PDF-6a text-quality measurement on the PDF-5a corpus.")
    parser.add_argument("--aggregate", action="store_true", help="merge shards into tables")
    parser.add_argument("--paired", action="store_true",
                        help="pair runs by arXiv id against --baseline-jsonl-dir")
    parser.add_argument("--baseline-jsonl-dir", default="")
    parser.add_argument("--compare", action="append", default=[],
                        metavar="LABEL=DIR", help="one run to pair against the baseline")
    parser.add_argument("--backend", choices=("default", "layout"), default="default",
                        help="PDF backend the server selects")
    parser.add_argument("--layout-python", default="",
                        help="interpreter of the pinned layout environment (layout backend)")
    parser.add_argument("--venv-path", default="",
                        help="root of the pinned layout environment, for its `du -sh`")
    parser.add_argument("--worker-source", default="",
                        help="the worker source the binary embeds (layout refusals)")
    parser.add_argument("--ids", default="scripts/pdf-sweep-ids.txt")
    parser.add_argument("--out", default="")
    parser.add_argument("--markdown", default="")
    parser.add_argument("--jsonl-dir", default="")
    parser.add_argument("--work-dir", default=".tmp/pdf-quality")
    parser.add_argument("--binary", default="")
    parser.add_argument("--state-root", default=".tmp/pdf-quality/state")
    parser.add_argument("--shard", type=int, default=0)
    parser.add_argument("--shards", type=int, default=8)
    parser.add_argument("--deadline-minutes", type=float, default=42.0)
    parser.add_argument("--per-document-minutes", type=float, default=4.5)
    parser.add_argument("--keep-coarse-text", action="store_true",
                        help="also record the full text of every coarse paragraph, at any "
                             "length (PDF-6e; off by default, and the default records are "
                             "byte-identical)")
    parser.add_argument("--keep-pdf-sentence-text", action="store_true",
                        help="also read the paragraph stream and record every paragraph's "
                             "full text plus the stream totals, as `pdf_paragraph` and "
                             "`pdf_stream_totals` records (PDF-6g; off by default, and the "
                             "default records are byte-identical)")
    args = parser.parse_args()
    if not args.keep_pdf_sentence_text:
        args.keep_pdf_sentence_text = False
    if not args.worker_source:
        args.worker_source = os.path.join(os.path.dirname(os.path.dirname(
            os.path.abspath(__file__))), "src", "parsing", "pdf_layout_worker.py")
    if args.paired:
        return paired_mode(args)
    if args.aggregate:
        if not args.jsonl_dir:
            parser.error("--aggregate needs --jsonl-dir")
        return aggregate_mode(args)
    if not args.binary or not args.out:
        parser.error("run mode needs --binary and --out")
    if args.backend == "layout" and not args.layout_python:
        parser.error("the layout backend needs --layout-python")
    args.cap_available = PROBE.cap_available()
    if not args.cap_available:
        print("::warning::systemd-run MemoryMax scope is not permitted; running without the cap",
              flush=True)
    return run_shard(args)


if __name__ == "__main__":
    sys.exit(main())
