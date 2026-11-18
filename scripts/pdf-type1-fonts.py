#!/usr/bin/env python3
"""PDF-6b: the population of PDFs that use Type 1 fonts with a built-in encoding and no /ToUnicode.

Measurement only: it downloads the committed corpus, asks PyMuPDF (the independent reference of
PDF-1a/1b) for every font's facts, and reports which documents the PDF-6b repair targets. It also
joins two `PDF wide sweep` result sets (a before and an after run) by document id, so the gate
table of a text-extraction change can be read from one artifact: outcome-class changes,
containment (median / p05 / minimum / count below 0.9), character ratio, peak RSS and open time.

The sweep's own aggregate is the authority for the whole-sample tables; this script exists for the
one thing it cannot say - which documents carry a Type 1 font with a built-in encoding and no
/ToUnicode - and for the per-document before/after join that the task gate asks for.

Usage (CI, PyMuPDF installed):

    python3 scripts/pdf-type1-fonts.py --ids scripts/pdf-sweep-ids.txt \\
        --work-dir "$RUNNER_TEMP/work" \\
        --before "$RUNNER_TEMP/before" --after "$RUNNER_TEMP/after" \\
        --out "$RUNNER_TEMP/type1/facts.jsonl" \\
        --markdown "$RUNNER_TEMP/type1/type1-fonts.md"

The corpus PDFs are not committed (the licence is CC-BY/CC-BY-SA/CC0 and the files are large), so
this script downloads them exactly as `scripts/pdf-sweep.py` does, re-checking the licence line.
"""

from __future__ import annotations

import argparse
import glob
import importlib.util
import json
import math
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SWEEP = os.path.join(HERE, "pdf-sweep.py")
CONTAINMENT_SUSPECT = 0.9
GLYPH_CAP = 256
DUP_RE = re.compile(rb"dup\s+(\d+)\s*/([^\s/\[\]{}()<>]+)\s+put")


def load_sweep():
    spec = importlib.util.spec_from_file_location("pdf_sweep", SWEEP)
    module = importlib.util.module_from_spec(spec)
    sys.modules["pdf_sweep"] = module
    spec.loader.exec_module(module)
    return module


SWEEP_MODULE = load_sweep()


def parse_builtin_encoding(clear: bytes) -> dict:
    """The built-in encoding of a Type 1 program, from its cleartext part only."""
    if re.search(rb"/Encoding\s+StandardEncoding\s+def", clear):
        return {"kind": "standard", "entries": []}
    if b"/Encoding" not in clear:
        return {"kind": "none", "entries": []}
    entries = []
    for match in DUP_RE.finditer(clear):
        code = int(match.group(1))
        if code > 255:
            continue
        name = match.group(2).decode("latin-1")
        if name == ".notdef":
            continue
        entries.append([code, name])
        if len(entries) >= GLYPH_CAP:
            break
    return {"kind": "array" if entries else "none", "entries": entries}


def font_program(doc, font_xref: int) -> tuple[bytes | None, int, int, str]:
    """The Type 1 program's cleartext bytes (`/Length1` of `/FontFile`), with its sizes.

    The fourth item says where the program is: `fontfile` (a Type 1 cleartext program, the only
    shape this repair reads), `fontfile3` (CFF/Type1C, whose encoding is not in a cleartext
    program), or `none` (no program reference at all).
    """
    kind, value = doc.xref_get_key(font_xref, "FontDescriptor")
    if kind != "xref":
        return None, 0, 0, "none"
    descriptor = int(value.split()[0])
    kind, value = doc.xref_get_key(descriptor, "FontFile")
    if kind != "xref":
        kind3, _ = doc.xref_get_key(descriptor, "FontFile3")
        return None, 0, 0, "fontfile3" if kind3 == "xref" else "none"
    program = int(value.split()[0])
    length1 = 0
    kind, value = doc.xref_get_key(program, "Length1")
    if kind == "int":
        length1 = int(value)
    try:
        data = doc.xref_stream(program)
    except Exception:  # noqa: BLE001 - an unreadable program is data, not a crash
        return None, length1, 0, "fontfile"
    if data is None:
        return None, length1, 0, "fontfile"
    if length1 <= 0 or length1 > len(data):
        length1 = min(len(data), 65536)
    return data[:length1], length1, len(data), "fontfile"


