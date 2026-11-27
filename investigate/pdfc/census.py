#!/usr/bin/env python3
"""pdfc: what still keeps PDF text paragraph-only in sentence mode.

For every input this census reports, on the sentence stream of one release binary:

1. the paragraphs, the coarse paragraphs, the coarse-only characters, their share of the
   document's paragraph characters, and the sentences the reader gets;
2. the reason the #139 fallback guard refused each coarse paragraph, re-derived here by the
   same rules the server runs (`src/domain/segmentation.rs`): the pairwise-doubled token, the
   bibliographic density window, and the missing-space glue count at paragraph level and per
   sentence, with the PDF-6d digit/uppercase narrowing and the PDF-6e repeated-name exemption
   already applied, plus the per-sentence re-check PDF-6f added and the non-prose content
   classes;
3. for a glue refusal, which junction kinds the deciding sentence needs, so the reasons can be
   ranked by coarse characters.

The rules are not copied: they are imported from `scripts/pdf-guard-events.py` (the guard's
event port, PDF-6d) and `scripts/pdf6f-sentence-price.py` (the guard's PDF-profile verdict and
the `sentence_ranges` port). `--trace-dir` compares this port against a scratch build that
writes one line per refused paragraph (`READING_MCP_PDFC_TRACE`), so a private run without an
instrumented binary still knows how far the port can be trusted.

    python3 investigate/pdfc/census.py --binary target/release/reading-mcp \
        --state-dir .tmp/pdfc/state --sources .tmp/pdfc/sources.txt \
        --out .tmp/pdfc/census

The sources file is one input per line: a path to a PDF, an `arxiv:<id>` or bare arXiv id, an
http(s) URL, or a JSON object line with an `id` (the shape of `scripts/pdf-sweep-ids.txt`, whose
`creator` / `producer` / `image_pages` classify the input type). Blank lines and `#` comments are
skipped. Downloads are cached under `--cache-dir`, so a second run reads the same bytes.

Standard library only.
"""

from __future__ import annotations

import argparse
import collections
import importlib.util
import json
import os
import re
import shutil
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = os.path.normpath(os.path.join(HERE, "..", "..", "scripts"))
ARXIV_PDF = "https://arxiv.org/pdf/{id}"
USER_AGENT = "reading-mcp-bench-pdfc"
FETCH_DELAY_S = 2.0
HEAD_CHARS = 120          # the example text kept per coarse paragraph
MAX_PDF_BYTES = 134217728
OPEN_TIMEOUT_S = 300.0
DOC_DEADLINE_S = 3600.0

GLUE_KINDS = ("digit_upper", "punct_upper", "paren_alpha", "caps_tail", "lower_upper")

REASON_DOUBLED = "corruption_doubled_token"
REASON_BIBLIOGRAPHIC = "corruption_bibliographic_run"
REASON_GLUE = "glue_missing_space_sentence"
REASON_ALL_FORMATS = "guard_all_formats"
REASON_CLASS = "content_class"


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
    """Exactly what the PDF profile counts: PDF-6d's narrowing plus PDF-6e's name exemption."""
    return GE.BASELINE(event) or GE.IMPLEMENTED(event, context)


def kinds_of(text: str, exempt=None, context=None) -> tuple[dict, int]:
    verdict = GE.guard_verdict(text, exempt, context)
    return dict(verdict["kinds"]), verdict["glue_events"]


def repeated_context(text: str) -> dict:
    return {"tokens": GE.token_counts(text)}


# --- the bibliographic rule, ported exactly --------------------------------------------------
# `scripts/pdf-guard-events.py` splits words on ASCII only, and the server splits on
# `char::is_alphanumeric`, so on Cyrillic/CJK text the two window differently. This census needs
# the server's own verdict, so the rule is ported here from `bibliographic_marker_counts`.

MONTH_PREFIXES = ("jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct",
                  "nov", "dec")
VOLUME_WORDS = ("vol", "volume", "no", "number", "issue", "series", "pp", "p", "pages")
RIGHTS_WORDS = ("copyright", "isbn", "issn")


def alphanumeric_words(text: str) -> list[str]:
    """`text.split(|ch| !ch.is_alphanumeric()).filter(!empty)`."""
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
    return word[:3] in MONTH_PREFIXES and all(ch.isalpha() for ch in word)


def nearby_number(words: list[str], index: int) -> bool:
    low = max(0, index - 2)
    high = min(index + 3, len(words))
    return any(low + offset != index and ascii_digit_first(word)
               for offset, word in enumerate(words[low:high]))


