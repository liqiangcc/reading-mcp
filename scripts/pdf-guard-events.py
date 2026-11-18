#!/usr/bin/env python3
"""Replica of the #139 fallback guard, measured over the PDF-6a per-unit artifact (PDF-6d).

`looks_like_unreliable_fallback_text` (`src/domain/segmentation.rs:1016`) is three rules:

- `contains_doubled_character_token`  - a pairwise-doubled alphabetic token (>= 6 chars);
- `missing_space_glue_events >= 3`    - the six junction shapes listed below;
- `has_bibliographic_metadata_run`    - >= 4 markers inside a 10-word window.

This script re-implements them in Python so that the events behind every coarse paragraph can be
counted, labelled and re-counted under a candidate exemption, without running the server.

It reads the `pdf-quality-shard-*` JSONL records of a `PDF quality probe` run. Those records keep
only `head` (first 300 characters) and `tail` (last 120) of each long unit, never the whole
paragraph, so every count here is a count *in the stored sample* of a paragraph, and a lower
bound on the paragraph's own count. `--agreement` reports what that costs.

    python3 scripts/pdf-guard-events.py --units .tmp/units.jsonl --report
    python3 scripts/pdf-guard-events.py --units .tmp/units.jsonl --sample 50 --seed 6
    python3 scripts/pdf-guard-events.py --text '7B and 3D and 13B-parameter'
"""

from __future__ import annotations

import argparse
import collections
import json
import random
import re
import sys

MIN_MISSING_SPACE_GLUE_EVENTS = 3
MIN_DOUBLED_RUN_CHARS = 6
MIN_BIBLIOGRAPHIC_MARKERS = 4
BIBLIOGRAPHIC_WINDOW_WORDS = 10

CONTEXT_CHARS = 12

KIND_DIGIT_UPPER = "digit_upper"
KIND_PUNCT_UPPER = "punct_upper"
KIND_PAREN_ALPHA = "paren_alpha"
KIND_CAPS_TAIL = "caps_tail"
KIND_LOWER_UPPER = "lower_upper"
KIND_DOUBLED_TOKEN = "doubled_token"
KIND_BIBLIOGRAPHIC = "bibliographic"

GLUE_KINDS = (KIND_DIGIT_UPPER, KIND_PUNCT_UPPER, KIND_PAREN_ALPHA, KIND_CAPS_TAIL,
              KIND_LOWER_UPPER)
PUNCT_BEFORE_UPPER = ",;:.!?"

DIGIT_RUN_RE = re.compile(r"\d+")
UPPER_RUN_RE = re.compile(r"\d+[^\S\n]?[A-Z]+")


# --------------------------------------------------------------------------------------------
# The three rules
# --------------------------------------------------------------------------------------------

def is_upper(ch: str) -> bool:
    """Rust `char::is_uppercase`: the Unicode Uppercase property."""
    return ch.isupper()


def is_lower(ch: str) -> bool:
    """Rust `char::is_lowercase`: the Unicode Lowercase property."""
    return ch.islower()


def is_alpha(ch: str) -> bool:
    """Rust `char::is_alphabetic`."""
    return ch.isalpha()


def is_ascii_digit(ch: str) -> bool:
    return "0" <= ch <= "9"


def uppercase_run_ending_at(chars: list[str], end: int) -> int:
    run = 0
    cursor = end
    while cursor >= 0 and is_upper(chars[cursor]):
        run += 1
        cursor -= 1
    return run


def is_surname_junction(chars: list[str], index: int) -> bool:
    if chars[index - 1] != "c":
        return False
    return ((index >= 2 and chars[index - 2] == "M")
            or (index >= 3 and chars[index - 3] == "M" and chars[index - 2] == "a"))


def digit_upper_shape(chars: list[str], index: int) -> tuple[str, int, int]:
    """The `digit-run + uppercase-run` shape of a digit->uppercase junction at `index`.

    Returns (shape, digit_run, upper_run). `shape` is the two runs as they appear.
    """
    start = index - 1
    while start >= 1 and is_ascii_digit(chars[start - 1]):
        start -= 1
    end = index
    while end + 1 < len(chars) and is_upper(chars[end + 1]):
        end += 1
    return "".join(chars[start:end + 1]), index - start, end - index + 1


