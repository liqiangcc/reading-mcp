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
  `get_text_units`, and record the measurements of PDF-6a:

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
import sys
import time

MIB = 1024 * 1024

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

LONG_CAUSES = ("glued_terminal", "glued_words", "bibliography", "math_table",
               "missed_boundary", "genuine")
SHORT_CAUSES = ("page_number", "marker", "heading_number", "math", "hyphen_fragment", "other")
HEADER_CASES = ("own_unit", "prefix", "suffix", "inside", "not_found")
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
                if len(line) >= MIN_LINE_CHARS:
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
        found = re.search(r"([^\W\d_][\w.\-]*)$", before, re.U)
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

def header_candidates(lines_per_page: list[list[str]], page_count: int) -> list[dict]:
    """Lines repeated on >= 80% of the pages, and page-number lines on >= 50%."""
    counts: collections.Counter = collections.Counter()
    for lines in lines_per_page:
        for line in set(lines):
            counts[line] += 1
    found: list[dict] = []
    for line, count in counts.items():
        if count < MIN_REPEAT_PAGES or count < HEADER_PAGE_SHARE * page_count:
            continue
        found.append({"line": line, "pages": count, "kind": "repeated_line"})
    page_numbers = []
    for line, count in counts.items():
        if count < MIN_REPEAT_PAGES or count < PAGENUM_PAGE_SHARE * page_count:
            continue
        if any(pattern.match(line) for pattern in PAGE_NUMBER_LINE_RES):
            page_numbers.append({"line": line, "pages": count, "kind": "page_number"})
    found.extend(page_numbers)
    found.sort(key=lambda item: (-item["pages"], -len(item["line"])))
    return found