def bibliographic_markers(text: str) -> int:
    """`has_bibliographic_metadata_run`'s best 10-word window of marker counts."""
    words = alphanumeric_words(text)
    if len(words) < GE.MIN_BIBLIOGRAPHIC_MARKERS:
        return 0
    counts: list[int] = []
    for index, word in enumerate(words):
        lower = word.lower()
        following = words[index + 1] if index + 1 < len(words) else None
        count = 0
        if lower in VOLUME_WORDS and following is not None and ascii_digit_first(following):
            count += 1
        if is_month_word(lower) and nearby_number(words, index):
            count += 1
        if len(word) == 4 and all("0" <= ch <= "9" for ch in word) \
                and 1800 <= int(word) <= 2099:
            count += 1
        if "http" in lower or "www." in lower or "url" in lower or lower == "doi":
            count += 1
        if lower in RIGHTS_WORDS:
            count += 1
        counts.append(count)
    window = min(GE.BIBLIOGRAPHIC_WINDOW_WORDS, len(counts))
    best = sum(counts[:window])
    current = best
    for index in range(window, len(counts)):
        current += counts[index] - counts[index - window]
        best = max(best, current)
    return best


def paragraph_reason(text: str) -> dict:
    """The guard's verdict on one coarse paragraph, with the evidence behind it.

    Mirrors `collect_section_segmentation`: doubled and bibliographic are paragraph-wide and
    come first; the glue count is judged on the paragraph, then re-judged sentence by sentence
    on the same ranges `main` splits (`rules=frozenset()`), and the paragraph stays coarse only
    when one of its own sentences reaches the threshold.
    """
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
    firing = []
    pieces = SP.sentence_ranges(text)
    for start, end in pieces:
        piece = text[start:end]
        sentence_kinds, sentence_events = kinds_of(piece, pdf_exempt, context)
        if sentence_events >= GE.MIN_MISSING_SPACE_GLUE_EVENTS:
            firing.append({"chars": end - start, "events": sentence_events,
                           "kinds": sentence_kinds, "head": piece[:HEAD_CHARS]})
    if not firing:
        return {"reason": "unexplained", "evidence": {"paragraph_events": events,
                                                      "sentences": len(pieces)}}
    return {"reason": REASON_GLUE,
            "evidence": {"kinds": kinds, "events": events, "bare_events": bare_events,
                         "sentences": len(pieces), "firing": firing}}


def deciding_kinds(reason: dict) -> list[str]:
    """The junction kinds the verdict needs: the smallest set that reaches the threshold.

    Greedy over the kind counts of the deciding scope (the firing sentence for a glue
    refusal), so a kind that only rides along with a sufficient one is not named.
    """
    if reason["reason"] == REASON_DOUBLED:
        return ["doubled_token"]
    if reason["reason"] == REASON_BIBLIOGRAPHIC:
        return ["bibliographic"]
    if reason["reason"] != REASON_GLUE:
        return ["none"]
    counts = reason["evidence"]["firing"][0]["kinds"]
    ordered = sorted(counts.items(), key=lambda item: -item[1])
    needed: list[str] = []
    total = 0
    for kind, count in ordered:
        if total >= GE.MIN_MISSING_SPACE_GLUE_EVENTS:
            break
        needed.append(kind)
        total += count
    return needed


# --------------------------------------------------------------------------------------------
# Inputs
# --------------------------------------------------------------------------------------------

ARXIV_ID_RE = re.compile(r"\A\d{4}\.\d{4,5}(v\d+)?\Z")


def parse_sources(path: str) -> list[dict]:
    """One entry per input: {source, id, title, creator, producer, image_pages, pages, group}."""
    entries: list[dict] = []
    base = os.path.dirname(os.path.abspath(path))
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if line.startswith("{"):
                record = json.loads(line)
                source = record.get("path") or f"arxiv:{record['id']}"
                entries.append({
                    "source": source,
                    "id": record.get("id"),
                    "title": record.get("title"),
                    "creator": record.get("creator"),
                    "producer": record.get("producer"),
                    "image_pages": record.get("image_pages"),
                    "pages": record.get("pages"),
                    "group": record.get("group"),
                })
                continue
            token = line.split()[0]
            entries.append({"source": token, "id": None, "title": None, "creator": None,
                            "producer": None, "image_pages": None, "pages": None,
                            "group": None, "_base": base})
    return entries