def glue_events(text: str) -> list[dict]:
    """Every `missing_space_glue_events` junction, with its kind and its context."""
    chars = list(text)
    events = []
    for index in range(1, len(chars)):
        previous, current = chars[index - 1], chars[index]
        kind = None
        if is_ascii_digit(previous) and is_upper(current):
            kind = KIND_DIGIT_UPPER
        elif previous in PUNCT_BEFORE_UPPER and is_upper(current):
            kind = KIND_PUNCT_UPPER
        elif previous == ")" and is_alpha(current):
            kind = KIND_PAREN_ALPHA
        elif (is_upper(previous) and is_lower(current)
              and uppercase_run_ending_at(chars, index - 1) >= 3):
            kind = KIND_CAPS_TAIL
        elif (is_lower(previous) and is_upper(current)
              and index >= 2 and is_alpha(chars[index - 2])
              and not is_surname_junction(chars, index)):
            kind = KIND_LOWER_UPPER
        if kind is None:
            continue
        event = {
            "kind": kind,
            "index": index,
            "left": "".join(chars[max(0, index - CONTEXT_CHARS):index]),
            "right": "".join(chars[index:index + CONTEXT_CHARS]),
        }
        if kind == KIND_DIGIT_UPPER:
            shape, digit_run, upper_run = digit_upper_shape(chars, index)
            event |= {"shape": shape, "digit_run": digit_run, "upper_run": upper_run,
                      "upper_ends_token": not (index + upper_run < len(chars)
                                               and is_alpha(chars[index + upper_run]))}
        events.append(event)
    return events


def token_is_pairwise_doubled(token: str) -> bool:
    doubled_runs = 0
    doubled_chars = 0
    index = 0
    while index < len(token):
        run = 1
        while index + run < len(token) and token[index + run] == token[index]:
            run += 1
        if run == 2:
            doubled_runs += 1
            doubled_chars += 2
        index += run
    return doubled_runs >= 3 and doubled_chars * 5 >= len(token) * 4


def doubled_token(text: str) -> str | None:
    chars = list(text)
    cursor = 0
    while cursor < len(chars):
        if not is_alpha(chars[cursor]):
            cursor += 1
            continue
        start = cursor
        while cursor < len(chars) and is_alpha(chars[cursor]):
            cursor += 1
        token = "".join(chars[start:cursor])
        if len(token) >= MIN_DOUBLED_RUN_CHARS and token_is_pairwise_doubled(token):
            return token
    return None


def is_month_word(word: str) -> bool:
    return (word[:3] in ("jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct",
                         "nov", "dec")
            and all(is_alpha(ch) for ch in word))


def nearby_number(words: list[str], index: int) -> bool:
    lo = max(0, index - 2)
    hi = min(index + 3, len(words))
    for offset, word in enumerate(words[lo:hi]):
        if lo + offset != index and word[:1].isdigit():
            return True
    return False


def bibliographic_markers(text: str) -> int:
    """`has_bibliographic_metadata_run`: the best 10-word window of marker counts."""
    words = [word for word in re.split(r"[^0-9A-Za-z]+", text) if word]
    if len(words) < MIN_BIBLIOGRAPHIC_MARKERS:
        return 0
    counts = []
    for index, word in enumerate(words):
        lower = word.lower()
        next_is_number = bool(words[index + 1][:1].isdigit()) if index + 1 < len(words) else False
        count = 0
        if lower in ("vol", "volume", "no", "number", "issue", "series", "pp", "p",
                     "pages") and next_is_number:
            count += 1
        if is_month_word(lower) and nearby_number(words, index):
            count += 1
        if (len(word) == 4 and word.isdigit() and word.isascii()
                and 1800 <= int(word) <= 2099):
            count += 1
        if "http" in lower or "www." in lower or "url" in lower or lower == "doi":
            count += 1
        if lower in ("copyright", "isbn", "issn"):
            count += 1
        counts.append(count)
    window = min(BIBLIOGRAPHIC_WINDOW_WORDS, len(counts))
    best = sum(counts[:window])
    current = best
    for index in range(window, len(counts)):
        current += counts[index] - counts[index - window]
        best = max(best, current)
    return best


