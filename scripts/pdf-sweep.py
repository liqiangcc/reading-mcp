#!/usr/bin/env python3
"""PDF-5a: a wide real-world PDF sweep with the deployed binary (measurement only).

Three modes, standard library plus PyMuPDF (the independent reference of PDF-1a/1b):

- `--select`: build the corpus list (`scripts/pdf-sweep-ids.txt`, JSON lines with a comment
  header) from the arXiv API across four groups: 100 ordinary papers (10 x 10 categories),
  10 pre-2005 papers (scan risk), 8 long documents (250-900 pages) and 24 layout hard cases
  (math / tables / raster figures / beamer slides). Every kept id has a licence line read from
  its abs page (CC-BY / CC-BY-SA / CC0 only), a page count, categories, producer and the
  metric that qualified it. Ids already in `pdf-corpus-ids.txt` or `pdf-cjk-ids.txt` are
  excluded. Downloads happen at CI time; only the list is committed.
- `--run`: for one shard of the list, download each PDF (re-checking the licence line), measure
  a PyMuPDF reference text, then open and read it through stdio MCP with the **default** PDF
  backend inside a 600 MB memory cap (`sudo systemd-run --scope -p MemoryMax=600M
  -p MemorySwapMax=0`, exactly as `scripts/pdf-probe.py`), and record the outcome class, open
  seconds, peak RSS (VmHWM), state-dir bytes, structure, 200 sequential sentence reads, one
  search, the quality proxies against the PyMuPDF reference, and the silent-failure flag.
  Writes one JSONL record per document.
- `--aggregate`: merge every shard's JSONL and print the tables used in
  `docs/design/pdf-sweep-data.md` (outcome classes, non-ok and suspect list by cause, open time
  and peak RSS by page bucket, long documents, the ranked reader-impact list).

Helpers are imported from `scripts/pdf-corpus.py` and `scripts/pdf-probe.py` and never edited.
This script never builds anything and changes no product code.
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import importlib.util
import json
import math
import os
import re
import shutil
import statistics
import sys
import time
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

MIB = 1024 * 1024
ARXIV_API = "https://export.arxiv.org/api/query"
ARXIV_ABS = "https://arxiv.org/abs/{id}"
ARXIV_PDF = "https://arxiv.org/pdf/{id}"
# >= 3 s between arXiv API calls (the hard rule). Abs pages and PDFs are fetched sequentially,
# one at a time, with a shorter pause: they are not the API.
ARXIV_DELAY_S = 3.0
FETCH_DELAY_S = 2.0
USER_AGENT = "reading-mcp-bench-pdf5a"
# arXiv offered CC-BY(-SA) 1.0/2.0/2.5 before 4.0, so the version is not pinned: the task's
# rule is the licence family (CC-BY / CC-BY-SA / CC0), not the version.
CC_LICENSE_RE = re.compile(r"creativecommons\.org/licenses/(by|by-sa)/[0-9.]+")
CC0_RE = re.compile(r"creativecommons\.org/publicdomain/zero/[0-9.]+")
MAX_PDF_BYTES = 134217728  # the server's own default READING_MCP_MAX_DOCUMENT_BYTES
DATE_FILTER = "submittedDate:[202301010000 TO 202612312359]"
OLD_DATE_FILTER = "submittedDate:[199401010000 TO 200412312359]"

CAP = "600M"
OPEN_TIMEOUT_S = 120.0
READ_CALLS = 200
SEARCH_LIMIT = 50
CONTAINMENT_PAGES = 20
GRAM = 5
TEXT_BUDGET_CHARS = 1500000
BATCH_ITEMS = 256
LOW_TEXT_CHARS = 50
RICH_REF_CHARS_PER_PAGE = 500
POOR_DEFAULT_CHARS_PER_PAGE = 100
CONTAINMENT_SUSPECT = 0.6
RASTER_SHARE = 0.2
HEADER_FOOTER_SHARE = 0.3

MATH_CHARS = set("=+−-<>()[]{}∑∫√≈≤≥≠∂∇∞±×÷∏∈∉⊂⊆∪∩→←↔∀∃αβγδεζηθικλμνξπρστυφχψωΓΔΘΛΞΠΣΦΨΩ")
TOKEN_RE = re.compile(r"[A-Za-z][A-Za-z\-]{5,}")

OUTCOME_CLASSES = ("ok", "ocr_required", "unreadable", "parse_failed", "too_large", "timeout",
                   "oom_killed", "download_failed", "not_run")

# (label, arXiv query, quota). 10 x 10 = the 100 ordinary papers, 6-40 pages.
ORDINARY_SLOTS = [
    ("cs.CL", "cat:cs.CL", 10),
    ("cs.CV", "cat:cs.CV", 10),
    ("cs.LG", "cat:cs.LG", 10),
    ("cs.CR", "cat:cs.CR", 10),
    ("cs.DB", "cat:cs.DB", 10),
    ("math", "(cat:math.NT OR cat:math.CO OR cat:math.PR OR cat:math.AG)", 10),
    ("physics", "(cat:hep-th OR cat:quant-ph OR cat:cond-mat.stat-mech OR cat:physics.flu-dyn)",
     10),
    ("q-bio", "(cat:q-bio.QM OR cat:q-bio.NC OR cat:q-bio.PE)", 10),
    ("stat", "(cat:stat.ME OR cat:stat.AP OR cat:stat.TH)", 10),
    ("econ/eess", "(cat:econ.EM OR cat:econ.GN OR cat:eess.SP OR cat:eess.IV)", 10),
]
OLD_QUERIES = [
    "submittedDate:[200401010000 TO 200412312359]",
    "(cat:cs.DL OR cat:cs.CL OR cat:cs.CV OR cat:cs.LG)",
    "(cat:math.NT OR cat:math.CO OR cat:math.PR)",
    "cat:hep-th",
]
LONG_QUERIES = [
    'abs:"lecture notes"',
    'abs:"these notes"',
    '(abs:thesis OR abs:"doctoral thesis" OR abs:"master thesis")',
    '(abs:textbook OR abs:monograph)',
    '(abs:"lecture notes" AND (cat:quant-ph OR cat:math-ph OR cat:hep-th))',
    '(abs:"course notes" OR abs:"review of" OR abs:survey)',
]
HARD_SLOTS = [
    ("hard-math", ['(cat:math.* AND (abs:theorem OR abs:"we prove"))']),
    ("hard-tables", ['(abs:table AND (cat:cs.DB OR cat:cs.CL OR cat:cs.IR))']),
    ("hard-figures", ['(abs:figure AND (cat:cs.CV OR cat:q-bio.QM OR cat:eess.IV))',
                      '(abs:"Fig." AND (cat:cs.CV OR cat:q-bio.QM OR cat:eess.IV))',
                      '(cat:cs.CV AND (abs:microscopy OR abs:segmentation))']),
    ("hard-slides", ['(abs:slides OR abs:beamer OR abs:"slide deck")',
                     '(co:slides OR co:beamer OR co:talk)',
                     '(abs:presentation AND (cat:cs.CL OR cat:cs.LG OR cat:cs.CV))']),
]
GROUP_TARGET = {
    "ordinary": 100,
    "old-scan": 10,
    "long": 8,
    "hard-math": 6,
    "hard-tables": 6,
    "hard-figures": 6,
    "hard-slides": 6,
}
GROUP_ORDER = ["ordinary", "long", "hard-math", "hard-tables", "hard-figures", "hard-slides",
               "old-scan"]
MAX_DOWNLOADS = 380
MAX_CANDIDATES_PER_SLOT = 40
HARD_MAX_CANDIDATES = 20
LONG_MAX_DOWNLOADS = 30
LONG_MAX_CANDIDATES = 30
OLD_MAX_CANDIDATES = 40


# --------------------------------------------------------------------------------------------
# Shared helpers (imported from the PDF-1a corpus script and the PDF probe, never edited)
# --------------------------------------------------------------------------------------------

def load_sibling(name: str):
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), name)
    spec = importlib.util.spec_from_file_location(name.replace("-", "_").replace(".py", ""), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CORPUS = load_sibling("pdf-corpus.py")
PROBE = CORPUS.load_probe()
fetch = CORPUS.fetch
squash = CORPUS.squash
license_marker = CORPUS.license_marker
write_json = CORPUS.write_json
append_jsonl = CORPUS.append_jsonl
gram_containment = CORPUS.gram_containment
percentile = CORPUS.percentile
fmt = CORPUS.fmt


def load_list(path: str) -> list[dict]:
    entries = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            entries.append(json.loads(line))
    return entries


def group_label(group: str | None) -> str:
    if group in ("ordinary", "old-scan", "long"):
        return group
    return "hard"


# --------------------------------------------------------------------------------------------
# Selection
# --------------------------------------------------------------------------------------------

def arxiv_query(query: str, start: int, max_results: int) -> list[dict]:
    """One arXiv API page. Old-style ids (`hep-th/9501001`) keep their archive prefix."""
    url = (f"{ARXIV_API}?search_query={urllib.parse.quote(query)}&start={start}"
           f"&max_results={max_results}&sortBy=relevance&sortOrder=descending")
    payload = fetch(url, 120)
    root = ET.fromstring(payload)
    namespace = {"atom": "http://www.w3.org/2005/Atom",
                 "arxiv": "http://arxiv.org/schemas/atom"}
    entries = []
    for entry in root.findall("atom:entry", namespace):
        identifier = entry.findtext("atom:id", default="", namespaces=namespace)
        match = re.search(r"abs/(.+?)(v\d+)?$", identifier)
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
            "submitted": entry.findtext("atom:published", default="", namespaces=namespace),
            "category": category,
        })
    return entries


def cc_marker(url: str) -> str | None:
    """The licence family of a URL, or None when it is not CC-BY / CC-BY-SA / CC0."""
    match = CC_LICENSE_RE.search(url) or CC0_RE.search(url)
    if match is None:
        return None
    return re.sub(r"^https?://", "", match.group(0))


def abs_license(identifier: str) -> tuple[str | None, str | None]:
    page = fetch(ARXIV_ABS.format(id=identifier), 120).decode("utf-8", "ignore")
    for found in re.findall(r"https?://(?:creativecommons\.org|arxiv\.org)/[a-z0-9./\-]+", page):
        marker = cc_marker(found)
        if marker is not None:
            return found, marker
    return None, None


def pdf_facts(path: str, group: str) -> dict:
    """Page count, text layer, raster pages, producer and the metric of this group."""
    import pymupdf

    text_chars = 0
    math_chars = 0
    cjk_chars = 0
    raster_pages = 0
    figure_pages = 0
    landscape_pages = 0
    image_pages = 0
    tables = 0
    with pymupdf.open(path) as document:
        pages = document.page_count
        metadata = document.metadata or {}
        for page in document:
            text = page.get_text()
            text_chars += len(squash(text))
            cjk_chars += sum(1 for char in text if CORPUS.is_cjk(char))
            if group == "hard-math":
                math_chars += sum(1 for char in text if char in MATH_CHARS)
            infos = page.get_image_info()
            page_area = page.rect.width * page.rect.height or 1.0
            largest = 0.0
            for image in infos:
                bbox = image.get("bbox") or [0, 0, 0, 0]
                area = max(0.0, (bbox[2] - bbox[0]) * (bbox[3] - bbox[1]))
                largest = max(largest, area / page_area)
            if infos:
                image_pages += 1
                if largest >= RASTER_SHARE:
                    raster_pages += 1
                if largest >= 0.05:
                    figure_pages += 1
            if page.rect.width > page.rect.height:
                landscape_pages += 1
            if group == "hard-tables":
                try:
                    tables += len(page.find_tables().tables)
                except Exception:  # noqa: BLE001 - a table detector failure is not a skip
                    pass
    chars_per_page = text_chars / pages if pages else 0.0
    facts = {
        "pages": pages,
        "text_chars": text_chars,
        "chars_per_page": round(chars_per_page, 1),
        "cjk_chars": cjk_chars,
        "image_pages": image_pages,
        "raster_pages": raster_pages,
        "figure_pages": figure_pages,
        "landscape_share": round(landscape_pages / pages, 3) if pages else 0.0,
        "producer": (metadata.get("producer") or "")[:120],
        "creator": (metadata.get("creator") or "")[:120],
    }
    if group == "hard-math":
        facts["math_per_1000"] = round(1000.0 * math_chars / max(text_chars, 1), 2)
    if group == "hard-tables":
        facts["tables"] = tables
    return facts


def qualify(group: str, facts: dict) -> tuple[bool, str, dict]:
    """Keep rule per group. Returns (keep, reason-or-metric, extra record fields)."""
    pages = facts["pages"]
    if group == "ordinary":
        if not 6 <= pages <= 40:
            return False, f"pages={pages}", {}
        if facts["chars_per_page"] < 200:
            return False, f"text_layer={facts['chars_per_page']}", {}
        return True, f"pages={pages}", {}
    if group == "old-scan":
        if not 4 <= pages <= 80:
            return False, f"pages={pages}", {}
        return True, f"pages={pages}", {}
    if group == "long":
        if not 250 <= pages <= 900:
            return False, f"pages={pages}", {}
        return True, f"pages={pages}", {}
    if not 4 <= pages <= 60:
        return False, f"pages={pages}", {}
    if group == "hard-math":
        if facts.get("math_per_1000", 0.0) < 15.0:
            return False, f"math_per_1000={facts.get('math_per_1000')}", {}
        return True, f"math_per_1000={facts['math_per_1000']}", {}
    if group == "hard-tables":
        if facts.get("tables", 0) < 6:
            return False, f"tables={facts.get('tables')}", {}
        return True, f"tables={facts['tables']}", {}
    if group == "hard-figures":
        if facts["figure_pages"] < 5:
            return False, f"figure_pages={facts['figure_pages']}", {}
        return True, f"figure_pages={facts['figure_pages']}", {}
    if group == "hard-slides":
        beamer = "beamer" in f"{facts['producer']} {facts['creator']}".lower()
        landscape = facts["landscape_share"] >= 0.6 and facts["chars_per_page"] <= 1500
        if not (beamer or landscape):
            return False, f"slides=no(landscape={facts['landscape_share']})", {}
        return True, "slides=beamer" if beamer else "slides=landscape", {}
    return False, "unknown_group", {}


class Selector:
    def __init__(self, work_dir: str, out: str, exclude: set[str]) -> None:
        self.work = os.path.join(work_dir, "select")
        os.makedirs(self.work, exist_ok=True)
        self.out = out
        self.exclude = exclude
        self.chosen: list[dict] = []
        self.skipped: list[dict] = []
        self.seen: set[str] = set()
        self.downloads = 0
        self.candidates = 0
        self.tried: collections.Counter = collections.Counter()
        self.kept: collections.Counter = collections.Counter()

    def write(self) -> None:
        header = [
            "# PDF-5a wide sweep corpus: arXiv papers, CC-BY(-SA)/CC0 only, one JSON object "
            "per line.",
            f"# Selected {time.strftime('%Y-%m-%d')} by scripts/pdf-sweep.py --select; "
            "downloads happen at CI time, only this list is committed.",
            "# id, title, group, category, pages, licence (abs-page link), producer, "
            "qualifying metric.",
        ]
        os.makedirs(os.path.dirname(os.path.abspath(self.out)) or ".", exist_ok=True)
        with open(self.out, "w", encoding="utf-8") as handle:
            handle.write("\n".join(header) + "\n")
            for entry in self.chosen:
                handle.write(json.dumps(entry, ensure_ascii=False, sort_keys=True) + "\n")

    def full(self, group: str) -> bool:
        return self.kept[group] >= GROUP_TARGET[group]

    def downloads_left(self) -> bool:
        return self.downloads < MAX_DOWNLOADS

    def consider(self, identifier: str, title: str, category: str, group: str,
                 submitted: str = "") -> bool:
        if self.full(group) or not self.downloads_left():
            return False
        if identifier in self.seen or identifier in self.exclude:
            return False
        self.seen.add(identifier)
        self.candidates += 1
        self.tried[group] += 1
        try:
            licence_url, marker = abs_license(identifier)
            time.sleep(FETCH_DELAY_S)
        except RuntimeError as exc:
            self.skipped.append({"id": identifier, "group": group,
                                 "reason": f"abs_failed: {exc}"})
            return False
        if marker is None:
            self.skipped.append({"id": identifier, "group": group, "reason": "licence_not_cc"})
            return False
        path = os.path.join(self.work, f"{identifier.replace('/', '_')}.pdf")
        if not os.path.isfile(path):
            self.downloads += 1
            try:
                data = fetch(ARXIV_PDF.format(id=identifier), 300, attempts=1)
            except Exception as exc:  # noqa: BLE001 - a failed download is a skip
                self.skipped.append({"id": identifier, "group": group,
                                     "reason": f"download_failed: {str(exc)[:120]}"})
                time.sleep(FETCH_DELAY_S)
                return False
            time.sleep(FETCH_DELAY_S)
            if not data.startswith(b"%PDF"):
                self.skipped.append({"id": identifier, "group": group, "reason": "not_a_pdf"})
                return False
            if len(data) > MAX_PDF_BYTES:
                self.skipped.append({"id": identifier, "group": group,
                                     "reason": f"bytes={len(data)}"})
                return False
            with open(path, "wb") as handle:
                handle.write(data)
        try:
            facts = pdf_facts(path, group)
        except Exception as exc:  # noqa: BLE001 - a broken PDF is a skip
            self.skipped.append({"id": identifier, "group": group,
                                 "reason": f"unreadable: {str(exc)[:120]}"})
            return False
        keep, reason, _extra = qualify(group, facts)
        if not keep:
            self.skipped.append({"id": identifier, "group": group, "reason": reason})
            return False
        self.chosen.append({
            "id": identifier,
            "title": title,
            "group": group,
            "category": category,
            "pages": facts["pages"],
            "license": licence_url,
            "license_marker": marker,
            "submitted": submitted[:10],
            "bytes": os.path.getsize(path),
            "sha256": hashlib.sha256(open(path, "rb").read()).hexdigest(),
            "producer": facts["producer"],
            "creator": facts["creator"],
            "metric": reason,
            "text_chars": facts["text_chars"],
            "image_pages": facts["image_pages"],
            "raster_pages": facts["raster_pages"],
        })
        self.kept[group] += 1
        print(f"{group} {self.kept[group]}/{GROUP_TARGET[group]} {identifier} "
              f"pages={facts['pages']} {reason} {title[:50]}", flush=True)
        self.write()
        return True

    def slot(self, query: str, group: str, quota: int, max_candidates: int,
             max_downloads: int | None = None) -> None:
        if group == "old-scan":
            query = f"({query}) AND {OLD_DATE_FILTER}"
        else:
            query = f"({query}) AND {DATE_FILTER}"
        downloads_at_start = self.downloads
        picked = 0
        start = 0
        scanned = 0
        while picked < quota and scanned < max_candidates and not self.full(group) \
                and self.downloads_left():
            if max_downloads is not None and self.downloads - downloads_at_start >= max_downloads:
                print(f"::warning::slot {group} stopped after {max_downloads} downloads",
                      flush=True)
                break
            try:
                batch = arxiv_query(query, start, 50)
            except Exception as exc:  # noqa: BLE001 - a rate limit ends this slot, not the run
                print(f"::warning::query failed for {group}: {str(exc)[:200]}", flush=True)
                break
            time.sleep(ARXIV_DELAY_S)
            if not batch:
                break
            start += len(batch)
            for candidate in batch:
                if picked >= quota or scanned >= max_candidates or self.full(group) \
                        or not self.downloads_left():
                    break
                if max_downloads is not None \
                        and self.downloads - downloads_at_start >= max_downloads:
                    break
                if candidate["id"] in self.seen:
                    continue
                scanned += 1
                if self.consider(candidate["id"], candidate["title"], candidate["category"],
                                 group, candidate["submitted"]):
                    picked += 1
        if picked < quota:
            print(f"::warning::slot {group} {query[:60]} found only {picked}/{quota}",
                  flush=True)


def select_mode(args) -> int:
    exclude: set[str] = set()
    for path in (args.exclude or []):
        for entry in load_list(path):
            exclude.add(entry["id"])
    selector = Selector(args.work_dir, args.out, exclude)
    print(f"excluding {len(exclude)} ids already measured by PDF-1a/PDF-1b", flush=True)

    for label, query, quota in ORDINARY_SLOTS:
        if selector.kept["ordinary"] >= GROUP_TARGET["ordinary"] or not selector.downloads_left():
            break
        selector.slot(query, "ordinary", quota, MAX_CANDIDATES_PER_SLOT)
    for query in LONG_QUERIES:
        if selector.kept["long"] >= GROUP_TARGET["long"] or not selector.downloads_left():
            break
        selector.slot(query, "long", GROUP_TARGET["long"] - selector.kept["long"],
                      LONG_MAX_CANDIDATES, LONG_MAX_DOWNLOADS)
    for group, queries in HARD_SLOTS:
        for query in queries:
            if selector.kept[group] >= GROUP_TARGET[group] or not selector.downloads_left():
                break
            selector.slot(query, group, GROUP_TARGET[group] - selector.kept[group],
                          HARD_MAX_CANDIDATES)
    for query in OLD_QUERIES:
        if selector.kept["old-scan"] >= GROUP_TARGET["old-scan"] or not selector.downloads_left():
            break
        selector.slot(query, "old-scan", GROUP_TARGET["old-scan"] - selector.kept["old-scan"],
                      OLD_MAX_CANDIDATES)

    selector.write()
    reasons = collections.Counter(item["reason"].split(":")[0].split("=")[0]
                                  for item in selector.skipped)
    print(f"wrote {len(selector.chosen)} entries to {args.out}")
    print(f"tried {selector.candidates} candidates, downloaded {selector.downloads} PDFs")
    for group in GROUP_ORDER:
        print(f"  {group}: kept {selector.kept[group]}/{GROUP_TARGET[group]}, "
              f"tried {selector.tried[group]}")
    print(f"rejected reasons: {reasons.most_common()}")
    report = os.path.join(args.work_dir, "select-report.json")
    write_json(report, {
        "chosen": selector.chosen,
        "skipped": selector.skipped,
        "candidates": selector.candidates,
        "downloads": selector.downloads,
        "target": GROUP_TARGET,
        "tried": dict(selector.tried),
        "kept": dict(selector.kept),
    })
    missing = {group: GROUP_TARGET[group] - selector.kept[group] for group in GROUP_ORDER
               if selector.kept[group] < GROUP_TARGET[group]}
    if missing:
        print(f"::warning::short of target: {missing}", flush=True)
    return 0


# --------------------------------------------------------------------------------------------
# Reference text (PyMuPDF, independent of the server)
# --------------------------------------------------------------------------------------------

def reference_facts(path: str) -> tuple[dict, str]:
    """Per-page reference text statistics, the font inventory and the first 20 pages' text."""
    import pymupdf

    page_chars: list[int] = []
    fonts: dict[tuple, dict] = {}
    first_texts: list[str] = []
    total_chars = 0
    total_cjk = 0
    image_pages = 0
    raster_pages = 0
    low_text_pages = 0
    low_text_image_pages = 0
    images_total = 0
    with pymupdf.open(path) as document:
        metadata = document.metadata or {}
        for page in document:
            text = page.get_text()
            chars = len(squash(text))
            page_chars.append(chars)
            total_chars += chars
            total_cjk += sum(1 for char in text if CORPUS.is_cjk(char))
            if chars < LOW_TEXT_CHARS:
                low_text_pages += 1
            infos = page.get_image_info()
            images_total += len(infos)
            page_area = page.rect.width * page.rect.height or 1.0
            largest = 0.0
            for image in infos:
                bbox = image.get("bbox") or [0, 0, 0, 0]
                area = max(0.0, (bbox[2] - bbox[0]) * (bbox[3] - bbox[1]))
                largest = max(largest, area / page_area)
            if infos:
                image_pages += 1
                if largest >= RASTER_SHARE:
                    raster_pages += 1
                if chars < LOW_TEXT_CHARS:
                    low_text_image_pages += 1
            for font in page.get_fonts(full=True):
                basefont = font[3] if len(font) > 3 else "?"
                subtype = font[2] if len(font) > 2 else "?"
                ext = font[1] if len(font) > 1 else "n/a"
                key = (basefont, subtype, ext)
                entry = fonts.setdefault(key, {"basefont": basefont, "subtype": subtype,
                                               "embedded": ext not in ("n/a", None),
                                               "pages": 0})
                entry["pages"] += 1
            if page.number < CONTAINMENT_PAGES:
                first_texts.append(text)
        pages = document.page_count
    inventory = sorted(fonts.values(), key=lambda item: item["pages"], reverse=True)[:8]
    facts = {
        "pages": pages,
        "chars": total_chars,
        "chars_per_page": round(total_chars / pages, 1) if pages else None,
        "cjk_chars": total_cjk,
        "pages_low_text": low_text_pages,
        "image_pages": image_pages,
        "raster_pages": raster_pages,
        "images_total": images_total,
        "low_text_image_pages": low_text_image_pages,
        "low_text_image_share": (round(low_text_image_pages / pages, 3) if pages else None),
        "fonts": inventory,
        "producer": (metadata.get("producer") or "")[:120],
        "creator": (metadata.get("creator") or "")[:120],
        "first20_chars": sum(len(squash(text)) for text in first_texts),
    }
    return facts, "\n".join(first_texts)


