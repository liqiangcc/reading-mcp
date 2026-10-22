#!/usr/bin/env python3
"""PDF-1b: which fonts make the default backend lose CJK characters.

Three modes, standard library plus PyMuPDF (and the pinned layout environment for the layout
backend):

- `--select`: query the arXiv API for CC-BY(-SA)/CC0 papers whose abstract or title mentions a
  CJK language or a CJK benchmark, download candidates to check the licence line, the page
  count and the CJK character count of the PDF text layer, and write the committed list
  (`scripts/pdf-cjk-ids.txt`, JSON lines with a comment header). Downloads happen at CI time;
  only the list is committed.
- `--run`: for one shard of the list, download each PDF, build a PyMuPDF font inventory
  (subtype, encoding/CMap, ToUnicode, embedding, descendant CIDSystemInfo/CIDToGIDMap, subset
  prefix, whether the font is used in the page content stream or only inside a Form XObject),
  count the CJK characters each font owns in the reference text, read the whole document with
  the default and the layout backend over stdio MCP exactly as `scripts/pdf-probe.py` does,
  and estimate per-font CJK survival by run containment. Writes JSONL plus one font-inventory
  JSON per paper.
- `--aggregate`: merge the shards, group the fonts by (Subtype, encoding/CMap, ToUnicode,
  embedded, CIDToGIDMap, producer family, content-stream location), and print the tables used
  in `docs/design/pdf-cjk-loss-data.md`.
- `--repro`: the minimal one-page repro for the worst font group (aggregate job).

It changes no product code and never builds anything.
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import importlib.util
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

ARXIV_API = "https://export.arxiv.org/api/query"
ARXIV_ABS = "https://arxiv.org/abs/{id}"
ARXIV_PDF = "https://arxiv.org/pdf/{id}"
ARXIV_DELAY_S = 4.0
USER_AGENT = "reading-mcp-bench-pdf1b"
CC_LICENSE_MARKERS = (
    "licenses/by/4.0",
    "licenses/by-sa/4.0",
    "publicdomain/zero/1.0",
)
CJK_MIN_CHARS = 200
MIN_PAGES, MAX_PAGES = 4, 40
MAX_PDF_BYTES = 60 * 1024 * 1024
MAX_DOWNLOADS = 150
TARGET_PAPERS = 25
SENTENCE_CAP = 20000
READ_BATCH = 256
RUN_MIN_LEN = 4
GRAM = 5

# Five papers already measured by PDF-1a (all CC-BY/-SA, all >= 200 CJK chars): 2610.18269 and
# 2610.20785 are required members, the other three are the highest-yield known candidates.
SEEDS = [
    ("2610.18269", "cs.CL"),
    ("2610.20785", "cs.CL"),
    ("2610.17703", "cs.CL"),
    ("2610.16201", "cs.CL"),
    ("2610.14101", "cs.CL"),
]

# (group label, arXiv query, how many to keep). Quotas sum to 20; with the five seeds that is
# the 25-paper target. A slot that cannot fill its quota leaves the remainder to the top-up
# rounds, which stay in cs.CL/cs.CV where CJK-rich PDFs are common.
SLOTS = [
    ("cs.CL", 'cat:cs.CL AND abs:"Chinese"', 3),
    ("cs.CL", 'cat:cs.CL AND abs:"Japanese"', 1),
    ("cs.CL", 'cat:cs.CL AND abs:"Korean"', 1),
    ("cs.CL", 'cat:cs.CL AND (abs:hanzi OR abs:kanji OR abs:"classical Chinese")', 1),
    ("cs.CV", 'cat:cs.CV AND (abs:"Chinese" OR abs:"Japanese" OR abs:hanzi OR abs:kanji)', 2),
    ("cs.CV", 'cat:cs.CV AND (abs:CJK OR abs:"character recognition" OR abs:"Chinese benchmark")', 1),
    ("cs.AI", 'cat:cs.AI AND (abs:"Chinese" OR abs:"Japanese" OR abs:"Korean")', 3),
    ("eess", '(cat:eess.AS OR cat:eess.IV) AND (abs:"Chinese" OR abs:"Japanese" OR abs:Korean '
             'OR abs:Mandarin OR abs:Cantonese)', 3),
    ("physics", '(cat:physics.soc-ph OR cat:physics.hist-ph OR cat:cond-mat.soft) AND '
                '(abs:"Chinese" OR abs:"Japanese")', 2),
    ("math", '(cat:math.HO OR cat:math.LO) AND (abs:"Chinese" OR abs:"Japanese" OR '
             'abs:"classical Chinese")', 2),
    ("q-bio", '(cat:q-bio.QM OR cat:q-bio.NC) AND (abs:"Chinese" OR abs:"Japanese" OR '
              'abs:"Korean")', 1),
]
TOPUP_QUERIES = [
    ('cat:cs.CL AND (abs:"Chinese benchmark" OR abs:CJK OR abs:Mandarin OR abs:Cantonese)', "cs.CL"),
    ('cat:cs.CL AND (abs:Chinese OR abs:Japanese OR abs:Korean) AND abs:benchmark', "cs.CL"),
    ('cat:cs.CL AND (abs:"Chinese" OR abs:"Japanese" OR abs:"Korean")', "cs.CL"),
    ('cat:cs.CV AND (abs:Chinese OR abs:Japanese OR abs:Korean)', "cs.CV"),
]
MAX_CANDIDATES_PER_SLOT = 40
# CC-BY(-SA) is what the licence check needs, and it is common on recent submissions only: the
# arXiv default licence of older papers is the non-exclusive one, which the check rejects. The
# date clause keeps relevance ranking while restricting candidates to the CC-friendly era.
DATE_FILTER = "submittedDate:[202301010000 TO 202612312359]"


# --------------------------------------------------------------------------------------------
# Shared helpers (imported from the PDF-1a corpus script, never modified)
# --------------------------------------------------------------------------------------------

def load_sibling(name: str):
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), name)
    spec = importlib.util.spec_from_file_location(name.replace("-", "_").replace(".py", ""), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CORPUS = load_sibling("pdf-corpus.py")
is_cjk = CORPUS.is_cjk
squash = CORPUS.squash
license_marker = CORPUS.license_marker
write_json = CORPUS.write_json
append_jsonl = CORPUS.append_jsonl
fmt = CORPUS.fmt
gram_containment = CORPUS.gram_containment


def fetch(url: str, timeout: float, attempts: int = 4) -> bytes:
    """Fetch with backoff; a 429 from the arXiv API gets a long, escalating wait."""
    last: Exception | None = None
    for attempt in range(attempts):
        try:
            request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
            with urllib.request.urlopen(request, timeout=timeout) as response:
                return response.read()
        except urllib.error.HTTPError as exc:
            last = exc
            wait = 30 * (attempt + 1) if exc.code == 429 else 5 * (attempt + 1)
            time.sleep(wait)
        except Exception as exc:  # noqa: BLE001 - retried below
            last = exc
            time.sleep(5 * (attempt + 1))
    raise RuntimeError(f"{url}: {last}")


def load_list(path: str) -> list[dict]:
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
           f"&max_results={max_results}&sortBy=relevance&sortOrder=descending")
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
            "submitted": entry.findtext("atom:published", default="", namespaces=namespace),
            "category": category,
        })
    return entries


def pdf_facts(path: str) -> dict:
    """Page count, CJK characters in the text layer, producer/creator."""
    import pymupdf

    with pymupdf.open(path) as document:
        cjk = 0
        for page in document:
            cjk += sum(1 for char in page.get_text() if is_cjk(char))
        metadata = document.metadata or {}
        return {
            "pages": document.page_count,
            "cjk_chars": cjk,
            "producer": (metadata.get("producer") or "")[:120],
            "creator": (metadata.get("creator") or "")[:120],
        }


def abs_license(identifier: str) -> tuple[str | None, str | None]:
    """The licence link on the abs page (authoritative), as `pdf-corpus.py` reads it."""
    page = fetch(ARXIV_ABS.format(id=identifier), 120).decode("utf-8", "ignore")
    for found in re.findall(r"https?://(?:creativecommons\.org|arxiv\.org)/[a-z0-9./\-]+", page):
        marker = license_marker(found)
        if marker is not None:
            return found, marker
    return None, None


class Selector:
    def __init__(self, work_dir: str, out: str = "") -> None:
        self.work = os.path.join(work_dir, "select")
        os.makedirs(self.work, exist_ok=True)
        self.chosen: list[dict] = []
        self.skipped: list[dict] = []
        self.seen: set[str] = set()
        self.downloads = 0
        self.candidates = 0
        self.out = out

    def write(self) -> None:
        """Write the list after every keep: a later API failure must not lose the work."""
        if not self.out:
            return
        header = [
            "# PDF-1b CJK font-loss corpus: arXiv papers with >= 200 CJK characters in the PDF "
            "text layer, CC-BY(-SA)/CC0 only.",
            f"# Selected {time.strftime('%Y-%m-%d')} by scripts/pdf-cjk.py --select; "
            "one JSON object per line.",
            "# id, title, group, category, pages, licence (abs-page link), cjk_chars, "
            "producer/creator.",
        ]
        os.makedirs(os.path.dirname(os.path.abspath(self.out)) or ".", exist_ok=True)
        with open(self.out, "w", encoding="utf-8") as handle:
            handle.write("\n".join(header) + "\n")
            for entry in self.chosen:
                handle.write(json.dumps(entry, ensure_ascii=False, sort_keys=True) + "\n")

    def full(self) -> bool:
        return len(self.chosen) >= TARGET_PAPERS

    def downloads_left(self) -> bool:
        return self.downloads < MAX_DOWNLOADS

    def consider(self, identifier: str, title: str, category: str, group: str,
                 submitted: str = "") -> bool:
        """Check licence, download, count CJK; keep the paper when it qualifies."""
        if self.full() or not self.downloads_left():
            return False
        if identifier in self.seen:
            return False
        self.seen.add(identifier)
        self.candidates += 1
        try:
            licence_url, marker = abs_license(identifier)
            time.sleep(ARXIV_DELAY_S)
        except RuntimeError as exc:
            self.skipped.append({"id": identifier, "reason": f"abs_failed: {exc}"})
            return False
        if marker is None:
            self.skipped.append({"id": identifier, "reason": "licence_not_cc"})
            return False
        path = os.path.join(self.work, f"{identifier}.pdf")
        if not os.path.isfile(path):
            self.downloads += 1
            try:
                data = fetch(ARXIV_PDF.format(id=identifier), 300, attempts=1)
            except Exception as exc:  # noqa: BLE001 - a failed download is a skip
                self.skipped.append({"id": identifier, "reason": f"download_failed: {exc}"})
                time.sleep(ARXIV_DELAY_S)
                return False
            time.sleep(ARXIV_DELAY_S)
            if not data.startswith(b"%PDF"):
                self.skipped.append({"id": identifier, "reason": "not_a_pdf"})
                return False
            if len(data) > MAX_PDF_BYTES:
                self.skipped.append({"id": identifier, "reason": f"bytes={len(data)}"})
                return False
            with open(path, "wb") as handle:
                handle.write(data)
        try:
            facts = pdf_facts(path)
        except Exception as exc:  # noqa: BLE001 - a broken PDF is a skip
            self.skipped.append({"id": identifier, "reason": f"unreadable: {exc}"})
            return False
        if not MIN_PAGES <= facts["pages"] <= MAX_PAGES:
            self.skipped.append({"id": identifier, "reason": f"pages={facts['pages']}"})
            return False
        if facts["cjk_chars"] < CJK_MIN_CHARS:
            self.skipped.append({"id": identifier, "reason": f"cjk={facts['cjk_chars']}"})
            return False
        self.chosen.append({
            "id": identifier,
            "title": title,
            "group": group,
            "category": category,
            "pages": facts["pages"],
            "license": licence_url,
            "license_marker": marker,
            "cjk_chars": facts["cjk_chars"],
            "submitted": submitted[:10],
            "bytes": os.path.getsize(path),
            "sha256": hashlib.sha256(open(path, "rb").read()).hexdigest(),
            "producer": facts["producer"],
            "creator": facts["creator"],
        })
        print(f"{group} {len(self.chosen)}/{TARGET_PAPERS} {identifier} pages={facts['pages']} "
              f"cjk={facts['cjk_chars']} {title[:60]}", flush=True)
        self.write()
        return True

    def slot(self, query: str, group: str, quota: int) -> None:
        query = f"({query}) AND {DATE_FILTER}"
        picked = 0
        start = 0
        scanned = 0
        while picked < quota and scanned < MAX_CANDIDATES_PER_SLOT and not self.full() \
                and self.downloads_left():
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
                if picked >= quota or scanned >= MAX_CANDIDATES_PER_SLOT or self.full() \
                        or not self.downloads_left():
                    break
                if candidate["id"] in self.seen:
                    continue
                scanned += 1
                if self.consider(candidate["id"], candidate["title"], candidate["category"],
                                 group, candidate["submitted"]):
                    picked += 1
        if picked < quota:
            print(f"::warning::slot {group} {query[:50]} found only {picked}/{quota}",
                  flush=True)


def select_mode(args) -> int:
    selector = Selector(args.work_dir, args.out)
    print(f"seeds: {len(SEEDS)} known CJK papers", flush=True)
    for identifier, group in SEEDS:
        if selector.full():
            break
        try:
            batch = arxiv_query(f"id:{identifier}", 0, 1)
            time.sleep(ARXIV_DELAY_S)
        except Exception as exc:  # noqa: BLE001 - metadata is cosmetic, keep going
            print(f"::warning::metadata for {identifier} failed: {exc}", flush=True)
            batch = []
        title = batch[0]["title"] if batch else ""
        category = batch[0]["category"] if batch else group
        selector.consider(identifier, title, category, group,
                          batch[0]["submitted"] if batch else "")
    for group, query, quota in SLOTS:
        if selector.full() or not selector.downloads_left():
            break
        selector.slot(query, group, quota)
    for query, group in TOPUP_QUERIES:
        if selector.full() or not selector.downloads_left():
            break
        selector.slot(query, group, TARGET_PAPERS - len(selector.chosen))

    selector.write()
    reasons = collections.Counter(item["reason"].split(":")[0] for item in selector.skipped)
    print(f"wrote {len(selector.chosen)} entries to {args.out}")
    print(f"tried {selector.candidates} candidates, downloaded {selector.downloads} PDFs; "
          f"skipped reasons: {reasons.most_common()}")
    write_json(os.path.join(selector.work, "select-report.json"), {
        "chosen": selector.chosen,
        "skipped": selector.skipped,
        "candidates": selector.candidates,
        "downloads": selector.downloads,
        "target": TARGET_PAPERS,
    })
    if len(selector.chosen) < TARGET_PAPERS:
        print(f"::warning::only {len(selector.chosen)}/{TARGET_PAPERS} papers qualified",
              flush=True)
    return 0 if len(selector.chosen) >= TARGET_PAPERS else 1


# --------------------------------------------------------------------------------------------
# Font inventory (part A) and content-stream location
# --------------------------------------------------------------------------------------------

RESOURCE_ENTRY = re.compile(r"/([^\s/<>\[\](){}]+)\s+(\d+)\s+(\d+)\s+R")
SHOW_OPERATOR = re.compile(rb"(?:\bTj\b|\bTJ\b|\bTf\b)")
TF_OPERATOR = re.compile(rb"/([^\s/<>\[\](){}]+)\s+[-\d.]+\s+Tf")
DO_OPERATOR = re.compile(rb"/([^\s/<>\[\](){}]+)\s+Do")


def resource_refs(doc, xref: int, key: str) -> dict[str, int]:
    """`/Key << /Name N 0 R ... >>` of an object's Resources, as {name: xref}."""
    kind, value = doc.xref_get_key(xref, f"Resources/{key}")
    if kind != "dict":
        return {}
    return {name: int(number) for name, number, _ in RESOURCE_ENTRY.findall(value)}