def guard_verdict(text: str, exempt_digit_upper=None) -> dict:
    """The guard's verdict on one paragraph, with the evidence that produced it.

    `exempt_digit_upper(event)` drops a digit->uppercase event from the count when it returns
    True. It is the PDF-6d candidate; the default keeps the rule exactly as it is today.
    """
    events = glue_events(text)
    counted = [event for event in events
               if not (exempt_digit_upper is not None
                       and event["kind"] == KIND_DIGIT_UPPER
                       and exempt_digit_upper(event))]
    doubled = doubled_token(text)
    markers = bibliographic_markers(text)
    kinds = collections.Counter(event["kind"] for event in counted)
    fires = (doubled is not None
             or len(counted) >= MIN_MISSING_SPACE_GLUE_EVENTS
             or markers >= MIN_BIBLIOGRAPHIC_MARKERS)
    return {
        "fires": fires,
        "glue_events": len(counted),
        "glue_events_unexempted": len(events),
        "kinds": dict(kinds),
        "doubled": doubled,
        "bibliographic_markers": markers,
        "events": counted,
    }


# --------------------------------------------------------------------------------------------
# Candidate exemptions
# --------------------------------------------------------------------------------------------

def exempt_single_capital_ending_token(event: dict) -> bool:
    """Candidate C1: one capital that ends its token (`7B`, `3D`, `13B-parameter`, `4K,`)."""
    return event["upper_run"] == 1 and event["upper_ends_token"]


def exempt_short_capital_run_ending_token(event: dict) -> bool:
    """Candidate C2: one or two capitals that end the token (`7B`, `3D`, `3AB`)."""
    return event["upper_run"] <= 2 and event["upper_ends_token"]


def exempt_capital_run_ending_token(event: dict) -> bool:
    """Candidate C3: any capital run that ends the token, however long (`7B`, `3AB`, `12ABC`)."""
    return event["upper_ends_token"]


def exempt_single_capital(event: dict) -> bool:
    """Candidate C4: any single capital, wherever it sits (`7B`, `7Binstruct`)."""
    return event["upper_run"] == 1


def exempt_caps_run_under_three(event: dict) -> bool:
    """Candidate C5: an uppercase run of one or two capitals, wherever it sits."""
    return event["upper_run"] <= 2


# Hand labels of the 50-event sample drawn with `--sample 50 --seed 6`, one per sampled event in
# the order the report prints them. `undamaged` is a model size, a unit, a dimension, a label or
# a figure panel; `glue` is two words merged; `garbled` is text whose code points are already
# wrong (2311.05131 is the book with the 0.328 containment), so the junction can be neither read
# nor judged. See `docs/design/pdf6d-guard-false-glue.md` for the reasoning per event.
SAMPLE_LABELS = (
    "undamaged", "undamaged", "garbled", "garbled", "garbled", "garbled", "garbled", "garbled",
    "garbled", "garbled", "garbled", "garbled", "garbled", "garbled", "garbled", "undamaged",
    "undamaged", "undamaged", "glue", "undamaged", "glue", "glue", "undamaged", "undamaged",
    "undamaged", "undamaged", "undamaged", "undamaged", "undamaged", "undamaged", "undamaged",
    "undamaged", "glue", "undamaged", "undamaged", "undamaged", "undamaged", "undamaged",
    "undamaged", "undamaged", "undamaged", "undamaged", "undamaged", "undamaged", "undamaged",
    "undamaged", "undamaged", "undamaged", "undamaged", "undamaged",
)
LABEL_UNDAMAGED = "undamaged"
LABEL_GLUE = "glue"