def header_placement(candidate: dict, lines_per_page: list[list[str]],
                     unit_texts: list[str]) -> dict:
    """Where the line lands in the reader's units, and where it sits on its page."""
    line = candidate["line"]
    cases: collections.Counter = collections.Counter()
    positions: collections.Counter = collections.Counter()
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
    for position, token in enumerate(tokens):
        if token in counts:
            counts[token] += 1
        elif token == b"Do" and position:
            name = tokens[position - 1]
            if name.startswith(b"/"):
                do_names.append(name[1:].decode("latin-1"))
    return {
        "Tj": counts[b"Tj"],
        "TJ": counts[b"TJ"],
        "quote": counts[b"'"],
        "dquote": counts[b'"'],
        "text_ops": sum(counts.values()),
        "do": len(do_names),
        "do_names": do_names,
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


def font_facts(doc, xref: int, resources_xref: int | None) -> dict:
    """Subtype, `/ToUnicode` presence and the PDF-1c CJK hint of one font."""
    subtype = ""
    try:
        _kind, value = doc.xref_get_key(xref, "Subtype")
        subtype = value.lstrip("/") if isinstance(value, str) else ""
    except Exception:  # noqa: BLE001
        pass
    to_unicode = xref_target(doc, xref, "ToUnicode")
    cjk = False
    if to_unicode is not None:
        try:
            cjk = cmap_has_cjk(doc.xref_stream(to_unicode) or b"")
        except Exception:  # noqa: BLE001
            cjk = False
    return {"xref": xref, "subtype": subtype, "to_unicode": to_unicode is not None,
            "cjk_cmap": cjk}


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
                   "do_names": []}
            for xref in page.get_contents() or []:
                try:
                    facts = operator_facts(document.xref_stream(xref) or b"")
                except Exception:  # noqa: BLE001
                    continue
                for key in ("Tj", "TJ", "quote", "dquote", "text_ops", "do"):
                    own[key] += facts[key]
                own["do_names"].extend(facts["do_names"])
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
            results.append({
                "page": page_number,
                "own": own,
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
# Reading the document through the server
# --------------------------------------------------------------------------------------------

def collect_units(server, document_id: str, sections: list[dict], deadline: float) -> dict:
    """Every sentence unit, with the unit kind and the degradation the server reports."""
    units: list[dict] = []
    coverage: list[dict] = []
    calls = 0
    chars = 0
    truncated = False
    error = None
    started = time.monotonic()
    for section in sections:
        if chars >= TEXT_BUDGET_CHARS or time.monotonic() > deadline:
            truncated = True
            break
        cursor = None
        first = True
        while True:
            if chars >= TEXT_BUDGET_CHARS or time.monotonic() > deadline:
                truncated = True
                break
            arguments = PROBE.text_units(document_id, section["section_id"],
                                         QUALITY_BATCH_ITEMS, cursor)
            try:
                data = PROBE.payload(server.tool("get_text_units", arguments)) or {}
            except PROBE.ProbeError as exc:
                error = str(exc)[:200]
                break
            calls += 1
            if first:
                coverage.append({
                    "section_id": section["section_id"],
                    "coverage": data.get("coverage") or {},
                    "total_items": (data.get("stream") or {}).get("total_items"),
                    "native_location": (data.get("target_section_locator") or {}).get(
                        "native_location"),
                    "page": ((section.get("location") or {}).get("page")),
                })
                first = False
            for item in data.get("items") or []:
                text = item.get("text") or ""
                units.append({
                    "text": text,
                    "degradation": item.get("degradation"),
                    "effective_kind": item.get("effective_kind"),
                    "content_class": item.get("content_class"),
                    "content_class_detail": item.get("content_class_detail"),
                    "section_id": section["section_id"],
                })
                chars += len(text)
            cursor = data.get("next_cursor")
            if not cursor:
                break
        if error:
            break
    return {"units": units, "coverage": coverage, "calls": calls, "chars": chars,
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
                  page_count: int) -> dict:
    """Classify the long and the short units and the what-if of every long cause."""
    reference_words = reference_tokens("\n".join(reference["texts"]))
    edges = edge_digits(reference["lines"])
    page_reference_shares: dict[int, float | None] = {
        number + 1: whitespace_share(text) for number, text in enumerate(reference["texts"])
    }
    texts = [unit["text"] for unit in units]
    long_units: list[dict] = []
    short_units: list[dict] = []
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
            })
    return {
        "long": {"units": len(long_units), "by_cause": dict(long_by_cause),
                 "by_kind": dict(long_by_kind)},
        "short": {"units": len(short_units), "by_cause": dict(short_by_cause)},
        "what_if": what_if_totals,
        "long_units": long_units,
        "short_units": short_units,
        "total_units": len(units),
    }


def header_measurement(reference: dict, units: list[dict]) -> dict:
    candidates = header_candidates(reference["lines"], reference["pages"])
    unit_texts = [normalize(unit["text"]) for unit in units]
    results = []
    totals: collections.Counter = collections.Counter()
    positions: collections.Counter = collections.Counter()
    for candidate in candidates:
        placement = header_placement(candidate, reference["lines"], unit_texts)
        totals.update(placement["cases"])
        positions.update(placement["positions"])
        results.append({"line": clipped(candidate["line"], 120),
                        "kind": candidate["kind"], "pages": candidate["pages"],
                        "cases": placement["cases"],
                        "positions": placement["positions"]})
    return {"candidates": results, "cases": dict(totals), "positions": dict(positions)}


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
    worst = sorted(deficits, key=lambda item: (item["ratio"] is None,
                                               item["ratio"] if item["ratio"] is not None else 1.0,
                                               -item["reference_chars"]))[:WORST_PAGES]
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
    }


