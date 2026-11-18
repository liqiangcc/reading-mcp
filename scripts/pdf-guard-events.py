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


def token_bounds(chars: list[str], index: int) -> tuple[int, int]:
    """The maximal alphanumeric run around `index`, both ends included."""
    start = index
    while start > 0 and chars[start - 1].isalnum():
        start -= 1
    end = index
    while end + 1 < len(chars) and chars[end + 1].isalnum():
        end += 1
    return start, end


def run_starting_at(chars: list[str], index: int, predicate) -> int:
    run = 0
    while index + run < len(chars) and predicate(chars[index + run]):
        run += 1
    return run


def lower_run_ending_at(chars: list[str], end: int) -> int:
    run = 0
    cursor = end
    while cursor >= 0 and is_lower(chars[cursor]):
        run += 1
        cursor -= 1
    return run


def glue_events(text: str) -> list[dict]:
    """Every `missing_space_glue_events` junction, with its kind and its context.

    Each event carries, besides its kind and 12 characters of context on each side, the shape
    its exemption rules need: the alphanumeric token it sits in, the lowercase run before the
    junction, the uppercase run after it, whether that run ends its token, and for a
    punctuation junction the punctuation and the token it opens.
    """
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
        token_start, token_end = token_bounds(chars, index)
        upper_run = run_starting_at(chars, index, is_upper)
        event = {
            "kind": kind,
            "index": index,
            "left": "".join(chars[max(0, index - CONTEXT_CHARS):index]),
            "right": "".join(chars[index:index + CONTEXT_CHARS]),
            "token": "".join(chars[token_start:token_end + 1]),
            "token_start": token_start,
            "token_end": token_end,
            "upper_run": upper_run,
            "upper_ends_token": not (index + upper_run <= token_end),
            "lower_run": lower_run_ending_at(chars, index - 1),
            "punct": previous if kind == KIND_PUNCT_UPPER else "",
            "before_punct": (chars[index - 2]
                             if kind == KIND_PUNCT_UPPER and index >= 2 else ""),
            "punct_run": (uppercase_run_ending_at(chars, index - 2)
                          if kind == KIND_PUNCT_UPPER and index >= 2 else 0),
            "punct_run_starts_token": (
                kind == KIND_PUNCT_UPPER and index >= 2
                and not (index >= 3 and is_alpha(chars[index - 3]))),
            "right_token": "".join(chars[index:token_bounds(chars, index)[1] + 1]),
        }
        if kind == KIND_DIGIT_UPPER:
            shape, digit_run, digit_upper_run = digit_upper_shape(chars, index)
            event |= {"shape": shape, "digit_run": digit_run, "upper_run": digit_upper_run,
                      "upper_ends_token": not (index + digit_upper_run < len(chars)
                                               and is_alpha(chars[index + digit_upper_run]))}
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


def guard_verdict(text: str, exempt=None, context=None) -> dict:
    """The guard's verdict on one paragraph, with the evidence that produced it.

    `exempt(event, context)` drops a junction event from the count when it returns True. The
    default keeps the rule exactly as it is today.
    """
    events = glue_events(text)
    counted = [event for event in events
               if not (exempt is not None and exempt(event, context))]
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

def exempt_single_capital_ending_token(event, context=None) -> bool:
    """Candidate C1: one capital that ends its token (`7B`, `3D`, `13B-parameter`, `4K,`)."""
    return (event["kind"] == KIND_DIGIT_UPPER and event["upper_run"] == 1
            and event["upper_ends_token"])


def exempt_short_capital_run_ending_token(event, context=None) -> bool:
    """Candidate C2: one or two capitals that end the token (`7B`, `3D`, `3AB`)."""
    return (event["kind"] == KIND_DIGIT_UPPER and event["upper_run"] <= 2
            and event["upper_ends_token"])


def exempt_capital_run_ending_token(event, context=None) -> bool:
    """Candidate C3: any capital run that ends the token, however long (`7B`, `3AB`, `12ABC`)."""
    return event["kind"] == KIND_DIGIT_UPPER and event["upper_ends_token"]