# --------------------------------------------------------------------------------------------
# The capped server
# --------------------------------------------------------------------------------------------

def wrapper_state(pid: int) -> str | None:
    """Process state letter from /proc, without reaping (`poll()` would lose the rusage)."""
    try:
        with open(f"/proc/{pid}/stat", encoding="utf-8") as handle:
            raw = handle.read()
        return raw.rsplit(") ", 1)[1].split()[0]
    except (OSError, IndexError):
        return None


def server_dead(server) -> bool:
    return wrapper_state(server.pid) in (None, "Z", "X")


def children_of(pid: int) -> list[int]:
    try:
        with open(f"/proc/{pid}/task/{pid}/children", encoding="utf-8") as handle:
            return [int(value) for value in handle.read().split()]
    except (OSError, ValueError):
        pass
    found = []
    try:
        for name in os.listdir("/proc"):
            if not name.isdigit():
                continue
            try:
                with open(f"/proc/{name}/stat", encoding="utf-8") as handle:
                    fields = handle.read().rsplit(") ", 1)[1].split()
                if int(fields[1]) == pid:
                    found.append(int(name))
            except (OSError, IndexError, ValueError):
                continue
    except OSError:
        pass
    return found


def comm_of(pid: int) -> str:
    try:
        with open(f"/proc/{pid}/comm", encoding="utf-8") as handle:
            return handle.read().strip()
    except OSError:
        return ""