def document_record(entry: dict, args, deadline: float) -> tuple[dict, list[dict]]:
    record: dict = {
        "kind_record": "document", "id": entry["id"], "group": entry["group"],
        "title": entry.get("title"), "pages": entry.get("pages"),
        "producer": entry.get("producer"), "creator": entry.get("creator"),
        "outcome": None, "error": None, "phase": "download",
        "sections": None, "structure_mode": None, "units": None,
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
    env = PROBE.server_env(state_dir, os.path.dirname(os.path.abspath(path)), "default", "")
    cap = SWEEP.CAP if args.cap_available else None
    command = PROBE.cap_command(args.binary, cap, env)
    server = PROBE.Server(command, env, os.path.join(state_dir, "server.log"),
                          PROBE.TOOL_TIMEOUT_S)
    document_id = None
    units: list[dict] = []
    coverage: list[dict] = []
    read: dict = {}
    try:
        server.initialize()
        record["phase"] = "open"
        try:
            result = server.tool("open_document", {"source": path}, timeout=OPEN_TIMEOUT_S)
        except PROBE.ProbeError as exc:
            record["error"] = str(exc)[:400]
            record["error_code"] = exc.code
            result = None
        if result is not None:
            document_id = (PROBE.payload(result) or {}).get("document_id")
        if document_id:
            record["phase"] = "structure"
            sections = PROBE.read_sections(server, document_id)
            record["sections"] = len(sections)
            record["structure_mode"] = (
                "page_fallback"
                if sections and all(re.fullmatch(r"section://page-\d+", str(section.get(
                    "section_id", ""))) for section in sections)
                else "headings")
            record["phase"] = "read"
            read = collect_units(server, document_id, sections, deadline)
            units = read["units"]
            coverage = read["coverage"]
            record["read"] = {key: read[key] for key in
                              ("calls", "chars", "truncated", "error", "seconds")}
            record["outcome"] = "ok"
    finally:
        server.close(grace=30.0)
    record["server_rc"] = server.rc
    record["killed"] = server.rc in PROBE.KILLED_CODES
    if record["outcome"] is None:
        record["outcome"] = ("oom_killed" if record["killed"]
                             else SWEEP.classify(record, "timed out" in str(record.get("error")),
                                                 str(record.get("error") or "")))
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
        "sections_with_coverage": len(coverage),
    }
    record["quality"] = quality
    pages = unit_pages(units, page_index)
    record["pages_matched"] = sum(1 for page in pages if page is not None)
    measured = measure_units(units, reference, pages, reference["pages"])
    long_units = measured.pop("long_units")
    short_units = measured.pop("short_units")
    record["long"] = measured["long"]
    record["long"]["share"] = (round(measured["long"]["units"] / measured["total_units"], 4)
                               if measured["total_units"] else None)
    record["short"] = measured["short"]
    record["short"]["share"] = (round(measured["short"]["units"] / measured["total_units"], 4)
                                if measured["total_units"] else None)
    record["what_if"] = measured["what_if"]
    record["units"] = measured["total_units"]
    record["headers"] = header_measurement(reference, units)

    suspect_loss = ((quality["containment_first20"] is not None
                     and quality["containment_first20"] < LOSS_CONTAINMENT)
                    or quality["private_use_share"] + quality["replacement_share"]
                    > BAD_CHAR_SHARE)
    if suspect_loss:
        record["loss"] = loss_measurement(path, reference, units, pages, quality)

    detail = []
    for unit in long_units:
        unit["id"] = entry["id"]
        unit["doc_units"] = measured["total_units"]
        detail.append(unit)
    for unit in short_units:
        unit["id"] = entry["id"]
        unit["doc_units"] = measured["total_units"]
        detail.append(unit)
    record["seconds"] = round(time.perf_counter() - started, 1)
    return record, detail