def exempt_single_capital(event, context=None) -> bool:
    """Candidate C4: any single capital, wherever it sits (`7B`, `7Binstruct`)."""
    return event["kind"] == KIND_DIGIT_UPPER and event["upper_run"] == 1


def exempt_caps_run_under_three(event, context=None) -> bool:
    """Candidate C5: an uppercase run of one or two capitals, wherever it sits."""
    return event["kind"] == KIND_DIGIT_UPPER and event["upper_run"] <= 2


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

# Hand labels of PDF-6e's seeded whole-paragraph samples, keyed by `id|unit|index|kind`. The
# tables and the reasoning are in `docs/design/pdf6e-guard-main-events.md`.
EMBEDDED_LABELS: dict[str, str] = {}

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


def load_labels(path: str) -> dict:
    """Hand labels of a seeded sample, keyed by `id|unit|index|kind`."""
    if not path:
        return dict(EMBEDDED_LABELS)
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)


def label_key(unit: dict, event: dict) -> str:
    return f"{unit['id']}|{unit['unit']}|{event['index']}|{event['kind']}"


def load_coarse(path: str) -> list[dict]:
    """The `coarse_unit` records: every coarse paragraph's full text, at any length."""
    coarse = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            record = json.loads(line)
            if record.get("kind_record") == "coarse_unit":
                coarse.append(record)
    return coarse


def token_counts(text: str) -> collections.Counter:
    """The paragraph's tokens: a maximal run of alphanumeric characters, the rule the guard uses.
    """
    counts: collections.Counter = collections.Counter()
    start = None
    for index, char in enumerate(text):
        if char.isalnum():
            if start is None:
                start = index
        elif start is not None:
            counts[text[start:index]] += 1
            start = None
    if start is not None:
        counts[text[start:]] += 1
    return counts


def paragraph_contexts(coarse: list[dict]) -> dict[int, dict]:
    """The token counts a repeated-token rule would need, at both scopes.

    The paragraph scope is the text the guard is handed. The document scope is the sum over the
    document's coarse paragraphs - a lower bound on the document's own count, because the run
    keeps no other unit's text - and it would have to be computed once per document, since the
    guard itself only sees one paragraph.
    """
    by_document: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    for unit in coarse:
        by_document[unit["id"]].update(token_counts(unit["text"]))
    return {id(unit): {"tokens": token_counts(unit["text"]),
                       "document_tokens": by_document[unit["id"]]}
            for unit in coarse}


def exempt_repeated_token_paragraph(event: dict, context: dict | None = None) -> bool:
    """A camel-case token spelled identically at least twice in the same paragraph."""
    return context is not None and context["tokens"][event["token"]] >= 2


def has_capital_run(token: str) -> bool:
    """A run of two or more ASCII capitals: `GPT`, `LL`, `QA`, `DPR`."""
    return re.search(r"[A-Z]{2}", token) is not None


def exempt_repeated_token_document(event: dict, context: dict | None = None) -> bool:
    """The same, counted once per document."""
    return context is not None and context["document_tokens"][event["token"]] >= 2


def exempt_lower_upper_caps_run_ends_token(event: dict, context: dict | None = None) -> bool:
    """L1: a lowercase->uppercase junction whose uppercase run is 2+ capitals and ends its
    token (`ChatGPT`, `LLaMA`, `OpenAI`), the mirror of PDF-6d's C1 for the same reason."""
    return (event["kind"] == KIND_LOWER_UPPER and event["upper_run"] >= 2
            and event["upper_ends_token"])


def exempt_lower_upper_any_run_ends_token(event: dict, context: dict | None = None) -> bool:
    """L2: the same without the run length (`ChatGPT`, `JohnR.`, `LLaMA`)."""
    return event["kind"] == KIND_LOWER_UPPER and event["upper_ends_token"]


def exempt_lower_upper_repeated_paragraph(event: dict, context: dict | None = None) -> bool:
    return event["kind"] == KIND_LOWER_UPPER and exempt_repeated_token_paragraph(event, context)


def exempt_lower_upper_repeated_document(event: dict, context: dict | None = None) -> bool:
    return event["kind"] == KIND_LOWER_UPPER and exempt_repeated_token_document(event, context)