CANDIDATES = {
    "C1 single-capital-ending-token": exempt_single_capital_ending_token,
    "C2 <=2-capitals-ending-token": exempt_short_capital_run_ending_token,
    "C3 any-capital-run-ending-token": exempt_capital_run_ending_token,
    "C4 any-single-capital": exempt_single_capital,
    "C5 uppercase-run<=2": exempt_caps_run_under_three,
}


# --------------------------------------------------------------------------------------------
# Reports over the artifact
# --------------------------------------------------------------------------------------------

def load_units(path: str) -> tuple[list[dict], list[dict], list[dict]]:
    """The documents, the coarse long units and the long units the guard let through."""
    documents, coarse, split = [], [], []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            record = json.loads(line)
            if record.get("rec") == "doc":
                documents.append(record)
            elif record.get("rec") == "long":
                (coarse if record.get("degradation") else split).append(record)
    return documents, coarse, split


def sample_text(unit: dict) -> str:
    """The stored sample of a long unit: its 300-character head and its 120-character tail.

    The harness strips every whitespace run to one space before clipping, so no junction inside
    either part is fabricated. The two parts are joined with a blank line, so the join cannot
    fabricate a junction either.
    """
    head, tail = unit.get("head") or "", unit.get("tail") or ""
    return head if tail in head else head + "\n\n" + tail


def pushes_to_threshold(verdict: dict) -> list[str]:
    """Which rule kinds the guard needs to reach its verdict."""
    kinds = []
    if verdict["doubled"] is not None:
        kinds.append(KIND_DOUBLED_TOKEN)
    if verdict["bibliographic_markers"] >= MIN_BIBLIOGRAPHIC_MARKERS:
        kinds.append(KIND_BIBLIOGRAPHIC)
    if verdict["glue_events"] >= MIN_MISSING_SPACE_GLUE_EVENTS:
        kinds.extend(sorted(verdict["kinds"]))
    return kinds