def run_shard(args) -> int:
    entries = SWEEP.load_list(args.ids)
    if not entries:
        print("::error::the corpus list is empty")
        return 1
    shard = [entry for index, entry in enumerate(entries) if index % args.shards == args.shard]
    print(f"shard {args.shard}/{args.shards}: {len(shard)} documents", flush=True)
    print("| id | pages | outcome | units | ws share | ref ws | ws ratio | long | long % | "
          "short | short % | headers | s |", flush=True)
    os.makedirs(args.state_root, exist_ok=True)
    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)
    shard_deadline = time.monotonic() + args.deadline_minutes * 60
    for entry in shard:
        if time.monotonic() > shard_deadline:
            append_jsonl(args.out, {"kind_record": "document", "id": entry["id"],
                                    "group": entry["group"], "pages": entry.get("pages"),
                                    "outcome": "not_run", "error": "shard deadline"})
            print(f"| {entry['id']} | {entry.get('pages')} | not_run | - | - | - | - | - | - | "
                  f"- | - | - | - |", flush=True)
            continue
        deadline = min(shard_deadline, time.monotonic() + args.per_document_minutes * 60)
        try:
            record, detail = document_record(entry, args, deadline)
        except Exception as exc:  # noqa: BLE001 - one document must not end the shard
            record = {"kind_record": "document", "id": entry["id"],
                      "group": entry["group"], "pages": entry.get("pages"),
                      "outcome": "measure_failed",
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
        print(f"| {entry['id']} | {record.get('pages')} | {record.get('outcome')} | "
              f"{fmt(record.get('units'))} | {fmt(whitespace.get('backend_share'), 4)} | "
              f"{fmt(whitespace.get('reference_share'), 4)} | {fmt(whitespace.get('ratio'), 3)} "
              f"| {fmt(long_stats.get('units'))} | {fmt(long_stats.get('share'), 4)} | "
              f"{fmt(short_stats.get('units'))} | {fmt(short_stats.get('share'), 4)} | "
              f"{own} | {fmt(record.get('seconds'), 1)} |", flush=True)
    return 0


# --------------------------------------------------------------------------------------------
# Aggregate
# --------------------------------------------------------------------------------------------

def load_records(root: str) -> tuple[list[dict], list[dict], list[dict]]:
    documents: list[dict] = []
    long_units: list[dict] = []
    short_units: list[dict] = []
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
    return documents, long_units, short_units


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
    documents, long_units, short_units = load_records(args.jsonl_dir)
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
        # Approximate what-if: split only the units of this cause, keep every other unit.
        for doc in docs:
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
    unmatched = sum(1 for unit in short_units
                    if unit.get("cause") == "page_number" and unit.get("page_matched") is False)
    lines += ["", f"Page-number units whose page could not be inferred from the reference: "
                  f"{unmatched}."]

    # 4. Running headers and footers
    case_totals: collections.Counter = collections.Counter()
    position_totals: collections.Counter = collections.Counter()
    candidate_rows = []
    for doc in docs:
        headers = doc.get("headers") or {}
        for candidate in headers.get("candidates") or []:
            candidate_rows.append((doc["id"], candidate))
            case_totals.update(candidate.get("cases") or {})
            position_totals.update(candidate.get("positions") or {})
    header_docs = {doc["id"] for doc in docs
                   if (doc.get("headers") or {}).get("candidates")}
    lines += ["", "## 4. Running headers and footers (H5)", "",
              f"Documents with a repeated line (>= 80% of pages) or a page-number line "
              f"(>= 50%): {len(header_docs)}; candidate lines: {len(candidate_rows)}.", "",
              "| Where the line lands | Occurrences |", "| --- | --- |"]
    for case in HEADER_CASES:
        lines.append(f"| `{case}` | {case_totals.get(case, 0)} |")
    lines += ["", "| Where the line sits on its page | Occurrences |", "| --- | --- |"]
    for position in PAGE_POSITIONS:
        lines.append(f"| `{position}` | {position_totals.get(position, 0)} |")
    lines += ["", "| Id | Line | Kind | Pages | Cases | Position |", "| --- | --- | --- | "
                  "--- | --- | --- |"]
    for identifier, candidate in sorted(candidate_rows, key=lambda item: -item[1]["pages"])[:20]:
        cases = ", ".join(f"{key}={value}" for key, value in sorted(
            (candidate.get("cases") or {}).items()))
        positions = ", ".join(f"{key}={value}" for key, value in sorted(
            (candidate.get("positions") or {}).items()))
        lines.append(f"| {identifier} | `{(candidate.get('line') or '')[:60]}` | "
                     f"{candidate.get('kind')} | {candidate.get('pages')} | {cases} | "
                     f"{positions} |")

    # 5. Silent loss and bad characters
    loss_docs = [doc for doc in docs if doc.get("loss")]
    lines += ["", "## 5. Silent loss and bad characters (H6)", "",
              f"Documents measured here: {len(loss_docs)} (containment below "
              f"{LOSS_CONTAINMENT} or over 1% U+FFFD/private-use).", "",
              "| Id | Pages | Containment | Backend chars | Reference chars | Char ratio | "
              "Worst page | Reference chars | Backend chars | Own text ops | Form text ops | "
              "Forms with a CJK font |", "| --- | --- | --- | --- | --- | --- | --- | --- | --- "
              "--- | --- | --- | --- |"]
    for doc in loss_docs:
        loss = doc["loss"]
        worst = (loss.get("worst_pages") or [{}])[0]
        lines.append(f"| {doc['id']} | {doc.get('pages')} | "
                     f"{fmt(loss.get('containment_first20'), 3)} | "
                     f"{loss.get('backend_chars')} | {loss.get('reference_chars')} | "
                     f"{fmt((doc.get('quality') or {}).get('char_ratio'), 3)} | "
                     f"{worst.get('page')} | {worst.get('reference_chars')} | "
                     f"{worst.get('backend_chars')} | "
                     f"{(worst.get('pdf') or {}).get('own', {}).get('text_ops')} | "
                     f"{worst.get('pdf', {}).get('form_text_ops')} | "
                     f"{worst.get('pdf', {}).get('form_cjk_fonts')} |")
    lines += ["", "| Id | Page | Fonts (subtype, /ToUnicode, CJK CMap) | Forms reached by Do |",
              "| --- | --- | --- | --- |"]
    for doc in loss_docs:
        for worst in (doc["loss"].get("worst_pages") or [])[:2]:
            pdf = worst.get("pdf") or {}
            fonts = "; ".join(f"{font['name']} ({font['subtype']}, "
                              f"{'ToUnicode' if font['to_unicode'] else 'no ToUnicode'}"
                              f"{', CJK' if font['cjk_cmap'] else ''})"
                              for font in (pdf.get("fonts") or [])[:4])
            forms = "; ".join(f"{form['name']} ({form['text_ops']} ops, "
                              f"{'CJK font' if form['has_cjk_font'] else 'no CJK font'})"
                              for form in (pdf.get("forms") or [])[:4])
            lines.append(f"| {doc['id']} | {worst.get('page')} | {fonts[:150]} | "
                         f"{forms[:150] or '-'} |")

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
            "header_cases": dict(case_totals),
            "header_positions": dict(position_totals),
            "header_documents": sorted(header_docs),
            "missed_boundary_tokens": missed_tokens.most_common(10),
            "loss_documents": [doc["id"] for doc in loss_docs],
        })
    return 0


# --------------------------------------------------------------------------------------------
# Entry point
# --------------------------------------------------------------------------------------------

def main() -> int:
    parser = argparse.ArgumentParser(
        description="PDF-6a text-quality measurement on the PDF-5a corpus.")
    parser.add_argument("--aggregate", action="store_true", help="merge shards into tables")
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
    args = parser.parse_args()
    if args.aggregate:
        if not args.jsonl_dir:
            parser.error("--aggregate needs --jsonl-dir")
        return aggregate_mode(args)
    if not args.binary or not args.out:
        parser.error("run mode needs --binary and --out")
    args.cap_available = PROBE.cap_available()
    if not args.cap_available:
        print("::warning::systemd-run MemoryMax scope is not permitted; running without the cap",
              flush=True)
    return run_shard(args)


if __name__ == "__main__":
    sys.exit(main())