def xobject_usage(doc, page) -> dict[int, str]:
    """Which font xrefs are used by `Tf` in the page content stream vs only inside Form XObjects.

    lopdf's text extractor walks the page content stream operations only: it has no `Do` arm, so
    text drawn from a Form XObject never reaches the output. Recording the distinction is what
    makes that loss visible in the per-font table.
    """
    used: dict[int, set[str]] = collections.defaultdict(set)

    def walk(resources_owner: int, content: bytes, location: str, depth: int) -> None:
        fonts = resource_refs(doc, resources_owner, "Font")
        for name in TF_OPERATOR.findall(content):
            key = name.decode("latin-1")
            if key in fonts:
                used[fonts[key]].add(location)
        if depth > 4:
            return
        xobjects = resource_refs(doc, resources_owner, "XObject")
        for name in DO_OPERATOR.findall(content):
            key = name.decode("latin-1")
            xref = xobjects.get(key)
            if xref is None:
                continue
            kind, subtype = doc.xref_get_key(xref, "Subtype")
            if kind != "name" or subtype != "/Form":
                continue
            try:
                stream = doc.xref_stream(xref)
            except Exception:  # noqa: BLE001 - an unreadable XObject carries no text
                continue
            if not SHOW_OPERATOR.search(stream):
                continue
            # A Form XObject without its own /Resources inherits the page's.
            owner = xref if doc.xref_get_key(xref, "Resources")[0] != "null" else resources_owner
            walk(owner, stream, "xobject", depth + 1)

    for xref in page.get_contents():
        try:
            content = doc.xref_stream(xref)
        except Exception:  # noqa: BLE001
            continue
        walk(page.xref, content, "page", 0)
    return {xref: ("page" if "page" in places else "xobject") for xref, places in used.items()}