def report(units: list[dict], sample_size: int, seed: int) -> None:
    by_kind = collections.Counter()
    shapes = collections.Counter()
    events_total = 0
    fires = 0
    per_paragraph = []
    all_events = []

    for unit in units:
        text = sample_text(unit)
        verdict = guard_verdict(text)
        events_total += len(verdict["events"])
        by_kind.update(event["kind"] for event in verdict["events"])
        fires += verdict["fires"]
        for event in verdict["events"]:
            if event["kind"] == KIND_DIGIT_UPPER:
                shapes[event["shape"]] += 1
                all_events.append((unit["id"], event))
        per_paragraph.append((unit, verdict, pushes_to_threshold(verdict)))

    print(f"# coarse long units measured: {len(units)} in "
          f"{len({unit['id'] for unit in units})} papers")
    print(f"# replica says the guard fires on {fires} of {len(units)} "
          f"({fires / len(units):.4f}) stored samples")
    print(f"# glue events in the stored samples: {events_total}\n")

    print("## events per rule kind (stored samples of all coarse paragraphs)\n")
    print("| Rule kind | Events | Paragraphs |")
    print("| --- | --- | --- |")
    paragraphs_by_kind = collections.Counter()
    for unit, verdict, _ in per_paragraph:
        for kind in set(verdict["kinds"]):
            paragraphs_by_kind[kind] += 1
    for kind, count in by_kind.most_common():
        print(f"| `{kind}` | {count} | {paragraphs_by_kind[kind]} |")
    doubled = sum(1 for _, verdict, _ in per_paragraph if verdict["doubled"] is not None)
    biblio = sum(1 for _, verdict, _ in per_paragraph
                 if verdict["bibliographic_markers"] >= MIN_BIBLIOGRAPHIC_MARKERS)
    print(f"| `{KIND_DOUBLED_TOKEN}` | {doubled} | {doubled} |")
    print(f"| `{KIND_BIBLIOGRAPHIC}` | {biblio} | {biblio} |")

    print("\n## the 30 most frequent digit-run + uppercase-run shapes\n")
    print("| Shape | Events |")
    print("| --- | --- |")
    for shape, count in shapes.most_common(30):
        print(f"| `{shape}` | {count} |")

    print("\n## which kinds push a paragraph to 3 or more\n")
    combos = collections.Counter(tuple(kinds) for _, _, kinds in per_paragraph)
    print("| Rule kinds that fired | Paragraphs |")
    print("| --- | --- |")
    for kinds, count in combos.most_common():
        label = " + ".join(f"`{kind}`" for kind in kinds) if kinds else "(none)"
        print(f"| {label} | {count} |")

    print("\n## paragraphs that fall below 3 counted events under each candidate\n")
    print("| Candidate | Freed paragraphs | Still flagged | Still flagged, of the 3 glued "
          "papers |")
    print("| --- | --- | --- | --- |")
    glued = ("2308.01909", "2404.05778", "2403.02247")
    for name, exempt in CANDIDATES.items():
        freed = still = glued_still = 0
        for unit, verdict, _ in per_paragraph:
            text = sample_text(unit)
            after = guard_verdict(text, exempt)
            if verdict["fires"] and not after["fires"]:
                freed += 1
            if after["fires"]:
                still += 1
                if unit["id"] in glued:
                    glued_still += 1
        print(f"| {name} | {freed} | {still} | {glued_still} |")

    print("\n## density threshold, measured but not implemented\n")
    print("| Events per 1,000 characters | Paragraphs freed | Still flagged | Of the 3 glued "
          "papers |")
    print("| --- | --- | --- | --- |")
    for rate in (1, 2, 3):
        freed = still = glued_still = 0
        for unit, verdict, _ in per_paragraph:
            text = sample_text(unit)
            density = verdict["glue_events"] * 1000 / max(1, len(text))
            fires = (verdict["doubled"] is not None
                     or verdict["bibliographic_markers"] >= MIN_BIBLIOGRAPHIC_MARKERS
                     or density >= rate)
            if verdict["fires"] and not fires:
                freed += 1
            if fires:
                still += 1
                if unit["id"] in glued:
                    glued_still += 1
        print(f"| {rate} | {freed} | {still} | {glued_still} |")

    print(f"\n## the three papers where PDF-6a found real space loss\n")
    print("| Id | Long units | Coarse long units | Fires today | C1 still flagged | "
          "Density 1/1000 still flagged |")
    print("| --- | --- | --- | --- | --- | --- |")
    for paper in glued:
        mine = [entry for entry in per_paragraph if entry[0]["id"] == paper]
        c1 = sum(1 for unit, _, _ in mine
                 if guard_verdict(sample_text(unit), exempt_single_capital_ending_token)["fires"])
        dense = sum(1 for unit, verdict, _ in mine
                    if verdict["doubled"] is not None
                    or verdict["bibliographic_markers"] >= MIN_BIBLIOGRAPHIC_MARKERS
                    or verdict["glue_events"] * 1000 / max(1, len(sample_text(unit))) >= 1)
        print(f"| {paper} | {len(mine)} | {len(mine)} | "
              f"{sum(1 for _, verdict, _ in mine if verdict['fires'])} | {c1} | {dense} |")

    # A seeded sample of digit->uppercase events, with the hand labels of the exemption decision.
    rng = random.Random(seed)
    picked = rng.sample(all_events, min(sample_size, len(all_events)))
    picked.sort(key=lambda item: (item[0], item[1]["index"]))
    labels = SAMPLE_LABELS if len(picked) == len(SAMPLE_LABELS) else ()
    print(f"\n## {len(picked)} digit->uppercase events, seed {seed}\n")
    print("| # | Id | Shape | Left | Right | Label |")
    print("| --- | --- | --- | --- | --- | --- |")
    for number, (paper, event) in enumerate(picked, start=1):
        left = event["left"].replace("|", "\\|")
        right = event["right"].replace("|", "\\|")
        label = labels[number - 1] if labels else ""
        print(f"| {number} | {paper} | `{event['shape']}` | `{left}` | `{right}` | "
              f"{label} |")

    if not labels:
        return
    print("\n## what each candidate would exempt, against the hand labels\n")
    print("| Candidate | Exempts of 50 | Undamaged | Glue | Garbled | Undamaged share |")
    print("| --- | --- | --- | --- | --- | --- |")
    for name, exempt in CANDIDATES.items():
        exempted = [(paper, event) for paper, event in picked if exempt(event)]
        counts = collections.Counter(labels[picked.index(item)] for item in exempted)
        undamaged = counts[LABEL_UNDAMAGED]
        share = undamaged / len(exempted) if exempted else 0.0
        print(f"| {name} | {len(exempted)} | {undamaged} | {counts[LABEL_GLUE]} | "
              f"{counts['garbled']} | {share:.2f} |")
    print("\n## events each candidate still counts, and where they are\n")
    print("| Candidate | digit->uppercase events still counted | Of the 50 sampled, glue kept |")
    print("| --- | --- | --- |")
    for name, exempt in CANDIDATES.items():
        still = sum(1 for _, event in all_events if not exempt(event))
        glue_kept = sum(1 for number, (_, event) in enumerate(picked)
                        if not exempt(event) and labels[number] == LABEL_GLUE)
        print(f"| {name} | {still} of {len(all_events)} | {glue_kept} |")