def probe(clear: bytes) -> str:
    """A short readable excerpt of a cleartext program, around `/Encoding` when it has one."""
    at = clear.find(b"/Encoding")
    start = max(0, at - 40) if at >= 0 else 0
    excerpt = clear[start:start + 300]
    return "".join(chr(byte) if 32 <= byte < 127 else "." for byte in excerpt)


def font_facts(path: str) -> dict:
    import pymupdf

    facts: dict = {"fonts": [], "targeted": False, "targeted_fonts": 0,
                   "targeted_glyphs": [], "error": None}
    seen: dict[int, dict] = {}
    span_fonts: set[str] = set()
    try:
        with pymupdf.open(path) as document:
            for page in document:
                try:
                    spans = {span["font"] for block in page.get_text("rawdict")["blocks"]
                             for line in block.get("lines", [])
                             for span in line.get("spans", [])}
                except Exception:  # noqa: BLE001 - a page that cannot be read is data
                    spans = set()
                for name in spans:
                    span_fonts.add(name)
                for font in page.get_fonts(full=True):
                    xref = int(font[0])
                    if xref <= 0 or xref in seen:
                        if xref in seen:
                            seen[xref]["pages"] += 1
                        continue
                    ext = font[1]
                    subtype = font[2]
                    basefont = font[3]
                    encoding = font[5] if len(font) > 5 else None
                    kind, to_unicode = document.xref_get_key(xref, "ToUnicode")
                    encoding_kind, encoding_value = document.xref_get_key(xref, "Encoding")
                    encoding_resolved = ""
                    if encoding_kind == "xref":
                        try:
                            encoding_resolved = document.xref_object(
                                int(str(encoding_value).split()[0])).strip()[:120]
                        except Exception:  # noqa: BLE001 - an unreadable object is data
                            encoding_resolved = "unreadable"
                    entry = {
                        "xref": xref,
                        "basefont": basefont,
                        "subtype": subtype,
                        "embedded": ext not in ("n/a", None),
                        "encoding": encoding,
                        "to_unicode": kind != "null",
                        "type_key": "Type" in document.xref_get_keys(xref),
                        "encoding_kind": encoding_kind,
                        "encoding_value": str(encoding_value)[:80],
                        "encoding_resolved": encoding_resolved,
                        "pages": 1,
                        "population": subtype == "Type1" and ext not in ("n/a", None)
                        and kind == "null" and encoding in ("", "Builtin", None),
                        "targeted": False,
                        "program_kind": None,
                        "builtin": None,
                        "length1": 0,
                        "program_bytes": 0,
                        "probe": "",
                        "glyphs": [],
                        "used_for_text": False,
                    }
                    if entry["population"]:
                        clear, length1, total, where = font_program(document, xref)
                        entry["program_kind"] = where
                        entry["length1"] = length1
                        entry["program_bytes"] = total
                        if clear is not None:
                            parsed = parse_builtin_encoding(clear)
                            entry["builtin"] = parsed["kind"]
                            entry["glyphs"] = parsed["entries"]
                            if parsed["kind"] == "none":
                                entry["probe"] = probe(clear)
                            else:
                                entry["targeted"] = True
                    seen[xref] = entry
    except Exception as exc:  # noqa: BLE001 - an unreadable reference is data, not a crash
        facts["error"] = f"{type(exc).__name__}: {str(exc)[:200]}"
    inventory = sorted(seen.values(), key=lambda item: (-item["pages"], item["basefont"]))
    facts["fonts"] = inventory
    facts["targeted_fonts"] = sum(1 for item in inventory if item["targeted"])
    facts["targeted"] = facts["targeted_fonts"] > 0
    for item in inventory:
        if item["population"]:
            name = item["basefont"]
            stripped = name.split("+", 1)[1] if "+" in name else name
            item["used_for_text"] = name in span_fonts or stripped in span_fonts
    facts["span_fonts"] = sorted(span_fonts)[:40]
    facts["population_fonts"] = sum(1 for item in inventory if item["population"])
    facts["population"] = facts["population_fonts"] > 0
    glyphs = []
    for item in inventory:
        if item["targeted"]:
            for code, name in item["glyphs"]:
                glyphs.append({"basefont": item["basefont"], "code": code, "name": name})
    facts["targeted_glyphs"] = glyphs
    return facts


