#!/usr/bin/env python3
"""Price PDF-6f candidates on the `coarse_unit` records of a --keep-coarse-text run.

The #139 guard gives one verdict per paragraph: `missing_space_glue_events` needs 3 junction
events counted over the whole paragraph, so a 2,000-character paragraph with one event in each
of three different sentences is refused as a whole. The candidates move where the count is
taken; the doubled-token and bibliographic rules keep their paragraph-wide check in every one.

- S1  per-sentence verdict: a refused paragraph is re-checked sentence by sentence with a
      Python port of `sentence_ranges`; it stays coarse only if at least one sentence fires
      the unchanged PDF-profile guard on its own text.
- S2  density: the paragraph fires only at >= 3 events *and* at least one event per N
      characters, for a curve of N.
- S3  quorum-two (own candidate): like S1 but the paragraph stays coarse only if at least TWO
      sentences fire. Priced to bound the loosening direction: it emits a paragraph that
      contains one glued sentence, which is what #139 exists to prevent.

    python3 scripts/pdf6f-sentence-price.py --records .tmp/records.jsonl
"""

from __future__ import annotations

import argparse
import collections
import json
import random
import re
import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
import importlib.util  # noqa: E402

spec = importlib.util.spec_from_file_location(
    "pdf_guard_events", __file__.rsplit("/", 1)[0] + "/pdf-guard-events.py")
GE = importlib.util.module_from_spec(spec)
spec.loader.exec_module(GE)

LONG_CHARS = 800
SHORT_CHARS = 5
MIN_EVENTS = GE.MIN_MISSING_SPACE_GLUE_EVENTS

# --------------------------------------------------------------------------------------------
# Python port of `sentence_ranges` (src/domain/segmentation.rs)
# --------------------------------------------------------------------------------------------

TERMINAL_MARKS = frozenset(".!?。！？")
SENTENCE_CLOSERS = frozenset("\"'”’)]}》」』】")
PROTECTED_ABBREVIATIONS = frozenset((
    "e.g", "i.e", "etc", "vs", "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "fig", "eq",
    "no", "inc", "ltd", "u.s", "u.k"))


def is_ws(ch: str) -> bool:
    # Rust char::is_whitespace == Unicode White_Space property.
    return ch.isspace()


def extend_terminal_cluster(chars: list[str], index: int) -> int:
    end = index + 1
    while end < len(chars) and chars[end] in TERMINAL_MARKS:
        end += 1
    while end < len(chars) and chars[end] in SENTENCE_CLOSERS:
        end += 1
    return end


def next_non_whitespace(chars: list[str], index: int) -> str | None:
    while index < len(chars):
        if not is_ws(chars[index]):
            return chars[index]
        index += 1
    return None


def is_identifier_char(ch: str) -> bool:
    return ch.isalnum() or ch in "_-$"


def preceding_period_token(chars: list[str], index: int) -> str:
    start = index
    while start > 0:
        ch = chars[start - 1]
        if ch.isalnum() or ch in "._-":
            start -= 1
        else:
            break
    return "".join(chars[start:index])


def ascii_terminal_is_terminal(chars: list[str], index: int) -> bool:
    boundary_end = extend_terminal_cluster(chars, index)
    return boundary_end == len(chars) or is_ws(chars[boundary_end])


INITIALS_TOKEN_RE = re.compile(r"[A-Za-z](\.[A-Za-z])*\Z")


def preceding_period_token_start(chars: list[str], index: int) -> tuple[int, str]:
    """`preceding_period_token` plus the start offset, so a PDF rule can read the word
    before the token (`et` for `et al.`)."""
    start = index
    while start > 0:
        ch = chars[start - 1]
        if ch.isalnum() or ch in "._-":
            start -= 1
        else:
            break
    return start, "".join(chars[start:index])


def word_before(chars: list[str], start: int) -> str | None:
    """The whitespace-separated word directly before position `start`, lowercased, or None
    when the token sits at the text start or does not follow whitespace."""
    cursor = start - 1
    if cursor < 0 or not is_ws(chars[cursor]):
        return None
    while cursor >= 0 and is_ws(chars[cursor]):
        cursor -= 1
    end = cursor + 1
    while cursor >= 0 and not is_ws(chars[cursor]):
        cursor -= 1
    return "".join(chars[cursor + 1:end]).lower() or None