def server_pid_of(wrapper_pid: int) -> int:
    """Descend the wrapper chain (sudo -> systemd-run -> server) to the server process."""
    current = wrapper_pid
    for _ in range(5):
        children = children_of(current)
        if not children:
            return current
        named = [child for child in children if comm_of(child).startswith("reading-mcp")]
        current = named[0] if named else children[0]
    return current


def server_peak_kib(server) -> int | None:
    """VmHWM of the server process itself (the wrapper's VmHWM is not the server's)."""
    return PROBE.peak_rss_kib(server_pid_of(server.pid))


# --------------------------------------------------------------------------------------------
# Quality proxies
# --------------------------------------------------------------------------------------------

def is_control(char: str) -> bool:
    code = ord(char)
    if code < 0x20 and char not in "\n\r\t":
        return True
    return 0x7F <= code <= 0x9F


def is_private_use(char: str) -> bool:
    code = ord(char)
    return 0xE000 <= code <= 0xF8FF or 0xF0000 <= code <= 0x10FFFD


def duplicate_lines(text: str, pages: int | None) -> dict:
    """A line repeated on more than 30% of the pages is header/footer noise."""
    lines = [squash(line) for line in text.splitlines()]
    lines = [line for line in lines if len(line) >= 4]
    counts = collections.Counter(lines)
    total = len(lines)
    repeated = [(line, count) for line, count in counts.items()
                if count >= 3 and pages and count / pages > HEADER_FOOTER_SHARE]
    occurrences = sum(count for _, count in repeated)
    worst = max(repeated, key=lambda item: item[1]) if repeated else None
    return {
        "lines": total,
        "repeated_lines": len(repeated),
        "repeated_occurrences": occurrences,
        "rate": round(occurrences / total, 4) if total else None,
        "worst_line": worst[0][:80] if worst else None,
        "worst_count": worst[1] if worst else None,
    }


