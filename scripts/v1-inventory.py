#!/usr/bin/env python3
"""Inventory every name of a v1 tool across src/, tests/, scripts/ and .github/.

Companion to docs/design/v2-4b-delete-v1.md. Re-run it any time the tree moves:

    python3 scripts/v1-inventory.py            # markdown report on stdout
    python3 scripts/v1-inventory.py --json     # machine-readable report

"Call count" is the number of lines that mention a v1 tool name as a whole
word (quoted tool calls, env values and Rust identifiers all count); it is an
upper bound on true invocations, which is what an inventory for deletion wants.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# The nine tools the v1 router registers (src/mcp/server.rs, tool_router_v1).
V1_TOOLS = [
    "list_documents",
    "list_directory",
    "open_document",
    "get_document_structure",
    "get_text_units",
    "search_document",
    "read_document",
    "get_source_view",
    "get_context",
]

SCAN_DIRS = ["src", "tests", "scripts", ".github"]

# src/ files that exist only to serve v1 tools or the READING_MCP_TOOLS switch,
# with the deletion note the design doc repeats. Line counts are measured, not
# hard-coded.
V1_ONLY_SRC = {
    "src/mcp/contracts.rs": "v1 request/response DTOs (OpenDocumentRequest .. GetContextResponse)",
    "src/application/get_document_structure.rs": "v1 structure use case + structure cursor",
    "src/application/get_context.rs": "v1 get_context use case",
    "src/application/get_text_units.rs": "v1 get_text_units use case (shared internals noted below)",
    "src/application/read_document.rs": "v1 read_document use case",
    "src/application/search_document.rs": "v1 search_document use case (shared internals noted below)",
    "src/application/read_cursor.rs": "v1 read continuation cursor",
    "src/application/text_unit_cursor.rs": "v1 get_text_units cursor",
    "src/application/structure_cursor.rs": "v1 get_document_structure cursor",
    "src/mcp/ocr_error_tests.rs": "in-crate tests bound to ToolSet::V1 + open_document",
}

# src/ files v1 calls into but v2 also needs; deleting v1 must keep them.
SHARED_SRC = {
    "src/application/open_document.rs": "v2 `open` delegates to OpenDocumentUseCase (open_reading.rs)",
    "src/application/text_unit_stream.rs": "v2 read/cite use whole_paragraph_kind/coarse_degradation",
    "src/application/get_text_units.rs": "kind/policy enums used by read_reading, sqlite, observability",
    "src/application/search_document.rs": "v2 `search` wraps the v1 search use case (search_reading.rs)",
    "src/application/list_documents.rs": "v2 `browse` lists documents",
    "src/application/list_directories.rs": "v2 `browse` lists directories",
    "src/application/source_view.rs": "v2 `source_view` shares the renderer",
    "src/application/locator_resolution.rs": "v2 cite/read resolve the same locators",
    "src/application/section_rows.rs": "shared section-tree rows for outline and structure",
    "src/mcp/profile_mapping.rs": "reading-profile mapping used by both routers",
}

WORD = {name: re.compile(r"\b" + re.escape(name) + r"\b") for name in V1_TOOLS}


def iter_files():
    for top in SCAN_DIRS:
        base = ROOT / top
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*")):
            if path.is_file() and not path.name.startswith(".") and "__pycache__" not in path.parts:
                yield path


def scan():
    """Return {path: {tool: line_count}} for every file that names a v1 tool."""
    hits: dict[str, dict[str, int]] = {}
    for path in iter_files():
        rel = path.relative_to(ROOT).as_posix()
        if rel == "scripts/v1-inventory.py":
            continue
        try:
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            continue
        counts = {name: 0 for name in V1_TOOLS}
        for line in lines:
            for name, word in WORD.items():
                counts[name] += len(word.findall(line))
        counts = {k: v for k, v in counts.items() if v}
        if counts:
            hits[rel] = counts
    return hits


def line_count(rel: str) -> int:
    try:
        return len((ROOT / rel).read_text(encoding="utf-8", errors="replace").splitlines())
    except OSError:
        return -1


def report(hits):
    by_dir: dict[str, list[str]] = {}
    for rel in hits:
        by_dir.setdefault(rel.split("/")[0], []).append(rel)
    totals = {name: sum(files.get(name, 0) for files in hits.values()) for name in V1_TOOLS}
    out = {
        "tools": {name: totals[name] for name in V1_TOOLS},
        "files_by_dir": {d: sorted(v) for d, v in sorted(by_dir.items())},
        "per_file": hits,
        "v1_only_src": {p: line_count(p) for p in V1_ONLY_SRC if (ROOT / p).exists()},
        "shared_src": {p: line_count(p) for p in SHARED_SRC if (ROOT / p).exists()},
    }
    return out


def print_markdown(rep):
    print("## v1 tool mention counts (lines containing the whole-word name)\n")
    print("| v1 tool | files | mentions |")
    print("|---|---:|---:|")
    for name, total in rep["tools"].items():
        files = sum(1 for f in rep["per_file"].values() if name in f)
        print(f"| `{name}` | {files} | {total} |")
    print()
    for d, files in rep["files_by_dir"].items():
        print(f"### {d}/ ({len(files)} files)\n")
        print("| file | " + " | ".join(f"`{t}`" for t in V1_TOOLS) + " |")
        print("|---|" + "---:|" * len(V1_TOOLS))
        for rel in files:
            row = rep["per_file"][rel]
            print(f"| `{rel}` | " + " | ".join(str(row.get(t, "")) for t in V1_TOOLS) + " |")
        print()
    print("### v1-only src/ files (delete with the switch)\n")
    print("| file | lines | note |")
    print("|---|---:|---|")
    for rel, n in rep["v1_only_src"].items():
        print(f"| `{rel}` | {n} | {V1_ONLY_SRC[rel]} |")
    print()
    print("### src/ files v2 shares — must keep\n")
    print("| file | lines | why v2 keeps it |")
    print("|---|---:|---|")
    for rel, n in rep["shared_src"].items():
        print(f"| `{rel}` | {n} | {SHARED_SRC[rel]} |")
    print()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true", help="emit JSON instead of markdown")
    args = parser.parse_args()
    rep = report(scan())
    if args.json:
        json.dump(rep, sys.stdout, indent=2)
        print()
    else:
        print_markdown(rep)


if __name__ == "__main__":
    main()