def ascii_period_is_terminal(chars: list[str], index: int,
                             rules: frozenset = frozenset()) -> bool:
    """`rules` names the PDF-only PDF-6g rules: `initials` (R-I) and `et_al` (R-A)."""
    if index + 1 < len(chars) and chars[index + 1] == ".":
        return False
    previous = chars[index - 1] if index >= 1 else None
    nxt = chars[index + 1] if index + 1 < len(chars) else None
    if previous is not None and previous.isascii() and previous.isdigit() \
            and nxt is not None and nxt.isascii() and nxt.isdigit():
        return False
    if previous is not None and is_identifier_char(previous) \
            and nxt is not None and is_identifier_char(nxt):
        return False
    if nxt is not None and nxt in "/\\":
        return False
    token_start, token = preceding_period_token_start(chars, index)
    token = token.lower()
    if token and token in PROTECTED_ABBREVIATIONS and index + 1 < len(chars):
        terminal_etc = (token == "etc" and ascii_terminal_is_terminal(chars, index)
                        and (next_non_whitespace(chars, extend_terminal_cluster(chars, index))
                             or "").isupper())
        if not terminal_etc:
            return False
    if "et_al" in rules and token == "al" \
            and word_before(chars, token_start) == "et" \
            and index + 1 < len(chars):
        # Like `etc`: only terminal when the next non-whitespace character is uppercase.
        terminal_al = (ascii_terminal_is_terminal(chars, index)
                       and (next_non_whitespace(
                           chars, extend_terminal_cluster(chars, index)) or "").isupper())
        if not terminal_al:
            return False
    initials = (bool(INITIALS_TOKEN_RE.match(token)) if "initials" in rules
                else len(token) == 1 and token.isascii() and token.isalpha())
    if initials and (next_non_whitespace(chars, index + 1) or "").isupper():
        return False
    return ascii_terminal_is_terminal(chars, index)


def sentence_boundary_end(chars: list[str], index: int,
                          rules: frozenset = frozenset()) -> int | None:
    ch = chars[index]
    if ch in "。！？":
        return extend_terminal_cluster(chars, index)
    if ch in "!?" and ascii_terminal_is_terminal(chars, index):
        return extend_terminal_cluster(chars, index)
    if ch == "." and ascii_period_is_terminal(chars, index, rules):
        return extend_terminal_cluster(chars, index)
    return None


def sentence_ranges(text: str, rules: frozenset = frozenset()) -> list[tuple[int, int]]:
    """The port of `sentence_ranges`: (start, end) char-index pairs inside `text`.

    `rules` is the PDF-6g addition: today's `sentence_ranges` is `rules=frozenset()`, the
    PDF rules are `rules={"initials", "et_al"}`."""
    chars = list(text)
    ranges = []
    start = 0
    while start < len(chars) and is_ws(chars[start]):
        start += 1
    cursor = start
    while cursor < len(chars):
        end = sentence_boundary_end(chars, cursor, rules)
        if end is not None:
            if start < end:
                ranges.append((start, end))
            start = end
            while start < len(chars) and is_ws(chars[start]):
                start += 1
            cursor = start
        else:
            cursor += 1
    tail_end = len(chars)
    while tail_end > 0 and is_ws(chars[tail_end - 1]):
        tail_end -= 1
    if start < tail_end:
        ranges.append((start, tail_end))
    return ranges


def sentences(text: str, rules: frozenset = frozenset()) -> list[str]:
    return [text[start:end] for start, end in sentence_ranges(text, rules)]


# --------------------------------------------------------------------------------------------
# The PDF-profile guard (PDF-6d C1 + PDF-6e R1 already shipped)
# --------------------------------------------------------------------------------------------

def counted_glue_events_pdf(text: str, context: dict | None = None) -> int:
    """`missing_space_glue_events(text, PdfProse)`: C1 already dropped, then R1's repeated-name
    re-count only when the count reaches the threshold."""
    verdict = GE.guard_verdict(text, GE.BASELINE)
    counted = verdict["glue_events"]
    if counted < MIN_EVENTS:
        return counted
    ctx = context or {"tokens": GE.token_counts(text)}
    after = GE.guard_verdict(
        text, lambda event, _ctx: GE.BASELINE(event) or GE.IMPLEMENTED(event, _ctx), ctx)
    return after["glue_events"]


def guard_fires_pdf(text: str, context: dict | None = None) -> bool:
    """`looks_like_unreliable_fallback_text(text, PdfProse)`."""
    return (GE.doubled_token(text) is not None
            or counted_glue_events_pdf(text, context) >= MIN_EVENTS
            or GE.bibliographic_markers(text) >= GE.MIN_BIBLIOGRAPHIC_MARKERS)


def fires_missing_space_only(text: str, context: dict | None = None) -> bool:
    """Only the count rule fires (no doubled, no bibliographic) - for candidate diagnostics."""
    return counted_glue_events_pdf(text, context) >= MIN_EVENTS