def descendant(doc, xref: int) -> int | None:
    kind, value = doc.xref_get_key(xref, "DescendantFonts")
    if kind != "array":
        return None
    match = re.search(r"(\d+)\s+0\s+R", value)
    return int(match.group(1)) if match else None


def font_descriptor(doc, xref: int) -> int | None:
    kind, value = doc.xref_get_key(xref, "FontDescriptor")
    if kind == "xref":
        return int(value.split()[0])
    child = descendant(doc, xref)
    if child is not None:
        kind, value = doc.xref_get_key(child, "FontDescriptor")
        if kind == "xref":
            return int(value.split()[0])
    return None


def cid_system_info(doc, xref: int) -> tuple[str | None, str | None]:
    child = descendant(doc, xref)
    if child is None:
        return None, None
    kind, value = doc.xref_get_key(child, "CIDSystemInfo")
    if kind != "dict":
        return None, None
    registry = re.search(r"/Registry\s*\(([^)]*)\)", value)
    ordering = re.search(r"/Ordering\s*\(([^)]*)\)", value)
    return (registry.group(1) if registry else None, ordering.group(1) if ordering else None)


def cid_to_gid_map(doc, xref: int) -> str | None:
    child = descendant(doc, xref)
    if child is None:
        return None
    kind, value = doc.xref_get_key(child, "CIDToGIDMap")
    if kind == "name":
        return value.lstrip("/")
    if kind == "xref":
        return "stream"
    return None


