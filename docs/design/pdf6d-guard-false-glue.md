# Which events keep a PDF paragraph coarse (PDF-6d)

Measurement for PDF-6a's ranked fix 2, then the one narrowing it justifies. Issue #232.

The #139 guard (`looks_like_unreliable_fallback_text`, `src/domain/segmentation.rs:1016`) refuses
to split a fallback paragraph that shows objective merge/corruption evidence. PDF-6a found that
2,468 of the 2,672 long units in the corpus are whole paragraphs the guard refused to split. This
note asks a narrower question: **which of the guard's evidence kinds actually decide those
verdicts, and which of them fire on text that is not damaged at all?**

Nothing in #139 is loosened. The threshold (`MIN_MISSING_SPACE_GLUE_EVENTS = 3`), the
doubled-character rule and the bibliographic rule stay exactly as they are.

## Provenance

- Artifact: the `pdf-quality-shard-*` artifacts of `PDF quality probe` run
  [`44236511100`](https://github.com/liqiangcc/reading-mcp/actions/runs/44236511100),
  head `83b66fe`, the final run of PDF-6a. 121 of 121 documents, 0 failed, 0 skipped.
- Read from the per-unit records: 121 document records, 2,672 `long_unit` records (2,468 of them
  `degradation=requested_sentence_but_paragraph_is_coarse_only`) and 10,841 `short_unit` records.
  Those totals reproduce PDF-6a's table exactly (2,672 long, 2,468 coarse, 61 papers over 5%,
  79,553 units, 10,841 short), so this is the same population.
- Nothing was compiled or run on the measuring host (disk, and CI is the only gate). The replica
  below is Python; the server numbers come from the runs.

### What the artifact keeps of a paragraph

`scripts/pdf-quality.py` stores `head` = the first 300 characters and `tail` = the last 120 of a
long unit, never the whole paragraph. Every count in this note is therefore a count **in the
420-character stored sample** of a paragraph of at least 801 characters, and a lower bound on the
paragraph's own count. The next section says what that costs.

## The replica

`scripts/pdf-guard-events.py` re-implements all three rules of the guard - the five
missing-space junction kinds, the pairwise-doubled token and the bibliographic marker window - and
records, per event, its rule kind and 12 characters of context on each side. It also carries the
five candidate exemptions below, so the same paragraph can be re-counted under each.

Two checks, both in `--agreement`:

| Check | Population | Result |
| --- | --- | --- |
| Fidelity | the six fixture strings of `tests/fallback_sentence_degradation.rs` | the replica's verdict matches the assertion in all six: the masthead, doubled, missing-space and bibliographic blobs fire; clean prose and citation prose do not |
| Precision | the 204 long units the guard **let through** (`degradation` absent) | the replica fires on **0** of them - no false positive in 204 |
| Recall | the 2,468 coarse paragraphs | the replica fires on **1,365** (0.5531) of their stored samples |

**Replica agreement = 0.5531** on the coarse paragraphs, and the shortfall is the truncation, not
the rules: of the 1,103 paragraphs whose stored sample does not fire, 439 have no counted glue
event at all in the sample and 664 have one or two (319 and 345), i.e. they need the evidence that
sits in the 48% of the paragraph the artifact does not keep. The precision row is the one that
bounds the replica's error in the other direction: a stored sample is a subset of its paragraph, so
a sample that fires proves the paragraph fires, and none of the 204 paragraphs the server did split
does. The 204 also give the only available counter-check on the rules themselves, and they pass it.

## 1. Events per rule kind, over all 2,468 coarse paragraphs

| Rule kind | Events | Paragraphs |
| --- | --- | --- |
| `lower_upper` (interior lowercase->uppercase, not a surname) | 10,393 | 1,484 |
| `punct_upper` (`,` `;` `:` `.` `!` `?` before an uppercase) | 4,734 | 1,294 |
| `caps_tail` (3+ capitals then a lowercase tail in one token) | 1,108 | 523 |
| `paren_alpha` (`)` before a letter) | 819 | 485 |
| `digit_upper` (**a digit directly before an uppercase**) | **355** | **251** |
| `doubled_token` (pairwise-doubled token) | 1 | 1 |
| `bibliographic` (>= 4 markers in a 10-word window) | 66 | 66 |

The digit->uppercase junction is **2.0% of the 17,409 counted events** and appears in 10.2% of the
coarse paragraphs. It is not what makes the guard fire in general - `lower_upper` and
`punct_upper` are - but it is the only kind that is also ordinary in undamaged AI and maths prose,
which is why PDF-6a named it.

