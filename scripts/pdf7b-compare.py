#!/usr/bin/env python3
"""PDF-7b-1 step 0: the positional walker's lines against PyMuPDF's dict.

Run inside `.github/workflows/pdf7b-probe.yml`. Per shard it:

* downloads the shard's papers with the same `download_pdf` the quality
  probe uses (`scripts/pdf-sweep.py`), into `--work-dir/pdfs`;
* runs the compiled `pdf7b1_positional_probe` test binary with
  `PDF7B_PROBE_DIR` pointed at that directory, which writes
  `positional-lines.jsonl` - one record per page with the walker's lines,
  block indices, glyph count and bail reason;
* opens every downloaded PDF with PyMuPDF and compares `page.get_text("dict")`
  lines and blocks against the walker's, line text compared after
  whitespace normalisation;
* writes a per-shard JSONL (one record per page) plus a summary line.

`--aggregate` merges every shard's JSONL into the report table: lines and
blocks per page, the share of the walker's lines whose text is identical to
a PyMuPDF line, the largest glyph count on any page, and the bail counts.
"""

from __future__ import annotations

import argparse
import collections
import importlib.util
import json
import os
import re
import subprocess
import sys


def load_sibling(name: str):
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), name)
    spec = importlib.util.spec_from_file_location(name.replace("-", "_").replace(".py", ""), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


SWEEP = load_sibling("pdf-sweep.py")


def normalize_ws(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip()


def pymupdf_lines(page) -> list[dict]:
    """One record per PyMuPDF line: text (spans concatenated) and its block."""
    out = []
    for block_no, block in enumerate(page.get_text("dict").get("blocks") or []):
        for line in block.get("lines") or []:
            text = "".join(span.get("text") or "" for span in line.get("spans") or [])
            out.append({"text": text, "block": block_no})
    return out


def compare_page(ours: list[dict], theirs: list[dict]) -> dict:
    """Multiset match of whitespace-normalised line text."""
    ours_text = [normalize_ws(line["t"]) for line in ours]
    theirs_text = [normalize_ws(line["text"]) for line in theirs]
    remaining = collections.Counter(theirs_text)
    identical = 0
    for text in ours_text:
        if text and remaining[text] > 0:
            remaining[text] -= 1
            identical += 1
    remaining = collections.Counter(ours_text)
    theirs_identical = 0
    for text in theirs_text:
        if text and remaining[text] > 0:
            remaining[text] -= 1
            theirs_identical += 1
    return {
        "our_lines": len(ours),
        "their_lines": len(theirs),
        "our_blocks": len({line["b"] for line in ours}),
        "their_blocks": len({line["block"] for line in theirs}),
        "identical": identical,
        "their_identical": theirs_identical,
    }


def run_shard(args) -> int:
    entries = SWEEP.load_list(args.ids)
    shard = [entry for index, entry in enumerate(entries)
             if index % args.shards == args.shard]
    compare_ids = {item.strip() for item in (args.compare_ids or "").split(",")
                   if item.strip()}
    print(f"shard {args.shard}/{args.shards}: {len(shard)} documents", flush=True)

    os.makedirs(args.out_dir, exist_ok=True)
    out_path = os.path.join(args.out_dir, f"compare-{args.shard}.jsonl")
    max_glyphs = 0
    bail_counts: collections.Counter = collections.Counter()
    compare_rows = []
    with open(out_path, "w", encoding="utf-8") as out:
        for entry in shard:
            path, status = SWEEP.download_pdf(entry, args.work_dir)
            if path is None:
                print(f"| {entry['id']} | download {status['download']}: {status['error']} |",
                      flush=True)
                continue
            env = dict(os.environ)
            env["PDF7B_PROBE_DIR"] = os.path.dirname(os.path.abspath(path))
            env["PDF7B_PROBE_OUT"] = args.out_dir
            # The probe test reads every PDF in the directory; run it on the
            # one file by pointing at a private subdirectory.
            one_dir = os.path.join(args.work_dir, "one")
            os.makedirs(one_dir, exist_ok=True)
            for old in os.listdir(one_dir):
                os.unlink(os.path.join(one_dir, old))
            target = os.path.join(one_dir, os.path.basename(path))
            try:
                os.link(path, target)
            except OSError:
                import shutil
                shutil.copy(path, target)
            probe_out = os.path.join(args.out_dir, "positional-lines.jsonl")
            if os.path.exists(probe_out):
                os.unlink(probe_out)
            completed = subprocess.run(
                [args.probe_bin, "pdf7b1_positional_probe",
                 "--ignored", "--nocapture", "--test-threads", "1"],
                env=env, capture_output=True, text=True, timeout=600)
            pages: list[dict] = []
            if os.path.exists(probe_out):
                with open(probe_out, encoding="utf-8") as handle:
                    pages = [json.loads(line) for line in handle if line.strip()]
            if not pages:
                print(f"| {entry['id']} | probe produced nothing: "
                      f"{completed.stdout[-200:]} {completed.stderr[-200:]} |",
                      flush=True)
                continue
            import pymupdf
            doc = pymupdf.open(path)
            for record in pages:
                for key in ("glyphs",):
                    max_glyphs = max(max_glyphs, record.get(key) or 0)
                if record.get("bail"):
                    bail_counts[record["bail"]] += 1
                page_no = record["page"] - 1
                if page_no >= len(doc):
                    continue
                theirs = pymupdf_lines(doc[page_no])
                stats = compare_page(record.get("lines") or [], theirs)
                row = {"id": record["id"], "page": record["page"],
                       "glyphs": record.get("glyphs"), "bail": record.get("bail"),
                       **stats}
                out.write(json.dumps(row) + "\n")
            doc.close()
            if entry["id"] in compare_ids:
                compare_rows.append(entry["id"])
            print(f"| {entry['id']} | {len(pages)} pages |", flush=True)
    summary = {"kind_record": "probe_summary", "shard": args.shard,
               "max_glyphs": max_glyphs, "bails": dict(bail_counts),
               "compare_ids_seen": compare_rows}
    with open(out_path, "a", encoding="utf-8") as out:
        out.write(json.dumps(summary) + "\n")
    print(json.dumps(summary), flush=True)
    return 0


def aggregate(args) -> int:
    pages = []
    summaries = []
    for name in sorted(os.listdir(args.jsonl_dir)):
        # The probe's own positional-lines.jsonl dumps also live in the shard
        # artifacts; only the compare-<shard>.jsonl rows are aggregated.
        if not (name.endswith(".jsonl") and name.startswith("compare-")):
            continue
        with open(os.path.join(args.jsonl_dir, name), encoding="utf-8") as handle:
            for line in handle:
                record = json.loads(line)
                if record.get("kind_record") == "probe_summary":
                    summaries.append(record)
                else:
                    pages.append(record)
    by_id: dict[str, list] = collections.defaultdict(list)
    for page in pages:
        by_id[page["id"]].append(page)
    compare_ids = {item.strip() for item in (args.compare_ids or "").split(",")
                   if item.strip()}

    def share(page) -> float | None:
        their = page.get("their_lines") or 0
        if not their:
            return None
        return (page.get("their_identical") or 0) / their

    lines = ["# PDF-7b-1 positional probe", ""]
    total_identical = sum(page.get("their_identical") or 0 for page in pages)
    total_lines = sum(page.get("their_lines") or 0 for page in pages)
    our_total = sum(page.get("our_lines") or 0 for page in pages)
    max_glyphs = max((page.get("glyphs") or 0 for page in pages), default=0)
    bails: collections.Counter = collections.Counter()
    for summary in summaries:
        bails.update(summary.get("bails") or {})
        max_glyphs = max(max_glyphs, summary.get("max_glyphs") or 0)
    lines += [
        (f"Pages compared: {len(pages)} in {len(by_id)} documents. "
         f"Lines: PyMuPDF {total_lines}, walker {our_total}; whitespace-"
         f"normalised identical: {total_identical} "
         f"({total_identical / total_lines:.3f} of PyMuPDF lines)."
         if total_lines else "No pages compared."), "",
        f"Largest glyph count on one page: **{max_glyphs}** "
        f"(cap proposal: 200,000).", "",
        f"Pages bailed out by the walker: {sum(bails.values())} "
        f"{dict(bails)}", "",
        "## Compared papers", "",
        "| Id | Pages | PyMuPDF lines | Walker lines | Identical | Share | "
        "PyMuPDF blocks | Walker blocks |", "| --- | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for identifier in sorted(compare_ids):
        rows = by_id.get(identifier) or []
        if not rows:
            lines.append(f"| {identifier} | - | - | - | - | - | - | - |")
            continue
        theirs = sum(row.get("their_lines") or 0 for row in rows)
        ours = sum(row.get("our_lines") or 0 for row in rows)
        identical = sum(row.get("their_identical") or 0 for row in rows)
        their_blocks = sum(row.get("their_blocks") or 0 for row in rows)
        our_blocks = sum(row.get("our_blocks") or 0 for row in rows)
        lines.append(f"| {identifier} | {len(rows)} | {theirs} | {ours} | "
                     f"{identical} | {identical / theirs:.3f} | {their_blocks} | "
                     f"{our_blocks} |" if theirs else
                     f"| {identifier} | {len(rows)} | 0 | {ours} | 0 | - | "
                     f"{their_blocks} | {our_blocks} |")
    worst = sorted(
        ((share(page) or 0, page) for page in pages if page.get("their_lines")),
        key=lambda item: item[0])[:15]
    lines += ["", "## Worst-matching pages", "",
              "| Id | Page | Share | PyMuPDF lines | Walker lines |",
              "| --- | --- | --- | --- | --- |"]
    for value, page in worst:
        lines.append(f"| {page['id']} | {page['page']} | {value:.3f} | "
                     f"{page['their_lines']} | {page['our_lines']} |")
    text = "\n".join(lines)
    print(text)
    if args.markdown:
        with open(args.markdown, "w", encoding="utf-8") as handle:
            handle.write(text + "\n")
    if args.out:
        per_doc = {}
        for identifier, rows in sorted(by_id.items()):
            theirs = sum(row.get("their_lines") or 0 for row in rows)
            identical = sum(row.get("their_identical") or 0 for row in rows)
            per_doc[identifier] = {
                "pages": len(rows),
                "their_lines": theirs,
                "our_lines": sum(row.get("our_lines") or 0 for row in rows),
                "identical": identical,
                "share": round(identical / theirs, 4) if theirs else None,
            }
        with open(args.out, "w", encoding="utf-8") as handle:
            json.dump({
                "pages": len(pages),
                "documents": len(by_id),
                "their_lines": total_lines,
                "our_lines": our_total,
                "identical": total_identical,
                "match_share": (round(total_identical / total_lines, 4)
                                if total_lines else None),
                "max_glyphs_page": max_glyphs,
                "bails": dict(bails),
                "per_document": per_doc,
            }, handle, indent=2)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--aggregate", action="store_true")
    parser.add_argument("--ids", default="scripts/pdf-sweep-ids.txt")
    parser.add_argument("--shard", type=int, default=0)
    parser.add_argument("--shards", type=int, default=8)
    parser.add_argument("--work-dir", default=".tmp/pdf7b-probe")
    parser.add_argument("--probe-bin", default="")
    parser.add_argument("--out-dir", default=".tmp/pdf7b-probe/out")
    parser.add_argument("--jsonl-dir", default="")
    parser.add_argument("--compare-ids", default="")
    parser.add_argument("--out", default="")
    parser.add_argument("--markdown", default="")
    args = parser.parse_args()
    if args.aggregate:
        return aggregate(args)
    if not args.probe_bin:
        parser.error("run mode needs --probe-bin")
    return run_shard(args)


if __name__ == "__main__":
    sys.exit(main())