def input_type(entry: dict, path: str) -> str:
    """LaTeX paper / Word document / scan with a text layer / other, from the corpus metadata."""
    text = f"{entry.get('creator') or ''} {entry.get('producer') or ''}".lower()
    name = os.path.basename(path).lower()
    if "scanned_pdf" in path or "raster-text-page" in name:
        return "scan_text_layer"
    if "microsoft" in text or "word" in text:
        return "word"
    if any(marker in text for marker in ("latex", "pdftex", "xdvipdfmx", "context mkiv",
                                         "dvips", "luatex", "xetex")):
        return "latex"
    pages, images = entry.get("pages"), entry.get("image_pages")
    if isinstance(pages, int) and isinstance(images, int) and pages and images * 2 >= pages:
        return "scan_text_layer"
    return "other"


def resolve(entry: dict, cache_dir: str) -> tuple[str | None, str]:
    """The local path of an input, downloading an arXiv id or a URL into `cache_dir`."""
    source = entry["source"]
    base = entry.get("_base") or os.getcwd()
    if os.path.isfile(source):
        return os.path.abspath(source), None
    if os.path.isfile(os.path.join(base, source)):
        return os.path.abspath(os.path.join(base, source)), None
    if source.startswith("arxiv:") or ARXIV_ID_RE.match(source):
        identifier = source.split(":", 1)[1] if source.startswith("arxiv:") else source
        url = ARXIV_PDF.format(id=identifier)
        name = f"{identifier.replace('/', '_')}.pdf"
    elif source.startswith("http://") or source.startswith("https://"):
        url = source
        name = re.sub(r"[^A-Za-z0-9._-]+", "_", source.rsplit("/", 1)[-1]) or "download.pdf"
    else:
        return None, f"not a file and not an arXiv id or URL: {source}"
    os.makedirs(cache_dir, exist_ok=True)
    path = os.path.join(cache_dir, name)
    if os.path.isfile(path) and os.path.getsize(path) > 0:
        return path, None
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=300) as response:
            data = response.read(MAX_PDF_BYTES + 1)
    except Exception as exc:  # noqa: BLE001 - reported per document
        return None, f"{type(exc).__name__}: {str(exc)[:160]}"
    time.sleep(FETCH_DELAY_S)
    if not data.startswith(b"%PDF"):
        return None, "response is not a PDF"
    if len(data) > MAX_PDF_BYTES:
        return None, "larger than READING_MCP_MAX_DOCUMENT_BYTES"
    with open(path, "wb") as handle:
        handle.write(data)
    return path, None


# --------------------------------------------------------------------------------------------
# One document through stdio MCP
# --------------------------------------------------------------------------------------------

def read_stream(server, handle: str, unit: str | None, deadline: float) -> dict:
    """Every item of one v2 `read` stream: [(handle, text, kind)], to the end."""
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