## 2. The 30 most frequent `digit-run + uppercase-run` shapes

| Shape | Events |
| --- | --- |
| `3D` | 79 |
| `7B` | 25 |
| `2D` | 19 |
| `8Ð` | 10 |
| `2F` | 7 |
| `2A` | 6 |
| `1M` | 6 |
| `2H` | 5 |
| `2L` | 5 |
| `59E` | 4 |
| `29E` | 4 |
| `2B` | 4 |
| `2S` | 3 |
| `2C` | 3 |
| `95HD` | 3 |
| `14B` | 3 |
| `1D` | 3 |
| `43Ł` | 3 |
| `25Ł` | 3 |
| `3M` | 3 |
| `4GUO` | 2 |
| `443X` | 2 |
| `4JESSICAJAYANDBENJAMINLEES` | 2 |
| `1Q` | 2 |
| `14P` | 2 |
| `6C` | 2 |
| `10D` | 2 |
| `2X` | 2 |
| `10M` | 2 |
| `3T` | 2 |

Three families sit in that table. Model sizes and dimensions (`3D` 79, `7B` 25, `2D` 19, `2F` 7,
`2A` 6, `1M` 6, `2H` 5, `2L` 5, `14B`, `1D`, `3M`) are undamaged text. Wrong code points (`8Ð`,
`43Ł`, `25Ł`, from 2311.05131) are damage, but damage the guard cannot judge and does not need this
event for. And the merged-name runs (`4GUO` from `10GUO-DONGHONG`, `4JESSICAJAYANDBENJAMINLEES`
from an author list, `443X`) are real glue - the shapes the guard was written for.

## 3. Which kinds push a paragraph to 3 or more

Per coarse paragraph, the rule kinds the guard needs for its verdict (a paragraph can be coarse for
the doubled-character or the bibliographic rule alone, so `(none)` means the sample shows neither
and the evidence is in the part of the paragraph the artifact does not keep):

| Rule kinds that fired | Paragraphs |
| --- | --- |
| (none) | 1,103 |
| `lower_upper` + `punct_upper` | 301 |
| `caps_tail` + `lower_upper` + `paren_alpha` + `punct_upper` | 177 |
| `lower_upper` + `paren_alpha` + `punct_upper` | 157 |
| `caps_tail` + `lower_upper` + `punct_upper` | 151 |
| `punct_upper` | 124 |
| `lower_upper` | 103 |
| `paren_alpha` + `punct_upper` | 59 |
| `bibliographic` | 54 |
| `caps_tail` + `lower_upper` | 31 |
| `caps_tail` + `digit_upper` + `lower_upper` + `paren_alpha` + `punct_upper` | 30 |
| `digit_upper` + `lower_upper` | 29 |
| `digit_upper` + `lower_upper` + `punct_upper` | 26 |
| `caps_tail` + `digit_upper` + `lower_upper` + `punct_upper` | 24 |
| `digit_upper` + `lower_upper` + `paren_alpha` + `punct_upper` | 17 |
| **`digit_upper` alone** | **14** |
| `caps_tail` + `digit_upper` + `lower_upper` | 12 |
| `caps_tail` | 11 |
| `digit_upper` + `punct_upper` | 9 |
| `lower_upper` + `paren_alpha` | 6 |
| `digit_upper` + `paren_alpha` + `punct_upper` | 5 |
| `paren_alpha` | 3 |
| `digit_upper` + `lower_upper` + `paren_alpha` | 3 |
| `bibliographic` + `lower_upper` | 3 |
| `caps_tail` + `digit_upper` | 3 |
| `bibliographic` + `digit_upper` + `lower_upper` + `punct_upper` | 2 |
| `bibliographic` + `lower_upper` + `paren_alpha` + `punct_upper` | 2 |
| `bibliographic` + `punct_upper` | 2 |
| `bibliographic` + `lower_upper` + `punct_upper` | 1 |
| `digit_upper` + `paren_alpha` | 1 |
| `bibliographic` + `digit_upper` | 1 |
| `doubled_token` | 1 |
| `bibliographic` + `caps_tail` + `lower_upper` + `punct_upper` | 1 |
| `caps_tail` + `digit_upper` + `lower_upper` + `paren_alpha` | 1 |
| `caps_tail` + `lower_upper` + `paren_alpha` | 1 |

**14 paragraphs are coarse because of digit->uppercase events alone**, and 166 more carry at least
one alongside other evidence that also reaches the threshold; the remaining 71 of the 251 with a
digit->uppercase event have one or two in the sample and no third, so this table cannot list them.
Removing the junction therefore moves a few dozen paragraphs - a small payoff compared with the
1,103 the guard refuses for reasons the sample cannot even show. That is the honest size of
PDF-6a's fix 2 as this task scopes it.