# --------------------------------------------------------------------------------------------
# Candidates
# --------------------------------------------------------------------------------------------

def verdict_s1(text: str, context: dict | None = None,
               rules: frozenset = frozenset()) -> dict:
    """Per-sentence: the paragraph stays coarse iff a sentence fires the unchanged guard."""
    pieces = sentences(text, rules)
    fired = [piece for piece in pieces if guard_fires_pdf(piece)]
    fires = (GE.doubled_token(text) is not None
             or GE.bibliographic_markers(text) >= GE.MIN_BIBLIOGRAPHIC_MARKERS
             or bool(fired))
    return {"fires": fires, "pieces": pieces, "firing_sentences": len(fired)}


def verdict_s2(text: str, chars_per_event: int, context: dict | None = None) -> dict:
    """Density: >= 3 counted events AND at least one event per N characters."""
    pieces = sentences(text)
    counted = counted_glue_events_pdf(text, context)
    fires = (GE.doubled_token(text) is not None
             or GE.bibliographic_markers(text) >= GE.MIN_BIBLIOGRAPHIC_MARKERS
             or (counted >= MIN_EVENTS and counted * chars_per_event >= len(text)))
    return {"fires": fires, "pieces": pieces, "firing_sentences": 0}


def verdict_s3(text: str, context: dict | None = None) -> dict:
    """Quorum-two: stays coarse iff at least two sentences fire (the loosening bound)."""
    pieces = sentences(text)
    fired = [piece for piece in pieces if guard_fires_pdf(piece)]
    fires = (GE.doubled_token(text) is not None
             or GE.bibliographic_markers(text) >= GE.MIN_BIBLIOGRAPHIC_MARKERS
             or len(fired) >= 2)
    return {"fires": fires, "pieces": pieces, "firing_sentences": len(fired)}


# --------------------------------------------------------------------------------------------
# Broken sentences, artifact approximation (records carry no section_id)
# --------------------------------------------------------------------------------------------

SENTENCE_TERMINALS = ".?!\u3002\uff1f\uff01"
TERMINAL_CLOSERS = "”’»)]}〉》】【〞〟"


def ends_with_terminal(text: str) -> bool:
    text = text.rstrip()
    while text and text[-1] in TERMINAL_CLOSERS:
        text = text[:-1].rstrip()
    return bool(text) and text[-1] in SENTENCE_TERMINALS