def census_document(entry: dict, path: str, args, trace_dir: str | None) -> dict:
    """The census of one document: paragraph coverage, coarse share, and per-paragraph reasons."""
    label = entry.get("id") or os.path.basename(path)
    record: dict = {
        "id": label,
        "source": entry["source"],
        "path": path,
        "type": input_type(entry, path),
        "title": entry.get("title"),
        "outcome": None,
        "error": None,
        "paragraphs": 0,
        "coarse_paragraphs": 0,
        "chars": 0,
        "coarse_chars": 0,
        "share": None,
        "sentences": 0,
        "reasons": {},
        "coarse": [],
    }
    state_dir = os.path.join(args.state_dir, re.sub(r"[^A-Za-z0-9._-]+", "_", label))
    shutil.rmtree(state_dir, ignore_errors=True)
    os.makedirs(state_dir, exist_ok=True)
    trace_path = os.path.join(trace_dir, re.sub(r"[^A-Za-z0-9._-]+", "_", label) + ".jsonl"
                              ) if trace_dir else None
    if trace_path and os.path.exists(trace_path):
        os.unlink(trace_path)
    env = PROBE.server_env(state_dir, os.path.dirname(os.path.abspath(path)), "default", "")
    if trace_path:
        env["READING_MCP_PDFC_TRACE"] = trace_path
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
        record["sections"] = PROBE.opened_section_count(opened)
        try:
            outline = PROBE.read_sections(server, handle)
        except PROBE.ProbeError:
            outline = []
        titles = {section["section_id"]: section.get("title") or "" for section in outline}
        record["section_titles"] = titles
        paragraphs = read_stream(server, handle, "paragraph", deadline)
        sentences = read_stream(server, handle, None, deadline)
        record["paragraph_stream"] = {"items": len(paragraphs["items"]),
                                     "truncated": paragraphs["truncated"],
                                     "error": paragraphs.get("error")}
        record["sentence_stream"] = {"items": len(sentences["items"]),
                                     "truncated": sentences["truncated"],
                                     "error": sentences.get("error")}
        by_key = {paragraph_key(item[0]): item[1] for item in paragraphs["items"]}
        coarse_items = [item for item in sentences["items"] if item[2] is not None]
        record["paragraphs"] = len(paragraphs["items"])
        record["chars"] = sum(len(item[1]) for item in paragraphs["items"])
        record["sentences"] = len(sentences["items"]) - len(coarse_items)
        record["coarse_paragraphs"] = len(coarse_items)
        reasons: collections.Counter = collections.Counter()
        reason_chars: collections.Counter = collections.Counter()
        coarse_chars = 0
        for item in coarse_items:
            item_handle, text, kind = item
            key = paragraph_key(item_handle)
            body = by_key.get(key, text)
            if kind == "paragraph":
                verdict = paragraph_reason(body)
            else:
                verdict = {"reason": REASON_CLASS, "evidence": {"class": kind}}
            chars = len(body)
            coarse_chars += chars
            reasons[verdict["reason"]] += 1
            reason_chars[verdict["reason"]] += chars
            record["coarse"].append({
                "handle": item_handle,
                "section": PROBE.handle_section(item_handle),
                "section_title": titles.get(PROBE.handle_section(item_handle), ""),
                "kind": kind,
                "chars": chars,
                "reason": verdict["reason"],
                "deciding_kinds": deciding_kinds(verdict),
                "evidence": verdict["evidence"],
                "head": re.sub(r"\s+", " ", body)[:HEAD_CHARS],
                **({"text": body} if args.keep_paragraph_text else {}),
            })
        record["coarse_chars"] = coarse_chars
        record["share"] = round(coarse_chars / record["chars"], 4) if record["chars"] else None
        record["reasons"] = dict(reasons)
        record["reason_chars"] = dict(reason_chars)
        record["outcome"] = "ok"
    finally:
        server.close(grace=30.0)
    if trace_path and os.path.exists(trace_path):
        record["trace"] = read_trace(trace_path)
        record["trace_agreement"] = compare_trace(record)
    return record


def read_trace(path: str) -> list[dict]:
    lines = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line:
                try:
                    lines.append(json.loads(line))
                except json.JSONDecodeError:
                    continue
    return lines


TRACE_REASON = {
    "corruption": None,           # doubled or bibliographic, decided by the fields
    "glue_sentence": REASON_GLUE,
    "content_class": REASON_CLASS,
    "all_formats_guard": REASON_ALL_FORMATS,
}


def trace_reason(line: dict) -> str:
    reason = line.get("reason")
    if reason == "corruption":
        if line.get("doubled"):
            return REASON_DOUBLED
        return REASON_BIBLIOGRAPHIC
    return TRACE_REASON.get(reason) or reason


def compare_trace(record: dict) -> dict:
    """Agreement of the port with the scratch build's own verdicts, keyed by (section, paragraph).

    The trace is written in the server's own paragraph order, so the two lists are zipped: any
    extra or missing line shows up as a length mismatch, and a reason mismatch is counted.
    """
    trace = record.get("trace") or []
    coarse = record["coarse"]
    matched = 0
    reason_mismatch = []
    for line, item in zip(trace, coarse):
        matched += 1
        if trace_reason(line) != item["reason"]:
            reason_mismatch.append({"handle": item["handle"], "port": item["reason"],
                                    "server": trace_reason(line)})
    return {"server_refusals": len(trace), "port_refusals": len(coarse),
            "compared": matched, "reason_mismatches": reason_mismatch[:20],
            "agree": not reason_mismatch and len(trace) == len(coarse)}


# --------------------------------------------------------------------------------------------
# Tables
# --------------------------------------------------------------------------------------------

def pct(value: float | None) -> str:
    return "n/a" if value is None else f"{100 * value:.1f}%"


REFERENCES_TITLE = re.compile(
    r"\b(references|bibliography|literature cited|works cited|参考文献)\b", re.IGNORECASE)


def in_references(item: dict) -> bool:
    return bool(REFERENCES_TITLE.search(item.get("section_title") or ""))