def quality_proxies(reference: dict, first20_text: str, text: str, units: list[str],
                    truncated: bool) -> dict:
    default_chars = len(squash(text))
    pages = reference.get("pages") or 0
    ref_chars = reference.get("chars") or 0
    ratio = round(default_chars / ref_chars, 3) if ref_chars and not truncated else None
    containment = gram_containment(first20_text, text) if text.strip() else None
    length = len(text) or 1
    controls = sum(1 for char in text if is_control(char))
    private = sum(1 for char in text if is_private_use(char))
    replacement = text.count("\ufffd")
    short = sum(1 for unit in units if len(unit.strip()) < 5)
    long_units = sum(1 for unit in units if len(unit.strip()) > 800)
    return {
        "default_chars": default_chars,
        "default_chars_per_page": (round(default_chars / pages, 1) if pages else None),
        "char_ratio": ratio,
        "containment_first20": containment,
        "text_truncated": truncated,
        "nonprintable_share": round(controls / length, 5),
        "private_use_share": round(private / length, 5),
        "replacement_share": round(replacement / length, 5),
        "units": len(units),
        "short_lt5_share": round(short / len(units), 4) if units else None,
        "long_gt800_share": round(long_units / len(units), 4) if units else None,
        "duplicate_lines": duplicate_lines(text, pages),
    }


def suspect_reasons(reference: dict, quality: dict) -> list[str]:
    reasons: list[str] = []
    ref_per_page = reference.get("chars_per_page") or 0
    default_per_page = quality.get("default_chars_per_page")
    if ref_per_page > RICH_REF_CHARS_PER_PAGE and default_per_page is not None \
            and default_per_page < POOR_DEFAULT_CHARS_PER_PAGE \
            and not quality.get("text_truncated"):
        reasons.append("ref_rich_default_poor")
    share = reference.get("low_text_image_share")
    if share is not None and share > 0.5:
        reasons.append("image_pages_without_text")
    containment = quality.get("containment_first20")
    if containment is not None and containment < CONTAINMENT_SUSPECT:
        reasons.append("containment_below_0.6")
    return reasons


