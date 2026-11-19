# PDF-7b-1 evidence: the walker's lines against PyMuPDF

Design §8 items 1–3 and 7, run by `.github/workflows/pdf7b-probe.yml`
(`pull_request` on `feat/pdf7b1-positional-lines`, runs 44480437692 and
44490657944) against
the 121-paper `pdf-quality` corpus. PyMuPDF is `page.get_text("dict")`:
one reference line per span line, one reference block per dict block. The
walker output is `probe_positional_page`, the same code the product path
uses.

A line **identical** means the same text after removing *all* whitespace
on both sides: stage 1 does not insert spaces at measured gaps (the
design's stage-2 rule, kept out of stage 1 by orchestrator review), so a
word run whose gap became a space on PyMuPDF's side still counts when the
characters agree. Multiset compare, not positional.

## Totals (head f92d745 — U+FFFD runs kept, counted)

| Measure | Value |
| --- | --- |
| Pages compared | 4,842 in 121 documents |
| PyMuPDF lines | 403,129 |
| Walker lines | 325,300 |
| Identical (whitespace-removed) | 181,519 (0.450 of all PyMuPDF lines) |
| Median per-document identical share | 0.616 |
| Largest glyph count on one page | 12,609 (cap 200,000) |
| Pages bailed | 206 (`unsupported_font` 206) |

Two earlier heads bracket this one: 7e3869b bailed any page with a
U+FFFD run (2,257 pages fell back, share 0.255); ef6839e dropped the
run instead, which glued its neighbours and halved the unit count
(78,349 → 14,832). The runner now keeps the run's replacement
characters like the old chunk path does and counts them in
`pdf_positional_undecodable_fragments`. The six decode-heavy documents
still compare near 0 — U+FFFD text cannot equal PyMuPDF's decoded text
— but their pages now produce positioned lines instead of unpositioned
fallback text.

## The ten required papers

| Id | PyMuPDF lines | Walker lines | Identical | Share | Bails |
| --- | --- | --- | --- | --- | --- |
| 2308.01909 | 207 | 198 | 181 | 0.874 | unsupported_font 1 |
| 2404.05778 | 652 | 569 | 525 | 0.805 | unsupported_font 2 |
| 2403.02247 | 942 | 571 | 342 | 0.363 | unsupported_font 1 |
| 2403.02626 | 2098 | 1400 | 982 | 0.468 | 0 |
| 2403.01859 | 924 | 781 | 629 | 0.681 | unsupported_font 1 |
| 2308.04244 | 879 | 716 | 518 | 0.589 | unsupported_font 1 |
| 2311.15310 | 2021 | 1814 | 1122 | 0.555 | unsupported_font 1 |
| 2401.16311 | 4516 | 4039 | 1937 | 0.429 | unsupported_font 1 |
| 2306.13140 | 1299 | 1225 | 1048 | 0.807 | unsupported_font 1 |
| 2310.11602 | 1301 | 1276 | 1063 | 0.817 | unsupported_font 1 |
| **Mean** | | | | **0.639** | |

The two-column papers in the corpus (atc14-paper-ongaro is the DIAG
fixture) read top-to-bottom per column after the consecutive-x0 band
split: bands are ordered by their highest baseline, then by x0.

## Best and worst documents

Best: 2410.05294 (1.000), 2407.10296 (1.000), 2403.19299 (0.983),
2307.01842 (0.950), 2310.11187 (0.912), 2307.11273 (0.906), 2310.09263
(0.891), 2305.10450 (0.876), 2403.08062 (0.876), 2308.01909 (0.874).

Worst: 2403.02247 (0.363), 2312.17065 (0.355), 2608.19179 (0.353),
2309.14539 (0.332), 2510.18414 (0.323), 2311.05131 (0.249), 2401.03649
(0.220); 2403.04799, 2311.05862 and 2306.17419 score 0.000 because every
page bails (`undecodable` twice, `unsupported_font` once) — lopdf's
decoders cannot name those fonts' codes at all, so the page falls back to
the old path and is not compared.

## What the gap is

The walker's own line text is faithful: on the Raft diag the emitted
lines are complete sentences in reading order. The share is driven by
where PyMuPDF puts line BOUNDARIES, not by wrong text:

- PyMuPDF emits a dict "line" per span run at a different font or size —
  an italic word, a citation number, a subscript — splitting what the
  walker (baseline + x-reach) keeps as one line.
- Six documents (2407.10296, 2403.04799, 2311.05862, 2306.17419,
  2311.18111, 2403.11395) decode to U+FFFD through lopdf's built-in
  encodings on every run but resolve through PyMuPDF's font handling;
  with runs dropped rather than bailed they emit no positional text and
  compare at 0.0–0.4, dragging the total. That is font-decoding
  coverage, not a layout gap.

Both are out of stage 1's scope: the first is the stage-3 merging work,
the second is font decoding coverage the fallback already absorbs.

## Stop-condition check

The task stops the work if fewer than 80% of the walker's lines match
PyMuPDF's on the ten papers. The mean share is **0.639** (whole corpus
0.442 against every PyMuPDF line). That is below 80%: the stage-1 layout
is shipped behind its fallback and the evidence above goes back to the
user as designed.
