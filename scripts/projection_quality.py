"""Issue #91 frozen projection-quality gate.

Consumes the canonical report emitted by tests/projection_boundary_engine.rs
(production LayoutPdfParser with local OCR disabled + production EpubParser).
Gold files define acceptance only; they never feed the parser. This scorer
never splits sentences itself — it scores the Rust text-segmentation/v3
paragraph/sentence facts recorded in the report against frozen gold.

Frozen thresholds (Issue #89):
  boundary precision/recall >= 0.99; wrong merge/split = 0;
  metadata contamination = 0; omission/duplication = 0;
  exact normalized read equality = 1.0; 3/3 identical runs;
  supported prose sentence-readable coverage >= 0.95;
  every non-exact region carries an explicit degradation code.
"""
import argparse
import json
import sys
from collections import Counter
from pathlib import Path

SCHEMA = "projection-quality-score/v1"
DETERMINISM_RUNS = 3
BOUNDARY_MIN = 0.99
COVERAGE_MIN = 0.95


def gold_sentence_ranges(paragraph):
    """Derive gold (start, end) char offsets by tiling the gold paragraph.

    Gold sentences must tile the paragraph text exactly, separated only by
    whitespace. Raises ValueError on malformed gold rather than guessing.
    """
    text = paragraph["text"]
    ranges = []
    cursor = 0
    for sentence in paragraph["sentences"]:
        start = text.index(sentence, cursor)
        end = start + len(sentence)
        if text[cursor:start].strip():
            raise ValueError("gold sentences omit non-separator prose")
        ranges.append((start, end))
        cursor = end
    if text[cursor:].strip():
        raise ValueError("gold sentences leave trailing prose uncovered")
    return ranges


def actual_sentence_ranges(paragraph):
    """Validate actual sentence ranges against the actual paragraph text."""
    ranges = []
    for sentence in paragraph["sentences"]:
        start, end = sentence["start"], sentence["end"]
        if not 0 <= start < end <= len(paragraph["text"]):
            raise ValueError("sentence range outside its paragraph")
        if paragraph["text"][start:end] != sentence["text"]:
            raise ValueError("sentence text is not its paragraph slice")
        ranges.append((start, end))
    return ranges