def encoding_label(doc, xref: int) -> str:
    kind, value = doc.xref_get_key(xref, "Encoding")
    if kind == "name":
        return value.lstrip("/")
    if kind == "dict":
        base = re.search(r"/BaseEncoding\s*/([^\s/>]+)", value)
        differences = "/Differences" in value
        if differences:
            return f"Differences({base.group(1) if base else 'none'})"
        return f"dict({base.group(1) if base else 'none'})"
    if kind == "xref":
        # An Encoding stream: a CMap program, so report its CMapName when it has one.
        try:
            stream = doc.xref_stream(int(value.split()[0])).decode("latin-1")
        except Exception:  # noqa: BLE001
            return "stream"
        name = re.search(r"/CMapName\s*/([^\s/>]+)", stream)
        return f"CMap:{name.group(1)}" if name else "stream"
    return "none"


def embedded_kinds(doc, descriptor: int | None) -> list[str]:
    if descriptor is None:
        return []
    found = []
    for key in ("FontFile", "FontFile2", "FontFile3"):
        if doc.xref_get_key(descriptor, key)[0] != "null":
            found.append(key)
    return found


def build_inventory(doc) -> list[dict]:
    """One record per font xref seen on any page, with the Part-A fields."""
    pages: dict[int, set[int]] = collections.defaultdict(set)
    names: dict[int, set[str]] = collections.defaultdict(set)
    types: dict[int, str] = {}
    basefonts: dict[int, str] = {}
    encodings: dict[int, str] = {}
    # location of the *use*, per page: the same font xref can be drawn by the page content
    # stream on one page and from a Form XObject on another.
    page_location: dict[tuple[int, int], str] = {}
    locations: dict[int, set[str]] = collections.defaultdict(set)
    for page in doc:
        for xref, usage in xobject_usage(doc, page).items():
            locations[xref].add(usage)
            page_location[(page.number + 1, xref)] = usage
        for entry in page.get_fonts(full=True):
            xref, _ext, font_type, basefont, resource, encoding, _referencer = entry[:7]
            pages[xref].add(page.number + 1)
            names[xref].add(resource)
            types[xref] = font_type
            basefonts[xref] = basefont
            encodings[xref] = encoding
    inventory = []
    for xref in sorted(pages):
        descriptor = font_descriptor(doc, xref)
        child = descendant(doc, xref)
        registry, ordering = cid_system_info(doc, xref)
        basefont = basefonts.get(xref, "")
        subset = re.match(r"^([A-Z]{6})\+", basefont)
        record = {
            "xref": xref,
            "pages": sorted(pages[xref]),
            "resource_names": sorted(names[xref]),
            "basefont": basefont,
            "subset_prefix": subset.group(1) if subset else None,
            "type": types.get(xref, ""),
            "encoding_field": encodings.get(xref, ""),
            "encoding": encoding_label(doc, xref),
            "to_unicode": doc.xref_get_key(xref, "ToUnicode")[0] != "null",
            "embedded": embedded_kinds(doc, descriptor),
            "descendant_subtype": (
                doc.xref_get_key(child, "Subtype")[1].lstrip("/")
                if child is not None and doc.xref_get_key(child, "Subtype")[0] == "name"
                else None),
            "cid_registry": registry,
            "cid_ordering": ordering,
            "cid_to_gid_map": cid_to_gid_map(doc, xref),
            "location": ("page" if "page" in locations.get(xref, set()) else "xobject"),
            "page_location": {page: page_location[(page, xref)]
                              for page in sorted(pages[xref]) if (page, xref) in page_location},
        }
        record["group"] = font_group_key(record, producer_family(doc))
        inventory.append(record)
    return inventory


def producer_family(doc) -> str:
    metadata = doc.metadata or {}
    text = f"{metadata.get('producer') or ''} {metadata.get('creator') or ''}".lower()
    for needle, family in (
        ("arxiv genpdf", "arXiv GenPDF"),
        ("pikepdf", "pikepdf"),
        ("pdftex", "pdfTeX"),
        ("xetex", "XeTeX"),
        ("luatex", "LuaTeX"),
        ("dvips", "dvips"),
        ("ghostscript", "Ghostscript"),
        ("microsoft word", "Word"),
        ("word", "Word"),
        ("powerpoint", "Office"),
        ("latex", "LaTeX"),
        ("tex", "TeX"),
        ("skia", "Skia"),
        ("chromium", "Chromium"),
        ("reportlab", "ReportLab"),
        ("fpdf", "FPDF"),
        ("pymupdf", "PyMuPDF"),
        ("mupdf", "MuPDF"),
        ("cairo", "Cairo"),
        ("indesign", "InDesign"),
    ):
        if needle in text:
            return family
    producer = (metadata.get("producer") or "").strip()
    return producer[:24] if producer else "unknown"


def font_group_key(font: dict, family: str, location: str | None = None) -> str:
    embedded = "embedded" if font["embedded"] else "not-embedded"
    to_unicode = "tounicode" if font["to_unicode"] else "no-tounicode"
    subtype = font["type"] or "?"
    if font["descendant_subtype"]:
        subtype = f"{subtype}/{font['descendant_subtype']}"
    cid_map = font["cid_to_gid_map"] or "-"
    return (f"{subtype}|{font['encoding']}|{to_unicode}|{embedded}|cidtogid={cid_map}|"
            f"{location or font['location']}|{family}")


# --------------------------------------------------------------------------------------------
# Reference CJK ownership (part B) and per-font survival (part C)
# --------------------------------------------------------------------------------------------

def strip_prefix(basefont: str) -> str:
    return re.sub(r"^[A-Z]{6}\+", "", basefont or "")


def span_names(basefont: str) -> list[str]:
    """Names a `rawdict` span may carry for this font: the subset prefix is stripped, and so is
    a trailing `-Identity-H`/`-Identity-V` that some producers append to BaseFont."""
    name = strip_prefix(basefont)
    names = [name]
    for suffix in ("-Identity-H", "-Identity-V"):
        if name.endswith(suffix):
            names.append(name[: -len(suffix)])
    return names