def exempt_lower_upper_repeated_name_token(event: dict, context: dict | None = None) -> bool:
    """R1, the implemented exemption: a lowercase->uppercase junction whose token occurs at
    least twice in the paragraph and carries a run of two or more capitals (`ChatGPT`,
    `LLaMA`, `ArabicaQA`, `CodeLLaMA`). The shape is what keeps `thecaseofRedPajama` and
    `StrongInference` counting even when they repeat."""
    return (event["kind"] == KIND_LOWER_UPPER
            and exempt_repeated_token_paragraph(event, context)
            and has_capital_run(event["token"]))


def exempt_punct_upper_initials(event: dict, context: dict | None = None) -> bool:
    """P1: a dot whose capital is the second letter of an initial or an acronym: `U.S.`,
    `J.R.`, `A.I.` - the capital before the dot starts a run of at most two capitals."""
    return (event["kind"] == KIND_PUNCT_UPPER and event["punct"] == "."
            and event["before_punct"].isupper() and event["punct_run"] <= 2
            and event["punct_run_starts_token"])


def exempt_punct_upper_initial_pair(event: dict, context: dict | None = None) -> bool:
    """P5: two single capitals around the dot, each ending its own token - `U.S.`, `J.R.`,
    `A.I.`, `C.R.` - and nothing else (`L. Lemma` keeps counting, `Lemma` is not an initial)."""
    return (event["kind"] == KIND_PUNCT_UPPER and event["punct"] == "."
            and event["punct_run"] == 1 and event["punct_run_starts_token"]
            and event["upper_run"] == 1 and event["upper_ends_token"])


def exempt_punct_upper_capital_before_dot(event: dict, context: dict | None = None) -> bool:
    """P2: any dot directly after a capital (`U.S.`, `GPT.X`)."""
    return (event["kind"] == KIND_PUNCT_UPPER and event["punct"] == "."
            and event["before_punct"].isupper())


def exempt_punct_upper_repeated_paragraph(event: dict, context: dict | None = None) -> bool:
    return event["kind"] == KIND_PUNCT_UPPER and exempt_repeated_token_paragraph(event, context)


def exempt_punct_upper_repeated_document(event: dict, context: dict | None = None) -> bool:
    return event["kind"] == KIND_PUNCT_UPPER and exempt_repeated_token_document(event, context)


MAIN_CANDIDATES = {
    "R1 repeated name-like token in the paragraph (implemented)":
        exempt_lower_upper_repeated_name_token,
    "P5 initial pair around the dot (measured, not shipped)":
        exempt_punct_upper_initial_pair,
    "L1 lowercase-prefix + 2+ capitals ending the token":
        exempt_lower_upper_caps_run_ends_token,
    "L2 lowercase-prefix + any capital run ending the token":
        exempt_lower_upper_any_run_ends_token,
    "L3 repeated token in the paragraph": exempt_lower_upper_repeated_paragraph,
    "L4 repeated token in the document": exempt_lower_upper_repeated_document,
    "P1 initials after the dot (U.S., J.R.)": exempt_punct_upper_initials,
    "P2 any capital before the dot": exempt_punct_upper_capital_before_dot,
    "P3 repeated token in the paragraph": exempt_punct_upper_repeated_paragraph,
    "P4 repeated token in the document": exempt_punct_upper_repeated_document,
}

SHAPE_CHARS = 40
GLUED_PAPERS = ("2308.01909", "2404.05778", "2403.02247")
LONG_CHARS = 800


# The guard as the server ships it today: PDF-6d's digit->uppercase narrowing is already in. A
# candidate's effect is measured on top of it, never instead of it.
BASELINE = exempt_single_capital_ending_token


def exempt_implemented(event: dict, context: dict | None = None) -> bool:
    """What PDF-6e ships: R1, and nothing else.

    P5 (the initial pair around the dot) is measured and priced but not shipped: the sentence
    splitter protects `u.s`, `u.k`, `e.g`, `i.e`, `etc`, the titles and a lone letter before a
    capital, but not `F.B.` or `C.M.`, so letting those paragraphs through chops them mid-citation.
    """
    return exempt_lower_upper_repeated_name_token(event, context)