def middle_token(text: str) -> str | None:
    middle = len(text) // 2
    best: tuple[int, str] | None = None
    for match in TOKEN_RE.finditer(text, max(0, middle - 4000)):
        if match.start() > middle + 4000:
            break
        distance = abs(match.start() - middle)
        if best is None or distance < best[0]:
            best = (distance, match.group(0))
    if best is not None:
        return best[1]
    # CJK documents have no long Latin token: search for four characters from the middle.
    for offset in range(0, min(4000, len(text) // 2)):
        candidate = text[middle + offset:middle + offset + 4]
        if len(candidate) == 4 and all(CORPUS.is_cjk(char) for char in candidate):
            return candidate
    return None


# --------------------------------------------------------------------------------------------
# Run mode
# --------------------------------------------------------------------------------------------

def classify(record: dict, timed_out: bool, message: str) -> str:
    code = record.get("error_code")
    if timed_out:
        return "timeout"
    if code == "OCR_REQUIRED":
        return "ocr_required"
    if code == "RESOURCE_LIMIT_EXCEEDED":
        return "too_large"
    if code == "PARSE_FAILED" and "no extractable text" in message:
        return "unreadable"
    return "parse_failed"


def collect_text(server, document_id: str, sections: list[dict], budget: int,
                 deadline: float) -> dict:
    """Read the whole document text in 256-item batches, bounded by a character budget."""
    units: list[str] = []
    chars = 0
    calls = 0
    truncated = False
    error = None
    started = time.monotonic()
    for section in sections:
        if chars >= budget or time.monotonic() > deadline:
            # Either the budget or the job deadline cut the text: both make the character
            # counts a lower bound, so the ratio must not be reported as a whole-document one.
            truncated = True
            break
        cursor = None
        while True:
            if chars >= budget or time.monotonic() > deadline:
                truncated = True
                break
            arguments = PROBE.text_units(document_id, section["section_id"], BATCH_ITEMS, cursor)
            try:
                data = PROBE.payload(server.tool("get_text_units", arguments)) or {}
            except PROBE.ProbeError as exc:
                error = str(exc)[:200]
                break
            calls += 1
            for item in data.get("items") or []:
                text = item.get("text") or ""
                units.append(text)
                chars += len(text)
            cursor = data.get("next_cursor")
            if not cursor:
                break
        if error:
            break
    return {"text": "\n".join(units), "units": units, "chars": chars, "calls": calls,
            "truncated": truncated, "error": error,
            "seconds": round(time.monotonic() - started, 2)}


def download_pdf(entry: dict, work_dir: str) -> tuple[str | None, dict]:
    """Download at CI time, re-checking the licence line on the abs page.

    `pdf-corpus.download_paper` is not reused: it names the file after the raw id, and an
    old-style id (`hep-th/9501001`) would need a nested directory.
    """
    pdf_dir = os.path.join(work_dir, "pdfs")
    os.makedirs(pdf_dir, exist_ok=True)
    path = os.path.join(pdf_dir, f"{entry['id'].replace('/', '_')}.pdf")
    status: dict = {"download": "ok", "error": None}
    marker = entry.get("license_marker") or cc_marker(entry.get("license") or "")
    try:
        _licence_url, found = abs_license(entry["id"])
        time.sleep(FETCH_DELAY_S)
    except RuntimeError as exc:
        status.update({"download": "download_failed", "error": f"abs: {str(exc)[:200]}"})
        return None, status
    if marker is None or found != marker:
        status.update({"download": "licence_mismatch",
                       "error": f"marker {marker!r} not on the abs page (found {found!r})"})
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
            time.sleep(FETCH_DELAY_S)
            return path, status
        except Exception as exc:  # noqa: BLE001 - retried below
            last = f"{type(exc).__name__}: {str(exc)[:160]}"
            time.sleep(5 * (attempt + 1))
    status.update({"download": "download_failed", "error": last})
    return None, status


def process_document(entry: dict, args, deadline: float) -> dict:
    record: dict = {
        "kind_record": "document", "id": entry["id"], "group": entry["group"],
        "title": entry.get("title"), "category": entry.get("category"),
        "license": entry.get("license"), "pages": entry.get("pages"),
        "producer": entry.get("producer"), "creator": entry.get("creator"),
        "metric": entry.get("metric"), "bytes": entry.get("bytes"),
        "outcome": None, "error_code": None, "error": None, "phase": "download",
        "download": "ok", "open_s": None, "peak_rss_vmhwm_kib": None,
        "peak_rss_total_kib": 0, "state_bytes_after_open": None, "state_sqlite_bytes": None,
        "sections": None, "sentences": None, "sentences_per_page": None,
        "structure_mode": None, "server_sections": None, "cap": CAP,
        "cap_available": args.cap_available, "server_rc": None, "died": False,
        "read": {}, "search": {}, "quality": {}, "text": {}, "reference": None,
        "suspect": False, "suspect_reasons": [],
    }
    started = time.perf_counter()
    path, status = download_pdf(entry, args.work_dir)
    record["download"] = status["download"]
    if path is None:
        record.update({"outcome": "download_failed", "error": status["error"]})
        record["seconds"] = round(time.perf_counter() - started, 1)
        return record

    reference: dict = {"pages": None, "chars": 0, "chars_per_page": None, "fonts": []}
    first20_text = ""
    try:
        reference, first20_text = reference_facts(path)
    except Exception as exc:  # noqa: BLE001 - an unreadable reference is data, not a crash
        record["reference_error"] = f"{type(exc).__name__}: {str(exc)[:200]}"
    record["reference"] = reference
    record["pages"] = reference.get("pages") or record.get("pages")

    state_dir = os.path.join(args.state_root, f"open-{entry['id'].replace('/', '_')}")
    shutil.rmtree(state_dir, ignore_errors=True)
    os.makedirs(state_dir, exist_ok=True)
    env = PROBE.server_env(state_dir, os.path.dirname(os.path.abspath(path)), "default", "")
    cap = CAP if args.cap_available else None
    command = PROBE.cap_command(args.binary, cap, env)
    server = PROBE.Server(command, env, os.path.join(state_dir, "server.log"),
                          PROBE.TOOL_TIMEOUT_S)
    died = False
    timed_out = False
    open_error: PROBE.ProbeError | None = None
    document_id = None
    sections: list[dict] = []
    full: dict = {"text": "", "units": [], "chars": 0, "truncated": False}
    try:
        server.initialize()
        record["phase"] = "open"
        began = time.perf_counter()
        try:
            result = server.tool("open_document", {"source": path}, timeout=OPEN_TIMEOUT_S)
        except PROBE.ProbeError as exc:
            result = None
            open_error = exc
            timed_out = "timed out" in str(exc)
            died = server_dead(server)
        record["open_s"] = round(time.perf_counter() - began, 3)
        record["peak_rss_vmhwm_kib"] = server_peak_kib(server)
        usage = PROBE.disk_usage(state_dir)
        record["state_bytes_after_open"] = usage["total_bytes"]
        record["state_sqlite_bytes"] = usage["sqlite_bytes"]
        if open_error is not None:
            record["error"] = str(open_error)[:500]
            record["error_code"] = open_error.code
            record["died"] = died
        else:
            data = PROBE.payload(result) or {}
            document_id = data.get("document_id")
            record["server_sections"] = data.get("section_count")
            record["server_title"] = (data.get("title") or "")[:120]

        if document_id:
            record["phase"] = "structure"
            sections = PROBE.read_sections(server, document_id)
            record["sections"] = len(sections)
            record["structure_mode"] = (
                "page_fallback"
                if sections and all(re.fullmatch(r"section://page-\d+", str(section.get(
                    "section_id", ""))) for section in sections)
                else "headings")
            totals, totals_error = PROBE.sentence_totals(server, document_id, sections, deadline)
            sentences = sum(totals) if totals and not totals_error else None
            record["sentences"] = sentences
            record["sentences_error"] = totals_error
            pages = reference["pages"]
            if sentences is not None and pages:
                record["sentences_per_page"] = round(sentences / pages, 2)

            record["phase"] = "read"
            if time.monotonic() < deadline:
                read = PROBE.read_sentences(server, document_id, sections, READ_CALLS, deadline)
                latencies = read["latencies_ms"]
                record["read"] = {
                    "calls": read["calls"], "sentences": len(read["texts"]),
                    "mean_ms": (round(sum(latencies) / len(latencies), 2) if latencies else None),
                    "p95_ms": percentile(latencies, 0.95),
                    "max_ms": max(latencies) if latencies else None,
                    "total_s": round(read["read_s"], 3),
                    "partial": bool(read["partial"]),
                    "errors": len(read["errors"]),
                    "first_error": read["errors"][0][:200] if read["errors"] else None,
                }
            else:
                record["read"] = {"error": "deadline before the sequential read"}

            record["phase"] = "full_text"
            if time.monotonic() < deadline:
                full = collect_text(server, document_id, sections, TEXT_BUDGET_CHARS, deadline)
                record["text"] = {"chars": full["chars"], "units": len(full["units"]),
                                  "calls": full["calls"], "truncated": full["truncated"],
                                  "seconds": full["seconds"], "error": full["error"]}

            record["phase"] = "search"
            token = middle_token(full["text"])
            if token and time.monotonic() < deadline:
                began = time.perf_counter()
                try:
                    hits = PROBE.payload(server.tool(
                        "search_document",
                        {"document_id": document_id, "query": token,
                         "limit": SEARCH_LIMIT})) or {}
                    found = hits.get("hits") or []
                    record["search"] = {
                        "query": token,
                        "ms": round((time.perf_counter() - began) * 1000.0, 2),
                        "hits": len(found),
                        "limit": SEARCH_LIMIT,
                        "limit_reached": len(found) >= SEARCH_LIMIT,
                    }
                except PROBE.ProbeError as exc:
                    record["search"] = {"query": token, "error": str(exc)[:200],
                                        "ms": round((time.perf_counter() - began) * 1000.0, 2)}
            else:
                record["search"] = {"error": "no token or deadline"}
            died = died or server_dead(server)
    except PROBE.ProbeError as exc:
        if record.get("open_s") is None:
            record["error"] = str(exc)[:500]
            record["error_code"] = exc.code
        else:
            record.setdefault("read", {})["error"] = str(exc)[:200]
        died = died or server_dead(server)
    finally:
        server.close(grace=30.0)
    record["server_rc"] = server.rc
    record["peak_rss_total_kib"] = server.peak_kib()
    if record.get("peak_rss_vmhwm_kib"):
        record["peak_rss_source"] = "vmhwm"
    elif record["peak_rss_total_kib"]:
        # The server is gone (killed or already exited): the wait4 rusage is all that is left.
        record["peak_rss_vmhwm_kib"] = record["peak_rss_total_kib"]
        record["peak_rss_source"] = "wait4"
    else:
        record["peak_rss_source"] = None
    if server.rc in PROBE.KILLED_CODES:
        died = True
    record["died"] = died
    record["killed"] = server.rc in PROBE.KILLED_CODES
    record["died_at"] = record["phase"] if died else None

    if document_id:
        quality = quality_proxies(reference, first20_text, full["text"], full["units"],
                                 bool(full.get("truncated")))
        record["quality"] = quality
        reasons = suspect_reasons(reference, quality)
        record["suspect"] = bool(reasons)
        record["suspect_reasons"] = reasons

    message = str(record.get("error") or "")
    if record.get("outcome") is None:
        if record["killed"]:
            record["outcome"] = "oom_killed"
        elif document_id is not None and open_error is None:
            record["outcome"] = "ok"
        elif document_id is None and open_error is None:
            record["outcome"] = "parse_failed"
            record["error"] = "open_document returned no document_id"
        elif died:
            record["outcome"] = "parse_failed"
            record["error"] = f"server exited (rc={server.rc}) before answering"
        else:
            record["outcome"] = classify(record, timed_out, message)
    record["seconds"] = round(time.perf_counter() - started, 1)
    return record


def document_line(record: dict) -> str:
    quality = record.get("quality") or {}
    read = record.get("read") or {}
    return (f"| {record['id']} | {record.get('group')} | {record.get('pages')} | "
            f"{record.get('outcome')} | {fmt(record.get('open_s'))} | "
            f"{fmt((record.get('peak_rss_vmhwm_kib') or 0) / 1024, 1)} | "
            f"{fmt(record.get('sections'))} | {fmt(quality.get('containment_first20'), 3)} | "
            f"{fmt(read.get('p95_ms'), 1)} | {fmt(quality.get('duplicate_lines', {}).get('rate'),
                                                  3)} | "
            f"{'suspect' if record.get('suspect') else ''} |")


