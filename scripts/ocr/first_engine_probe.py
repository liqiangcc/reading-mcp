"""First fixed-configuration real-engine probe on Coordinator-frozen public PDFs.

No reading-mcp/OCR adapter implementation claim. Gold is loaded only after full
page recognition, solely for scoring; it never drives engine inputs or crops.
"""
import argparse
import csv
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import re
import resource
import signal
import subprocess
import time
import unicodedata

import pymupdf

FIXTURE_SHA = "7b68a6e7b1a518ef7276f8d3a0ac006639121117"
MANIFEST_SHA = "4e3b1dd9d84a7a401e4f2d5c773b510118f5bba3c5aa7fe8f112704827a17ff2"
LANGUAGES = {"F02": "eng", "F06": "eng", "F07": "chi_sim", "F08": "eng+chi_sim", "F11": "eng", "F14": "eng"}


def dump(path, data):
    path.write_text(json.dumps(data, ensure_ascii=False, sort_keys=True, indent=2) + "\n", encoding="utf-8")


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def normalize(text):
    return " ".join(unicodedata.normalize("NFC", text).split())


def edits(reference, actual):
    # Raw Levenshtein edit count, not SequenceMatcher heuristics.
    previous = list(range(len(actual) + 1))
    for i, expected in enumerate(reference, 1):
        current = [i]
        for j, observed in enumerate(actual, 1):
            current.append(min(current[-1] + 1, previous[j] + 1,
                               previous[j - 1] + (expected != observed)))
        previous = current
    return previous[-1]


def metric(reference, actual, words=False):
    ref, got = normalize(reference), normalize(actual)
    if words:
        ref, got = ref.split(), got.split()
    distance = edits(ref, got)
    return {"edits": distance, "gold_count": len(ref), "rate": distance / len(ref) if ref else None}


def cjk(char):
    return "\u3400" <= char <= "\u9fff" or "\uf900" <= char <= "\ufaff"


def join_words(words):
    """Minimal script-aware spacing; no spelling, punctuation or hyphen repair."""
    result = ""
    for word in words:
        text = word["text"]
        if result:
            last, first = result[-1], text[0]
            no_space = ((cjk(last) and cjk(first))
                        or (cjk(last) and unicodedata.category(first).startswith("P"))
                        or (unicodedata.category(last).startswith("P") and cjk(first)))
            if not no_space:
                result += " "
        result += text
    return result


def parse_tsv(text, sx, sy):
    words = []
    for row in csv.DictReader(io.StringIO(text), delimiter="\t", quoting=csv.QUOTE_NONE):
        if row["level"] != "5" or not row.get("text", "").strip():
            continue
        x, y, w, h = [int(row[key]) for key in ("left", "top", "width", "height")]
        words.append({"text": row["text"], "confidence_native_0_100": float(row["conf"]),
                      "bbox_points": [x / sx, y / sy, (x + w) / sx, (y + h) / sy],
                      "block": int(row["block_num"]), "paragraph": int(row["par_num"]),
                      "line": int(row["line_num"]), "engine_order": len(words)})
    return words


def overlaps(word, paragraph):
    x0, y0, x1, y1 = word["bbox_points"]
    center = ((x0 + x1) / 2, (y0 + y1) / 2)
    return any(line["bbox"][0] - 1 <= center[0] <= line["bbox"][2] + 1
               and line["bbox"][1] - 1 <= center[1] <= line["bbox"][3] + 1
               for line in paragraph["lines"])


def score(gold, words, language):
    paragraphs = gold["paragraphs"]
    classified = []
    for word in words:
        matches = [i for i, para in enumerate(paragraphs) if overlaps(word, para)]
        classified.append(matches[0] if len(matches) == 1 else None)
    selected = [word for word, index in zip(words, classified) if index is not None]
    raw_text, prose_text = join_words(words), join_words(selected)
    para_results, observed_order = [], []
    for index in classified:
        if index is not None and index not in observed_order:
            observed_order.append(index)
    inversions = sum(observed_order[i] > observed_order[j]
                     for i in range(len(observed_order)) for j in range(i + 1, len(observed_order)))
    total_pairs = len(paragraphs) * (len(paragraphs) - 1) // 2
    found_pairs = len(observed_order) * (len(observed_order) - 1) // 2
    for index, paragraph in enumerate(paragraphs):
        observed = join_words([word for word, owner in zip(words, classified) if owner == index])
        para_results.append({"id": paragraph["id"], "actual": observed,
                             "cer": metric(paragraph["text"], observed),
                             "wer": None if paragraph["font"] == "goldcjk" else metric(paragraph["text"], observed, True)})
    # Paragraph boundary comparison in engine order, separately from sentence boundaries.
    pairs = [(word, owner) for word, owner in zip(words, classified) if owner is not None]
    gold_boundaries, engine_boundaries = set(), set()
    for i in range(1, len(pairs)):
        a, ai = pairs[i - 1]
        b, bi = pairs[i]
        if ai != bi:
            gold_boundaries.add(i)
        if (a["block"], a["paragraph"]) != (b["block"], b["paragraph"]):
            engine_boundaries.add(i)
    shared = len(gold_boundaries & engine_boundaries)
    denominator = len(gold_boundaries) + len(engine_boundaries)
    english_errors = sum(p["wer"]["edits"] for p in para_results if p["wer"] is not None)
    english_count = sum(p["wer"]["gold_count"] for p in para_results if p["wer"] is not None)
    return {
        "raw_engine_all_text": raw_text,
        "raw_all_cer_vs_gold_prose": metric(gold["text"], raw_text),
        "raw_all_wer_vs_gold_prose": None if language == "chi_sim" else metric(gold["text"], raw_text, True),
        "gold_region_scored_prose_text": prose_text,
        "gold_region_prose_cer_engine_order": metric(gold["text"], prose_text),
        "gold_region_english_wer": {"edits": english_errors, "gold_count": english_count,
                                    "rate": english_errors / english_count if english_count else None},
        "paragraphs_in_engine_order": observed_order,
        "gold_paragraph_count": len(paragraphs), "covered_paragraph_count": len(observed_order),
        "paragraph_order_concordant_pairs": found_pairs - inversions,
        "paragraph_order_gold_pairs": total_pairs,
        "paragraph_order_score": (found_pairs - inversions) / total_pairs if total_pairs else 1.0 if observed_order else 0.0,
        "engine_paragraph_boundary_f1_word_alignment": 2 * shared / denominator if denominator else 1.0,
        "per_gold_paragraph": para_results,
        "unassigned_words": [w for w, owner in zip(words, classified) if owner is None],
        "sentence_boundary_f1": None,
        "sentence_boundary_status": "requires later canonical MCP integration; not inferred from OCR paragraph counts",
        "scope_warning": "Gold-region assignment is scoring only, not a production adapter or its coverage claim",
    }