def cjk_runs(document) -> list[dict]:
    """Consecutive CJK runs from `rawdict` spans, with the span's font name and page."""
    runs: list[dict] = []
    for page in document:
        raw = page.get_text("rawdict")
        for block in raw.get("blocks", []):
            for line in block.get("lines", []):
                for span in line.get("spans", []):
                    current: list[str] = []
                    for char in span.get("chars", []):
                        value = char.get("c", "")
                        if is_cjk(value):
                            current.append(value)
                        elif current:
                            runs.append({"page": page.number + 1, "font": span.get("font", ""),
                                         "text": "".join(current)})
                            current = []
                    if current:
                        runs.append({"page": page.number + 1, "font": span.get("font", ""),
                                     "text": "".join(current)})
    return runs


def attribute_runs(runs: list[dict], inventory: list[dict]) -> tuple[dict, set]:
    """(page, span font name) -> one font xref, plus the keys where the name is ambiguous.

    Two font xrefs on one page can share a BaseFont (a producer that embeds the same face
    twice). `rawdict` only reports the BaseFont, so the run is attributed to the lowest xref
    and the ambiguity is counted instead of being silently double-counted.
    """
    lookup: dict[tuple[int, str], list[int]] = collections.defaultdict(list)
    for font in inventory:
        for name in span_names(font["basefont"]):
            for page in font["pages"]:
                lookup[(page, name)].append(font["xref"])
    ambiguous = {key for key, xrefs in lookup.items() if len(set(xrefs)) > 1}
    return {key: min(set(xrefs)) for key, xrefs in lookup.items()}, ambiguous


def per_font_survival(document, inventory: list[dict], default_text: str) -> dict:
    """Per-font CJK survival against the default backend's text, two ways.

    A run is a maximal sequence of CJK characters inside one `rawdict` span. Two measures:

    - character containment (primary, available for every font): the multiset of CJK characters
      the font owns in the reference, checked against the multiset of CJK characters the default
      backend returned. A character is claimed by at most one font (fonts are visited in xref
      order and the returned characters are decremented as they are claimed), so the totals
      cannot exceed what the backend actually returned. Order-free: it cannot see a font whose
      characters survive but in the wrong order or on the wrong page.
    - run containment (secondary, only where the font's spans hold runs of >= RUN_MIN_LEN
      characters): the whitespace-free run must be a substring of the whitespace-free default
      text. Position-sensitive, but CJK TeX subsets that emit one glyph per span never produce
      a run, so it is reported next to the character measure, not instead of it.
    """
    runs = cjk_runs(document)
    lookup, ambiguous = attribute_runs(runs, inventory)
    stats: dict[tuple[int, str], dict] = collections.defaultdict(
        lambda: {"runs": 0, "runs_found": 0, "run_chars": 0, "run_found": 0,
                 "chars": 0, "chars_found": 0, "short_run_chars": 0})
    owned: dict[tuple[int, str], collections.Counter] = collections.defaultdict(
        collections.Counter)
    page_location = {(page, font["xref"]): location
                     for font in inventory
                     for page, location in font["page_location"].items()}
    aggregate_location = {font["xref"]: font["location"] for font in inventory}
    unattributed = {"runs": 0, "chars": 0, "ambiguous_runs": 0}
    squashed_default = squash(default_text)
    for run in runs:
        key = (run["page"], run["font"])
        xref = lookup.get(key)
        if xref is None:
            unattributed["runs"] += 1
            unattributed["chars"] += len(run["text"])
            continue
        if key in ambiguous:
            unattributed["ambiguous_runs"] += 1
        location = page_location.get((run["page"], xref), aggregate_location.get(xref, "page"))
        bucket = (xref, location)
        length = len(run["text"])
        needle = squash(run["text"])
        entry = stats[bucket]
        owned[bucket].update(run["text"])
        if length < RUN_MIN_LEN:
            entry["short_run_chars"] += length
            continue
        entry["runs"] += 1
        entry["run_chars"] += length
        if needle and needle in squashed_default:
            entry["runs_found"] += 1
            entry["run_found"] += length
    # Per character, the claim budget is min(reference count, returned count): duplicated
    # section text in the returned document cannot claim more characters than the reference has,
    # and a character the backend never returned cannot be claimed at all.
    reference_counts: collections.Counter = collections.Counter()
    for counts in owned.values():
        reference_counts.update(counts)
    returned_counts = collections.Counter(char for char in default_text if is_cjk(char))
    budget = collections.Counter({char: min(count, returned_counts.get(char, 0))
                                  for char, count in reference_counts.items()})
    for bucket in sorted(owned):
        entry = stats[bucket]
        for char, count in owned[bucket].items():
            entry["chars"] += count
            taken = min(count, budget.get(char, 0))
            if taken:
                budget[char] -= taken
                entry["chars_found"] += taken
    return {"stats": stats, "unattributed": unattributed}


# --------------------------------------------------------------------------------------------
# Backend runs
# --------------------------------------------------------------------------------------------

def read_backend(probe, binary: str, pdf_path: str, backend: str, layout_python: str,
                 state_root: str, label: str, deadline: float) -> tuple[dict, str]:
    """Open and read the whole document through `get_text_units`, batching 256 items per call.

    The PDF section model can return the same text from two different sections (a heading
    section that covers its siblings' range), which would inflate the recovered count. Such a
    section is skipped as a whole: its concatenated text is compared with the sections already
    read, and an identical one is dropped. Repeated *units* inside one section are kept, so a
    paper that legitimately repeats a prompt line is not penalised. `units_read`,
    `units_kept` and `duplicate_sections` make the adjustment visible.
    """
    import shutil

    record: dict = {
        "outcome": "error", "error_code": None, "error": None, "open_s": None,
        "peak_rss_kib": 0, "sections": None, "sentences": None, "units_read": 0,
        "units_kept": 0, "duplicate_sections": 0, "capped": False, "partial": False,
        "read_error": None, "chars": 0, "cjk_chars": 0,
    }
    texts: list[str] = []
    seen_sections: set[str] = set()
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
            record["sections"] = len(sections)
            totals = 0
            for section in sections:
                if record["units_read"] >= SENTENCE_CAP:
                    record["capped"] = True
                    break
                if time.monotonic() > deadline:
                    record["partial"] = True
                    break
                cursor = None
                section_texts: list[str] = []
                while True:
                    if record["units_read"] >= SENTENCE_CAP:
                        record["capped"] = True
                        break
                    if time.monotonic() > deadline:
                        record["partial"] = True
                        break
                    arguments = probe.text_units(document_id, section["section_id"],
                                                READ_BATCH, cursor)
                    try:
                        data = probe.payload(server.tool("get_text_units", arguments)) or {}
                    except probe.ProbeError as exc:
                        record["read_error"] = str(exc)[:200]
                        break
                    stream = data.get("stream") or {}
                    totals += int(stream.get("total_items") or 0)
                    for item in data.get("items") or []:
                        record["units_read"] += 1
                        section_texts.append(item.get("text") or "")
                    cursor = data.get("next_cursor")
                    if not cursor:
                        break
                section_text = "\n".join(section_texts)
                fingerprint = squash(section_text)
                if fingerprint and fingerprint in seen_sections:
                    record["duplicate_sections"] += 1
                    continue
                if fingerprint:
                    seen_sections.add(fingerprint)
                record["units_kept"] += len(section_texts)
                texts.extend(section_texts)
            record["sentences"] = totals or None
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
    joined = "\n".join(texts)
    record["chars"] = len(squash(joined))
    record["cjk_chars"] = sum(1 for char in joined if is_cjk(char))
    return record, joined


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
        page = fetch(ARXIV_ABS.format(id=entry["id"]), 120).decode("utf-8", "ignore")
        time.sleep(ARXIV_DELAY_S)
    except RuntimeError as exc:
        status.update({"download": "download_failed", "error": f"abs: {exc}"})
        return None, status
    if marker is None or marker not in page:
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