def reason_rows(records: list[dict]) -> list[tuple[str, str, int, int, int]]:
    """(reason, primary kind, paragraphs, coarse chars, in a references section) rows.

    The primary kind is the one the deciding sentence leans on most, so a paragraph appears in
    exactly one row and the ranking reads as a cause rather than as a kind combination.
    """
    by_reason: dict[tuple[str, str], dict] = {}
    for record in records:
        for item in record["coarse"]:
            primary = (item["deciding_kinds"] or ["none"])[0]
            row = by_reason.setdefault((item["reason"], primary),
                                       {"paragraphs": 0, "chars": 0, "references": 0})
            row["paragraphs"] += 1
            row["chars"] += item["chars"]
            row["references"] += int(in_references(item))
    rows = sorted(by_reason.items(), key=lambda pair: -pair[1]["chars"])
    return [(reason, primary, row["paragraphs"], row["chars"], row["references"])
            for (reason, primary), row in rows]


def coarse_share_table(records: list[dict]) -> list[str]:
    out = ["| input | type | paragraphs | coarse | chars | coarse chars | share | sentences |",
           "| --- | --- | --- | --- | --- | --- | --- | --- |"]
    for record in sorted(records, key=lambda r: -(r["share"] or 0)):
        out.append(f"| {record['id']} | {record['type']} | {record['paragraphs']} | "
                   f"{record['coarse_paragraphs']} | {record['chars']} | "
                   f"{record['coarse_chars']} | {pct(record['share'])} | "
                   f"{record['sentences']} |")
    return out


def totals_row(label: str, records: list[dict]) -> str:
    chars = sum(r["chars"] for r in records)
    coarse = sum(r["coarse_chars"] for r in records)
    paragraphs = sum(r["paragraphs"] for r in records)
    coarse_paragraphs = sum(r["coarse_paragraphs"] for r in records)
    share = coarse / chars if chars else None
    return (f"| {label} | {len(records)} | {paragraphs} | {coarse_paragraphs} | {chars} | "
            f"{coarse} | {pct(share)} |")


def type_totals(records: list[dict]) -> list[str]:
    out = ["| type | inputs | paragraphs | coarse | chars | coarse chars | share |",
           "| --- | --- | --- | --- | --- | --- | --- |"]
    groups = collections.defaultdict(list)
    for record in records:
        groups[record["type"]].append(record)
    for name in sorted(groups, key=lambda name: -sum(r["coarse_chars"] for r in groups[name])):
        out.append(totals_row(name, groups[name]))
    out.append(totals_row("all", records))
    return out


def report(records: list[dict], trace_note: str) -> str:
    ok = [record for record in records if record["outcome"] == "ok"]
    failed = [record for record in records if record["outcome"] != "ok"]
    out = ["# pdfc census", ""]
    out.append(f"{len(ok)} of {len(records)} inputs measured, {len(failed)} not.")
    out.append("")
    out.append("## Coarse share by type")
    out.append("")
    out.extend(type_totals(ok))
    out.append("")
    out.append("## Reasons, ranked by coarse characters (all inputs)")
    out.append("")
    out.append("| reason | primary kind | paragraphs | coarse chars | in references |")
    out.append("| --- | --- | --- | --- | --- |")
    for reason, primary, paragraphs, chars, references in reason_rows(ok):
        out.append(f"| {reason} | {primary} | {paragraphs} | {chars} | {references} |")
    out.append("")
    out.append("## Reasons by input type")
    out.append("")
    groups = collections.defaultdict(list)
    for record in ok:
        groups[record["type"]].append(record)
    for name in sorted(groups):
        out.append(f"### {name}")
        out.append("")
        out.append("| reason | primary kind | paragraphs | coarse chars | in references |")
        out.append("| --- | --- | --- | --- | --- |")
        for reason, primary, paragraphs, chars, references in reason_rows(groups[name]):
            out.append(f"| {reason} | {primary} | {paragraphs} | {chars} | {references} |")
        out.append("")
    out.append("## Coarse share by input")
    out.append("")
    out.extend(coarse_share_table(ok))
    if failed:
        out.append("")
        out.append("## Not measured")
        out.append("")
        out.append("| input | outcome | error |")
        out.append("| --- | --- | --- |")
        for record in failed:
            out.append(f"| {record['id']} | {record['outcome']} | "
                       f"{(record['error'] or '').replace('|', '/')[:200]} |")
    if trace_note:
        out.append("")
        out.append("## Port against the scratch build")
        out.append("")
        out.append(trace_note)
    return "\n".join(out) + "\n"


