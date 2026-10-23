#!/usr/bin/env python3
"""PDF-2a: where a default-backend PDF open spends its time (measurement only).

Three modes, all standard library except `facts` (PyMuPDF):

- `run`: open each input once through the real server (`READING_MCP_TOOLS=v1`, OCR off, default
  PDF backend) with `READING_MCP_TELEMETRY=1`, optionally under a `perf record` wrapper, and
  record wall time, the telemetry phase durations, the per-phase RSS/VmHWM the server reports,
  the peak RSS from `wait4` and the server's own section count.
- `facts`: the document-side facts the lopdf cost model needs, read with PyMuPDF: page count,
  the number of fonts in each page's `/Resources /Font` dictionary (lopdf resolves every one of
  them, used or not, on every page and twice per page), and each font's `/ToUnicode` stream
  size in bytes.
- `report`: turn the run rows, the facts and the `perf` artifacts (folded stacks from
  `scripts/perf-collapse.py`, flat self-time from `perf report`) into the note's tables.

Nothing here builds or runs Rust: the workflow builds the release binary with line tables and
frame pointers exactly as `cpu-profile.yml` does, and this script only drives it over stdio.

The lopdf call counts are a model of the code, not a counter: `extract_page_texts_raw`
(`src/parsing/pdf.rs:211`) calls `extract_text_chunks_with_limit` and `extract_text_with_limit`
per page, and each of those resolves the encoding of every font in the page's resource
dictionary (`lopdf-0.44.0/src/parser_aux.rs:115-130`), which parses that font's `/ToUnicode`
CMap (`object.rs:589-596`) with no cache anywhere. `facts` measures the inputs of that model
and `report` prints the resulting per-page parse count next to the profile's own share for the
CMap parser symbol.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import sys
from collections import Counter, defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
MIB = 1024 * 1024


def load_sibling(name: str):
    path = os.path.join(HERE, name)
    spec = importlib.util.spec_from_file_location(name.replace("-", "_").replace(".py", ""), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


MEMORY = load_sibling("memory-profile.py")
COLLAPSE = load_sibling("perf-collapse.py")
CPU_REPORT = load_sibling("cpu-profile-report.py")

PHASES = (
    ("retrieve", "Retrieve"),
    ("parse", "Parse"),
    ("repository_save", "Save doc"),
    ("text_unit_sets_write", "Unit sets + streams"),
    ("index", "FTS index"),
)

# ---------------------------------------------------------------------------------------------
# Buckets: the same exclusive/inclusive idea as scripts/cpu-profile-report.py, with PDF work.
# ---------------------------------------------------------------------------------------------

ALLOCATION = "Allocation (malloc/free/realloc/memcpy)"
OTHER = "Other"

BUCKETS: list[tuple[str, tuple[str, ...]]] = [
    (
        "lopdf: /ToUnicode CMap parse",
        (
            "tounicodecmap",
            "cmap_parser",
            "cmap_section",
            "get_to_unicode_encoding",
            "get_font_encoding_inner",
            "get_font_encoding_with_limit",
            "get_base_encoding",
        ),
    ),
    (
        "lopdf: page content decode and text walk",
        (
            "parser_aux",
            "collect_text",
            "content::decode",
            "get_page_content",
            "get_page_fonts",
            "get_page_resources",
            "extract_text_chunks_from_page",
            "extract_text_with_limit",
            "extract_text_chunks_with_limit",
        ),
    ),
    (
        "lopdf: document load, xref, stream decode",
        (
            "lopdf::reader",
            "decode_xref",
            "xref",
            "load_mem",
            "decompressed_content",
            "get_plain_content",
            "stream::",
            "flate2",
            "inflate",
            "miniz",
        ),
    ),
    (
        "pdf.rs extraction loop and structure",
        (
            "parsing::pdf",
            "normalize_pdf_text",
            "infer_numbered_headings",
            "build_page_sections",
            "build_toc_sections",
            "source_fragments",
            "parse_numbered_heading",
        ),
    ),
    (
        "pdf_text_repair pre-pass (PDF-1c)",
        ("pdf_text_repair", "prepare_text_extraction", "inline_page_text_xobjects"),
    ),
    (
        "normalization, hashing, segmentation",
        (
            "normaliz",
            "unicode_normalization",
            "sha2",
            "sha256",
            "digest::",
            "segment",
            "sentence_ranges",
            "paragraph_ranges",
            "is_terminal_mark",
            "collapse_whitespace",
        ),
    ),
    (
        "SQLite (save, section text, metadata)",
        ("sqlite", "rusqlite", "vdbe", "btree", "pager", "unixwrite", "stored_document"),
    ),
    (
        "FTS index",
        ("fts", "tokeniz", "search::", "tantivy", "trigram"),
    ),
    (
        "serde_json",
        ("serde_json", "serde_core::ser"),
    ),
    (
        ALLOCATION,
        (
            "malloc",
            "free",
            "calloc",
            "realloc",
            "memcpy",
            "memmove",
            "memset",
            "__rust_alloc",
            "__rust_dealloc",
            "__rust_realloc",
            "tcache",
        ),
    ),
]

# The top-level functions a stack is attributed to, deepest first. These are the frames that
# answer "which part of the open is this sample in"; the report shows the deepest one present.
MILESTONES: tuple[tuple[str, str], ...] = (
    ("pdf.rs parse_sync", "PdfParser::parse_sync"),
    ("pdf.rs extract_page_texts_raw", "extract_page_texts_raw"),
    ("pdf.rs prepare_text_extraction", "prepare_text_extraction"),
    ("lopdf ToUnicodeCMap::parse", "ToUnicodeCMap::parse"),
    ("lopdf parser::cmap_parser::parse", "cmap_parser::parse"),
    ("lopdf ToUnicodeCMap::from_sections", "from_sections"),
    ("lopdf get_font_encoding_inner", "get_font_encoding_inner"),
    ("lopdf get_page_fonts", "get_page_fonts"),
    ("lopdf extract_text_chunks_from_page", "extract_text_chunks_from_page"),
    ("lopdf Content::decode", "content::decode"),
    ("lopdf get_page_content_with_limit", "get_page_content_with_limit"),
    ("lopdf decompressed_content", "decompressed_content"),
    ("lopdf Document::load_mem", "load_mem"),
)


def base_name(frame: str) -> str:
    return CPU_REPORT.base_name(frame)


def deepest_match(frames: list[str], patterns: tuple[str, ...]) -> int | None:
    for index in range(len(frames) - 1, -1, -1):
        frame = base_name(frames[index]).lower()
        if any(pattern in frame for pattern in patterns):
            return index
    return None


def classify(frames: list[str]) -> tuple[str, set[str]]:
    matched: dict[str, int] = {}
    present: set[str] = set()
    for name, patterns in BUCKETS:
        index = deepest_match(frames, patterns)
        if index is None:
            continue
        matched[name] = index
        present.add(name)
    if not matched:
        return OTHER, present
    order = [bucket for bucket, _ in BUCKETS]
    exclusive = max(matched, key=lambda name: (matched[name], -order.index(name)))
    return exclusive, present


def milestone(frames: list[str]) -> str:
    """The deepest known top-level frame of the stack, or `-`."""
    best: tuple[int, str] | None = None
    for index in range(len(frames) - 1, -1, -1):
        frame = frames[index]
        for label, needle in MILESTONES:
            if needle in frame:
                if best is None or index > best[0]:
                    best = (index, label)
                break
        if best is not None:
            return best[1]
    return "-"


# ---------------------------------------------------------------------------------------------
# run: drive the server over stdio, exactly as memory-profile.py does
# ---------------------------------------------------------------------------------------------

def parse_inputs(values: list[str]) -> list[dict]:
    inputs = []
    for value in values:
        label, _, path = value.partition("=")
        if not path:
            raise SystemExit(f"--input wants label=path, got {value!r}")
        inputs.append({"label": label, "path": os.path.abspath(path)})
    return inputs


def run_mode(args: argparse.Namespace) -> int:
    os.makedirs(args.out_dir, exist_ok=True)
    rows: list[dict] = []
    for entry in parse_inputs(args.input):
        label = entry["label"]
        state_dir = os.path.join(args.state_dir, label)
        os.makedirs(state_dir, exist_ok=True)
        telemetry = os.path.join(args.out_dir, f"{label}.telemetry.jsonl")
        env = MEMORY.server_environment([os.path.dirname(entry["path"])], state_dir, None,
                                       telemetry=True)
        wrapper = None
        if args.perf and label in args.perf_label:
            data = os.path.join(args.out_dir, f"{label}.perf.data")
            wrapper = (f"{args.perf} record -F 999 -g --call-graph fp -o {data} --")
        command = MEMORY.server_command(args.binary, wrapper, label)
        samples: dict = {}
        peak, seconds, error, timed_out, sections, title, *_ = MEMORY.run_one(
            command, env, entry["path"], args.timeout, stderr_path=telemetry, samples=samples)
        events = MEMORY.read_telemetry_events(telemetry)
        sums = MEMORY.telemetry_sums(events, success_only=True)
        phases = {event: sums.get(event, (0, 0.0))[1] / 1000.0 for event, _ in PHASES}
        row = {
            "label": label,
            "path": entry["path"],
            "bytes": os.path.getsize(entry["path"]),
            "open_seconds": round(seconds, 3) if seconds is not None else None,
            "peak_rss_kib": peak,
            "ok": error is None and not timed_out,
            "error": error,
            "sections": sections,
            "title": title,
            "phases": phases,
            "phase_counts": {event: sums.get(event, (0, 0))[0] for event, _ in PHASES},
            "phase_rss": MEMORY.telemetry_rss_phases(events),
            "perf": label in args.perf_label,
            "event_count": len(events),
            # The memory the open leaves resident: sampled from /proc while the server is alive,
            # which `wait4`'s peak cannot show.
            "rss_after_open_kib": samples.get("rss_after_open_kib"),
            "rss_settled_kib": samples.get("rss_settled_kib"),
        }
        rows.append(row)
        print(f"| {label} | {row['open_seconds']} s | {row['peak_rss_kib'] // 1024} MiB | "
              f"{(row['rss_after_open_kib'] or 0) // 1024} MiB resident | "
              f"{row['sections']} sections | {error or 'ok'} |", flush=True)
        with open(args.out, "w", encoding="utf-8") as handle:
            json.dump(rows, handle, indent=1, sort_keys=True)
    return 0


# ---------------------------------------------------------------------------------------------
# facts: page count, fonts per page resources, /ToUnicode sizes (PyMuPDF)
# ---------------------------------------------------------------------------------------------

SHOW_TOKEN = re.compile(
    rb"/[^\s/<>\[\](){}]+\s+[-\d.]+\s+Tf|\[(.*?)\]\s*TJ|"
    rb"(\((?:\\.|[^\\()])*\)|<[0-9A-Fa-f\s]*>)\s*(?:Tj|'|\")", re.S)
STRING_TOKEN = re.compile(rb"<[0-9A-Fa-f\s]*>|\((?:\\.|[^\\()])*\)", re.S)


def shown_strings(data: bytes) -> int:
    """How many strings a content stream shows (one `decode_text_object` call each)."""
    total = 0
    for match in SHOW_TOKEN.finditer(data):
        if match.group(1) is not None:
            total += len(STRING_TOKEN.findall(match.group(1)))
        else:
            total += 1
    return total


def facts_mode(args: argparse.Namespace) -> int:
    import pymupdf

    facts: dict[str, dict] = {}
    for entry in parse_inputs(args.input):
        document = pymupdf.open(entry["path"])
        try:
            per_page = []
            shown = []
            fonts: dict[int, dict] = {}
            for page in document:
                count = 0
                for content in page.get_contents():
                    try:
                        count += shown_strings(document.xref_stream(content))
                    except Exception:  # noqa: BLE001 - an unreadable stream shows no text
                        continue
                shown.append(count)
                names = {}
                for item in page.get_fonts(full=True):
                    xref, _ext, kind, basefont, resource, encoding = item[:6]
                    names[resource] = xref
                    record = fonts.setdefault(xref, {
                        "xref": xref, "basefont": basefont, "type": kind,
                        "encoding": encoding, "pages": 0,
                    })
                    record["pages"] += 1
                per_page.append(len(names))
            for record in fonts.values():
                kind, value = document.xref_get_key(record["xref"], "ToUnicode")
                record["to_unicode"] = kind == "xref"
                record["to_unicode_bytes"] = 0
                if kind == "xref":
                    stream = document.xref_stream(int(value.split()[0]))
                    record["to_unicode_bytes"] = len(stream)
            facts[entry["label"]] = {
                "pages": document.page_count,
                "toc_entries": len(document.get_toc()),
                "fonts_per_page": per_page,
                "fonts_in_resources": sum(per_page),
                "strings_shown_per_page": shown,
                "strings_shown": sum(shown),
                "fonts": sorted(fonts.values(), key=lambda item: -item["to_unicode_bytes"]),
            }
        finally:
            document.close()
        fact = facts[entry["label"]]
        print(f"| {entry['label']} | {fact['pages']} pages | {fact['fonts_in_resources']} font "
              f"entries | {len(fact['fonts'])} distinct fonts | "
              f"{fact['strings_shown']} strings shown |", flush=True)
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump(facts, handle, indent=1, sort_keys=True)
    return 0


# ---------------------------------------------------------------------------------------------
# report
# ---------------------------------------------------------------------------------------------

def load_json(path: str):
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)


def phase_table(rows: list[dict]) -> str:
    lines = [
        "| Input | Pages | Open s | Retrieve s | Parse s | Save doc s | Unit sets + streams s "
        "| FTS index s | Sum of phases s | Unaccounted s |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for row in rows:
        phases = row.get("phases") or {}
        total = sum(phases.get(event, 0.0) for event, _ in PHASES)
        open_s = row.get("open_seconds") or 0.0
        pages = row.get("pages", "-")
        lines.append(
            "| {label} | {pages} | {open_s:.2f} | {retrieve:.3f} | {parse:.3f} | {save:.3f} "
            "| {units:.3f} | {index:.3f} | {total:.3f} | {rest:.3f} |".format(
                label=row["label"], pages=pages, open_s=open_s,
                retrieve=phases.get("retrieve", 0.0),
                parse=phases.get("parse", 0.0),
                save=phases.get("repository_save", 0.0),
                units=phases.get("text_unit_sets_write", 0.0),
                index=phases.get("index", 0.0),
                total=total, rest=max(0.0, open_s - total)))
    return "\n".join(lines)


def scaling_table(rows: list[dict]) -> str:
    lines = [
        "| Input | Pages | Open s | s/page | vs 100 pages (per page) |",
        "| --- | ---: | ---: | ---: | ---: |",
    ]
    base: dict[str, float] = {}
    for row in rows:
        pages = row.get("pages")
        open_s = row.get("open_seconds")
        if not pages or not open_s or not row["label"].startswith("text-long-"):
            continue
        key = row["label"].rsplit("-", 1)[0]
        per_page = open_s / pages
        if pages == 100:
            base[key] = per_page
        ratio = per_page / base[key] if key in base else None
        lines.append(f"| {row['label']} | {pages} | {open_s:.2f} | {per_page * 1000:.1f} ms | "
                     f"{'-' if ratio is None else f'{ratio:.2f}x'} |")
    return "\n".join(lines)


def rss_table(rows: list[dict]) -> str:
    lines = [
        "| Input | Peak RSS MiB | after retrieve | after parse | after save | after unit sets "
        "| after index | peak VmHWM MiB |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for row in rows:
        rss = row.get("phase_rss") or {}
        cells = []
        for event, _ in PHASES:
            value = rss.get(event)
            cells.append("-" if not value else f"{value[0] / 1024:.1f}")
        hwm = max((value[1] for value in rss.values()), default=0)
        lines.append(f"| {row['label']} | {row['peak_rss_kib'] / 1024:.1f} | "
                     + " | ".join(cells) + f" | {hwm / 1024:.1f} |")
    return "\n".join(lines)


def read_folded(path: str) -> tuple[Counter, Counter]:
    return CPU_REPORT.read_folded(path)


def top_tables(label: str, folded: str, self_flat: str, wall: float, top: int) -> str:
    stacks, _leaves = read_folded(folded)
    total = sum(stacks.values())
    if total == 0:
        return f"### {label}\n\nno samples in {folded}\n"

    inclusive: Counter = Counter()
    exclusive: Counter = Counter()
    owner_votes: dict[str, Counter] = defaultdict(Counter)
    for stack, weight in stacks.items():
        frames = stack.split(";")
        bucket, present = classify(frames)
        exclusive[bucket] += weight
        for name in present:
            inclusive[name] += weight
        path = milestone(frames)
        leaf = frames[-1]
        owner_votes[leaf][path] += weight

    lines = [f"### {label}: {wall:.2f} s wall, {total} samples ({total / wall:.0f}/s)", ""]
    lines.append("| Bucket | ms | % of open | anywhere in stack (inclusive) |")
    lines.append("| --- | ---: | ---: | ---: |")
    for name, _ in BUCKETS:
        share = 100.0 * exclusive.get(name, 0) / total
        inside = 100.0 * inclusive.get(name, 0) / total
        lines.append(f"| {name} | {share / 100.0 * wall * 1000.0:.0f} | {share:.1f} | "
                     f"{inside:.1f} |")
    share = 100.0 * exclusive.get(OTHER, 0) / total
    lines.append(f"| {OTHER} | {share / 100.0 * wall * 1000.0:.0f} | {share:.1f} | "
                 f"{100.0 * inclusive.get(OTHER, 0) / total:.1f} |")
    lines.append("")
    lines.append("`ms` and `% of open` are a partition: each sample is charged to the deepest "
                 "frame that belongs to a bucket. `anywhere in stack` counts a sample once per "
                 "bucket present and sums over 100%.")
    lines.append("")

    # Inclusive per symbol, from the folded stacks.
    symbol_inclusive: Counter = Counter()
    symbol_owner: dict[str, Counter] = defaultdict(Counter)
    for stack, weight in stacks.items():
        frames = stack.split(";")
        path = milestone(frames)
        for name in set(frames):
            symbol_inclusive[name] += weight
            symbol_owner[name][path] += weight
    lines.append(f"Top {top} inclusive symbols (a sample counts for every frame in its stack):")
    lines.append("")
    lines.append("| # | Symbol | Inclusive % | ms | deepest top-level frame |")
    lines.append("| ---: | --- | ---: | ---: | --- |")
    for rank, (symbol, weight) in enumerate(symbol_inclusive.most_common(top), start=1):
        path = symbol_owner[symbol].most_common(1)[0][0]
        lines.append(f"| {rank} | `{short(symbol)}` | {100.0 * weight / total:.2f} | "
                     f"{weight / total * wall * 1000.0:.0f} | {path} |")
    lines.append("")

    lines.append(f"Top {top} self-time symbols:")
    lines.append("")
    lines.append("| # | Symbol | Self % | ms | deepest top-level frame |")
    lines.append("| ---: | --- | ---: | ---: | --- |")
    rows = CPU_REPORT.read_flat_self(self_flat) if os.path.exists(self_flat) else []
    if rows:
        for rank, (symbol, _dso, share) in enumerate(rows[:top], start=1):
            path = owner_votes.get(symbol, Counter()).most_common(1)
            lines.append(f"| {rank} | `{short(symbol)}` | {share:.2f} | "
                         f"{share / 100.0 * wall * 1000.0:.0f} | "
                         f"{path[0][0] if path else '-'} |")
    else:
        leaves = Counter()
        for stack, weight in stacks.items():
            leaves[stack.rsplit(";", 1)[-1]] += weight
        for rank, (symbol, weight) in enumerate(leaves.most_common(top), start=1):
            share = 100.0 * weight / total
            path = owner_votes.get(symbol, Counter()).most_common(1)
            lines.append(f"| {rank} | `{short(symbol)}` | {share:.2f} | "
                         f"{share / 100.0 * wall * 1000.0:.0f} | "
                         f"{path[0][0] if path else '-'} |")
    lines.append("")
    return "\n".join(lines)


def short(symbol: str, limit: int = 96) -> str:
    text = symbol
    for prefix in ("reading_mcp::", "lopdf::", "core::", "alloc::", "std::"):
        text = text.replace(prefix, "")
    return text if len(text) <= limit else text[: limit - 1] + "…"


def lopdf_table(rows: list[dict], facts: dict[str, dict], second_pass: set[str]) -> str:
    lines = [
        "| Input | Pages | Fonts in page `/Resources` (sum) | Strings shown per page | "
        "`pdf.rs` parses per page | `pdf_layout` second pass | `/ToUnicode` parses per page "
        "(model) | Total parses | `/ToUnicode` bytes (biggest) | CMap bytes parsed per page |",
        "| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: |",
    ]
    for row in rows:
        fact = facts.get(row["label"])
        if not fact:
            continue
        pages = fact["pages"] or 1
        first = 2 * fact["fonts_in_resources"]
        strings = fact.get("strings_shown", 0)
        sections = row.get("sections")
        if row["label"] in second_pass:
            second, label = True, "runs (in the profile)"
        elif fact.get("toc_entries"):
            second, label = False, "does not run (native TOC)"
        elif isinstance(sections, int) and sections == pages:
            second, label = False, "does not run (page fallback)"
        else:
            second, label = True, "runs (inferred headings, no /Outlines)"
        parses = first + (strings if second else 0)
        to_unicode = [font for font in fact["fonts"] if font["to_unicode"]]
        biggest = max((font["to_unicode_bytes"] for font in to_unicode), default=0)
        lines.append(
            f"| {row['label']} | {pages} | {fact['fonts_in_resources']} | "
            f"{strings / pages:.1f} | {first / pages:.2f} | {label} | "
            f"{parses / pages:.1f} | {parses} | "
            f"{biggest} | {biggest * parses / pages:.0f} |")
    lines.append("")
    lines.append("`pdf.rs parses per page` is 2 x the fonts in the page's `/Resources`: "
                 "`extract_page_texts_raw` (`src/parsing/pdf.rs:211-244`) calls "
                 "`extract_text_chunks_with_limit` and `extract_text_with_limit`, and each "
                 "resolves every font in the page's resource dictionary "
                 "(`lopdf-0.44.0/src/parser_aux.rs:115-130`). The `pdf_layout` pass is "
                 "`pdf_v8::PdfParser::parse_sync` (`src/parsing/pdf_v8.rs:29-51`): when the "
                 "structure provenance is `inferred_numbered_headings` it reopens the PDF and "
                 "calls `extract_text_fragment_evidence`, whose `decode_text_object` "
                 "(`src/parsing/pdf_layout.rs:418-438`) resolves the font encoding again for "
                 "**every string shown** - no cache. The second pass runs only on the "
                 "`inferred_numbered_headings` provenance, so it is claimed when a document has "
                 "no `/Outlines` (`toc_entries == 0` from PyMuPDF) and its section count differs "
                 "from its page count, and it is corroborated by `decode_text_object` in the "
                 "folded stacks where a profile exists. The profile's share for "
                 "`ToUnicodeCMap::parse` is in section 2. This is a model of the code, not a "
                 "counter.")
    return "\n".join(lines)


def cmap_symbol_share(label: str, folded: str) -> dict:
    stacks, _ = read_folded(folded)
    total = sum(stacks.values()) or 1
    hits = 0
    for stack, weight in stacks.items():
        lowered = stack.lower()
        if "tounicodecmap::parse" in lowered or "cmap_parser" in lowered:
            hits += weight
    return {"label": label, "share": 100.0 * hits / total, "samples": total}


def has_symbol(folded: str, needle: str) -> bool:
    if not os.path.exists(folded):
        return False
    with open(folded, encoding="utf-8", errors="replace") as handle:
        return any(needle in line for line in handle)


def report_mode(args: argparse.Namespace) -> int:
    rows = load_json(args.runs)
    facts = load_json(args.facts) if args.facts and os.path.exists(args.facts) else {}
    for row in rows:
        fact = facts.get(row["label"])
        if fact:
            row["pages"] = fact["pages"]
    out: list[str] = []
    out.append("## 1. Wall time by phase")
    out.append("")
    out.append("Telemetry events (`READING_MCP_TELEMETRY=1`) of one `open_document` per input, "
               "`READING_MCP_TOOLS=v1`, OCR off, default PDF backend.")
    out.append("")
    out.append(phase_table(rows))
    out.append("")

    if args.perf_dir and os.path.isdir(args.perf_dir):
        out.append("## 2. CPU profile")
        out.append("")
        out.append("Release build with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` and "
                   "`-C force-frame-pointers=yes`, `perf record -F 999 -g --call-graph fp`.")
        out.append("")
        shares = []
        for label in args.profile_label:
            folded = os.path.join(args.perf_dir, f"{label}.folded")
            if not os.path.exists(folded):
                continue
            wall = next((row["open_seconds"] for row in rows
                         if row["label"] == label and row["open_seconds"]), 0.0)
            out.append(top_tables(label, folded, os.path.join(args.perf_dir,
                                                              f"{label}.self-flat.txt"),
                                  wall, args.top))
            shares.append(cmap_symbol_share(label, folded))
        if shares:
            out.append("Samples with the CMap parser on the stack: "
                       + ", ".join(f"`{item['label']}` {item['share']:.2f}% "
                                   f"({item['samples']} samples)" for item in shares) + ".")
            out.append("")

    out.append("## 3. Scaling")
    out.append("")
    out.append(scaling_table(rows))
    out.append("")

    second_pass = {label for label in args.profile_label
                   if has_symbol(os.path.join(args.perf_dir or "", f"{label}.folded"),
                                 "decode_text_object")}
    out.append("## 4. Per-page cost inside lopdf")
    out.append("")
    out.append(lopdf_table(rows, facts, second_pass))
    out.append("")

    out.append("## 5. Memory")
    out.append("")
    out.append("Peak RSS from `wait4` (`ru_maxrss`) and the `rss_kib`/`hwm_kib` the server's "
               "telemetry events carry after each phase (read from `/proc/self/status`).")
    out.append("")
    out.append(rss_table(rows))
    out.append("")

    text = "\n".join(out)
    print(text)
    if args.out:
        with open(args.out, "w", encoding="utf-8") as handle:
            handle.write(text + "\n")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--mode", choices=("run", "facts", "report"), required=True)
    parser.add_argument("--input", action="append", default=[],
                        help="label=path, repeatable")
    parser.add_argument("--binary", default="")
    parser.add_argument("--out", default="")
    parser.add_argument("--out-dir", default="")
    parser.add_argument("--state-dir", default="")
    parser.add_argument("--timeout", type=float, default=1800.0)
    parser.add_argument("--perf", default=None, help="perf binary; wraps the labelled runs")
    parser.add_argument("--perf-label", action="append", default=[],
                        help="label to wrap with perf, repeatable")
    parser.add_argument("--runs", default="")
    parser.add_argument("--facts", default="")
    parser.add_argument("--perf-dir", default="")
    parser.add_argument("--profile-label", action="append", default=[])
    parser.add_argument("--top", type=int, default=25)
    args = parser.parse_args()
    if args.mode == "run":
        if not args.binary or not args.input or not args.out:
            parser.error("--mode run needs --binary, --input and --out")
        args.out_dir = args.out_dir or os.path.dirname(os.path.abspath(args.out))
        args.state_dir = args.state_dir or os.path.join(args.out_dir, "state")
        return run_mode(args)
    if args.mode == "facts":
        if not args.input or not args.out:
            parser.error("--mode facts needs --input and --out")
        return facts_mode(args)
    if not args.runs:
        parser.error("--mode report needs --runs")
    return report_mode(args)


if __name__ == "__main__":
    sys.exit(main())