## 4. What each candidate exemption frees

Five shapes were counted, on the stored samples of all 2,468 coarse paragraphs:

- **C1** a digit->uppercase junction does not count when the uppercase run is **one** capital and
  that capital **ends its token** (`7B`, `3D`, `13B-parameter`, `4K,`, `Fig. 3A`);
- **C2** the same for one or two capitals (`3AB`);
- **C3** the same for a capital run of any length (`12ABC`);
- **C4** any single capital, wherever it sits (`7Binstruct` too);
- **C5** any capital run of one or two capitals, wherever it sits.

| Candidate | Freed paragraphs | Still flagged | digit->uppercase events still counted | Still flagged, of the 3 glued papers |
| --- | --- | --- | --- | --- |
| today | - | 1,365 | 355 of 355 | 36 |
| **C1** single-capital-ending-token | **28** | **1,337** | **134** | **36** |
| C2 <=2-capitals-ending-token | 35 | 1,330 | 115 | 36 |
| C3 any-capital-run-ending-token | 44 | 1,321 | 84 | 36 |
| C4 any-single-capital | 36 | 1,329 | 59 | 36 |
| C5 uppercase-run<=2 | 43 | 1,322 | 33 | 36 |

"Freed" counts paragraphs that fire today and do not fire under the candidate, on the stored
sample: 28 paragraphs in 17 papers, the largest contributor being 2407.10296 (9 of the 28 - 22 of
its 98 coarse paragraphs fire in their stored sample today, 13 under C1; it is the 3D-printing
paper).

## 5. A seeded sample of 50 digit->uppercase events, hand-labelled

Drawn with `--sample 50 --seed 6`, so the same 50 come back every run. `undamaged` is a model size,
a unit, a dimension, a label or a figure panel; `glue` is two words merged; `garbled` is text whose
code points are already wrong (2311.05131, the 0.328-containment book), where the junction can be
neither read nor judged.