def trace_summary(records: list[dict]) -> str:
    compared = [record for record in records if record.get("trace_agreement")]
    if not compared:
        return ""
    agree = sum(1 for record in compared if record["trace_agreement"]["agree"])
    mismatches = sum(len(record["trace_agreement"]["reason_mismatches"])
                     for record in compared)
    refusals = sum(record["trace_agreement"]["server_refusals"] for record in compared)
    port = sum(record["trace_agreement"]["port_refusals"] for record in compared)
    lines = [f"- {len(compared)} inputs carry a trace; the port and the server agree on every "
             f"refusal in {agree} of them.",
             f"- {refusals} server refusals against {port} port refusals, "
             f"{mismatches} reason mismatches."]
    for record in compared:
        if not record["trace_agreement"]["agree"]:
            lines.append(f"- {record['id']}: {json.dumps(record['trace_agreement'])[:400]}")
    return "\n".join(lines)


# --------------------------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------------------------

def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", required=True, help="the release reading-mcp binary")
    parser.add_argument("--state-dir", required=True,
                        help="one state directory per input goes under here")
    parser.add_argument("--sources", required=True, help="the sources file")
    parser.add_argument("--out", default=None,
                        help="output directory (default: <state-dir>/census)")
    parser.add_argument("--cache-dir", default=None,
                        help="where downloads are cached (default: <state-dir>/pdfs)")
    parser.add_argument("--trace-dir", default=None,
                        help="trace directory of an instrumented scratch build, to check the port")
    parser.add_argument("--limit", type=int, default=None, help="only the first N inputs")
    parser.add_argument("--keep-paragraph-text", action="store_true",
                        help="also write the full text of every coarse paragraph to "
                             "coarse-text.jsonl (the what-if analysis needs it; the text is "
                             "the document's own, so leave it off for a private corpus)")
    parser.add_argument("--keep-state", action="store_true",
                        help="keep each input's state directory instead of removing it")
    args = parser.parse_args(argv)

    args.state_dir = os.path.abspath(args.state_dir)
    out_dir = os.path.abspath(args.out or os.path.join(args.state_dir, "census"))
    cache_dir = os.path.abspath(args.cache_dir or os.path.join(args.state_dir, "pdfs"))
    trace_dir = os.path.abspath(args.trace_dir) if args.trace_dir else None
    if trace_dir:
        os.makedirs(trace_dir, exist_ok=True)
    os.makedirs(out_dir, exist_ok=True)
    entries = parse_sources(args.sources)
    if args.limit:
        entries = entries[:args.limit]
    records: list[dict] = []
    jsonl = open(os.path.join(out_dir, "census.jsonl"), "w", encoding="utf-8")
    texts = (open(os.path.join(out_dir, "coarse-text.jsonl"), "w", encoding="utf-8")
             if args.keep_paragraph_text else None)
    try:
        for index, entry in enumerate(entries, 1):
            path, error = resolve(entry, cache_dir)
            if path is None:
                record = {"id": entry.get("id") or entry["source"], "source": entry["source"],
                          "outcome": "download_failed", "error": error, "type": "other",
                          "paragraphs": 0, "coarse_paragraphs": 0, "chars": 0,
                          "coarse_chars": 0, "share": None, "sentences": 0, "reasons": {},
                          "coarse": []}
            else:
                record = census_document(entry, path, args, trace_dir)
                if not args.keep_state:
                    shutil.rmtree(os.path.join(
                        args.state_dir,
                        re.sub(r"[^A-Za-z0-9._-]+", "_", record["id"])), ignore_errors=True)
            if texts is not None:
                for item in record["coarse"]:
                    text = item.pop("text", None)
                    if text is not None:
                        texts.write(json.dumps({"id": record["id"], "handle": item["handle"],
                                                "section_title": item["section_title"],
                                                "reason": item["reason"],
                                                "deciding_kinds": item["deciding_kinds"],
                                                "text": text}, ensure_ascii=False) + "\n")
                texts.flush()
            records.append(record)
            jsonl.write(json.dumps(record, ensure_ascii=False) + "\n")
            jsonl.flush()
            print(f"[{index}/{len(entries)}] {record['id']} {record['outcome']} "
                  f"coarse={pct(record['share'])} sentences={record['sentences']}",
                  file=sys.stderr)
    finally:
        jsonl.close()
        if texts is not None:
            texts.close()
    note = trace_summary(records)
    with open(os.path.join(out_dir, "report.md"), "w", encoding="utf-8") as handle:
        handle.write(report(records, note))
    print(os.path.join(out_dir, "report.md"))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