def score_case(case, entry, gold):
    result = {"violations": []}
    fail = result["violations"].append

    # Determinism: DETERMINISM_RUNS identical normalized hashes and unit facts.
    hashes = entry.get("normalized_hashes", [])
    fingerprints = entry.get("unit_fingerprints", [])
    result["determinism"] = {
        "runs": len(hashes),
        "distinct_normalized_hashes": len(set(hashes)),
        "distinct_unit_fingerprints": len(set(fingerprints)),
    }
    if not (len(hashes) == DETERMINISM_RUNS and len(set(hashes)) == 1
            and len(fingerprints) == DETERMINISM_RUNS
            and len(set(fingerprints)) == 1):
        fail(f"{case}: determinism requires {DETERMINISM_RUNS}/{DETERMINISM_RUNS} "
             "identical normalized hashes and unit fingerprints")

    # Exact normalized read equality = 1.0.
    reads = entry.get("exact_reads", {})
    total, equal = reads.get("total", 0), reads.get("equal", 0)
    result["exact_reads"] = {"total": total, "equal": equal}
    if not total or equal != total:
        fail(f"{case}: exact normalized read equality {equal}/{total} != 1.0")

    codes = entry.get("degradation_codes", [])
    result["degradation_codes"] = codes
    for code in gold.get("required_degradation_codes", []):
        if code not in codes:
            fail(f"{case}: required degradation code {code!r} absent")

    gold_paragraphs = gold["paragraphs"]
    actual_paragraphs = entry.get("paragraphs", [])

    # Paragraph-level omission/duplication on the (section, text) stream.
    gold_keys = Counter((p["section"], p["text"]) for p in gold_paragraphs)
    actual_keys = Counter((p["section"], p["text"]) for p in actual_paragraphs)
    omission = sum((gold_keys - actual_keys).values())
    duplication = sum((actual_keys - gold_keys).values())
    # Order check on the matched stream.
    if [p["text"] for p in actual_paragraphs] != [p["text"] for p in gold_paragraphs]:
        fail(f"{case}: paragraph stream differs from gold (order/content)")

    metadata_texts = [p["text"] for p in gold_paragraphs if p.get("metadata")]
    contamination = 0
    wrong_merge = wrong_split = 0
    boundary_tp = boundary_predicted = boundary_expected = 0
    prose_chars = sentence_chars = 0
    non_exact_regions = 0

    for gold_p, actual_p in zip(gold_paragraphs, actual_paragraphs):
        if gold_p["text"] != actual_p["text"]:
            continue  # already counted as omission + duplication
        if actual_p["section"] != gold_p["section"]:
            fail(f"{case}: section {actual_p['section']!r} != gold {gold_p['section']!r}")
        expected_page = (None if gold_p["page"] is None else
                         {"kind": "page", "page_number": gold_p["page"]})
        if actual_p["page"] != expected_page:
            fail(f"{case}: page binding {actual_p['page']} != gold {expected_page}")
        if actual_p["content_class"] != gold_p["content_class"]:
            fail(f"{case}: content_class {actual_p['content_class']!r} != "
                 f"gold {gold_p['content_class']!r}")
        if actual_p["eligible"] != gold_p["eligible"]:
            fail(f"{case}: eligibility {actual_p['eligible']} != gold "
                 f"{gold_p['eligible']} for {gold_p['text'][:40]!r}")
            if gold_p.get("metadata"):
                contamination += 1
        if not actual_p.get("exact_read", False):
            non_exact_regions += 1

        if not gold_p["eligible"]:
            if actual_p["sentences"]:
                fail(f"{case}: coarse region emitted sentences: "
                     f"{gold_p['text'][:40]!r}")
            continue

        prose_chars += len(gold_p["text"])
        expected = gold_sentence_ranges(gold_p)
        observed = actual_sentence_ranges(actual_p)
        sentence_chars += sum(end - start for start, end in observed)

        expected_ends = {end for _, end in expected}
        observed_ends = {end for _, end in observed}
        boundary_tp += len(expected_ends & observed_ends)
        boundary_predicted += len(observed_ends)
        boundary_expected += len(expected_ends)

        wrong_merge += sum(
            1 for start, end in observed
            if any(start < b < end for b in expected_ends))
        wrong_split += sum(
            1 for start, end in expected
            if any(start < b < end for b in observed_ends))

        # Char-level omission/duplication over prose: gold sentence chars
        # covered by zero observed sentences are omitted; chars covered by
        # more than one observed sentence are duplicated. Whitespace
        # separators between sentences are legitimately uncovered.
        mask = [0] * len(actual_p["text"])
        for start, end in observed:
            for i in range(start, end):
                mask[i] += 1
        omission += sum(
            1 for start, end in expected
            for i in range(start, end) if mask[i] == 0)
        duplication += sum(1 for n in mask if n > 1)

    for actual_p in actual_paragraphs:
        if actual_p["eligible"]:
            for meta in metadata_texts:
                if meta in actual_p["text"] or any(
                        meta in s["text"] for s in actual_p["sentences"]):
                    contamination += 1
                    break

    precision = boundary_tp / boundary_predicted if boundary_predicted else 1.0
    recall = boundary_tp / boundary_expected if boundary_expected else 1.0
    coverage = sentence_chars / prose_chars if prose_chars else 1.0
    result["boundary"] = {"precision": precision, "recall": recall,
                          "true_positives": boundary_tp,
                          "predicted": boundary_predicted,
                          "expected": boundary_expected}
    result["counts"] = {"wrong_merge": wrong_merge, "wrong_split": wrong_split,
                        "metadata_contamination": contamination,
                        "omission": omission, "duplication": duplication,
                        "non_exact_regions": non_exact_regions}
    result["sentence_readable_coverage"] = coverage

    if min(precision, recall) < BOUNDARY_MIN:
        fail(f"{case}: boundary precision/recall {precision:.4f}/{recall:.4f} "
             f"< {BOUNDARY_MIN}")
    if wrong_merge:
        fail(f"{case}: wrong sentence merges = {wrong_merge}")
    if wrong_split:
        fail(f"{case}: wrong sentence splits = {wrong_split}")
    if contamination:
        fail(f"{case}: metadata contamination = {contamination}")
    if omission:
        fail(f"{case}: omitted gold units = {omission}")
    if duplication:
        fail(f"{case}: duplicated/inserted units = {duplication}")
    if coverage < COVERAGE_MIN:
        fail(f"{case}: sentence-readable coverage {coverage:.4f} < {COVERAGE_MIN}")
    if (non_exact_regions or omission or duplication) and not codes:
        fail(f"{case}: non-exact/degraded regions lack explicit degradation codes")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True,
                        help="canonical report from projection_boundary_engine")
    parser.add_argument("--gold", type=Path,
                        default=Path("tests/projection_quality/gold"))
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    report = json.loads(args.report.read_text())
    if report.get("schema") != "projection-quality-report/v1":
        raise SystemExit("unsupported report schema: "
                         f"{report.get('schema')!r}")
    cases = report.get("cases", {})

    results = {"schema": SCHEMA,
               "thresholds": {"boundary_min": BOUNDARY_MIN,
                              "coverage_min": COVERAGE_MIN,
                              "determinism_runs": DETERMINISM_RUNS,
                              "wrong_merge": 0, "wrong_split": 0,
                              "metadata_contamination": 0,
                              "omission": 0, "duplication": 0,
                              "exact_read_equality": 1.0},
               "cases": {}, "failures": []}

    gold_files = {path.stem: path for path in args.gold.glob("*.json")}
    for case in sorted(set(cases) | set(gold_files)):
        if case not in cases:
            results["cases"][case] = {"violations": ["missing canonical report entry"]}
            results["failures"].append(case)
            continue
        if case not in gold_files:
            results["cases"][case] = {"violations": ["missing gold file"]}
            results["failures"].append(case)
            continue
        gold = json.loads(gold_files[case].read_text())
        try:
            scored = score_case(case, cases[case], gold)
        except Exception as error:
            scored = {"violations": [f"scoring error: {error}"]}
        results["cases"][case] = scored
        if scored["violations"]:
            results["failures"].append(case)

    results["status"] = "fail" if results["failures"] else "pass"
    encoded = json.dumps(results, ensure_ascii=False, indent=2) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(encoded)
    sys.stdout.write(encoded)
    if results["failures"]:
        raise SystemExit("projection quality gate failed: "
                         + ",".join(results["failures"]))


if __name__ == "__main__":
    main()