| # | Id | Shape | Left | Right | Label |
| --- | --- | --- | --- | --- | --- |
| 1 | 2301.07141 | `2L` | `, Ontario N2` | `L 2Y5, Canad` | undamaged (postal code) |
| 2 | 2306.11843 | `2V` | `base. Table2` | `Vec ( Zhang ` | undamaged (model name) |
| 3 | 2311.05131 | `1О` | `ТАЛИЗАЦИИ §1` | `Оболочкиидет` | garbled |
| 4 | 2311.05131 | `1Н` | `ТОПОЛОГИИ §1` | `Непрерывныео` | garbled |
| 5 | 2311.05131 | `24Æ` | `21 æŁß8.1.24` | `ÆßŁºØ im 8 ˆ` | garbled |
| 6 | 2311.05131 | `25Ł` | `р 239 5.1.25` | `Ł5.1.26,ßÆºŒ` | garbled |
| 7 | 2311.05131 | `16ŁŒ` | `ıææ, Œ1.3.16` | `ŁŒß,ŁŁ ŁæŒŁÆ` | garbled |
| 8 | 2311.05131 | `8Ð` | `æ b ¨ y ÐÝ 8` | `Ð i ’ p a i ` | garbled |
| 9 | 2311.05131 | `8Ð` | ` i q¨ y ÐÝ 8` | `Ð j ’ p a i ` | garbled |
| 10 | 2311.05131 | `18Ł` | `ıª. ¨ 7.4.18` | `Ł7.4 : O ‹ p` | garbled |
| 11 | 2311.05131 | `43Ł` | `æØß,Œı5.2.43` | `Ł5. 2.44,ªºŁ` | garbled |
| 12 | 2311.05131 | `43Ł` | `2.41,5. 2.43` | `Ł5.2.44Ł,ªºŁ` | garbled |
| 13 | 2311.05131 | `44Ł` | ` 2.43Ł5.2.44` | `Ł,ªºŁæºæ, æº` | garbled |
| 14 | 2311.05131 | `32Ł` | `(ii),Ł6.1.32` | `Łªæº (6.2.25` | garbled |
| 15 | 2311.05131 | `32Ł` | `(ii),Ł6.1.32` | `Łªæº (6.2.25` | garbled |
| 16 | 2312.14388 | `21K` | `H03595, JP21` | `K19767, JST/` | undamaged (grant id) |
| 17 | 2312.15179 | `4M` | ` Tripura 2.4` | `M 60 0.42 0.` | undamaged (unit) |
| 18 | 2312.16335 | `10M` | `ing L 200-10` | `M, the bench` | undamaged (model size) |
| 19 | 2401.01137 | `8GUO` | `8` | `GUO-DONGHONG` | glue (running header) |
| 20 | 2401.02387 | `10H` | `ared ACF, 10` | `Hz (g) Squar` | undamaged (unit) |
| 21 | 2401.16311 | `4JESSICAJAYANDBENJAMINLEES` | `4` | `JESSICAJAYAN` | glue (author list) |
| 22 | 2401.16311 | `12JESSICAJAYANDBENJAMINLEES` | `12` | `JESSICAJAYAN` | glue (author list) |
| 23 | 2403.02247 | `7B` | `l:Aneﬃcient7` | `Binstruct-mo` | undamaged (model size) |
| 24 | 2403.02626 | `40B` | `o. Falcon-40` | `B: an open l` | undamaged (model size) |
| 25 | 2403.02642 | `3D` | `the RELLIS-3` | `D dataset [1` | undamaged (dataset name) |
| 26 | 2403.03883 | `7B` | `el, SaulLM-7` | `B , has been` | undamaged (model size) |
| 27 | 2403.08062 | `474GB` | `f RAM ad 474` | `GB of instan` | undamaged (unit) |
| 28 | 2403.17848 | `7B` | ` Mixtral 8x7` | `B utrality ` | undamaged (model size) |
| 29 | 2403.18158 | `2F` | `= 60 VCSL F2` | `F mAP 0.52 0` | undamaged (F2F metric) |
| 30 | 2403.18158 | `2F` | `= 60 VCSL F2` | `F mAP 0.52 0` | undamaged (F2F metric) |
| 31 | 2403.18575 | `3D` | `e used for 3` | `D hand-mesh ` | undamaged (dimension) |
| 32 | 2403.19144 | `3DMM` | `ble Model (3` | `DMM) [ 4 ]. ` | undamaged (model name) |
| 33 | 2406.06945 | `12P` | `12` | `Probabilisti` | glue (list number) |
| 34 | 2407.10296 | `3D` | `r creating 3` | `D models and` | undamaged (dimension) |
| 35 | 2407.10296 | `3D` | `ng them on 3` | `D printers i` | undamaged (dimension) |
| 36 | 2407.10296 | `3D` | `e field of 3` | `D printing ` | undamaged (dimension) |
| 37 | 2407.10296 | `3D` | ` basics of 3` | `D scanning a` | undamaged (dimension) |
| 38 | 2407.10296 | `3D` | `tting up a 3` | `D printer an` | undamaged (dimension) |
| 39 | 2407.10296 | `3D` | `d printing 3` | `D models rem` | undamaged (dimension) |
| 40 | 2407.10296 | `3D` | `ks to make 3` | `D modeling e` | undamaged (dimension) |
| 41 | 2407.10296 | `3D` | `r creating 3` | `D models He` | undamaged (dimension) |
| 42 | 2407.10296 | `3D` | `g the KScan3` | `D program: M` | undamaged (product name) |
| 43 | 2407.10296 | `3D` | `bility for 3` | `D scanning [` | undamaged (dimension) |
| 44 | 2407.10296 | `3D` | `ments with 3` | `D interactio` | undamaged (dimension) |
| 45 | 2502.07239 | `2G` | `oMimicV2, S2` | `G-Diﬀusion, ` | undamaged (model name) |
| 46 | 2505.21329 | `5A` | `ology_2LWSQ5` | `A2.csv Candi` | undamaged (file name) |
| 47 | 2603.17845 | `2D` | `e Contrast 2` | `D OmniPose (` | undamaged (dimension) |
| 48 | 2603.17845 | `2D` | `uorescence 2` | `D Usiigaci (` | undamaged (dimension) |
| 49 | 2604.10609 | `2A` | `f HEK and N2` | `A cells resp` | undamaged (cell line) |
| 50 | 2604.10609 | `2A` | `f HEK and N2` | `A cells resp` | undamaged (cell line) |

**33 undamaged, 4 glue, 13 garbled.** Per candidate, over the same 50:

| Candidate | Exempts of 50 | Undamaged | Glue | Garbled | Undamaged share |
| --- | --- | --- | --- | --- | --- |
| **C1** single-capital-ending-token | 35 | 28 | **0** | 7 | 0.80 |
| C2 <=2-capitals-ending-token | 36 | 29 | **0** | 7 | 0.81 |
| C3 any-capital-run-ending-token | 40 | 30 | **3** | 7 | 0.75 |
| C4 any-single-capital | 44 | 31 | **1** | 12 | 0.70 |
| C5 uppercase-run<=2 | 46 | 32 | **1** | 13 | 0.70 |

