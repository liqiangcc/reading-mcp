"""Evaluate real Rust boundaries; gold is used only for post-ingestion scoring."""
import argparse
from array import array
import json
from pathlib import Path
from canonical_quality import norm


def alignment(reference, actual):
    # Unit-cost Levenshtein, diagonal then deletion then insertion tie-break.
    # This maps observed boundary positions; it never edits canonical text.
    rows = [array("I", range(len(actual) + 1))]
    for i, expected in enumerate(reference, 1):
        row = array("I", [i])
        for j, value in enumerate(actual, 1):
            row.append(min(rows[-1][j] + 1, row[-1] + 1, rows[-1][j-1] + (expected != value)))
        rows.append(row)
    i, j = len(reference), len(actual)
    mapping = {}
    while i or j:
        mapping.setdefault(j, i)
        if i and j and rows[i][j] == rows[i-1][j-1] + (reference[i-1] != actual[j-1]):
            i, j = i-1, j-1
        elif i and rows[i][j] == rows[i-1][j] + 1:
            i -= 1
        else:
            j -= 1
    mapping.setdefault(0, 0)
    return mapping, rows[-1][-1]


def boundaries(paragraphs, gold=False):
    text = ""
    paragraph_ends, sentence_ends = [], []
    for paragraph in paragraphs:
        raw = paragraph["text"]
        if text:
            text += " "
        offset = len(text)
        normalized = norm(raw)
        if not normalized:
            raise ValueError("empty prose paragraph")
        text += normalized
        paragraph_ends.append(len(text))
        previous = 0
        for sentence in paragraph["sentences"]:
            if gold:
                start, end = [v - paragraph["source_range"][0] for v in sentence["source_range"]]
            else:
                start, end = sentence["start"], sentence["end"]
            if not 0 <= previous <= start < end <= len(raw) or norm(raw[previous:start]):
                raise ValueError("sentence ranges overlap, reorder, or omit prose")
            if raw[start:end] != sentence["text"]:
                raise ValueError("sentence text is not its actual paragraph slice")
            sentence_ends.append(offset + len(norm(raw[:end])))
            previous = end
        if norm(raw[previous:]):
            raise ValueError("sentence stream leaves unrepresented prose")
    return text, paragraph_ends, sentence_ends


def score(expected, observed, mapping):
    matched = {mapping.get(position) for position in observed} & set(expected)
    tp = len(matched)  # Duplicate predictions cannot claim the same gold twice.
    precision = tp / len(observed) if observed else 0
    recall = tp / len(expected) if expected else 0
    return {"true_positives": tp, "predicted": len(observed), "expected": len(expected),
            "precision": precision, "recall": recall,
            "f1": 2 * precision * recall / (precision + recall) if precision + recall else 0}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    observations = json.loads(args.input.read_text())
    results, failures = {}, []
    cases = ("F01", "F02", "F03", "F04-form", "F04-flat", "F05", "F06", "F07", "F08", "F12", "F13", "F14")
    for case in cases:
        try:
            observation = observations[case]
            actual = [p for p in observation["paragraphs"] if p["eligible"]]
            gold = json.loads((Path("tests/fixtures/scanned_pdf/gold") / f"{case}.json").read_text())
            ref, rp, rs = boundaries(gold["paragraphs"], gold=True)
            got, ap, ass = boundaries(actual)
            mapping, errors = alignment(ref, got)
            p, s = score(rp, ap, mapping), score(rs, ass, mapping)
            threshold = 1.0 if case in ("F01", "F03", "F04-form", "F04-flat") else .95
            results[case] = {"paragraph": p, "sentence": s, "threshold": threshold,
                             "alignment_errors": errors, "reference_chars": len(ref)}
            if min(p["f1"], s["f1"]) < threshold:
                failures.append(case)
        except Exception as error:
            results[case] = {"error": str(error)}
            failures.append(case)
    results["scope"] = "Aligned paragraph/sentence end boundaries; not independent reading-order or full coverage acceptance"
    results["failures"] = failures
    encoded = json.dumps(results, ensure_ascii=False, indent=2) + "\n"
    args.output.write_text(encoded)
    print(encoded, end="", flush=True)
    if failures or not observations:
        raise SystemExit("canonical boundary gate failed")


if __name__ == "__main__":
    main()