def process_paper(probe, entry: dict, args, deadline: float, inventory_dir: str) -> dict:
    import pymupdf

    record: dict = {
        "kind_record": "paper", "id": entry["id"], "title": entry["title"],
        "group": entry.get("group"), "category": entry.get("category"),
        "pages": entry.get("pages"), "license": entry.get("license"),
        "producer": entry.get("producer"), "creator": entry.get("creator"),
        "outcome": "ok", "download": None, "error": None,
        "reference": None, "default": None, "layout": None, "fonts": [],
        "page_text": [], "unattributed": None,
    }
    started = time.perf_counter()
    pdf_path, download_status = download_paper(entry, args.work_dir)
    record["download"] = download_status["download"]
    if pdf_path is None:
        record.update({"outcome": "download_failed", "error": download_status["error"]})
        return record
    document = pymupdf.open(pdf_path)
    try:
        record["metadata"] = {key: (document.metadata or {}).get(key) for key in
                              ("producer", "creator", "format")}
        record["producer_family"] = producer_family(document)
        reference_chars = 0
        reference_cjk = 0
        page_text = []
        for page in document:
            text = page.get_text()
            reference_chars += len(squash(text))
            cjk = sum(1 for char in text if is_cjk(char))
            reference_cjk += cjk
            page_text.append({"page": page.number + 1, "cjk": cjk, "text": text})
        record["reference"] = {"pages": document.page_count, "chars": reference_chars,
                               "cjk_chars": reference_cjk}
        inventory = build_inventory(document)
        write_json(os.path.join(inventory_dir, f"{entry['id']}.json"),
                   {"id": entry["id"], "producer_family": record["producer_family"],
                    "metadata": record["metadata"], "fonts": inventory})
    finally:
        document.close()

    default_record, default_text = read_backend(
        probe, args.binary, pdf_path, "default", "", args.state_root, entry["id"], deadline)
    record["default"] = default_record

    document = pymupdf.open(pdf_path)
    try:
        survival = per_font_survival(document, inventory, default_text)
        page_rows = []
        for row in page_text:
            page_rows.append({
                "page": row["page"], "cjk": row["cjk"],
                "containment": gram_containment(row["text"], default_text),
            })
        record["page_text"] = page_rows
    finally:
        document.close()

    empty = {"runs": 0, "runs_found": 0, "run_chars": 0, "run_found": 0, "chars": 0,
             "chars_found": 0, "short_run_chars": 0}
    by_xref = {font["xref"]: font for font in inventory}
    fonts = []
    buckets = dict(survival["stats"])
    for xref, font in by_xref.items():
        # A font xref used by the page stream and by a Form XObject is split into one record
        # per location: the two uses lose text for different reasons.
        seen_locations = [location for (font_xref, location) in buckets if font_xref == xref]
        if not seen_locations:
            seen_locations = [font["location"]]
        for location in sorted(set(seen_locations)):
            stats = buckets.get((xref, location)) or empty
            entry_record = {key: font[key] for key in
                            ("xref", "basefont", "subset_prefix", "type", "encoding",
                             "to_unicode", "embedded", "descendant_subtype", "cid_registry",
                             "cid_ordering", "cid_to_gid_map", "pages")}
            entry_record["location"] = location
            entry_record["group"] = font_group_key(font, record["producer_family"], location)
            entry_record.update(stats)
            fonts.append(entry_record)
    record["fonts"] = fonts
    record["unattributed"] = survival["unattributed"]

    layout_record, _layout_text = read_backend(
        probe, args.binary, pdf_path, "layout", args.layout_python, args.state_root,
        entry["id"], deadline)
    record["layout"] = layout_record
    record["seconds"] = round(time.perf_counter() - started, 1)
    return record


def run_shard(args) -> int:
    probe = CORPUS.load_probe()
    entries = load_list(args.corpus)
    if not entries:
        print("::error::corpus list is empty")
        return 1
    shard = [entry for index, entry in enumerate(entries) if index // args.per_shard == args.shard]
    print(f"shard {args.shard}: {len(shard)} papers "
          f"({', '.join(entry['id'] for entry in shard)})", flush=True)
    os.makedirs(args.state_root, exist_ok=True)
    os.makedirs(args.inventory_dir, exist_ok=True)
    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)
    shard_deadline = time.monotonic() + args.deadline_minutes * 60
    for entry in shard:
        if time.monotonic() > shard_deadline:
            record = {"kind_record": "paper", "id": entry["id"], "group": entry.get("group"),
                      "title": entry["title"], "pages": entry.get("pages"),
                      "outcome": "not_run", "error": "shard deadline"}
            append_jsonl(args.out, record)
            print(f"| {entry['id']} | - | not_run (shard deadline) |", flush=True)
            continue
        paper_deadline = min(shard_deadline,
                             time.monotonic() + args.paper_deadline_minutes * 60)
        record = process_paper(probe, entry, args, paper_deadline, args.inventory_dir)
        append_jsonl(args.out, record)
        default = record.get("default") or {}
        layout = record.get("layout") or {}
        print(f"| {entry['id']} | {record.get('pages')} | ref cjk "
              f"{(record.get('reference') or {}).get('cjk_chars')} | default "
              f"{default.get('error_code') or default.get('outcome')} "
              f"{default.get('cjk_chars')} | layout "
              f"{layout.get('error_code') or layout.get('outcome')} "
              f"{layout.get('cjk_chars')} | {record.get('seconds')}s |", flush=True)
    return 0


