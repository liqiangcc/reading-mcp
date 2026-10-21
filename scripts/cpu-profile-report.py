#!/usr/bin/env python3
"""Turn one folded perf profile into the R5a open/read bucket tables.

Reads the folded stacks that `scripts/perf-collapse.py` writes (root;...;leaf <count>) plus the
open's measured wall time, and prints markdown:

- where the time goes, grouped into the R5a buckets, as inclusive % of samples (a sample counts
  for every bucket that appears anywhere in its stack), the same share as milliseconds of the
  measured wall time, and an exclusive % where each sample is charged to exactly one bucket
  (the deepest frame that belongs to a bucket, so a `malloc` leaf is charged to allocation);
- the top self-time symbols, which are just the leaf frames of the same stacks.

Standard library only. This script never runs perf; give it the folded file.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter

# Bucket order is the order the R5a task lists them in. Matching is a case-insensitive substring
# test on the demangled symbol, so Rust frames look like
# `reading_mcp::domain::segmentation::sentence_ranges::h1a2b3c`.
ALLOCATION = "Allocation (malloc/free/realloc/memcpy)"
OTHER = "Other"

BUCKETS: list[tuple[str, tuple[str, ...]]] = [
    (
        "EPUB/ZIP read and inflate",
        (
            "miniz",
            "flate2",
            "inflate",
            "zlib_rs",
            "zip::",
            "zip_",
            "ziparchive",
            "crc32",
            "deflate",
            "read_zip",
            "parsing::archive",
            "retrieval::file",
        ),
    ),
    (
        "XHTML/HTML parse",
        (
            "html5ever",
            "markup5ever",
            "xml5ever",
            "scraper",
            "selectors",
            "tendril",
            "parsing::html",
            "parse_html",
            "decode_html",
            "content_root",
            "remove_noise",
            "collect_content",
            "build_html_sections",
            "render_blocks",
            "normalized_element_text",
            "capture_html_metadata",
        ),
    ),
    (
        # The EPUB container/spine reconciliation only: not the whole `parsing::epub` module,
        # which is the parent frame of the ZIP read and the XHTML parse as well.
        "EPUB structure validation and reconciliation",
        (
            "epub_validator",
            "validate_epub",
            "epub_document_inner",
            "attach_epub_validation",
            "epub_structure",
            "epub_navigation",
            "reconcil",
            "remap_epub",
            "append_remapped_blocks",
            "roxmltree",
        ),
    ),
    (
        "Text normalization and normalized block map",
        (
            "normaliz",
            "unicode_normalization",
            "collapse_whitespace",
            "parsing::text::",
            "normalized_text",
            "normalized_block",
        ),
    ),
    (
        "Sentence/paragraph segmentation",
        (
            "segment",
            "sentence_ranges",
            "sentence_boundary",
            "paragraph_ranges",
            "paragraph_candidates",
            "paragraph_content_class",
            "skip_whitespace",
            "trim_trailing_whitespace",
            "extend_terminal_cluster",
            "is_terminal_mark",
            "looks_like_unreliable",
            "missing_space_glue",
            "bibliographic",
            "doubled_character",
        ),
    ),
    (
        "Reading profile and reliability inspection",
        (
            "reading_profile",
            "reliability",
        ),
    ),
    (
        "SHA-256 / hashing",
        (
            "sha2",
            "sha256",
            "digest::",
            "block_buffer",
        ),
    ),
    (
        "document_json serialize",
        (
            "serde_json",
            "serde_core::ser",
            "storeddocument",
            "document_json",
        ),
    ),
    (
        "SQLite writes",
        (
            "sqlite",
            "rusqlite",
            "vdbe",
            "btree",
            "pager",
            "unixwrite",
            "query_row",
            "load_text_slice",
            "insert_section_text",
            "section_layout",
            "stored_document",
            "load_stored_document",
        ),
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
            "operator new",
            "operator delete",
        ),
    ),
]

SQLITE_MARKERS = ("sqlite", "rusqlite", "vdbe", "unixwrite")


def base_name(frame: str) -> str:
    """The frame without generic arguments, so `and_then<&NormalizedBlockMap, ...>` is `and_then`.

    `perf script` prints inlined frames by their DWARF name, which for a Rust generic adapter is
    the bare function name followed by its type arguments. Matching the arguments would charge a
    `core` adapter to whatever module happens to be its type parameter, so drop them, except in
    `<Type as Trait>::method` names where the type is the subject of the frame.
    """
    if frame.startswith("<"):
        return frame
    out: list[str] = []
    depth = 0
    for character in frame:
        if character == "<":
            depth += 1
            continue
        if character == ">":
            depth = max(0, depth - 1)
            continue
        if depth == 0:
            out.append(character)
    return "".join(out)


def deepest_match(frames: list[str], patterns: tuple[str, ...]) -> int | None:
    """Index of the deepest (leaf-most) frame matching any pattern, or None."""
    for index in range(len(frames) - 1, -1, -1):
        frame = base_name(frames[index]).lower()
        if any(pattern in frame for pattern in patterns):
            return index
    return None


def classify(frames: list[str]) -> tuple[str, set[str]]:
    """Return (exclusive bucket, set of buckets present anywhere in the stack)."""
    matched: dict[str, int] = {}
    present: set[str] = set()
    for name, patterns in BUCKETS:
        index = deepest_match(frames, patterns)
        if index is None:
            continue
        if name == "document_json serialize":
            # serde_json inside SQLite is the section-layout blob, not the stored document JSON.
            sqlite_ancestor = deepest_match(frames, SQLITE_MARKERS)
            if sqlite_ancestor is not None and sqlite_ancestor < index:
                present.add("SQLite writes")
                continue
        matched[name] = index
        present.add(name)
    if not matched:
        return OTHER, present
    # Deepest frame wins; ties keep the bucket order above.
    exclusive = max(matched, key=lambda name: (matched[name], -[b for b, _ in BUCKETS].index(name)))
    return exclusive, present


def read_folded(path: str) -> tuple[Counter, Counter]:
    stacks: Counter = Counter()
    leaves: Counter = Counter()
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            stack, _, count = line.rpartition(" ")
            if not stack:
                continue
            try:
                weight = int(count)
            except ValueError:
                continue
            stacks[stack] += weight
            leaves[stack.rsplit(";", 1)[-1]] += weight
    return stacks, leaves


FLAT_ROW = re.compile(r"^(\d+\.\d+)%\s+(\S+)\s+\[([.k])\]\s+(.+)$")


def read_flat_self(path: str) -> list[tuple[str, str, float]]:
    """Rows of `perf report --no-children --sort dso,symbol --no-call-graph`: (symbol, dso, %)."""
    rows: list[tuple[str, str, float]] = []
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            match = FLAT_ROW.match(line.strip())
            if match is None:
                continue
            overhead, dso, kind, symbol = match.groups()
            # The symbol column is padded with spaces before the IPC columns.
            symbol = re.split(r"\s{2,}", symbol)[0]
            if kind == "k":
                dso = "[kernel.kallsyms]"
            rows.append((symbol.strip(), dso, float(overhead)))
    return rows


def wall_seconds(args: argparse.Namespace) -> float:
    if args.wall_seconds is not None:
        return args.wall_seconds
    if args.wall_from_json:
        with open(args.wall_from_json, encoding="utf-8") as handle:
            rows = json.load(handle)
        if isinstance(rows, dict):
            if rows.get("total_s"):
                return float(rows["total_s"])
            raise SystemExit(f"no total_s in {args.wall_from_json}")
        label = args.row_label or args.label
        for row in rows:
            if row.get("label") == label and row.get("open_seconds"):
                return float(row["open_seconds"])
        raise SystemExit(f"no open_seconds for label {label!r} in {args.wall_from_json}")
    raise SystemExit("give --wall-seconds or --wall-from-json")


def render(args: argparse.Namespace) -> str:
    stacks, leaves = read_folded(args.folded)
    total = sum(stacks.values())
    if total == 0:
        raise SystemExit(f"no samples in {args.folded}")
    wall = wall_seconds(args)

    inclusive: Counter = Counter()
    exclusive: Counter = Counter()
    for stack, weight in stacks.items():
        bucket, present = classify(stack.split(";"))
        exclusive[bucket] += weight
        for name in present:
            inclusive[name] += weight

    unknown = sum(weight for symbol, weight in leaves.items() if symbol.startswith("[unknown]"))
    lines = [
        f"### {args.label}: {args.heading or 'first open'}, {wall:.2f} s wall, "
        f"{total} samples ({total / wall:.0f}/s)",
        "",
        "| Bucket | ms | % of open | Inside this phase (inclusive) |",
        "| --- | ---: | ---: | ---: |",
    ]
    names = [name for name, _ in BUCKETS] + [OTHER]
    for name in names:
        share = 100.0 * exclusive.get(name, 0) / total
        inside = 100.0 * inclusive.get(name, 0) / total
        lines.append(
            f"| {name} | {share / 100.0 * wall * 1000.0:.0f} | {share:.1f} | {inside:.1f} |"
        )
    lines.append("")
    lines.append(
        f"`ms` and `% of open` are a partition: every sample is charged to the deepest frame that "
        f"belongs to a bucket, so they sum to the measured wall time. `Inside this phase` counts a "
        f"sample once per bucket in its stack, so it double counts and sums over 100%; it answers "
        f"how much of the open runs inside a phase, and it is inflated for wrapper calls such as "
        f"`epub_validator::validate_epub_document_inner`, which parses the book inside the "
        f"validation call. Unresolved leaf frames (`[unknown]@<module>`): "
        f"{100.0 * unknown / total:.1f}% of samples."
    )
    lines.append("")
    lines.append(f"Top {args.top} self-time symbols:")
    lines.append("")
    lines.append("| # | Symbol | Module | Self % | ms |")
    lines.append("| ---: | --- | --- | ---: | ---: |")
    if args.self_flat:
        # perf report's own symbolization: one row per map symbol, which is stable across runs.
        rows = read_flat_self(args.self_flat)
        for rank, (symbol, dso, share) in enumerate(rows[: args.top], start=1):
            label = symbol if not symbol.startswith("0x") else f"`{symbol}` (unresolved)"
            lines.append(
                f"| {rank} | {label} | {dso} | {share:.2f} | {share / 100.0 * wall * 1000.0:.0f} |"
            )
    else:
        for rank, (symbol, weight) in enumerate(leaves.most_common()[: args.top], start=1):
            share = 100.0 * weight / total
            lines.append(
                f"| {rank} | `{symbol}` | | {share:.2f} | {share / 100.0 * wall * 1000.0:.0f} |"
            )
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description="Bucket one folded perf profile for R5a.")
    parser.add_argument("--label", required=True, help="input label, e.g. zh-large")
    parser.add_argument("--heading", default=None, help="what was measured, e.g. 'first open'")
    parser.add_argument("--folded", required=True, help="folded stacks from perf-collapse.py")
    parser.add_argument(
        "--self-flat",
        default=None,
        help="`perf report --no-children --sort dso,symbol --no-call-graph` output for the top list",
    )
    parser.add_argument("--wall-seconds", type=float, default=None, help="measured wall time")
    parser.add_argument(
        "--wall-from-json",
        default=None,
        help="memory-profile result JSON to read the row's open_seconds from",
    )
    parser.add_argument("--row-label", default=None, help="row label in that JSON")
    parser.add_argument("--top", type=int, default=15)
    parser.add_argument("--md", default=None, help="also append the markdown to this file")
    args = parser.parse_args()

    text = render(args)
    print(text)
    if args.md:
        with open(args.md, "a", encoding="utf-8") as handle:
            handle.write(text + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