def engine_manifest(engine, tessdata):
    libraries = subprocess.check_output(["ldd", engine], text=True)
    paths = {Path(engine).resolve()}
    paths.update(Path(match) for match in re.findall(r"(/[^\s()]+)", libraries))
    paths.update(tessdata / name for name in ("eng.traineddata", "chi_sim.traineddata"))
    return {"engine_version": subprocess.check_output([engine, "--version"], text=True).splitlines(),
            "files": {str(path): {"sha256": digest(path), "bytes": path.stat().st_size}
                      for path in sorted(paths) if path.is_file()}}


def run(root, output, engine, tessdata):
    assert digest(root / "manifest.json") == MANIFEST_SHA, "frozen manifest mismatch"
    output.mkdir(parents=True, exist_ok=False)
    manifest = json.loads((root / "manifest.json").read_text(encoding="utf-8"))
    for name, value in manifest["files"].items():
        assert digest(root / name) == value["sha256"], name
    dump(output / "engine-manifest.json", engine_manifest(engine, tessdata))
    results = []
    for identifier, language in LANGUAGES.items():
        directory = output / identifier
        directory.mkdir()
        started = time.monotonic()
        with pymupdf.open(root / "pdf" / (identifier + ".pdf")) as document:
            assert len(document) == 1
            pixmap = document[0].get_pixmap(dpi=300, colorspace=pymupdf.csRGB, alpha=False)
            sx, sy = pixmap.width / document[0].rect.width, pixmap.height / document[0].rect.height
            pixels = pixmap.width * pixmap.height
            pixmap.save(directory / "input.png")
            del pixmap
        rendered = time.monotonic()
        environment = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "OMP_THREAD_LIMIT": "1"}
        command = ["/usr/bin/time", "-f", '{"max_rss_kib":%M,"user_seconds":%U,"system_seconds":%S,"exit_status":%x}',
                   "-o", str(directory / "usage.json"), engine, str(directory / "input.png"),
                   str(directory / "engine"), "--tessdata-dir", str(tessdata),
                   "-l", language, "--oem", "1", "--psm", "3", "--dpi", "300", "tsv"]
        with (directory / "stderr.txt").open("wb") as errors:
            process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=errors,
                                       env=environment, start_new_session=True)
            try:
                status = process.wait(timeout=60)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
                results.append({"id": identifier, "error": "diagnostic ceiling 60s exceeded", "page_budget_pass": False})
                continue
        finished = time.monotonic()
        assert status == 0, f"{identifier}: engine failed, inspect public stderr artifact"
        words = parse_tsv((directory / "engine.tsv").read_text(encoding="utf-8"), sx, sy)
        # Gold is not read until all page recognition is finished.
        gold = json.loads((root / "gold" / (identifier + ".json")).read_text(encoding="utf-8"))
        scored = score(gold, words, language)
        report = {"id": identifier, "language": language, "psm": 3, "oem": 1, "dpi": 300,
                  "pixels": pixels, "render_seconds": rendered - started,
                  "engine_seconds": finished - rendered, "render_plus_engine_seconds": finished - started,
                  "page_budget_seconds": 15, "page_budget_pass": finished - rendered <= 15,
                  "resources": json.loads((directory / "usage.json").read_text()), "metrics": scored}
        dump(directory / "words.json", words)
        dump(directory / "result.json", report)
        results.append(report)
        print(json.dumps({"id": identifier, "cer": scored["gold_region_prose_cer_engine_order"],
                          "english_wer": scored["gold_region_english_wer"], "order": scored["paragraph_order_score"],
                          "engine_seconds": report["engine_seconds"], "peak_kib": report["resources"]["max_rss_kib"]}), flush=True)
    final = {"schema": "ocr-first-engine-probe/v1", "fixture_sha": FIXTURE_SHA,
             "fixture_manifest_sha256": MANIFEST_SHA, "host": {"platform": platform.platform(), "cpus": os.cpu_count()},
             "parent_process_peak_rss_kib": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss,
             "cold_semantics": "new engine process each sample; OS file cache not purged; not connector cold-open acceptance",
             "diagnostic_timeout": "60s only to observe failures; production 15s/page budget reported separately",
             "adapter_implemented": False, "results": results}
    dump(output / "summary.json", final)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixtures", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--engine", default="/usr/bin/tesseract")
    parser.add_argument("--tessdata", default="/usr/share/tesseract-ocr/5/tessdata", type=Path)
    args = parser.parse_args()
    run(args.fixtures.resolve(), args.output.resolve(), args.engine, args.tessdata)