IMPLEMENTED = exempt_implemented


def shape_of(event: dict) -> str:
    """The token shape of a junction: its token, or the punctuation and the token it opens."""
    if event["kind"] == KIND_PUNCT_UPPER:
        return event["punct"] + event["right_token"][:SHAPE_CHARS]
    return event["token"][:SHAPE_CHARS]


def full_events(coarse: list[dict]) -> list[tuple[dict, dict]]:
    """Every junction event of every coarse paragraph, with its paragraph."""
    pairs = []
    for unit in coarse:
        for event in glue_events(unit["text"]):
            pairs.append((unit, event))
    return pairs


# The seeded samples the hand labels were drawn from: the two kind samples and the candidate
# samples use different seeds, so each table states its own.
KIND_SAMPLE_SEEDS = {KIND_LOWER_UPPER: 16, KIND_PUNCT_UPPER: 17}
CANDIDATE_SAMPLE_SEED = 18


def sample_kind(coarse: list[dict], kind: str, sample_size: int, seed: int, labels: dict) -> None:
    pairs = [(unit, event) for unit, event in full_events(coarse) if event["kind"] == kind]
    rng = random.Random(seed)
    picked = rng.sample(pairs, min(sample_size, len(pairs)))
    picked.sort(key=lambda item: (item[0]["id"], item[0]["unit"], item[1]["index"]))
    print(f"## {len(picked)} `{kind}` events, seed {seed}, of {len(pairs)} in the corpus\n")
    print("| # | Id | Unit | Shape | Left | Right | Label |")
    print("| --- | --- | --- | --- | --- | --- | --- |")
    for number, (unit, event) in enumerate(picked, start=1):
        left = unit["text"][max(0, event["index"] - 15):event["index"]].replace("|", "\\|")
        right = unit["text"][event["index"]:event["index"] + 15].replace("|", "\\|")
        print(f"| {number} | {unit['id']} | {unit['unit']} | `{shape_of(event)}` | `{left}` | "
              f"`{right}` | {labels.get(label_key(unit, event), '')} |")


def sample_candidate(coarse: list[dict], name: str, sample_size: int, seed: int,
                     labels: dict) -> None:
    exempt = MAIN_CANDIDATES[name]
    contexts = paragraph_contexts(coarse)
    pairs = [(unit, event) for unit in coarse
             for event in glue_events(unit["text"])
             if exempt(event, contexts[id(unit)])]
    rng = random.Random(seed)
    picked = rng.sample(pairs, min(sample_size, len(pairs)))
    picked.sort(key=lambda item: (item[0]["id"], item[0]["unit"], item[1]["index"]))
    print(f"## {len(picked)} events exempted by {name}, seed {seed}, of {len(pairs)}\n")
    print("| # | Id | Unit | Shape | Left | Right | Label |")
    print("| --- | --- | --- | --- | --- | --- | --- |")
    for number, (unit, event) in enumerate(picked, start=1):
        left = unit["text"][max(0, event["index"] - 15):event["index"]].replace("|", "\\|")
        right = unit["text"][event["index"]:event["index"] + 15].replace("|", "\\|")
        print(f"| {number} | {unit['id']} | {unit['unit']} | `{shape_of(event)}` | `{left}` | "
              f"`{right}` | {labels.get(label_key(unit, event), '')} |")