# --------------------------------------------------------------------------------------------
# Aggregate
# --------------------------------------------------------------------------------------------

def load_records_recursive(jsonl_dir: str) -> list[dict]:
    """Every `*.jsonl` under the directory, at any depth (artifacts keep their subdirectories)."""
    records: list[dict] = []
    for root, _dirs, names in os.walk(jsonl_dir):
        for name in sorted(names):
            if not name.endswith(".jsonl"):
                continue
            with open(os.path.join(root, name), encoding="utf-8") as handle:
                for line in handle:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        records.append(json.loads(line))
                    except json.JSONDecodeError:
                        continue
    return records


def group_rows(papers: list[dict]) -> list[dict]:
    groups: dict[str, dict] = {}
    for paper in papers:
        for font in paper.get("fonts") or []:
            key = font["group"]
            entry = groups.setdefault(key, {
                "group": key, "papers": set(), "chars": 0, "chars_found": 0,
                "run_chars": 0, "run_found": 0, "short_run_chars": 0, "fonts": 0,
                "type": font["type"], "encoding": font["encoding"],
                "to_unicode": font["to_unicode"], "embedded": bool(font["embedded"]),
                "cid_to_gid_map": font["cid_to_gid_map"], "location": font["location"],
                "producer_family": key.rsplit("|", 1)[-1],
            })
            entry["papers"].add(paper["id"])
            entry["fonts"] += 1
            entry["chars"] += font["chars"]
            entry["chars_found"] += font["chars_found"]
            entry["run_chars"] += font.get("run_chars") or 0
            entry["run_found"] += font.get("run_found") or 0
            entry["short_run_chars"] += font.get("short_run_chars") or 0
    rows = []
    for entry in groups.values():
        entry["papers"] = len(entry["papers"])
        entry["loss_pct"] = (round(100.0 * (entry["chars"] - entry["chars_found"])
                                   / entry["chars"], 1) if entry["chars"] else None)
        entry["run_loss_pct"] = (round(100.0 * (entry["run_chars"] - entry["run_found"])
                                       / entry["run_chars"], 1) if entry["run_chars"] else None)
        rows.append(entry)
    rows.sort(key=lambda item: (-(item["chars"] - item["chars_found"]), -item["chars"]))
    return rows