The column that decides it is **Glue**: C1 and C2 exempt none of the four real glue events in the
sample, C3 loses three of them (`8GUO`, `4JESSICA...`, `12JESSICA...`), C4 and C5 lose the list
number `12P`. The four the sample keeps under C1 are exactly the shapes the guard exists for, and
`3642SCIENCE` is safe in all five candidates because its uppercase run is 7.

C1 and C2 differ by one event: `3DMM` (event 32, undamaged, a model name) has a two-capital run, so
C2 exempts it and C1 does not. C1 is the narrower rule and the one this task takes.

The garbled seven are the reason the undamaged share is 0.80 and not higher. They are not a
counter-argument: the paragraphs they come from are 2311.05131's, and C1 leaves **485 of that
book's 487 coarse paragraphs flagged** (it is the book with 0.328 containment and 37 Type1 fonts
without `/ToUnicode`), so the exemption cannot un-protect text the guard is right about.

## 6. The three papers where PDF-6a found real space loss

2308.01909 (whitespace ratio 0.283), 2404.05778 (0.386) and 2403.02247 (0.418). Their long units:

| Id | Long units | Coarse long units | Fires in the stored sample today | Under C1 | Under a 1-event/1,000-char density rule |
| --- | --- | --- | --- | --- | --- |
| 2308.01909 | 5 | 5 | 5 | 5 | 5 |
| 2404.05778 | 16 | 16 | 15 | 15 | 16 |
| 2403.02247 | 16 | 16 | 16 | 16 | 16 |
| **total** | **37** | **37** | **36** | **36** | **37** |

C1 changes nothing for the three papers: every one of their coarse paragraphs stays coarse. The
one 2404.05778 paragraph that does not fire in its stored sample is flagged on evidence outside the
sample; the density column counts it because density 1/1,000 is satisfied by 1 event in 420
characters, which shows how the sample distorts that measurement (see below).

## 7. Measured but not implemented: a density threshold

Replacing the absolute count with events per 1,000 characters is a change to #139 itself, so it is
reported and not implemented. On the stored samples (420 of >= 801 characters, so the sample's
density is an **upper bound** on the paragraph's):

| Events per 1,000 characters | Paragraphs freed | Still flagged |
| --- | --- | --- |
| 1 | 0 | 2,029 |
| 2 | 0 | 2,029 |
| 3 | 0 | 1,710 |

No paragraph is freed at any of the three rates, and 1 and 2 flag 664 more than today: a
420-character sample with 1 event is 2.4 events per 1,000, which a whole 2,000-character paragraph
would need 5 events to reach. The direction of the answer is therefore "a density rule is not a
loosening at these rates", but the artifact cannot price it on whole paragraphs, and the three
glued papers' paragraphs all stay flagged at all three rates (37 of 37, above). This is a question
for the user, with a full-text run as its precondition, not a change in this PR.

## 8. The narrowing

Taken from the data, and the only one:

> **A digit->uppercase junction does not count when the uppercase run is exactly one capital and
> that capital ends its token** - the next character is not alphabetic. So `7B`, `7B,`,
> `13B-parameter`, `3D`, `4K,`, `Fig. 3A` no longer count; `3642SCIENCE`, `4GUO`,
> `4JESSICA...`, `7Binstruct`, `5Gnetwork` and `474GB` (a two-capital run) still do.

It is PDF-only, it applies to nothing else in the guard, and it removes 221 of the 355
digit->uppercase events measured, keeping all four real glue events in the hand sample. It does
not remove every false positive - `474GB` (event 27) is a unit and still counts, because a
two-capital run is also the shape of `10GUO-DONGHONG`. That is the price of not weakening the
rule past what the sample proves.

### Why it keeps #139's promise

#139 says glued, damaged text must never be handed out as a clean sentence. C1 removes a junction
only when a single capital terminates a token, which is the shape of a size, a unit, a dimension or
a panel label; the shapes that carry real glue - a capital run of two or more, or a capital
continued by lowercase - are untouched. On the evidence:

- 0 of the 4 real glue events in the 50-event sample are exempted;
- 485 of 2311.05131's 487 coarse paragraphs stay flagged;
- all 37 coarse paragraphs of the three papers where PDF-6a measured real space loss stay flagged;
- the 204 long units the guard lets through are still not flagged (no verdict changes from
  split to coarse either - the exemption can only ever remove evidence).