def agreement(units: list[dict], split: list[dict]) -> None:
    """Recall on the coarse paragraphs, and precision on the paragraphs the guard let through.

    The server refused to split every unit in `units`, so a sample that fires is a true positive
    by construction; how many do not fire is what the 420-character sample costs. A sample of a
    `split` unit that fires is a replica false positive: the stored sample is a subset of a
    paragraph whose whole text the guard did not flag.
    """
    fires = sum(1 for unit in units if guard_verdict(sample_text(unit))["fires"])
    missed = len(units) - fires
    counts = collections.Counter(
        min(guard_verdict(sample_text(unit))["glue_events"], MIN_MISSING_SPACE_GLUE_EVENTS)
        for unit in units if not guard_verdict(sample_text(unit))["fires"])
    false_positives = sum(1 for unit in split
                          if guard_verdict(sample_text(unit))["fires"])
    print(f"# recall on the {len(units)} coarse paragraphs: {fires} fire in their stored "
          f"sample ({fires / len(units):.4f}), {missed} do not")
    print(f"#   of those {missed}, counted glue events in the sample: "
          f"0 = {counts[0]}, 1 = {counts[1]}, 2 = {counts[2]}")
    print(f"# precision on the {len(split)} long units the guard let through: "
          f"{false_positives} fire in their stored sample ({false_positives} false positives)")
    if false_positives:
        for unit in split:
            verdict = guard_verdict(sample_text(unit))
            if verdict["fires"]:
                print(f"#   {unit['id']} unit {unit['unit']} chars {unit['chars']} "
                      f"kinds {verdict['kinds']} bib {verdict['bibliographic_markers']} "
                      f"doubled {verdict['doubled']}")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--units", default="", help="the extracted long-unit JSONL")
    parser.add_argument("--text", default="", help="check one string and print its events")
    parser.add_argument("--report", action="store_true")
    parser.add_argument("--agreement", action="store_true")
    parser.add_argument("--sample", type=int, default=50)
    parser.add_argument("--seed", type=int, default=6)
    args = parser.parse_args(argv)

    if args.text:
        verdict = guard_verdict(args.text)
        for event in verdict["events"]:
            shape = f" {event['shape']}" if "shape" in event else ""
            print(f"{event['kind']}{shape}: ...{event['left']}[{event['right']}]...")
        print(f"glue events: {verdict['glue_events']}; "
              f"doubled: {verdict['doubled']}; "
              f"bibliographic markers: {verdict['bibliographic_markers']}; "
              f"fires: {verdict['fires']}")
        return 0

    if not args.units:
        parser.error("--units is required unless --text is given")
    _, units, split = load_units(args.units)
    if args.agreement:
        agreement(units, split)
    if args.report:
        report(units, args.sample, args.seed)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