def run_shard(args) -> int:
    entries = load_list(args.ids)
    if not entries:
        print("::error::the corpus list is empty")
        return 1
    shard = [entry for index, entry in enumerate(entries) if index % args.shards == args.shard]
    print(f"shard {args.shard}/{args.shards}: {len(shard)} documents", flush=True)
    print("| id | group | pages | outcome | open s | peak RSS MiB | sections | containment | "
          "p95 ms | dup lines | flag |", flush=True)
    os.makedirs(args.state_root, exist_ok=True)
    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)
    shard_deadline = time.monotonic() + args.deadline_minutes * 60
    for entry in shard:
        if time.monotonic() > shard_deadline:
            append_jsonl(args.out, {
                "kind_record": "document", "id": entry["id"], "group": entry["group"],
                "pages": entry.get("pages"), "producer": entry.get("producer"),
                "outcome": "not_run", "error": "shard deadline", "suspect": False,
                "suspect_reasons": [],
            })
            print(f"| {entry['id']} | {entry['group']} | {entry.get('pages')} | not_run | - | "
                  f"- | - | - | - | - | shard deadline |", flush=True)
            continue
        deadline = min(shard_deadline, time.monotonic() + args.per_document_minutes * 60)
        record = process_document(entry, args, deadline)
        append_jsonl(args.out, record)
        print(document_line(record), flush=True)
    return 0


# --------------------------------------------------------------------------------------------
# Aggregate
# --------------------------------------------------------------------------------------------

def load_records_recursive(root: str) -> list[dict]:
    records: list[dict] = []
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
                        records.append(json.loads(line))
                    except json.JSONDecodeError:
                        continue
    return records


def bucket_of(pages: int | None) -> str:
    if pages is None:
        return "unknown"
    if pages < 5:
        return "<5"
    if pages < 20:
        return "5-20"
    if pages < 100:
        return "20-100"
    if pages < 400:
        return "100-400"
    return "400+"


BUCKETS = ("<5", "5-20", "20-100", "100-400", "400+")


def font_summary(reference: dict) -> str:
    fonts = (reference or {}).get("fonts") or []
    if not fonts:
        return "-"
    parts = [f"{font['basefont']} ({font['subtype']}, "
             f"{'embedded' if font['embedded'] else 'not embedded'}, {font['pages']}p)"
             for font in fonts[:3]]
    return "; ".join(parts)


def image_summary(reference: dict) -> str:
    reference = reference or {}
    if not reference.get("images_total"):
        return "no raster images"
    return (f"{reference.get('images_total')} images on {reference.get('image_pages')} pages, "
            f"{reference.get('raster_pages')} pages with a raster over 20% of the page")


def cause_of(record: dict) -> str:
    outcome = record.get("outcome")
    if outcome == "ocr_required":
        return "refused as an image-only scan (OCR_REQUIRED)"
    if outcome == "unreadable":
        return "no text layer at all (PARSE_FAILED: no extractable text)"
    if outcome == "too_large":
        return "over the 128 MiB document limit"
    if outcome == "timeout":
        return "open over 120 s"
    if outcome == "oom_killed":
        return f"server killed under the 600 MB cap (during {record.get('died_at')})"
    if outcome == "download_failed":
        return "download failed"
    if outcome == "parse_failed":
        return f"parse failed ({record.get('error_code')})"
    reasons = record.get("suspect_reasons") or []
    if "ref_rich_default_poor" in reasons:
        return "reference text present but the default backend returns almost nothing"
    if "image_pages_without_text" in reasons:
        return "mostly image-only pages, yet the open reports ok"
    if "containment_below_0.6" in reasons:
        return "text extracted but does not match the reference"
    return "other"