def aggregate_mode(args) -> int:
    if not os.path.isdir(args.jsonl_dir):
        print(f"::error::no shard results in {args.jsonl_dir}")
        return 1
    records = load_records_recursive(args.jsonl_dir)
    papers = [record for record in records if record.get("kind_record") == "paper"]
    if not papers:
        print(f"::error::no paper records in {args.jsonl_dir}")
        return 1
    papers.sort(key=lambda item: (item.get("group") or "", item.get("id") or ""))
    measured = [paper for paper in papers if paper.get("reference")]

    per_paper = ["| Paper | Group | Pages | Producer | Ref CJK | Default CJK | Default units | "
                 "CJK recovered | Loss | Layout | Layout CJK |",
                 "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for paper in papers:
        reference = paper.get("reference") or {}
        default = paper.get("default") or {}
        layout = paper.get("layout") or {}
        fonts = paper.get("fonts") or []
        chars = sum(font["chars"] for font in fonts)
        found = sum(font["chars_found"] for font in fonts)
        loss = f"{100.0 * (chars - found) / chars:.1f}%" if chars else "-"
        per_paper.append(
            f"| {paper['id']} | {paper.get('group')} | {paper.get('pages')} | "
            f"{(paper.get('producer_family') or '-')} | {reference.get('cjk_chars', '-')} | "
            f"{default.get('cjk_chars', '-')} | {default.get('units_kept', '-')} | "
            f"{found}/{chars} | {loss} | "
            f"{layout.get('error_code') or layout.get('outcome') or '-'} | "
            f"{layout.get('cjk_chars', '-')} |")

    groups = group_rows(papers)
    group_table = ["| Font group (Subtype/Encoding/ToUnicode/embedded/CIDToGIDMap/location/"
                   "producer) | Papers | Fonts | Ref CJK | Recovered | Loss | Run chars | "
                   "Run loss |",
                   "| --- | --- | --- | --- | --- | --- | --- | --- |"]
    for row in groups:
        if not row["chars"]:
            continue
        group_table.append(
            f"| `{row['group']}` | {row['papers']} | {row['fonts']} | {row['chars']} | "
            f"{row['chars_found']} | {row['loss_pct']}% | {row['run_chars']} | "
            f"{fmt(row['run_loss_pct'])}% |")

    location: dict[str, list[int]] = {}
    for paper in papers:
        for font in paper.get("fonts") or []:
            bucket = location.setdefault(font["location"], [0, 0])
            bucket[0] += font["chars"]
            bucket[1] += font["chars_found"]
    location_rows = ["| Text location | Ref CJK | Recovered | Loss |",
                     "| --- | --- | --- | --- |"]
    for name in sorted(location):
        chars, found = location[name]
        loss = f"{100.0 * (chars - found) / chars:.1f}%" if chars else "-"
        location_rows.append(f"| {name} | {chars} | {found} | {loss} |")

    lossy = []
    for paper in measured:
        chars = sum(font["chars"] for font in paper.get("fonts") or [])
        found = sum(font["chars_found"] for font in paper.get("fonts") or [])
        rate = (chars - found) / chars if chars else 0.0
        paper["loss_rate"] = round(rate, 4)
        if rate > 0.05:
            lossy.append(paper["id"])

    worst = next((row for row in groups if row["chars"] and row["loss_pct"] is not None
                  and row["chars_found"] < row["chars"]), None)
    summary = {
        "papers": len(papers),
        "papers_measured": len(measured),
        "papers_with_loss": len(lossy),
        "papers_loss_ids": lossy,
        "download_failed": [paper["id"] for paper in papers
                            if paper.get("outcome") == "download_failed"],
        "not_run": [paper["id"] for paper in papers if paper.get("outcome") == "not_run"],
        "reference_cjk": sum((paper.get("reference") or {}).get("cjk_chars") or 0
                             for paper in measured),
        "default_cjk": sum((paper.get("default") or {}).get("cjk_chars") or 0
                           for paper in measured),
        "groups": [{key: (sorted(value) if isinstance(value, set) else value)
                    for key, value in row.items()} for row in groups],
        "worst_group": worst,
        "location": {name: {"chars": value[0], "found": value[1]}
                     for name, value in location.items()},
        "layout": {
            "ok": len([paper for paper in papers
                       if (paper.get("layout") or {}).get("outcome") == "ok"]),
            "refused": len([paper for paper in papers
                            if (paper.get("layout") or {}).get("error_code") == "OCR_REQUIRED"]),
            "other": [(paper["id"], (paper.get("layout") or {}).get("error_code"))
                      for paper in papers
                      if (paper.get("layout") or {}).get("outcome") != "ok"
                      and (paper.get("layout") or {}).get("error_code") != "OCR_REQUIRED"],
            "cjk": sum((paper.get("layout") or {}).get("cjk_chars") or 0 for paper in papers),
        },
        "page_loss": [
            {"id": paper["id"],
             "pages": [row for row in (paper.get("page_text") or [])
                       if (row.get("containment") or 0) < 0.5 and row.get("cjk")]}
            for paper in measured
            if any((row.get("containment") or 0) < 0.5 and row.get("cjk")
                   for row in (paper.get("page_text") or []))
        ],
    }
    text = "\n".join([
        "# PDF-1b CJK font-loss results", "",
        "## Per paper", "", *per_paper, "",
        "## Font groups (main result)", "", *group_table, "",
        "## Text location", "", *location_rows, "",
        f"Papers with more than 5% of their reference CJK characters missing: "
        f"{len(lossy)}/{len(measured)} ({', '.join(lossy) or 'none'}).", "",
        f"Layout backend: {summary['layout']['ok']} ok, {summary['layout']['refused']} "
        f"OCR_REQUIRED, {len(summary['layout']['other'])} other failures.", "",
    ])
    print(text)
    if args.markdown:
        with open(args.markdown, "w", encoding="utf-8") as handle:
            handle.write(text + "\n")
    if args.out:
        write_json(args.out, summary)
    return 0


# --------------------------------------------------------------------------------------------
# Minimal repro (part F)
# --------------------------------------------------------------------------------------------

def repro_mode(args) -> int:
    import pymupdf

    probe = CORPUS.load_probe()
    with open(args.summary, encoding="utf-8") as handle:
        summary = json.load(handle)
    worst = summary.get("worst_group") or {}
    if not worst:
        print("::error::no worst group in the summary")
        return 1
    key = worst["group"]
    records = load_records_recursive(args.jsonl_dir)
    target = None
    for record in records:
        if record.get("kind_record") != "paper":
            continue
        for font in record.get("fonts") or []:
            if font["group"] == key and font["chars"] > font["chars_found"]:
                if target is None or font["pages"][0] < target[1]:
                    target = (record, font["pages"][0])
    if target is None:
        print("::error::no paper carries the worst group")
        return 1
    record, page_number = target
    entry = {"id": record["id"], "license": record.get("license"),
             "license_marker": license_marker(record.get("license") or "")}
    pdf_path, status = download_paper(entry, args.work_dir)
    if pdf_path is None:
        print(f"::error::repro download failed: {status['error']}")
        return 1
    os.makedirs(args.out_dir, exist_ok=True)
    repro_path = os.path.join(args.out_dir, f"{record['id']}-page{page_number}-repro.pdf")
    with pymupdf.open(pdf_path) as document:
        document.select([page_number - 1])
        document.save(repro_path, garbage=4, deflate=True)
    reference = 0
    with pymupdf.open(repro_path) as document:
        for page in document:
            reference += sum(1 for char in page.get_text() if is_cjk(char))
    result, text = read_backend(probe, args.binary, repro_path, "default", "",
                                args.state_root, f"repro-{record['id']}",
                                time.monotonic() + 600)
    recovered = sum(1 for char in text if is_cjk(char))
    report = {
        "group": key, "paper": record["id"], "page": page_number,
        "repro": os.path.basename(repro_path),
        "reference_cjk": reference, "default_cjk": recovered,
        "default_outcome": result.get("error_code") or result.get("outcome"),
        "still_loses": recovered < reference,
    }
    write_json(os.path.join(args.out_dir, "repro.json"), report)
    print(json.dumps(report, indent=1))
    return 0


# --------------------------------------------------------------------------------------------
# Entry point
# --------------------------------------------------------------------------------------------

def main() -> int:
    parser = argparse.ArgumentParser(description="PDF-1b CJK font-loss measurement.")
    parser.add_argument("--select", action="store_true", help="build the paper list")
    parser.add_argument("--aggregate", action="store_true", help="merge shards into tables")
    parser.add_argument("--repro", action="store_true", help="one-page repro for the worst group")
    parser.add_argument("--corpus", default="scripts/pdf-cjk-ids.txt")
    parser.add_argument("--out", default="")
    parser.add_argument("--markdown", default="")
    parser.add_argument("--summary", default="")
    parser.add_argument("--jsonl-dir", default="")
    parser.add_argument("--work-dir", default=".tmp/pdf-cjk")
    parser.add_argument("--inventory-dir", default="")
    parser.add_argument("--out-dir", default="")
    parser.add_argument("--binary", default="")
    parser.add_argument("--layout-python", default="")
    parser.add_argument("--state-root", default=".tmp/pdf-cjk/state")
    parser.add_argument("--shard", type=int, default=0)
    parser.add_argument("--per-shard", type=int, default=5)
    parser.add_argument("--deadline-minutes", type=float, default=70.0)
    parser.add_argument("--paper-deadline-minutes", type=float, default=12.0)
    args = parser.parse_args()
    if args.select:
        if not args.out:
            parser.error("--select needs --out")
        return select_mode(args)
    if args.aggregate:
        if not args.jsonl_dir:
            parser.error("--aggregate needs --jsonl-dir")
        return aggregate_mode(args)
    if args.repro:
        if not (args.jsonl_dir and args.summary and args.binary and args.out_dir):
            parser.error("--repro needs --jsonl-dir, --summary, --binary and --out-dir")
        return repro_mode(args)
    if not args.binary or not args.out:
        parser.error("run mode needs --binary and --out")
    if not args.layout_python:
        parser.error("run mode needs --layout-python")
    if not args.inventory_dir:
        parser.error("run mode needs --inventory-dir")
    return run_shard(args)


if __name__ == "__main__":
    sys.exit(main())