def norm1(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip()


# --------------------------------------------------------------------------------------------
# Loading
# --------------------------------------------------------------------------------------------

def load_records(path: str) -> tuple[dict[str, dict], dict[str, list[dict]]]:
    """document records by id, and per-id unit records sorted by unit index."""
    docs: dict[str, dict] = {}
    units: dict[str, list[dict]] = collections.defaultdict(list)
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            record = json.loads(line)
            kind = record.get("kind_record")
            if kind == "document":
                docs[record["id"]] = record
            elif kind in ("coarse_unit", "long_unit", "short_unit"):
                units[record["id"]].append(record)
    for records in units.values():
        records.sort(key=lambda record: record["unit"])
    return docs, units


def merge_causes(records: list[dict]) -> list[dict]:
    """Attach the long-unit `cause` to the coarse_unit record of the same unit index."""
    causes = {record["unit"]: record["cause"] for record in records
              if record["kind_record"] == "long_unit"}
    for record in records:
        if record["kind_record"] == "coarse_unit" and record["unit"] in causes:
            record["cause"] = causes[record["unit"]]
    return records


def projected_units(records: list[dict], freed: dict[int, list[str]]) -> list[dict]:
    """The document's projected unit stream under a candidate.

    A freed coarse paragraph expands into its sentences; every other record stands in for the
    units it represents. Indices the artifact does not record (ordinary sentence units) count
    as one unit each, neither long nor short, with unknown text.
    """
    projected = []
    for record in records:
        index = record["unit"]
        if index in freed:
            for piece in freed[index]:
                projected.append({"text": piece, "kind": "sentence",
                                  "chars": len(piece.strip())})
        else:
            kind = record["kind_record"]
            degraded = bool(record.get("degradation")) or kind == "coarse_unit"
            text = (record.get("text") or "") if degraded else (
                record.get("text") or (record.get("head") or "") +
                (record.get("tail") or ""))
            projected.append({"text": text, "kind": kind, "chars": record["chars"],
                              "coarse": degraded,
                              "non_prose": record.get("content_class") == "non_prose",
                              "paragraph_short": record.get("effective_kind") == "paragraph"
                              and record["chars"] <= 120})
    return projected


# --------------------------------------------------------------------------------------------
# PDF-6g: R-I (multi-letter initials) and R-A (et al.), priced on the paragraph stream of a
# --keep-pdf-sentence-text run
# --------------------------------------------------------------------------------------------

SPLIT_ELIGIBLE_DETAILS = {"prose_or_unknown", "native_paragraph"}


def load_pdf_paragraphs(path: str) -> tuple[dict[str, list[dict]], dict[str, dict]]:
    """`pdf_paragraph` records (ordered by `unit`) and the `pdf_stream_totals` record, per
    document id."""
    paragraphs: dict[str, list[dict]] = collections.defaultdict(list)
    totals: dict[str, dict] = {}
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            record = json.loads(line)
            kind = record.get("kind_record")
            if kind == "pdf_paragraph":
                paragraphs[record["id"]].append(record)
            elif kind == "pdf_stream_totals":
                totals[record["id"]] = record
    for records in paragraphs.values():
        records.sort(key=lambda record: record["unit"])
    return paragraphs, totals


def removed_boundaries(text: str, rules: frozenset) -> list[tuple[int, frozenset]]:
    """Old-rule boundaries the new rules suppress, each paired with the minimal rule set
    that suppresses it: `(end_char_offset, {rule, ...})`."""
    old = {end for _start, end in sentence_ranges(text)}
    new = {end for _start, end in sentence_ranges(text, rules)}
    removed = []
    for end in sorted(old - new):
        minimal = frozenset(
            rule for rule in ("initials", "et_al")
            if rule in rules and end not in
            {e for _s, e in sentence_ranges(text, frozenset({rule}))})
        removed.append((end, minimal))
    return removed


def doc_stream(paragraphs: list[dict], rules: frozenset) -> list[dict]:
    """The document's projected unit stream: each split-eligible paragraph is re-segmented
    with `rules`; every other paragraph is one unit of its own."""
    units: list[dict] = []
    for paragraph in paragraphs:
        text = paragraph["text"]
        eligible = (paragraph.get("content_class_detail") in SPLIT_ELIGIBLE_DETAILS
                    and not paragraph.get("degradation"))
        if not eligible:
            units.append({"text": text,
                          "paragraph_coarse": True,
                          "non_prose": paragraph.get("content_class") == "non_prose"})
            continue
        for start, end in sentence_ranges(text, rules):
            units.append({"text": text[start:end], "paragraph_coarse": False,
                          "non_prose": False})
    return units


def stream_metrics(paragraphs: list[dict], rules: frozenset) -> dict:
    units = doc_stream(paragraphs, rules)
    short = long = broken = 0
    last: dict | None = None
    for unit in units:
        text = unit["text"].strip()
        if len(text) < SHORT_CHARS:
            short += 1
        if len(text) > LONG_CHARS:
            long += 1
        skip = (unit["non_prose"] or not text or ends_with_terminal(text)
                or (unit["paragraph_coarse"] and len(text) <= 120))
        if last is not None and last["open"] and text[:1].islower():
            broken += 1
        last = {"open": not skip} if text else last
    total = len(units)
    return {"units": total, "short": short, "long": long, "broken": broken,
            "m1": bool(total) and long / total > 0.05,
            "short_over_10pct": bool(total) and short / total > 0.10}


def pdf6g_mode(args) -> int:
    docs, _units = load_records(args.records)
    paragraphs, totals = load_pdf_paragraphs(args.records)
    if not paragraphs:
        print("::error::no pdf_paragraph records; rerun the probe with "
              "--keep-pdf-sentence-text")
        return 1
    missing = sorted(set(docs) - set(paragraphs))
    if missing:
        print(f"# {len(missing)} document records carry no paragraphs "
              f"(not opened or flagged off): {', '.join(missing[:10])}")

    # Verbatim fidelity: under today's rules the projected stream must reproduce the
    # sentence stream the server emitted, item for item.
    checked = exact = 0
    for identifier, records in sorted(paragraphs.items()):
        total = totals.get(identifier) or {}
        if total.get("sentence_truncated") or total.get("paragraph_truncated"):
            continue
        expected = total.get("sentence_total_items")
        if expected is None:
            continue
        checked += 1
        projected = stream_metrics(records, frozenset())["units"]
        if projected == expected:
            exact += 1
        else:
            print(f"# fidelity miss {identifier}: projected {projected} units, "
                  f"server stream {expected}")
    print(f"# verbatim fidelity: {exact} of {checked} non-truncated documents reproduce "
          f"the sentence stream exactly")

    RULE_SETS = {"R-I initials": frozenset({"initials"}),
                 "R-A et al.": frozenset({"et_al"})}
    both = frozenset({"initials", "et_al"})

    base_metrics: dict[str, dict] = {}
    for identifier, records in paragraphs.items():
        base_metrics[identifier] = stream_metrics(records, frozenset())
    # Cross-check the projected baseline against the harness's own document numbers.
    drift = []
    for identifier, metric in base_metrics.items():
        doc = docs.get(identifier) or {}
        if (doc.get("broken_sentences") is not None
                and metric["broken"] != doc["broken_sentences"]):
            drift.append((identifier, doc["broken_sentences"], metric["broken"]))
    print(f"# broken-sentence drift vs the harness document records: {len(drift)} documents"
          + (f" ({drift[:8]})" if drift else ""))

    def price(rules: frozenset) -> dict:
        removed: list[dict] = []
        metrics: dict[str, dict] = {}
        for identifier, records in paragraphs.items():
            for record in records:
                if (record.get("content_class_detail") not in SPLIT_ELIGIBLE_DETAILS
                        or record.get("degradation")):
                    continue
                for end, minimal in removed_boundaries(record["text"], rules):
                    removed.append({"id": identifier, "unit": record["unit"],
                                    "end": end, "rules": sorted(minimal),
                                    "record": record})
            metrics[identifier] = stream_metrics(records, rules)
        return {"removed": removed, "metrics": metrics}

    results = {name: price(rules) for name, rules in RULE_SETS.items()}
    results["R-I+R-A"] = price(both)

    print("\n## PDF-6g rules, each priced on the paragraph stream\n")
    print("| Rule | Boundaries removed | Units merged | Papers touched | Short (<5) | "
          "Long (>800) | M1 (>5% long) | Broken sentences |")
    print("| --- | --- | --- | --- | --- | --- | --- | --- |")
    base = {"units": sum(m["units"] for m in base_metrics.values()),
            "short": sum(m["short"] for m in base_metrics.values()),
            "long": sum(m["long"] for m in base_metrics.values()),
            "m1": sum(1 for m in base_metrics.values() if m["m1"]),
            "broken": sum(m["broken"] for m in base_metrics.values()),
            "short10": [i for i, m in base_metrics.items() if m["short_over_10pct"]]}
    print(f"| today | - | - | - | {base['short']} | {base['long']} | {base['m1']} | "
          f"{base['broken']} |")
    for name, result in results.items():
        removed = result["removed"]
        papers = {item["id"] for item in removed}
        metrics = result["metrics"]
        short = sum(m["short"] for m in metrics.values())
        long = sum(m["long"] for m in metrics.values())
        m1 = sum(1 for m in metrics.values() if m["m1"])
        broken = sum(m["broken"] for m in metrics.values())
        # A removed boundary merges the two units it separated; chains merge more.
        print(f"| {name} | {len(removed)} | {len(removed)} | {len(papers)} | {short} | "
              f"{long} | {m1} | {broken} |")
        result["short10"] = [i for i, m in metrics.items() if m["short_over_10pct"]]
    print(f"\nPapers above 10% short units: today {len(base['short10'])}; "
          + "; ".join(f"{name} {len(res['short10'])}" for name, res in results.items()))

    # PDF-6f interaction: a refused prose paragraph is re-checked sentence by sentence;
    # under the new ranges a paragraph can flip to freed (or, in principle, to refused).
    flips = 0
    for identifier, records in paragraphs.items():
        for record in records:
            if (record.get("content_class_detail") not in SPLIT_ELIGIBLE_DETAILS
                    or not record.get("degradation")
                    or record.get("content_class") == "non_prose"):
                continue
            text = record["text"]
            old_verdict = verdict_s1(text, {"tokens": GE.token_counts(text)})
            new_verdict = verdict_s1(text, {"tokens": GE.token_counts(text)}, both)
            if old_verdict["fires"] != new_verdict["fires"]:
                flips += 1
    print(f"# coarse paragraphs whose PDF-6f verdict flips under the new ranges: {flips}")

    # Hand-labelling sample: up to --sample removed boundaries per rule, seeded, at most 8
    # per document.
    rng = random.Random(args.seed)
    for name in ("R-I initials", "R-A et al."):
        # Every removal the rule explains, alone or jointly with the other rule, is fair
        # to label under the rule.
        rule = {"R-I initials": "initials", "R-A et al.": "et_al"}[name]
        pool = [item for item in results["R-I+R-A"]["removed"] if rule in item["rules"]]
        rng.shuffle(pool)
        per_doc: collections.Counter = collections.Counter()
        chosen = []
        for item in pool:
            if len(chosen) >= args.sample:
                break
            if per_doc[item["id"]] >= 8:
                continue
            per_doc[item["id"]] += 1
            chosen.append(item)
        print(f"\n## {name} removed-boundary sample (seed {args.seed}): {len(chosen)} of "
              f"{len(pool)} boundaries, {len(per_doc)} documents\n")
        print("| # | Id | Unit | Context (60 chars each side) | Label |")
        print("| --- | --- | --- | --- | --- |")
        for number, item in enumerate(chosen, 1):
            text = item["record"]["text"]
            end = item["end"]
            context = norm1(text[max(0, end - 60):end + 60])
            print(f"| {number} | {item['id']} | {item['unit']} | `{context}` | |")

    if args.out:
        with open(args.out, "w", encoding="utf-8") as handle:
            json.dump({
                "fidelity": {"checked": checked, "exact": exact},
                "baseline": base,
                "rules": {
                    name: {
                        "removed": len(res["removed"]),
                        "papers": len({item["id"] for item in res["removed"]}),
                        "short": sum(m["short"] for m in res["metrics"].values()),
                        "long": sum(m["long"] for m in res["metrics"].values()),
                        "m1": sum(1 for m in res["metrics"].values() if m["m1"]),
                        "broken": sum(m["broken"] for m in res["metrics"].values()),
                    } for name, res in results.items()
                },
            }, handle, indent=1)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--records", required=True,
                        help="the shard JSONL records concatenated (all kind_record types)")
    parser.add_argument("--seed", type=int, default=23)
    parser.add_argument("--sample", type=int, default=50)
    parser.add_argument("--out", default="")
    parser.add_argument("--sample-candidate", default="S1 per-sentence")
    parser.add_argument("--pdf6g", action="store_true",
                        help="price the PDF-6g rules (R-I initials, R-A et al.) on the "
                             "pdf_paragraph records of a --keep-pdf-sentence-text run")
    args = parser.parse_args()
    if args.pdf6g:
        return pdf6g_mode(args)

    docs, units = load_records(args.records)
    for records in units.values():
        merge_causes(records)

    coarse = [record for records in units.values() for record in records
              if record["kind_record"] == "coarse_unit"]
    prose = [unit for unit in coarse if unit.get("content_class") != "non_prose"]

    print(f"# documents: {len(docs)}; coarse_unit records: {len(coarse)} "
          f"({len(prose)} prose)")

    # ---- replica agreement on the server refusals (>= 0.99 required) ---------------------
    fired = 0
    misses = []
    contexts: dict[int, dict] = {}
    for unit in coarse:
        if unit.get("content_class") == "non_prose":
            continue
        contexts[id(unit)] = {"tokens": GE.token_counts(unit["text"])}
        if guard_fires_pdf(unit["text"], contexts[id(unit)]):
            fired += 1
        else:
            misses.append(unit)
    print(f"# replica fires on {fired} of {len(prose)} prose coarse paragraphs "
          f"({fired / len(prose):.4f})")
    for unit in misses[:20]:
        print(f"#   missed {unit['id']} unit {unit['unit']} chars {unit['chars']}")

    # ---- sentence-split fidelity -----------------------------------------------------------
    # (a) stored head/tail segments of units the server emitted: a segment the port splits
    #     would be part of one server sentence -> disagreement.
    segments = split_segments = 0
    for records in units.values():
        for record in records:
            if record.get("degradation") or record["kind_record"] == "coarse_unit":
                continue  # a refused paragraph, not a served sentence
            if record["kind_record"] == "short_unit":
                text = record.get("text") or ""
                if text:
                    segments += 1
                    split_segments += len(sentence_ranges(text)) > 1
            elif record["kind_record"] == "long_unit":
                for key in ("head", "tail"):
                    text = record.get(key) or ""
                    if text:
                        segments += 1
                        split_segments += len(sentence_ranges(text)) > 1
    print(f"# fidelity A: {segments - split_segments} of {segments} stored unit segments "
          f"({(segments - split_segments) / max(1, segments):.4f}) reproduce as one piece")

    # (b) joins of consecutively recorded units: the server cut there, so the port must cut
    #     the joined text at the join position. Pairs whose left side has no terminal are
    #     paragraph-boundary artefacts of the reconstruction and are counted separately.
    joins = cut_joins = ambiguous = 0
    for records in units.values():
        by_index = {record["unit"]: record for record in records}
        for record in records:
            if record.get("degradation") or record["kind_record"] == "coarse_unit":
                continue
            left = right = None
            if record["kind_record"] == "short_unit":
                left = record.get("text") or ""
            elif record["kind_record"] == "long_unit":
                left = record.get("tail") or ""
            nxt = by_index.get(record["unit"] + 1)
            if (nxt is None or nxt["kind_record"] == "coarse_unit"
                    or nxt.get("degradation")):
                continue
            if nxt["kind_record"] == "short_unit":
                right = nxt.get("text") or ""
            else:
                right = nxt.get("head") or ""
            if not left or not right or not ends_with_terminal(left):
                ambiguous += bool(left and right and not ends_with_terminal(left))
                continue
            joins += 1
            joined = left + " " + right
            pieces = sentence_ranges(joined)
            cut_joins += any(end == len(left) for _, end in pieces)
    print(f"# fidelity B: {cut_joins} of {joins} recorded unit joins "
          f"({cut_joins / max(1, joins):.4f}) cut where the server cut; "
          f"{ambiguous} joins skipped (left side has no terminal)")

    # ---- candidates ------------------------------------------------------------------------
    missed_boundary = [unit for unit in prose if unit.get("cause") == "missed_boundary"]
    print(f"# coarse paragraphs with cause missed_boundary: {len(missed_boundary)}")

    candidates = {"S1 per-sentence": verdict_s1, "S3 quorum-two (own)": verdict_s3}
    density_ns = (100, 150, 200, 250, 300, 400, 500, 750, 1000)
    results: dict[str, dict] = {}

    def price(name: str, verdict_fn, ctx_needed: bool) -> dict:
        freed_by_doc: dict[str, dict[int, list[str]]] = collections.defaultdict(dict)
        freed = freed_chars = produced = freed_missed = short_new = 0
        for unit in prose:
            verdict = verdict_fn(unit["text"],
                                 contexts[id(unit)] if ctx_needed else None)
            if not verdict["fires"]:
                freed += 1
                freed_chars += unit["chars"]
                pieces = verdict["pieces"]
                produced += len(pieces)
                short_new += sum(1 for piece in pieces if len(piece.strip()) < SHORT_CHARS)
                if unit.get("cause") == "missed_boundary":
                    freed_missed += 1
                freed_by_doc[unit["id"]][unit["unit"]] = pieces
        results[name] = {"freed": freed, "chars": freed_chars, "sentences": produced,
                         "missed": freed_missed, "short_new": short_new,
                         "freed_by_doc": dict(freed_by_doc)}
        return results[name]

    s1 = price("S1 per-sentence", verdict_s1, True)
    s3 = price("S3 quorum-two (own)", verdict_s3, True)
    for n in density_ns:
        name = f"S2 density N={n}"
        results[name] = price(name,
                              lambda text, ctx, n=n: verdict_s2(text, n, ctx),
                              True)

    print("\n## candidate table\n")
    print("| Candidate | Paragraphs freed | Chars freed | Sentences produced | "
          "missed_boundary freed | New short (<5) |")
    print("| --- | --- | --- | --- | --- | --- |")
    for name in ("S1 per-sentence", "S3 quorum-two (own)",
                 *[f"S2 density N={n}" for n in density_ns]):
        res = results[name]
        print(f"| {name} | {res['freed']} | {res['chars']} | {res['sentences']} | "
              f"{res['missed']} | {res['short_new']} |")

    # ---- projected metrics per candidate ----------------------------------------------------
    # Recompute each paper's unit stream with freed paragraphs expanded into sentences.
    def project(name: str) -> dict:
        freed_by_doc = results[name]["freed_by_doc"]
        m1 = long_total = short_total = broken_total = 0
        broken_up = []
        for identifier, doc in docs.items():
            records = units.get(identifier, [])
            freed = freed_by_doc.get(identifier, {})
            stream = projected_units(records, freed)
            # Indices with no record at all are ordinary sentences: count them as units that
            # are neither long nor short and cannot be judged broken (unknown text).
            recorded = {record["unit"] for record in records}
            gaps = (doc.get("units") or 0) - len(recorded)
            total = len(stream) + gaps
            long_count = sum(1 for item in stream if item["chars"] > LONG_CHARS)
            short_count = sum(1 for item in stream if item["chars"] < SHORT_CHARS)
            # broken: a unit with known text, no terminal, next known-text unit starts
            # lowercase; gaps break the adjacency (unknown follower).
            broken = 0
            for idx, item in enumerate(stream):
                if item.get("non_prose") or item.get("paragraph_short"):
                    continue
                text = item["text"]
                if not text or ends_with_terminal(text):
                    continue
                follower = stream[idx + 1] if idx + 1 < len(stream) else None
                if follower is None or not follower["text"]:
                    continue
                if follower["text"].lstrip()[:1].islower():
                    broken += 1
            if total and long_count / total > 0.05:
                m1 += 1
            long_total += long_count
            short_total += short_count
            broken_total += broken
            base_broken = doc.get("broken_sentences") or 0
            if broken > base_broken:
                broken_up.append((identifier, broken - base_broken))
        return {"m1": m1, "long": long_total, "short": short_total, "broken": broken_total,
                "broken_up": sorted(broken_up, key=lambda item: -item[1])}

    # artifact approximation of today's broken count (same method, no freeing)
    results["__baseline__"] = {"freed_by_doc": {}}
    base = project("__baseline__")
    print(f"\n## projected metrics (artifact approximation; baseline broken by the same "
          f"method: {base['broken']}, harness total was computed with section ids)\n")
    print("| Candidate | M1 (>5% long) | Long units | Short units | Broken | "
          "Papers with broken up |")
    print("| --- | --- | --- | --- | --- | --- |")
    print(f"| today | {sum(1 for d in docs.values() if (d.get('long') or {}).get('share') or 0 > 0.05)} | "
          f"{sum((d.get('long') or {}).get('units') or 0 for d in docs.values())} | "
          f"{sum((d.get('short') or {}).get('units') or 0 for d in docs.values())} | "
          f"{sum(d.get('broken_sentences') or 0 for d in docs.values())} | - |")
    for name in ("S1 per-sentence", "S3 quorum-two (own)",
                 *[f"S2 density N={n}" for n in density_ns]):
        proj = project(name)
        results[name]["projected"] = proj
        print(f"| {name} | {proj['m1']} | {proj['long']} | {proj['short']} | "
              f"{proj['broken']} | {len(proj['broken_up'])} |")

    # ---- samples for hand labelling ----------------------------------------------------------
    sample_name = args.sample_candidate
    rng = random.Random(args.seed)
    s1_freed = results[sample_name]["freed_by_doc"]
    sample_docs = sorted(s1_freed)
    rng.shuffle(sample_docs)
    candidate_rows = []
    per_doc: dict[str, int] = collections.Counter()
    pool = []
    for identifier in sample_docs:
        for unit_index, pieces in sorted(s1_freed[identifier].items()):
            for position, piece in enumerate(pieces):
                pool.append((identifier, unit_index, position, piece))
    rng.shuffle(pool)
    for identifier, unit_index, position, piece in pool:
        if len(candidate_rows) >= args.sample:
            break
        if per_doc[identifier] >= 8:
            continue
        per_doc[identifier] += 1
        candidate_rows.append((identifier, unit_index, position, piece))
    print(f"\n## candidate sample ({sample_name}, seed {args.seed}): "
          f"{len(candidate_rows)} sentences from {len(per_doc)} documents\n")
    print("| # | Id | Unit | Sentence | Text (<=160) | Label |")
    print("| --- | --- | --- | --- | --- | --- |")
    for number, (identifier, unit_index, position, piece) in enumerate(candidate_rows, 1):
        print(f"| {number} | {identifier} | {unit_index} | {position + 1} | "
              f"`{norm1(piece)[:160]}` | |")

    # control: sentences the server already emits today. Only recorded units carry
    # text: the 5..800-char majority is invisible to the artifact, so the control is drawn
    # from the recorded tail (units <5 and >800 chars) and is labelled against the same
    # sentence/merged/fragment/garbled rubric.
    control_pool = []
    for identifier, records in units.items():
        for record in records:
            if record.get("degradation"):
                continue
            if record["kind_record"] == "short_unit":
                control_pool.append((identifier, record["unit"],
                                     record.get("text") or ""))
            elif record["kind_record"] == "long_unit":
                control_pool.append((identifier, record["unit"],
                                     record.get("head") or ""))
    rng.shuffle(control_pool)
    control_per_doc: dict[str, int] = collections.Counter()
    control_rows = []
    for identifier, unit_index, text in control_pool:
        if len(control_rows) >= args.sample:
            break
        if control_per_doc[identifier] >= 8 or not text:
            continue
        control_per_doc[identifier] += 1
        control_rows.append((identifier, unit_index, text))
    print(f"\n## control sample (seed {args.seed}): {len(control_rows)} units "
          f"from {len(control_per_doc)} documents\n")
    print("| # | Id | Unit | Text (<=160) | Label |")
    print("| --- | --- | --- | --- | --- |")
    for number, (identifier, unit_index, text) in enumerate(control_rows, 1):
        print(f"| {number} | {identifier} | {unit_index} | `{norm1(text)[:160]}` | |")

    if args.out:
        with open(args.out, "w", encoding="utf-8") as handle:
            json.dump({name: {key: value for key, value in res.items()
                              if key != "freed_by_doc"}
                       for name, res in results.items() if not name.startswith("__")},
                      handle, indent=1)
    return 0


if __name__ == "__main__":
    sys.exit(main())