def full_report(coarse: list[dict], sample_size: int, seed: int, labels: dict) -> None:
    """The whole-paragraph tables of PDF-6e (Step 1)."""
    contexts = paragraph_contexts(coarse)
    prose = [unit for unit in coarse if unit.get("content_class") != "non_prose"]
    raw = {id(unit): guard_verdict(unit["text"]) for unit in coarse}
    verdicts = {id(unit): guard_verdict(unit["text"], BASELINE) for unit in coarse}
    by_kind = collections.Counter()
    paragraphs_by_kind = collections.Counter()
    for unit in coarse:
        verdict = raw[id(unit)]
        by_kind.update(event["kind"] for event in verdict["events"])
        for kind in set(verdict["kinds"]):
            paragraphs_by_kind[kind] += 1

    print(f"# coarse paragraphs measured: {len(coarse)} in "
          f"{len({unit['id'] for unit in coarse})} papers, {len(prose)} of them prose")
    fires = sum(1 for unit in prose if verdicts[id(unit)]["fires"])
    print(f"# the replica fires on {fires} of {len(prose)} prose paragraphs "
          f"({fires / len(prose):.4f})")
    missed = [(unit, verdicts[id(unit)]) for unit in prose if not verdicts[id(unit)]["fires"]]
    for unit, verdict in missed[:20]:
        print(f"#   missed {unit['id']} unit {unit['unit']} chars {unit['chars']} "
              f"events {verdict['glue_events']} doubled {verdict['doubled']} "
              f"bib {verdict['bibliographic_markers']} class {unit.get('content_class')}")
    non_prose = [unit for unit in coarse if unit.get("content_class") == "non_prose"]
    print(f"# non-prose coarse paragraphs (not prose, excluded): {len(non_prose)}\n")

    print("## events per rule kind, on whole paragraphs\n")
    print("| Rule kind | Events | Paragraphs |")
    print("| --- | --- | --- |")
    for kind in (KIND_LOWER_UPPER, KIND_PUNCT_UPPER, KIND_CAPS_TAIL, KIND_PAREN_ALPHA,
                 KIND_DIGIT_UPPER):
        print(f"| `{kind}` | {by_kind[kind]} | {paragraphs_by_kind[kind]} |")
    doubled = sum(1 for unit in coarse if raw[id(unit)]["doubled"] is not None)
    biblio = sum(1 for unit in coarse
                 if raw[id(unit)]["bibliographic_markers"] >= MIN_BIBLIOGRAPHIC_MARKERS)
    print(f"| `{KIND_DOUBLED_TOKEN}` | {doubled} | {doubled} |")
    print(f"| `{KIND_BIBLIOGRAPHIC}` | {biblio} | {biblio} |")

    print("\n## which kinds push a paragraph to 3 or more\n")
    combos = collections.Counter(tuple(pushes_to_threshold(raw[id(unit)]))
                                for unit in coarse)
    print("| Rule kinds that fired | Paragraphs |")
    print("| --- | --- |")
    for kinds, count in combos.most_common():
        label = " + ".join(f"`{kind}`" for kind in kinds) if kinds else "(none)"
        print(f"| {label} | {count} |")

    print("\n## paragraphs that fall below 3 when one kind stops counting\n")
    print("| Kind that stops counting | Paragraphs freed | Still flagged | Of the 3 glued "
          "papers |")
    print("| --- | --- | --- | --- |")
    for kind in (KIND_LOWER_UPPER, KIND_PUNCT_UPPER, KIND_CAPS_TAIL, KIND_PAREN_ALPHA,
                 KIND_DIGIT_UPPER):
        freed = still = glued_still = 0
        for unit in coarse:
            before = verdicts[id(unit)]
            after = guard_verdict(unit["text"],
                                  lambda event, _context, k=kind:
                                  BASELINE(event) or event["kind"] == k)
            if before["fires"] and not after["fires"]:
                freed += 1
            if after["fires"]:
                still += 1
                if unit["id"] in GLUED_PAPERS:
                    glued_still += 1
        print(f"| `{kind}` | {freed} | {still} | {glued_still} |")

    for kind in (KIND_LOWER_UPPER, KIND_PUNCT_UPPER):
        shapes = collections.Counter()
        papers: dict[str, set] = collections.defaultdict(set)
        for unit, event in full_events(coarse):
            if event["kind"] != kind:
                continue
            shape = shape_of(event)
            shapes[shape] += 1
            papers[shape].add(unit["id"])
        print(f"\n## the 40 most frequent `{kind}` token shapes\n")
        print("| Shape | Events | Papers |")
        print("| --- | --- | --- |")
        for shape, count in shapes.most_common(40):
            print(f"| `{shape}` | {count} | {len(papers[shape])} |")
        print(f"\n# `{kind}` shapes: {len(shapes)} distinct")

    print("\n## density threshold, on whole paragraphs, measured but not implemented\n")
    print("| Events per 1,000 characters | Paragraphs freed | Paragraphs newly flagged | "
          "Still flagged | Of the 3 glued papers |")
    print("| --- | --- | --- | --- | --- |")
    for rate in (1, 2, 3, 5):
        freed = newly = still = glued_still = 0
        for unit in coarse:
            verdict = verdicts[id(unit)]
            density = verdict["glue_events"] * 1000 / max(1, len(unit["text"]))
            fires_after = (verdict["doubled"] is not None
                           or verdict["bibliographic_markers"] >= MIN_BIBLIOGRAPHIC_MARKERS
                           or density >= rate)
            if verdict["fires"] and not fires_after:
                freed += 1
            if fires_after and not verdict["fires"]:
                newly += 1
            if fires_after:
                still += 1
                if unit["id"] in GLUED_PAPERS:
                    glued_still += 1
        print(f"| {rate} | {freed} | {newly} | {still} | {glued_still} |")

    print("\n## the three papers where PDF-6a found real space loss\n")
    print("| Id | Coarse paragraphs | Of them long | Fires today |")
    print("| --- | --- | --- | --- |")
    for paper in GLUED_PAPERS:
        mine = [unit for unit in coarse if unit["id"] == paper]
        print(f"| {paper} | {len(mine)} | {sum(1 for u in mine if u['chars'] > LONG_CHARS)} | "
              f"{sum(1 for u in mine if verdicts[id(u)]['fires'])} |")
    print(f"| **total** | {sum(1 for u in coarse if u['id'] in GLUED_PAPERS)} | "
          f"{sum(1 for u in coarse if u['id'] in GLUED_PAPERS and u['chars'] > LONG_CHARS)} | "
          f"{sum(1 for u in coarse if u['id'] in GLUED_PAPERS and verdicts[id(u)]['fires'])} |")

    print("\n## what each candidate exempts, on whole paragraphs\n")
    print("| Candidate | Events exempted | Paragraphs freed | Still flagged | Of the 3 glued "
          "papers |")
    print("| --- | --- | --- | --- | --- |")
    for name, exempt in MAIN_CANDIDATES.items():
        exempted = freed = still = glued_still = 0
        for unit in coarse:
            before = verdicts[id(unit)]
            after = guard_verdict(
                unit["text"],
                lambda event, _context, ex=exempt: BASELINE(event) or ex(event, _context),
                contexts[id(unit)])
            exempted += before["glue_events"] - after["glue_events"]
            if before["fires"] and not after["fires"]:
                freed += 1
            if after["fires"]:
                still += 1
                if unit["id"] in GLUED_PAPERS:
                    glued_still += 1
        print(f"| {name} | {exempted} | {freed} | {still} | {glued_still} |")

    print("\n## the shipped exemption\n")
    exempted = freed = still = glued_still = 0
    freed_units = []
    for unit in coarse:
        before = verdicts[id(unit)]
        after = guard_verdict(
            unit["text"],
            lambda event, _context: BASELINE(event) or IMPLEMENTED(event, _context),
            contexts[id(unit)])
        exempted += before["glue_events"] - after["glue_events"]
        if before["fires"] and not after["fires"]:
            freed += 1
            freed_units.append(unit)
        if after["fires"]:
            still += 1
            if unit["id"] in GLUED_PAPERS:
                glued_still += 1
    print(f"R1 exempts {exempted} events; {freed} paragraphs fall below the threshold, "
          f"{still} stay flagged, {glued_still} of the 38 glued-paper paragraphs stay flagged.\n")
    per_paper = collections.Counter(unit["id"] for unit in freed_units)
    print("| Paper | Paragraphs freed |")
    print("| --- | --- |")
    for paper, count in per_paper.most_common(12):
        print(f"| {paper} | {count} |")
    print(f"| **total, {len(per_paper)} papers** | {freed} |")

    print("\n## the three glued papers under each candidate\n")
    print("| Candidate | 2308.01909 | 2404.05778 | 2403.02247 | Total still flagged |")
    print("| --- | --- | --- | --- | --- |")
    for name, exempt in MAIN_CANDIDATES.items():
        counts = []
        for paper in GLUED_PAPERS:
            counts.append(sum(
                1 for unit in coarse if unit["id"] == paper
                and guard_verdict(unit["text"],
                                  lambda event, _context, ex=exempt:
                                  BASELINE(event) or ex(event, _context),
                                  contexts[id(unit)])["fires"]))
        print(f"| {name} | {counts[0]} | {counts[1]} | {counts[2]} | {sum(counts)} |")

    if not labels:
        return
    print("\n## the hand-labelled samples\n")
    print("| Sample | Events | Undamaged | Glue | Garbled | Undamaged share |")
    print("| --- | --- | --- | --- | --- | --- |")
    for kind in (KIND_LOWER_UPPER, KIND_PUNCT_UPPER):
        pairs = [(unit, event) for unit, event in full_events(coarse) if event["kind"] == kind]
        kind_seed = KIND_SAMPLE_SEEDS[kind]
        rng = random.Random(kind_seed)
        picked = rng.sample(pairs, min(sample_size, len(pairs)))
        got = [labels.get(label_key(unit, event)) for unit, event in picked]
        got = [label for label in got if label]
        if not got:
            continue
        counts = collections.Counter(got)
        print(f"| `{kind}`, seed {kind_seed} | {len(got)} | {counts[LABEL_UNDAMAGED]} | "
              f"{counts[LABEL_GLUE]} | {counts['garbled']} | "
              f"{counts[LABEL_UNDAMAGED] / len(got):.2f} |")

    print("\n## each candidate against its own seeded sample\n")
    print("| Candidate | Sampled | Exempted | Undamaged | Glue | Garbled | Undamaged share |")
    print("| --- | --- | --- | --- | --- | --- | --- |")
    for name, exempt in MAIN_CANDIDATES.items():
        pairs = [(unit, event) for unit in coarse for event in glue_events(unit["text"])
                 if exempt(event, contexts[id(unit)])]
        rng = random.Random(CANDIDATE_SAMPLE_SEED)
        picked = rng.sample(pairs, min(sample_size, len(pairs)))
        got = [labels[label_key(unit, event)] for unit, event in picked
               if label_key(unit, event) in labels]
        if not got:
            continue
        counts = collections.Counter(got)
        print(f"| {name} | {len(picked)} | {len(got)} | {counts[LABEL_UNDAMAGED]} | "
              f"{counts[LABEL_GLUE]} | {counts['garbled']} | "
              f"{counts[LABEL_UNDAMAGED] / len(got):.2f} |")


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
    parser.add_argument("--coarse", default="",
                        help="the coarse_unit records of a --keep-coarse-text run (PDF-6e)")
    parser.add_argument("--text", default="", help="check one string and print its events")
    parser.add_argument("--report", action="store_true")
    parser.add_argument("--report-full", action="store_true",
                        help="the whole-paragraph tables of PDF-6e")
    parser.add_argument("--agreement", action="store_true")
    parser.add_argument("--sample", type=int, default=50)
    parser.add_argument("--seed", type=int, default=6)
    parser.add_argument("--sample-kind", default="",
                        help="print a seeded sample of one rule kind's events")
    parser.add_argument("--sample-candidate", default="",
                        help="print a seeded sample of the events a candidate would exempt")
    parser.add_argument("--labels", default="", help="a JSON file of hand labels")
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

    if args.coarse:
        coarse = load_coarse(args.coarse)
        labels = load_labels(args.labels)
        if args.sample_kind:
            sample_kind(coarse, args.sample_kind, args.sample, args.seed, labels)
            return 0
        if args.sample_candidate:
            sample_candidate(coarse, args.sample_candidate, args.sample, args.seed, labels)
            return 0
        if args.report_full:
            full_report(coarse, args.sample, args.seed, labels)
            return 0
        parser.error("--coarse needs --report-full, --sample-kind or --sample-candidate")

    if not args.units:
        parser.error("--units is required unless --text or --coarse is given")
    _, units, split = load_units(args.units)
    if args.agreement:
        agreement(units, split)
    if args.report:
        report(units, args.sample, args.seed)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