# --------------------------------------------------------------------------------------------
# The before/after join
# --------------------------------------------------------------------------------------------

def load_records(directory: str) -> dict[str, dict]:
    records: dict[str, dict] = {}
    if not directory:
        return records
    for path in sorted(glob.glob(os.path.join(directory, "**", "*.jsonl"), recursive=True)):
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                try:
                    record = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if record.get("kind_record") != "document" or "id" not in record:
                    continue
                records[record["id"]] = record
    return records


def quality(record: dict | None) -> dict:
    if not record:
        return {}
    return record.get("quality") or {}


def peak_mib(record: dict) -> float | None:
    kib = record.get("peak_rss_vmhwm_kib") or record.get("peak_rss_total_kib")
    return round(kib / 1024, 1) if kib else None


def percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, math.ceil(fraction * len(ordered)) - 1))
    return ordered[index]


def rounded(value: float | None) -> str:
    return "-" if value is None else f"{value:.3f}"


def sample_table(records: dict[str, dict]) -> dict:
    containment = [quality(r).get("containment_first20") for r in records.values()]
    containment = [value for value in containment if value is not None]
    ratios = [quality(r).get("char_ratio") for r in records.values()]
    ratios = [value for value in ratios if value is not None]
    rss = [peak_mib(r) for r in records.values()]
    rss = [value for value in rss if value is not None]
    open_s = [r.get("open_s") for r in records.values()]
    open_s = [value for value in open_s if value is not None]
    outcomes: dict[str, int] = {}
    for record in records.values():
        outcome = record.get("outcome") or "unknown"
        outcomes[outcome] = outcomes.get(outcome, 0) + 1
    return {
        "documents": len(records),
        "outcomes": outcomes,
        "containment_median": percentile(containment, 0.5),
        "containment_p05": percentile(containment, 0.05),
        "containment_min": min(containment) if containment else None,
        "containment_below_0_9": sum(1 for value in containment
                                     if value < CONTAINMENT_SUSPECT),
        "char_ratio_median": percentile(ratios, 0.5),
        "rss_median": percentile(rss, 0.5),
        "rss_p95": percentile(rss, 0.95),
        "open_median": percentile(open_s, 0.5),
        "open_p95": percentile(open_s, 0.95),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ids", required=True)
    parser.add_argument("--work-dir", required=True)
    parser.add_argument("--before", default="", help="directory of the before run's JSONL")
    parser.add_argument("--after", default="", help="directory of the after run's JSONL")
    parser.add_argument("--out", required=True, help="per-document facts, JSONL")
    parser.add_argument("--markdown", default="", help="the gate table")
    parser.add_argument("--limit", type=int, default=0, help="first N documents only")
    args = parser.parse_args()

    entries = SWEEP_MODULE.CORPUS.load_corpus(args.ids)
    if args.limit:
        entries = entries[:args.limit]
    before = load_records(args.before)
    after = load_records(args.after)
    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)

    rows = []
    for index, entry in enumerate(entries, 1):
        path, status = SWEEP_MODULE.download_pdf(entry, args.work_dir)
        row = {"kind_record": "type1-fonts", "id": entry["id"], "group": entry.get("group"),
               "download": status["download"], "error": status.get("error")}
        if path is None:
            row.update({"targeted": False, "targeted_fonts": 0, "population": False,
                        "population_fonts": 0, "fonts": [], "targeted_glyphs": []})
        else:
            row.update(font_facts(path))
        row["before"] = {"outcome": (before.get(entry["id"]) or {}).get("outcome"),
                         "containment": quality(before.get(entry["id"]))
                         .get("containment_first20")}
        row["after"] = {"outcome": (after.get(entry["id"]) or {}).get("outcome"),
                        "containment": quality(after.get(entry["id"]))
                        .get("containment_first20")}
        rows.append(row)
        with open(args.out, "a", encoding="utf-8") as handle:
            handle.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
        print(f"[{index}/{len(entries)}] {entry['id']} targeted={row['targeted']} "
              f"fonts={row['targeted_fonts']} download={row['download']}", flush=True)

    population = [row for row in rows if row.get("population")]
    targeted = [row for row in rows if row.get("targeted")]
    lines = ["## Type 1 built-in encoding without /ToUnicode", ""]
    lines.append(f"Corpus: {len(rows)} documents. {len(population)} of them embed at least one "
                 "Type 1 font with a built-in encoding and no `/ToUnicode` (PyMuPDF font facts: "
                 "`Type1`, embedded, no `/ToUnicode`, `/Encoding` empty or `Builtin`); "
                 f"{len(targeted)} of those carry the font as a `/FontFile` Type 1 program whose "
                 "cleartext names its codes, which is the shape this repair reads.")
    lines.append("")
    lines.append("| Id | Group | Population fonts | Targeted fonts | Used for text | "
                 "Containment before | after |")
    lines.append("| --- | --- | --- | --- | --- | --- | --- |")
    for row in sorted(population, key=lambda item: item["id"]):
        used = sum(1 for font in row["fonts"]
                   if font.get("population") and font.get("used_for_text"))
        lines.append(f"| {row['id']} | {row['group']} | {row['population_fonts']} | "
                     f"{row['targeted_fonts']} | {used} | "
                     f"{rounded(row['before']['containment'])} | "
                     f"{rounded(row['after']['containment'])} |")
    lines.append("")
    for label, subset in (("Population", population), ("The documents this repair reads",
                                                       targeted)):
        before_values = [row["before"]["containment"] for row in subset
                         if row["before"]["containment"] is not None]
        after_values = [row["after"]["containment"] for row in subset
                        if row["after"]["containment"] is not None]
        lines.append(f"| {label} | before | after |")
        lines.append("| --- | --- | --- |")
        lines.append(f"| documents | {len(subset)} | {len(subset)} |")
        lines.append(f"| containment median | {rounded(percentile(before_values, 0.5))} | "
                     f"{rounded(percentile(after_values, 0.5))} |")
        lines.append(f"| containment below 0.9 | "
                     f"{sum(1 for value in before_values if value < CONTAINMENT_SUSPECT)} | "
                     f"{sum(1 for value in after_values if value < CONTAINMENT_SUSPECT)} |")
        lines.append("")

    reasons: dict[tuple[str, str], dict] = {}
    for row in population:
        for font in row["fonts"]:
            if not font.get("population") or font["targeted"]:
                continue
            key = (font["program_kind"] or "none", font["builtin"] or "-",
                   "yes" if font.get("type_key") else "no", font.get("encoding_kind") or "-")
            bucket = reasons.setdefault(key, {"fonts": 0, "documents": set(), "probe": ""})
            bucket["fonts"] += 1
            bucket["documents"].add(row["id"])
            if font.get("probe") and not bucket["probe"]:
                bucket["probe"] = font["probe"]
    if reasons:
        lines.append("### Why the other fonts are not read")
        lines.append("")
        lines.append("| Program | Cleartext `/Encoding` | `/Type` key | Dict `/Encoding` | "
                     "Fonts | Documents |")
        lines.append("| --- | --- | --- | --- | --- | --- |")
        for (where, encoding, type_key, dict_encoding), bucket in sorted(reasons.items()):
            lines.append(f"| `{where}` | `{encoding}` | {type_key} | `{dict_encoding}` | "
                         f"{bucket['fonts']} | {len(bucket['documents'])} |")
        lines.append("")
        lines.append("`fontfile3` is a CFF/Type1C program: the encoding is not in cleartext, "
                     "so it is the task's stretch goal. `fontfile` with `none` is a Type 1 "
                     "program whose first `/Length1` bytes name no code; its excerpt:")
        lines.append("")
        for (where, encoding, _type_key, _dict_encoding), bucket in sorted(reasons.items()):
            if where == "fontfile" and bucket["probe"]:
                lines.append(f"```\n{bucket['probe']}\n```")
                lines.append("")

    if before and after:
        before_sample = sample_table(before)
        after_sample = sample_table(after)
        changed = [doc_id for doc_id in sorted(set(before) | set(after))
                   if (before.get(doc_id) or {}).get("outcome")
                   != (after.get(doc_id) or {}).get("outcome")]
        lines.append("## Whole sample, before vs after")
        lines.append("")
        lines.append(f"Before: {len(before)} documents, after: {len(after)} documents. "
                     f"Outcome-class changes: {len(changed)}"
                     + (f" ({', '.join(changed[:10])})" if changed else ""))
        lines.append("")
        lines.append("| Measure | before | after |")
        lines.append("| --- | --- | --- |")
        for key, label, digits in (
            ("containment_median", "containment median", 3),
            ("containment_p05", "containment p05", 3),
            ("containment_min", "containment minimum", 3),
            ("containment_below_0_9", "documents below 0.9", 0),
            ("char_ratio_median", "char-ratio median", 3),
            ("rss_median", "peak RSS median (MiB)", 1),
            ("rss_p95", "peak RSS p95 (MiB)", 1),
            ("open_median", "open time median (s)", 3),
            ("open_p95", "open time p95 (s)", 3),
        ):
            left, right = before_sample[key], after_sample[key]
            fmt = (lambda value: "-" if value is None else f"{value:.{digits}f}")
            lines.append(f"| {label} | {fmt(left)} | {fmt(right)} |")
        lines.append("")
        lines.append("Outcome classes before: "
                     + ", ".join(f"{key} {value}"
                                 for key, value in sorted(before_sample["outcomes"].items())))
        lines.append("")
        lines.append("Outcome classes after: "
                     + ", ".join(f"{key} {value}"
                                 for key, value in sorted(after_sample["outcomes"].items())))
        lines.append("")
        per_doc = [f"| {doc_id} | {rounded(quality(before.get(doc_id)).get('containment_first20'))} "
                   f"| {rounded(quality(after.get(doc_id)).get('containment_first20'))} |"
                   for doc_id in sorted(set(before) & set(after))]
        lines.append("<details><summary>Per-document containment (before, after)</summary>")
        lines.append("")
        lines.append("| Id | before | after |")
        lines.append("| --- | --- | --- |")
        lines.extend(per_doc)
        lines.append("")
        lines.append("</details>")
        lines.append("")

    glyph_counts: dict[tuple[str, str], int] = {}
    for row in rows:
        for glyph in row.get("targeted_glyphs") or []:
            key = (glyph["basefont"], glyph["name"])
            glyph_counts[key] = glyph_counts.get(key, 0) + 1
    lines.append("## Glyph names in the targeted fonts (top 200 by font)")
    lines.append("")
    for (basefont, name), count in sorted(glyph_counts.items())[:200]:
        lines.append(f"- `{basefont}` `{name}` ({count} documents)")
    lines.append("")

    text = "\n".join(lines)
    print(text)
    if args.markdown:
        os.makedirs(os.path.dirname(os.path.abspath(args.markdown)) or ".", exist_ok=True)
        with open(args.markdown, "w", encoding="utf-8") as handle:
            handle.write(text + "\n")
    print(f"targeted documents: {len(targeted)} of {len(rows)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