def aggregate_mode(args) -> int:
    records = [record for record in load_records_recursive(args.jsonl_dir)
               if record.get("kind_record") == "document"]
    if not records:
        print(f"::error::no document records under {args.jsonl_dir}")
        return 1
    records.sort(key=lambda item: (GROUP_ORDER.index(group_label(item.get("group")))
                                   if group_label(item.get("group")) in GROUP_ORDER else 99,
                                   item.get("group") or "", item.get("id") or ""))
    docs = [record for record in records if record.get("outcome") != "not_run"]
    not_run = [record for record in records if record.get("outcome") == "not_run"]

    # 1. Outcome classes
    groups = ["ordinary", "old-scan", "long", "hard"]
    header = "| Group | Documents | " + " | ".join(OUTCOME_CLASSES) + " |"
    rows = [header, "| --- | --- | " + " | ".join("---" for _ in OUTCOME_CLASSES) + " |"]
    totals: collections.Counter = collections.Counter()
    for group in groups + ["all"]:
        subset = docs if group == "all" else [doc for doc in docs
                                              if group_label(doc.get("group")) == group]
        counts = collections.Counter(doc.get("outcome") for doc in subset)
        if group == "all":
            totals = counts
        rows.append(f"| {group} | {len(subset)} | "
                    + " | ".join(str(counts.get(name, 0)) for name in OUTCOME_CLASSES) + " |")

    # 2. Non-ok and suspect documents, grouped by cause
    flagged = [doc for doc in docs
               if doc.get("outcome") != "ok" or doc.get("suspect")]
    by_cause: dict[str, list[dict]] = collections.OrderedDict()
    for doc in flagged:
        by_cause.setdefault(cause_of(doc), []).append(doc)
    cause_rows = ["| Cause | Documents |", "| --- | --- |"]
    for cause, items in sorted(by_cause.items(), key=lambda pair: -len(pair[1])):
        cause_rows.append(f"| {cause} | {len(items)} |")
    detail_rows = ["| Id | Group | Pages | Producer | Outcome | Reason | Fonts (top 3) | "
                   "Images | Containment | Char ratio |",
                   "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for cause, items in sorted(by_cause.items(), key=lambda pair: -len(pair[1])):
        for doc in items:
            quality = doc.get("quality") or {}
            reason = ", ".join(doc.get("suspect_reasons") or []) or \
                (doc.get("error_code") or doc.get("outcome"))
            detail_rows.append(
                f"| {doc['id']} | {doc.get('group')} | {doc.get('pages')} | "
                f"{(doc.get('producer') or '-')[:40]} | {doc.get('outcome')} | "
                f"{str(reason)[:60]} | {font_summary(doc.get('reference') or {})[:70]} | "
                f"{image_summary(doc.get('reference') or {})[:60]} | "
                f"{fmt(quality.get('containment_first20'), 3)} | "
                f"{fmt(quality.get('char_ratio'), 3)} |")

    # 3. Open time and peak RSS by page bucket
    bucket_rows = ["| Pages | Documents | Median open s | p95 open s | Worst open s | "
                   "Median peak RSS MiB | p95 peak RSS MiB | Worst peak RSS MiB |",
                   "| --- | --- | --- | --- | --- | --- | --- | --- |"]
    for bucket in BUCKETS:
        subset = [doc for doc in docs if bucket_of(doc.get("pages")) == bucket]
        opens = [doc["open_s"] for doc in subset if doc.get("open_s") is not None]
        rss = [(doc.get("peak_rss_vmhwm_kib") or 0) / 1024 for doc in subset
               if doc.get("peak_rss_vmhwm_kib")]
        bucket_rows.append(
            f"| {bucket} | {len(subset)} | "
            f"{fmt(round(statistics.median(opens), 2) if opens else None)} | "
            f"{fmt(percentile(opens, 0.95))} | {fmt(max(opens) if opens else None)} | "
            f"{fmt(round(statistics.median(rss), 1) if rss else None, 1)} | "
            f"{fmt(percentile(rss, 0.95), 1)} | {fmt(max(rss) if rss else None, 1)} |")
    opens_all = [doc["open_s"] for doc in docs if doc.get("open_s") is not None]
    rss_all = [(doc.get("peak_rss_vmhwm_kib") or 0) / 1024 for doc in docs
               if doc.get("peak_rss_vmhwm_kib")]
    bucket_rows.append(
        f"| all | {len(docs)} | "
        f"{fmt(round(statistics.median(opens_all), 2) if opens_all else None)} | "
        f"{fmt(percentile(opens_all, 0.95))} | {fmt(max(opens_all) if opens_all else None)} | "
        f"{fmt(round(statistics.median(rss_all), 1) if rss_all else None, 1)} | "
        f"{fmt(percentile(rss_all, 0.95), 1)} | {fmt(max(rss_all) if rss_all else None, 1)} |")

    slowest = sorted([doc for doc in docs if doc.get("open_s") is not None],
                     key=lambda doc: -doc["open_s"])[:5]
    heaviest = sorted([doc for doc in docs if doc.get("peak_rss_vmhwm_kib")],
                      key=lambda doc: -doc["peak_rss_vmhwm_kib"])[:5]
    worst_rows = ["| Rank | Slowest open: id | Pages | Open s | Heaviest: id | Pages | "
                  "Peak RSS MiB |", "| --- | --- | --- | --- | --- | --- | --- |"]
    for index in range(5):
        slow = slowest[index] if index < len(slowest) else {}
        heavy = heaviest[index] if index < len(heaviest) else {}
        worst_rows.append(
            f"| {index + 1} | {slow.get('id', '-')} | {slow.get('pages', '-')} | "
            f"{fmt(slow.get('open_s'))} | {heavy.get('id', '-')} | {heavy.get('pages', '-')} | "
            f"{fmt((heavy.get('peak_rss_vmhwm_kib') or 0) / 1024, 1)} |")

    # 4. Long documents
    long_docs = [doc for doc in docs if doc.get("group") == "long"]
    long_rows = ["| Id | Pages | Open s | s/page | Peak RSS MiB | RSS MiB/page | Sections | "
                 "Structure | Sentences | Sentences/page | Chars | Containment | Search ms | "
                 "Cap hit |",
                 "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | "
                 "--- | --- |"]
    for doc in sorted(long_docs, key=lambda item: -(item.get("pages") or 0)):
        pages = doc.get("pages") or 0
        open_s = doc.get("open_s")
        rss = (doc.get("peak_rss_vmhwm_kib") or 0) / 1024
        quality = doc.get("quality") or {}
        search = doc.get("search") or {}
        long_rows.append(
            f"| {doc['id']} | {pages} | {fmt(open_s)} | "
            f"{fmt(round(open_s / pages, 4) if open_s and pages else None, 4)} | "
            f"{fmt(rss, 1)} | {fmt(round(rss / pages, 3) if pages else None, 3)} | "
            f"{fmt(doc.get('sections'))} | {fmt(doc.get('structure_mode'))} | "
            f"{fmt(doc.get('sentences'))} | {fmt(doc.get('sentences_per_page'))} | "
            f"{fmt(quality.get('default_chars'))} | "
            f"{fmt(quality.get('containment_first20'), 3)} | {fmt(search.get('ms'))} | "
            f"{'yes' if doc.get('outcome') == 'oom_killed' else 'no'} |")

    # 5. What would matter to a reader, ranked by how many documents it hits
    def counted(predicate) -> int:
        return len([doc for doc in docs if predicate(doc)])

    def value(doc: dict, *path):
        current = doc
        for key in path:
            if not isinstance(current, dict):
                return None
            current = current.get(key)
        return current

    def rss_mib(doc: dict) -> float:
        return (doc.get("peak_rss_vmhwm_kib") or 0) / 1024

    impacts = [
        ("open over 10 s", counted(lambda doc: (value(doc, "open_s") or 0) > 10.0)),
        ("open over 60 s", counted(lambda doc: (value(doc, "open_s") or 0) > 60.0)),
        ("peak RSS over 400 MiB", counted(lambda doc: rss_mib(doc) > 400.0)),
        ("refused, unreadable or killed (non-ok)",
         counted(lambda doc: doc.get("outcome") != "ok")),
        ("silent failure (open reports ok but the text is suspect)",
         counted(lambda doc: doc.get("suspect"))),
        ("5-gram containment below 0.6",
         counted(lambda doc: value(doc, "quality", "containment_first20") is not None
                 and value(doc, "quality", "containment_first20") < 0.6)),
        ("more than 10% of sentences shorter than 5 chars",
         counted(lambda doc: (value(doc, "quality", "short_lt5_share") or 0) > 0.10)),
        ("more than 5% of sentences longer than 800 chars",
         counted(lambda doc: (value(doc, "quality", "long_gt800_share") or 0) > 0.05)),
        ("a line repeated on over 30% of pages (header/footer noise)",
         counted(lambda doc: bool(value(doc, "quality", "duplicate_lines", "repeated_lines")))),
        ("over 1% private-use or replacement characters",
         counted(lambda doc: (value(doc, "quality", "private_use_share") or 0)
                 + (value(doc, "quality", "replacement_share") or 0) > 0.01)),
        ("search slower than 1000 ms",
         counted(lambda doc: (value(doc, "search", "ms") or 0) > 1000.0)),
        ("read p95 slower than 50 ms",
         counted(lambda doc: (value(doc, "read", "p95_ms") or 0) > 50.0)),
        ("state directory over 100 MiB after the open",
         counted(lambda doc: (value(doc, "state_bytes_after_open") or 0) > 100 * MIB)),
    ]
    impact_rows = ["| What a reader would notice | Documents |", "| --- | --- |"]
    for label, count in sorted(impacts, key=lambda item: -item[1]):
        impact_rows.append(f"| {label} | {count} |")

    # 6. Selection: tried versus kept
    selection_rows: list[str] = []
    if args.select_report and os.path.isfile(args.select_report):
        with open(args.select_report, encoding="utf-8") as handle:
            report = json.load(handle)
        skipped = report.get("skipped") or []
        reasons = collections.Counter(str(item.get("reason", "?")).split(":")[0].split("=")[0]
                                      for item in skipped)
        selection_rows = ["| Group | Kept | Target | Candidates tried | Rejections |",
                          "| --- | --- | --- | --- | --- |"]
        for group in GROUP_ORDER:
            kept = (report.get("kept") or {}).get(group, 0)
            tried = (report.get("tried") or {}).get(group, 0)
            target = (report.get("target") or {}).get(group, 0)
            selection_rows.append(f"| {group} | {kept} | {target} | {tried} | {tried - kept} |")
        selection_rows += ["", f"Total candidates tried: {report.get('candidates')}, PDFs "
                               f"downloaded: {report.get('downloads')}.", "",
                           "| Rejection reason | Candidates |", "| --- | --- |"]
        for reason, count in reasons.most_common():
            selection_rows.append(f"| {reason} | {count} |")
        by_group_reason = collections.Counter(
            (str(item.get("group")), str(item.get("reason", "?")).split(":")[0].split("=")[0])
            for item in skipped)
        selection_rows += ["", "| Group | Reason | Candidates |", "| --- | --- | --- |"]
        for (group, reason), count in sorted(by_group_reason.items()):
            selection_rows.append(f"| {group} | {reason} | {count} |")

    summary = {
        "documents": len(docs),
        "not_run": [doc["id"] for doc in not_run],
        "outcome_counts": dict(totals),
        "suspects": [{"id": doc["id"], "reasons": doc.get("suspect_reasons")}
                     for doc in docs if doc.get("suspect")],
        "non_ok": [{"id": doc["id"], "outcome": doc.get("outcome"),
                    "error_code": doc.get("error_code"), "error": doc.get("error")}
                   for doc in docs if doc.get("outcome") != "ok"],
        "slowest": [{"id": doc["id"], "open_s": doc.get("open_s")} for doc in slowest],
        "heaviest": [{"id": doc["id"],
                      "peak_rss_mib": round((doc.get("peak_rss_vmhwm_kib") or 0) / 1024, 1)}
                     for doc in heaviest],
        "cap_hits": [doc["id"] for doc in docs if doc.get("outcome") == "oom_killed"],
        "cap_enforced": all(doc.get("cap_available") for doc in docs) if docs else None,
        "impacts": dict(impacts),
    }

    text = "\n".join([
        "# PDF-5a wide real-world sweep", "",
        f"Documents measured: {len(docs)} (not run: {len(not_run)}). "
        f"Memory cap 600 MB: {'enforced' if summary['cap_enforced'] else 'NOT enforced'}.", "",
        "## Outcome classes", "", *rows, "",
        "## Non-ok and suspect documents by cause", "", *cause_rows, "", *detail_rows, "",
        "## Open time and peak RSS by page bucket", "", *bucket_rows, "",
        *worst_rows, "",
        "## Long documents", "", *long_rows, "",
        "## What would matter to a reader (ranked by documents hit)", "", *impact_rows, "",
        *(["## Selection: tried versus kept", ""] + selection_rows + [""]
          if selection_rows else []),
    ])
    print(text)
    if args.markdown:
        os.makedirs(os.path.dirname(os.path.abspath(args.markdown)) or ".", exist_ok=True)
        with open(args.markdown, "w", encoding="utf-8") as handle:
            handle.write(text + "\n")
    if args.out:
        write_json(args.out, summary)
    if args.suspects:
        with open(args.suspects, "w", encoding="utf-8") as handle:
            for doc in flagged:
                handle.write(json.dumps({
                    "id": doc.get("id"), "group": doc.get("group"),
                    "pages": doc.get("pages"), "producer": doc.get("producer"),
                    "outcome": doc.get("outcome"), "error_code": doc.get("error_code"),
                    "suspect_reasons": doc.get("suspect_reasons"),
                    "cause": cause_of(doc),
                    "containment_first20": (doc.get("quality") or {}).get("containment_first20"),
                    "char_ratio": (doc.get("quality") or {}).get("char_ratio"),
                    "fonts": (doc.get("reference") or {}).get("fonts"),
                    "images": {key: (doc.get("reference") or {}).get(key) for key in
                               ("images_total", "image_pages", "raster_pages",
                                "low_text_image_pages")},
                }, ensure_ascii=False) + "\n")
    return 0


# --------------------------------------------------------------------------------------------
# Entry point
# --------------------------------------------------------------------------------------------

def main() -> int:
    parser = argparse.ArgumentParser(description="PDF-5a wide real-world PDF sweep.")
    parser.add_argument("--select", action="store_true", help="build the corpus list")
    parser.add_argument("--aggregate", action="store_true", help="merge shards into tables")
    parser.add_argument("--ids", default="scripts/pdf-sweep-ids.txt")
    parser.add_argument("--exclude", action="append", default=[],
                        help="id list to exclude from selection (repeatable)")
    parser.add_argument("--out", default="")
    parser.add_argument("--markdown", default="")
    parser.add_argument("--suspects", default="")
    parser.add_argument("--jsonl-dir", default="")
    parser.add_argument("--select-report", default="")
    parser.add_argument("--work-dir", default=".tmp/pdf-sweep")
    parser.add_argument("--binary", default="")
    parser.add_argument("--state-root", default=".tmp/pdf-sweep/state")
    parser.add_argument("--shard", type=int, default=0)
    parser.add_argument("--shards", type=int, default=8)
    parser.add_argument("--deadline-minutes", type=float, default=42.0)
    parser.add_argument("--per-document-minutes", type=float, default=4.5)
    args = parser.parse_args()
    if args.select:
        if not args.out:
            parser.error("--select needs --out")
        return select_mode(args)
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
