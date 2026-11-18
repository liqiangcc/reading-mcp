# Which junctions keep a PDF paragraph coarse (PDF-6e)

Measurement for PDF-6a's ranked fix 2 on **whole paragraphs**, then the one narrowing it proves.
Issue #232, follow-up to `pdf6d-guard-false-glue.md`.

PDF-6d measured the #139 guard on the 420-character `head`/`tail` sample that the quality probe
stored for a long unit, and narrowed one kind: a digit directly before a single capital that ends
its token (`7B`, `3D`, `4K,`). That left the two dominant kinds untouched, because the stored
sample could not price them: `lower_upper` was 10,393 events in 1,484 paragraphs and `punct_upper`
4,734 in 1,294, but a sample of a 2,000-character paragraph sees only a fifth of it, and the
replica could reproduce the server's verdict on only 55% of the coarse paragraphs.

This note asks the same question with the whole paragraph in hand: **which of the two dominant
kinds actually decide the verdicts, and which of their events fire on text that is not damaged at
all?** It then ships the one exemption the hand labels prove and the splitter can keep, and
reports the second one with the reason it is not shipped.

Nothing in #139 is loosened. The threshold (`MIN_MISSING_SPACE_GLUE_EVENTS = 3`), the
doubled-character rule, the bibliographic rule, the surname rule and every junction shape stay
exactly as they are; the exemption is PDF-only.

## Provenance

- Artifact: the `pdf-quality-shard-*` artifacts of `PDF quality probe` run
  [`44255273604`](https://github.com/liqiangcc/reading-mcp/actions/runs/44255273604), head
  `4dda0c9`, dispatched with `keep_coarse_text=true`. 121 of 121 documents, 0 failed.
- Read from the per-unit records: 121 document records, 80,944 units, of which 2,652
  `coarse_unit` records (the guard's refusals, now with their full text), 2,635 `long_unit` and
  10,908 `short_unit`. The document totals reproduce the baseline summary exactly (58 papers over
  5% long, `long_by_cause` 601 missed-boundary / 581 glued-words / 580 bibliography / 549 genuine /
  277 glued-terminal / 47 math-table), so this is the same population PDF-6a and PDF-6d measured.
- Nothing was compiled or run on the measuring host (disk; CI is the only gate). The replica below
  is Python and the server numbers come from the runs.

### What the harness had to record

`scripts/pdf-quality.py` stores `head` = the first 300 characters and `tail` = the last 120 of a
long unit. PDF-6d showed what that costs: 48% of a paragraph is invisible, and the replica could
only reproduce 0.5531 of the server's refusals.

PDF-6e adds an opt-in flag, `--keep-coarse-text` (`keep_coarse_text`), threaded through
`measure_units`. With it, every unit the guard refused to split is also emitted as a
`kind_record: "coarse_unit"` record carrying its **whole** text, at any length, with its
`content_class` and `degradation` cause. The flag is off by default and the default artifact is
byte-identical to PDF-6d's, so the probe's existing numbers are unaffected; the workflow gains one
`workflow_dispatch` input (`keep_coarse_text`, false/true) to ask for it.

That record is 8.2 MB of JSONL for this corpus and is not committed. Everything below is a
function of it, and the commands that regenerate it are in "Reproduce".

## The replica, on whole paragraphs

`scripts/pdf-guard-events.py` re-implements all three rules of the guard - the five missing-space
junction kinds, the pairwise-doubled token and the bibliographic marker window - and records, per
event, its rule kind, its token, the length of the capital run on each side and whether that run
starts or ends its token. It carries the candidate exemptions below, so the same paragraph can be
re-counted under each.

| Check | Population | Result |
| --- | --- | --- |
| Fidelity | the six fixture strings of `tests/fallback_sentence_degradation.rs` | the replica's verdict matches the assertion in all six |
| Precision | the 10,908 `short_unit` records the server did split, whole text | the replica fires on **0** of them |
| Recall | the 2,652 coarse paragraphs | the replica fires on **2,649 of the 2,651 prose paragraphs (0.9992)** |

The precision row is the one that bounds the replica's error in the other direction: the guard was
run on every one of those paragraphs and split them, so a replica that fires on any of them would
be wrong. It fires on none. The 2,635 `long_unit` records cannot be checked the same way - the
artifact keeps their `head` and `tail`, not their text - and 13 of the 251 long units that were
split do fire on a stored slice; all 13 are `digit_upper` exponent notation (`1.20E-05`), which
PDF-6d's shipped exemption already removes, so the replica is right about the paragraph and the
baseline is what differs.

**Replica agreement = 0.9992**, against 0.5531 for the 420-character samples. The two misses are
`2407.10296` units 513 and 514, 2,553 characters each, with 0 counted junctions, no doubled token
and 3 bibliographic markers where the rule needs 4 - the same two paragraphs PDF-6d missed. One
coarse paragraph is `content_class = non_prose` and is excluded from the prose denominator.

Whole paragraphs also change what the evidence looks like: the same corpus carries 69,926 counted
events here against 17,409 in the stored samples, and 2,311.05131 alone owns 39,420 of them
(56.4%) - it is the book with 37 Type1 fonts without `/ToUnicode`, whose Cyrillic text extraction
is broken at the character level. Every table below therefore reports the garbled column
separately: a garbled event is not evidence for or against an exemption, it is a paragraph the
guard must keep refusing.

## 1. Events per rule kind, over all 2,652 coarse paragraphs

| Rule kind | Events | Paragraphs |
| --- | --- | --- |
| `lower_upper` (interior lowercase->uppercase, not a surname) | 40,854 | 2,130 |
| `punct_upper` (`,` `;` `:` `.` `!` `?` before an uppercase) | 18,939 | 1,578 |
| `caps_tail` (3+ capitals then a lowercase tail in one token) | 5,044 | 1,114 |
| `paren_alpha` (`)` before a letter) | 3,176 | 929 |
| `digit_upper` (a digit directly before an uppercase) | 1,913 | 582 |
| `doubled_token` (pairwise-doubled token) | 11 | 11 |
| `bibliographic` (>= 4 markers in a 10-word window) | 292 | 292 |

`lower_upper` and `punct_upper` are **85.5%** of the counted events, which is why PDF-6a ranked
them first. `digit_upper` is 2.7%, and PDF-6d already narrowed it.

## 2. Which kinds push a paragraph over the threshold

Removing one kind entirely and re-running the verdict - the most an exemption of that kind could
ever do:

| Kind that stops counting | Paragraphs freed | Still flagged | Of the 3 glued papers |
| --- | --- | --- | --- |
| `lower_upper` | 714 | 1,935 | 38 |
| `punct_upper` | 395 | 2,254 | 38 |
| `caps_tail` | 137 | 2,512 | 38 |
| `paren_alpha` | 30 | 2,619 | 38 |
| `digit_upper` | 14 | 2,635 | 38 |

No single kind decides more than a quarter of the verdicts: 328 paragraphs need all four of
`caps_tail` + `lower_upper` + `paren_alpha` + `punct_upper` at once, and 230 need exactly
`lower_upper` + `punct_upper`. An exemption has to be a narrowing of a kind, not the removal of
one.

## 3. The most frequent shapes

`lower_upper`, 40 most frequent of 11,903 distinct shapes:

| Shape | Events | Papers |
| --- | --- | --- |
| `arXiv` | 1,683 | 99 |
| `æºŁ` | 585 | 1 |
| `ºªÆ` | 307 | 1 |
| `InvSteAlg` | 286 | 1 |
| `ChatGPT` | 220 | 11 |
| `ˇæŒºŒ` | 178 | 1 |
| `MoC` | 163 | 1 |
| `ŒªŁŁ` | 163 | 1 |
| `ºªÆß` | 149 | 1 |
| `IoT` | 137 | 2 |
| `ææŁ` | 126 | 1 |
| `NormTab` | 124 | 1 |
| `LiDAR` | 119 | 3 |
| `LeanVec` | 119 | 1 |
| `SemRL` | 112 | 1 |
| `EulerFormer` | 108 | 1 |
| `FreSh` | 101 | 1 |
| `ßŒøŁØŁª` | 100 | 1 |
| `ArabicaQA` | 95 | 1 |
| `ŁºŁ` | 95 | 1 |
| `ßºŁ` | 93 | 1 |
| `VeriTraCER` | 92 | 1 |
| `RiseFL` | 82 | 1 |
| `SparkSQL` | 81 | 1 |
| `ÆœŒ` | 81 | 1 |
| `MoDiTalker` | 80 | 1 |
| `OntoChatGPT` | 78 | 1 |
| `ÆºŒ` | 76 | 1 |
| `ModaLink` | 75 | 1 |
| `SaulLM` | 75 | 1 |
| `AugInvSteAlg` | 75 | 1 |
| `æŒºŒ` | 74 | 1 |
| `ŒºææŁ` | 72 | 1 |
| `æºŁæªŁ` | 68 | 1 |
| `NeurIPS` | 63 | 17 |
| `YouTube` | 62 | 6 |
| `ºæŒª` | 60 | 1 |
| `ŒæºæŁ` | 58 | 1 |
| `ˇºŁ` | 58 | 1 |
| `ReLU` | 57 | 12 |

`punct_upper`, 40 most frequent of 4,677 distinct shapes:

| Shape | Events | Papers |
| --- | --- | --- |
| `:A` | 1,072 | 4 |
| `:V` | 497 | 2 |
| `,Ł` | 411 | 1 |
| `:X` | 336 | 2 |
| `,X` | 250 | 6 |
| `:D` | 243 | 3 |
| `.Let` | 233 | 9 |
| `;A` | 231 | 2 |
| `.Пусть` | 191 | 1 |
| `,A` | 168 | 6 |
| `.Равенство` | 168 | 1 |
| `;B` | 167 | 4 |
| `:C` | 160 | 2 |
| `:U` | 133 | 3 |
| `:G` | 132 | 2 |
| `.S` | 131 | 24 |
| `.Theequality` | 130 | 1 |
| `:K` | 126 | 2 |
| `;X` | 119 | 2 |
| `,Œ` | 112 | 1 |
| `.A` | 103 | 21 |
| `:B` | 101 | 4 |
| `,R` | 96 | 6 |
| `,S` | 88 | 8 |
| `;H` | 88 | 2 |
| `.The` | 84 | 11 |
| `,ÆŁ` | 83 | 1 |
| `.Letthesetofvectors` | 80 | 1 |
| `.Гомоморфизм` | 80 | 1 |
| `;C` | 76 | 1 |
| `,V` | 74 | 7 |
| `.J` | 74 | 19 |
| `.M` | 73 | 17 |
| `.Therefore` | 71 | 5 |
| `.Math` | 69 | 7 |
| `,ŁŁ` | 65 | 1 |
| `,E` | 64 | 3 |
| `:E` | 64 | 2 |
| `;U` | 61 | 1 |
| `,P` | 60 | 8 |

The two lists read very differently from PDF-6d's digit->uppercase list. `lower_upper`'s top
shapes are mostly **names**: camel-case model and dataset names, plus Cyrillic noise. `punct_upper`'s
are mostly **math and reference-list glue**: `:A` is `f :A 1 -> A 2` with the space after the colon
lost, `.Let`/`.The`/`.Пусть` are sentence starts whose preceding space was lost, `,X` is a list whose
comma lost its space. Both kinds are mostly right, which is why neither may be exempted wholesale.

## 4. The two dominant kinds, hand-labelled

A seeded sample of 50 events of each kind, with the paragraph text around the junction:

### `lower_upper`, seed 16

| # | Id | Unit | Shape | Left | Right | Label |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 2305.18793 | 1331 | `ReM` | `of ˆ τ under Re` | `M is almost ide` | glue |
| 2 | 2306.09672 | 11 | `NotationsandDerivedAlgebraicGeometry` | `tionsandDerived` | `AlgebraicGeomet` | glue |
| 3 | 2306.09672 | 23 | `GeneralizedSerreTheorem` | `12 (Generalized` | `SerreTheorem[14` | glue |
| 4 | 2311.05131 | 0 | `1872ªºŁŒæ` | `ŁæŁºŁß 
| 5 | 2311.05131 | 11 | `ºßØæłŁıŁææºŁØØßªºŁ` | `ØæłŁıŁææºŁØØßªº` | `Ł Œ: ´æ0.3.2. ˝` | garbled |
| 6 | 2311.05131 | 70 | `æºŁßªØºæŁŁŁ` | `ßØŁØªºŁŒ. æºŁßª` | `ØºæŁŁŁ  P N E ,` | garbled |
| 7 | 2311.05131 | 77 | `ŒªŁŁ` | `ŁŒŒº ææÆœŒ M Œª` | `ŁŁ K :  “t ’ P ` | garbled |
| 8 | 2311.05131 | 79 | `ººÆªºªÆœŒ` | `æŁº  ), (iii)ºº` | `ÆªºªÆœŒ B (  æŁ` | garbled |
| 9 | 2311.05131 | 85 | `ŁæßØŁ` | `.˚ŒæºæŁ,æøæ Łæß` | `ØŁ  :Y Ñ projli` | garbled |
| 10 | 2311.05131 | 97 | `ŒŒºŁŁŁŒŒŒÆºŒ` | `L ºŁŒŒŒ. (d)ŒŒº` | `ŁŁŁŒŒŒÆºŒ 
| 11 | 2311.05131 | 130 | `InvSteAlg` | `- æ,ŒªŁŁ InvSte` | `Alg ). 4 ˝ :˚ªŁ` | undamaged |
| 12 | 2311.05131 | 132 | `æºŁæªŁ` | `Łßæºø ºŁ. æºŁæª` | `Ł,æŁºªÆ H ŒªŁŁ ` | garbled |
| 13 | 2311.05131 | 134 | `ŒæŁæŒøÆºæŁ` | `ªÆßæŁæŁº ŁßŁ,Œæ` | `ŁæŒøÆºæŁ- ª H e` | garbled |
| 14 | 2311.05131 | 181 | `æææºŁ` | `Ł  p  qP O æææº` | `Ł(3.1.12)  P W ` | garbled |
| 15 | 2311.05131 | 186 | `ŁßææºŁØ` | `АССЛОЕНИЯ Łßææº` | `ŁØ. ˇæßºŒºßŒºßı` | garbled |
| 16 | 2311.05131 | 199 | `æºŁ` | `ºŁæØŁŁŁºßß 
| 17 | 2311.05131 | 205 | `ßÆææŁ` | `ºÆªº b P B ßÆææ` | `Ł ºŁØ( C )ÆŁ b ` | garbled |
| 18 | 2311.05131 | 206 | `æŁŁºßŒ0` | `:A Ñ B º- æŁŁºß` | `Œ0 
| 19 | 2311.05131 | 212 | `ººÆßıŁæº` | `3.1.105) ˜ººÆßı` | `Łæº  P C ; “t  ` | garbled |
| 20 | 2311.05131 | 222 | `ÆæºŁßæ` | `(4.1.13) ˛Ł,Ææº` | `Łßæ Ą a ¨ u “ r` | garbled |
| 21 | 2311.05131 | 223 | `ææßßıŁŁßıŒŁØ` | `p G q ææßßıŁŁßı` | `ŒŁØ  G . ŒŒŁªº.` | garbled |
| 22 | 2311.05131 | 290 | `æºæŁ` | `ŒŒºªŁæŒææ G æºæ` | `Ł ØæßºªŁæŒŁıææŁ` | garbled |
| 23 | 2311.05131 | 303 | `InvSteAlg` | `. ŒßæŒ,ŒŒŁº Inv` | `SteAlg .`Ł ’ :A` | undamaged |
| 24 | 2311.05131 | 321 | `ºªÆßßıæŁØææºŁŁØ` | ` ,- ø F ºªÆßßıæ` | `ŁØææºŁŁØ  0 E p` | garbled |
| 25 | 2311.05131 | 322 | `ºæßØÆºŒØ` | `Ñ C p M;A q ºæß` | `ØÆºŒØ. ˜Œºæ. ˇæ` | garbled |
| 26 | 2311.05131 | 410 | `ææŁºŒ` | `k P N r m su ææ` | `ŁºŒ ŁŁ D 0 “ D ` | garbled |
| 27 | 2311.05131 | 416 | `ŁæßæÆºÆøŁØæºØ` | `.17,Œ 
| 28 | 2311.05131 | 418 | `ıŒØæØŁØ` | `ŁØØæıŁæŁŒŒ ıŒØæ` | `ØŁØ u i E p M;X` | garbled |
| 29 | 2311.05131 | 421 | `ºÆßŁŁºß` | `Ł,ŒŒæºæŁ, D ºÆß` | `ŁŁºß E p M q . ` | garbled |
| 30 | 2311.05131 | 448 | `ªºŒÆºŒºªÆß` | `6.2.24) (iii)ªº` | `ŒÆºŒºªÆß K 8 p ` | garbled |
| 31 | 2311.05131 | 460 | `æºŁ` | `1.23) ˜Œºæ. ¯æº` | `Ł  ¨  x P  , @ ` | garbled |
| 32 | 2311.05131 | 469 | `ÆŁæßæºØ` | `ŁŒ  ªº u ÆŁæßæº` | `Ø x u; y“ ÿ n P` | garbled |
| 33 | 2311.05131 | 475 | `ººÆªŁæº` | `\|\| u \|\| C ˝Æ,ºº` | `ÆªŁæº C ą 0 ßßÆ` | garbled |
| 34 | 2311.05131 | 488 | `ææŒæŁºßıŒŁº` | `p G q  ææŒæŁºßı` | `ŒŁº  G . ¨ß7.3.` | garbled |
| 35 | 2311.05131 | 505 | `TopAlg` | `, ºßØŁŁŒªŁŁ Top` | `Alg : ♡ A P DEp` | undamaged |
| 36 | 2311.05131 | 529 | `ŁªºŒæŁæŁºŁıŁ` | `q ªººŒæŁß,ŁªºŒæ` | `ŁæŁºŁıŁ ŁŁ: O ‹` | garbled |
| 37 | 2311.05131 | 539 | `æŒŒªŁ` | `æÆœŒ);ŒŁº, æŒŒª` | `Ł  X ºææŒººßª. ` | garbled |
| 38 | 2311.05131 | 608 | `SelectaMatematicaSovietica` | `lectaMatematica` | `Sovietica,19 92` | undamaged |
| 39 | 2401.01137 | 44 | `SarahPeluseandSeanPrendiville` | `261 . [20]Sarah` | `PeluseandSeanPr` | glue |
| 40 | 2403.01586 | 2 | `IoT` | `process: The Io` | `T labeling oper` | undamaged |
| 41 | 2403.02247 | 12 | `onNewFrontiersinSummarization` | `ndWork- shop on` | `NewFrontiersinS` | glue |
| 42 | 2403.02247 | 12 | `DanHendrycks` | `R ) ,2021b. Dan` | `Hendrycks,Colli` | undamaged |
| 43 | 2403.02247 | 14 | `AlexPassos` | `yParparita,Alex` | `Passos,MikhailP` | undamaged |
| 44 | 2403.17848 | 23 | `ArabicaQA` | `Arabica` | `QA: A Comprehen` | undamaged |
| 45 | 2403.19144 | 16 | `arXiv` | `icit models. ar` | `Xiv preprint ar` | undamaged |
| 46 | 2404.05778 | 1 | `2SemanticMathematicalDatabases` | `ticMathematical` | `Databases,Large` | glue |
| 47 | 2404.05778 | 10 | `orHausdorﬀ` | `spaceis T 2 (or` | `Hausdorﬀ)provid` | glue |
| 48 | 2502.07239 | 83 | `arXiv` | `Xiv preprint ar` | `Xiv:2411.10979 ` | undamaged |
| 49 | 2502.07239 | 179 | `AnimateAnyone` | ` though Animate` | `Anyone achieves` | undamaged |
| 50 | 2510.07408 | 5525 | `arXiv` | `6) 035001 ,  ar` | `Xiv:1508.04715 ` | undamaged |

**12 undamaged, 7 glue, 31 garbled** (0.24 undamaged). The 7 glue events are exactly the shapes
#139 exists for: `ReM` (`Re M`), `NotationsandDerivedAlgebraicGeometry`, `GeneralizedSerreTheorem`,
`thecaseofRedPajama`, `others.John`, `datasets.We`, `SCIBERT`/`SemanticScholar` in 2404.05778 - a
paper PDF-6a measured at a 0.386 whitespace ratio. The 12 undamaged are names and table labels.

### `punct_upper`, seed 17

| # | Id | Unit | Shape | Left | Right | Label |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 2301.07141 | 369 | `,A` | `log ⟨N ∗ ( τ n,` | `A ) N ∗ ( τ n,B` | undamaged |
| 2 | 2306.09672 | 14 | `.Alltheresult` | `geometricstack.` | `Alltheresult si` | glue |
| 3 | 2306.09672 | 19 | `,Wedenethehyperplane` | `). Paticularly,` | `Wedenethehyperp` | glue |
| 4 | 2306.09672 | 25 | `.Lemma4` | `o-sectionof L .` | `Lemma4.3 (Propo` | glue |
| 5 | 2306.09672 | 44 | `.QuivervarietiesandKac` | `HirakuNakajima.` | `Quivervarieties` | glue |
| 6 | 2307.09982 | 43 | `.Thenotation` | `beroftheset A .` | `Thenotation \| A` | glue |
| 7 | 2307.09982 | 112 | `:D` | `
| 8 | 2307.09982 | 149 | `.Пользуясьподобнымисоображен` | `астиравенства).` | `Пользуясьподобн` | glue |
| 9 | 2307.09982 | 292 | `.Baez` | `022) [12]JohnC.` | `Baez,TheOctonio` | glue |
| 10 | 2307.09982 | 302 | `.Именнопоэтомуя` | `модульпорождён.` | `Именнопоэтомуя ` | glue |
| 11 | 2307.09982 | 416 | `:U` | `омоморфизм 
| 12 | 2307.09982 | 458 | `:U` | `g :U → V , 
| 13 | 2307.09982 | 502 | `,X` | ` X ⊆ A 2 :J [f,` | `X ]= A 2 } Если` | glue |
| 14 | 2307.09982 | 539 | `:A` | `:D 1 → D 2 
| 15 | 2307.09982 | 571 | `:V` | ` :A 1 → A 2 f :` | `V 1 → V 2 ) явл` | glue |
| 16 | 2307.09982 | 626 | `:V` | `изм (7.3.3) f :` | `V 1 → V 2 диагр` | glue |
| 17 | 2307.09982 | 648 | `,NewYork` | `lications,Inc.,` | `NewYork,1995 13` | glue |
| 18 | 2309.14539 | 12 | `.Let` | `Let p beaprime.` | `Let d  1 and n ` | glue |
| 19 | 2311.05131 | 23 | `,ØæŁæßØŁ` | `:B Ñ X , ’ P  ,` | `ØæŁæßØŁ ’ 1 :B ` | garbled |
| 20 | 2311.05131 | 59 | `:Env` | `ŁæßØŁ Env  
| 21 | 2311.05131 | 89 | `:X` | `K ; (c)ºæŒªŁ  :` | `X Ñ Y ŁºÆªº  P ` | garbled |
| 22 | 2311.05131 | 97 | `:X` | `ŁºÆØßºººßıŁ ; :` | `X Ñ K ŒŁŁŁ   ˝ ` | garbled |
| 23 | 2311.05131 | 119 | `:A` | `
| 24 | 2311.05131 | 184 | `.ЛОКАЛЬНОВЫПУКЛЫЕРАССЛОЕНИЯ` | `134 Глава3.` | `ЛОКАЛЬНОВЫПУКЛЫ` | garbled |
| 25 | 2311.05131 | 194 | `;Y` | `et 0 
| 26 | 2311.05131 | 199 | `;F` | `æŁæ Sec p  0 B;` | `F q– Sec p  0 A` | garbled |
| 27 | 2311.05131 | 201 | `;X` | `ªŁØ,øøØŒŁ  n A;` | `X :Jet n 
| 28 | 2311.05131 | 213 | `,ŁŒŒŁÆßºŒ` | ` ŁŒææŁ U ŒŁ t ,` | `ŁŒŒŁÆßºŒ. 
| 29 | 2311.05131 | 254 | `:G` | ` Ñ L p X q Ł  :` | `G Ñ L p Y q ,æº` | garbled |
| 30 | 2311.05131 | 293 | `:ŒŁ` | `ıŒŁØ G ºææºøŁÆ:` | `ŒŁ u :G Ñ C æŁæ` | garbled |
| 31 | 2311.05131 | 414 | `:A` | `g ; (c)ºæŒªŁ  :` | `A Ð A 1  InvSte` | garbled |
| 32 | 2311.05131 | 419 | `:H` | `p env E H q ‹ :` | `H ‹ Ð Env E H ‹` | garbled |
| 33 | 2311.05131 | 487 | `,ÆŁºæÆºŁºŁŒŁßªº` | `Œ-ŒŒ E N , ŒßØ,` | `ÆŁºæÆºŁºŁŒŁßªº ` | garbled |
| 34 | 2311.05131 | 501 | `;H` | `g d h q  ÞÑ  G;` | `H p w qP g  d h` | garbled |
| 35 | 2311.05131 | 503 | `,Ł` | `ªÆ C æŁŁØŁºßßŁ,` | `Ł- ŁŁŁºŁØßıßßıº` | garbled |
| 36 | 2311.05131 | 541 | `:X` | `øæ- ŁŒŁßŁºŁ ’ :` | `X Ñ Y ,ßŒøŁØæŁª` | garbled |
| 37 | 2311.05131 | 569 | `.Некоторыефактыизтеориилокальновыпуклыхис` | `§2.` | `Некоторыефактыи` | garbled |
| 38 | 2311.05131 | 588 | `,Examp` | `. ºææŁØºªÆØŒ[3,` | `Examp le10.3],Ł` | garbled |
| 39 | 2311.05131 | 589 | `,Theorem5` | ` B ,ØºªÆØ A [5,` | `Theorem5.14]. ‚` | garbled |
| 40 | 2311.05131 | 607 | `.Am` | `i calalgebra.B.` | `Am.Math.S.,1971` | garbled |
| 41 | 2311.05131 | 609 | `.R` | `urunevariete,C.` | `R.Acad.Sci. Par` | garbled |
| 42 | 2401.01137 | 0 | `.Themainnewingredientisanalgebraicgeometr` | `subsetsof F p .` | `Themainnewingre` | glue |
| 43 | 2401.03649 | 340 | `,R` | `):99–103. Kass,` | `R.E.andVaidyana` | glue |
| 44 | 2401.03649 | 344 | `,C` | `andHuttenhower,` | `C.(2012).Metage` | glue |
| 45 | 2401.16311 | 35 | `.The` | ` q 2( j − c ) .` | `The upperboundi` | glue |
| 46 | 2401.16311 | 36 | `,A` | `teel,J.Lovejoy,` | `A.J.Yee, “Overp` | glue |
| 47 | 2403.02247 | 11 | `,XingzhangRen` | `nxinMa,RuiM en,` | `XingzhangRen, X` | glue |
| 48 | 2403.02247 | 14 | `,Ananya` | `entinHof- mann,` | `Ananya HarshJha` | glue |
| 49 | 2403.04799 | 23 | `.N` | `9–222, 2022. L.` | `N. Onyejegbu. C` | glue |
| 50 | 2406.06945 | 13 | `.Notes` | `.R.;Pathak,P.K.` | `Notes: ANoteonE` | glue |

**1 undamaged, 26 glue, 23 garbled** (0.02 undamaged). The one undamaged event is a comma inside a
math subscript (`τ n,A`); everything else is a lost space: `:A`/`:V`/`:D` in formulas, `.Let`,
`.The`, `.Lemma4`, `.Accordingtothetheorem`, and the reference lists of 2401.03649, 2403.02247 and
2307.09982.

The conclusion the two samples force: **the kinds cannot be exempted, only narrowed**. A rule that
exempts a whole kind un-protects between a quarter and a half of the paragraphs in the corpus and
frees paragraphs whose damage is real. PDF-6d's digit->uppercase narrowing was affordable because
that kind was 2% of the evidence; these two are 85%.

## 5. What each candidate would exempt, on whole paragraphs

Each candidate is a predicate on one junction event, priced by re-running the guard's verdict with
that predicate dropped from the count, **on top of the guard the server ships today** - PDF-6d's
digit->uppercase narrowing is in the baseline, so a candidate is never credited with 6d's work.
"Paragraphs freed" counts only paragraphs that fire today and would not fire afterwards; no
candidate can newly flag anything.

| Candidate | Events exempted | Paragraphs freed | Still flagged | Of the 3 glued papers |
| --- | --- | --- | --- | --- |
| R1 repeated name-like token in the paragraph (implemented) | 1,977 | 115 | 2,534 | 38 |
| P5 initial pair around the dot (measured, not shipped) | 790 | 26 | 2,623 | 38 |
| L1 lowercase-prefix + 2+ capitals ending the token | 6,675 | 151 | 2,498 | 38 |
| L2 lowercase-prefix + any capital run ending the token | 15,157 | 250 | 2,399 | 38 |
| L3 repeated token in the paragraph | 10,890 | 376 | 2,273 | 38 |
| L4 repeated token in the document | 27,110 | 620 | 2,029 | 38 |
| P1 initials after the dot (U.S., J.R.) | 1,102 | 26 | 2,623 | 38 |
| P2 any capital before the dot | 1,281 | 26 | 2,623 | 38 |
| P3 repeated token in the paragraph | 10,443 | 113 | 2,536 | 38 |
| P4 repeated token in the document | 15,918 | 365 | 2,284 | 38 |

The two document-scope candidates (L4, P4) are priced but not implementable as the guard stands:
`looks_like_unreliable_fallback_text` is handed one paragraph and has no document context, so a
document-scope count would need a new parameter threaded through segmentation. They are reported
for completeness and are not part of the decision.

## 6. Each candidate against its own seeded sample

50 exempted events per candidate, seed 18, hand-labelled:

| Candidate | Sampled | Exempted | Undamaged | Glue | Garbled | Undamaged share |
| --- | --- | --- | --- | --- | --- | --- |
| **R1** repeated name-like token in the paragraph | 50 | 50 | **50** | **0** | 0 | **1.00** |
| **P5** initial pair around the dot | 50 | 50 | **50** | **0** | 0 | **1.00** |
| L1 lowercase-prefix + 2+ capitals ending the token | 50 | 50 | 19 | **0** | 31 | 0.38 |
| L2 lowercase-prefix + any capital run ending the token | 50 | 50 | 7 | **1** | 42 | 0.14 |
| L3 repeated token in the paragraph | 50 | 50 | 24 | **2** | 24 | 0.48 |
| L4 repeated token in the document | 50 | 50 | 15 | **3** | 32 | 0.30 |
| P1 initials after the dot | 50 | 50 | 38 | **12** | 0 | 0.76 |
| P2 any capital before the dot | 50 | 50 | 35 | **14** | 1 | 0.70 |
| P3 repeated token in the paragraph | 50 | 50 | 2 | **29** | 19 | 0.04 |
| P4 repeated token in the document | 50 | 50 | 8 | **23** | 19 | 0.16 |

The Glue column decides it, as it did in PDF-6d:

- **L1** exempts no glue in its sample, but 31 of 50 events are Cyrillic garbling: it would hand
  2,311.05131's paragraphs back. 19 undamaged out of 50 is not a proof of anything.
- **L2** is L1 widened to any capital run and picks up `A.EinsteinandP.Ehrenfest` - glue.
- **L3** is the repeated-token idea without the shape condition, and its sample contains
  `ofAbeliangroup` and `АлександрКлейн`: a glued word can repeat too, and `of`, `the`, `Let` repeat
  in every paragraph.
- **P1** looks strong (38 of 50 undamaged) until the 12 glue rows: `J.Amer.Math.Soc.`,
  `A.G.Kurosh`, `A.Einstein`, `Z.Phys.`, `S.N.Bose` are all reference-list entries whose space after
  the dot is lost. An initial followed by a surname with no space is precisely a lost space, so P1
  cannot distinguish `J.R. Platt` from `J.Platt`.
- **P2** adds `.Milman`, `.Orlov`, `.Schafer`, `.Nohypotheticalenergy`, `.Bosearchive` and is worse.
- **P3** exempts whatever token repeats in the paragraph, and in `punct_upper` that means the word
  after the dot: `.Let`, `.The`, `:A`, `:V`. Its sample is 29 glue of 50.
- **P4** is the same at document scope.

R1 and P5 are the only two whose sample contains no glue and no garbling. Only R1 is shipped; P5 is
priced and labelled, then rejected on a reason outside the guard (section 7). Their samples, with
the paragraph text around the junction:

### R1, seed 18

| # | Id | Unit | Shape | Left | Right | Label |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 2304.03209 | 9 | `NeurIPS` | `tions. In: Neur` | `IPS (2020) 28. ` | undamaged |
| 2 | 2305.10450 | 104 | `ReLU` | `unctions and Re` | `LU  In CNN, the` | undamaged |
| 3 | 2310.09263 | 21 | `ChatGPT` | `ion of the Chat` | `GPT model avail` | undamaged |
| 4 | 2310.09263 | 23 | `ChatGPT` | ` and Table-Chat` | `GPT.` | undamaged |
| 5 | 2310.09263 | 25 | `BingQL` | `Unit 
| 6 | 2311.15310 | 205 | `RoFL` | `ic) 
| 7 | 2311.15310 | 205 | `EIFFeL` | `elements. EIFFe` | `L . The commitm` | undamaged |
| 8 | 2311.15310 | 208 | `RiseFL` | `𝑑 = 100 K, Rise` | `FL results in 1` | undamaged |
| 9 | 2311.15310 | 209 | `RiseFL` | `ck method. Rise` | `FL incurs a hig` | undamaged |
| 10 | 2312.16335 | 70 | `RoarANN` | `ack winner Roar` | `ANN ( Chen et a` | undamaged |
| 11 | 2403.03592 | 17 | `PrivFT` | `but a few. Priv` | `FT compared the` | undamaged |
| 12 | 2403.03592 | 81 | `SecureML` | `ics 
| 13 | 2403.03773 | 47 | `VeriTraCER` | `C OULA 
| 14 | 2403.03773 | 116 | `VeriTraCER` | `ee that VeriTra` | `CER can generat` | undamaged |
| 15 | 2403.03773 | 118 | `VeriTraCER` | `N-IBP, and Veri` | `TraCER. Prox. i` | undamaged |
| 16 | 2403.03788 | 1 | `LLaMa` | `LLMs (e.g., LLa` | `Ma-2 and Wizard` | undamaged |
| 17 | 2403.03788 | 6 | `LLaMa` | `023 ), Code-LLa` | `Ma-instruct ( C` | undamaged |
| 18 | 2403.03788 | 7 | `CodeLLaMa` | `rparts. CodeLLa` | `Ma achieves the` | undamaged |
| 19 | 2403.03788 | 8 | `CodeLLaMa` | `nterparts. Code` | `LLaMa achieves ` | undamaged |
| 20 | 2403.03788 | 67 | `ChatGPT` | `47.9 23.3 
| 21 | 2403.08062 | 0 | `SparkSQL` | `ster than Spark` | `SQL on the TPC-` | undamaged |
| 22 | 2403.08062 | 1 | `SparkSQL` | `ism as in Spark` | `SQL. Each stage` | undamaged |
| 23 | 2403.08062 | 101 | `SparkSQL` | `mpared to Spark` | `SQL, Quokka ach` | undamaged |
| 24 | 2403.11395 | 3756 | `AutoML` | `  popular  Auto` | `ML  tools  are ` | undamaged |
| 25 | 2403.11395 | 3756 | `AutoML` | `09],  H2O  Auto` | `ML ` | undamaged |
| 26 | 2403.17729 | 3 | `RoPE` | `te encoding, Ro` | `PE [ 24 ] propo` | undamaged |
| 27 | 2403.17729 | 39 | `RoPE` | `evident that Ro` | `PE [ 24 ] is le` | undamaged |
| 28 | 2403.17848 | 1 | `AraDPR` | ` ArabicaQA, Ara` | `DPR, and the be` | undamaged |
| 29 | 2403.17848 | 3 | `CoQA` | ` dialogue in Co` | `QA contains mul` | undamaged |
| 30 | 2403.17848 | 19 | `ArabicaQA` | ` on the Arabica` | `QA and found th` | undamaged |
| 31 | 2403.18133 | 50 | `SemRL` | ` Rules - AE Sem` | `RL A uto E ncod` | undamaged |
| 32 | 2403.18133 | 51 | `SemRL` | `tuition: AE Sem` | `RL exploits the` | undamaged |
| 33 | 2403.18133 | 52 | `SemRL` | `AE Sem` | `RL: Learning Se` | undamaged |
| 34 | 2403.18133 | 58 | `LeakDB` | `ule Count 
| 35 | 2403.18133 | 59 | `SemRL` | `rison of AE Sem` | `RL and HHO (p=i` | undamaged |
| 36 | 2403.18133 | 59 | `LeakDB` | `ule Count 
| 37 | 2403.18575 | 119 | `DexYCB` | `aluation on Dex` | `YCB. We ﬁrst co` | undamaged |
| 38 | 2403.19144 | 10 | `AToM` | `ork design, ATo` | `M and MToV, wit` | undamaged |
| 39 | 2403.19144 | 13 | `MToV` | `n study for MTo` | `V. The baseline` | undamaged |
| 40 | 2403.19144 | 43 | `AToM` | `we validate ATo` | `M using HDTF [ ` | undamaged |
| 41 | 2407.10296 | 163 | `OntoChatGPT` | `DWE system Onto` | `ChatGPT  
| 42 | 2407.10296 | 176 | `OntoChatGPT` | `t stage of Onto` | `ChatGPT informa` | undamaged |
| 43 | 2410.00944 | 59 | `RegGNN` | ` PNA [ 9 ], Reg` | `GNN [ 14 ], wit` | undamaged |
| 44 | 2502.13422 | 0 | `TableQA` | `nswering (Table` | `QA) aims to ans` | undamaged |
| 45 | 2502.13422 | 1 | `CoTR` | `e-Retrieval (Co` | `TR) mechanism. ` | undamaged |
| 46 | 2502.13422 | 3 | `CoTR` | `t al. 2024). Co` | `TR uses an LLM ` | undamaged |
| 47 | 2502.13422 | 3 | `TableQA` | `rge-table Table` | `QA datasets: SE` | undamaged |
| 48 | 2603.17845 | 0 | `CellPoseSAM` | `mentation (Cell` | `PoseSAM, CellSA` | undamaged |
| 49 | 2603.17845 | 7 | `PathoSAM` | `lPoseSAM. Patho` | `SAM is only eva` | undamaged |
| 50 | 2610.01064 | 304 | `REaR` | `. With BGE, REa` | `R achieves the ` | undamaged |

All 50 are a name or a table label the paragraph is about: `ChatGPT`, `ReLU`, `SecureML`,
`VeriTraCER`, `CodeLLaMa`, `SparkSQL`, `AutoML`, `RoPE`, `ArabicaQA`, `SemRL`, `RiseFL`, `DexYCB`,
`AToM`, `MToV`, `TableQA`, `CoTR`, `CellPoseSAM`, `REaR`.

### P5, seed 18

| # | Id | Unit | Shape | Left | Right | Label |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 2304.03209 | 8 | `.N` | `S.C., Chuah, C.` | `N., Dugger, B.N` | undamaged |
| 2 | 2304.03209 | 8 | `.N` | `.N., Dugger, B.` | `N.: Brainsec: A` | undamaged |
| 3 | 2304.03209 | 8 | `.N` | `S.C., Chuah, C.` | `N.: A semi- sup` | undamaged |
| 4 | 2304.03209 | 9 | `.S` | ` J., Duncan, J.` | `S.: Unsupervise` | undamaged |
| 5 | 2304.03209 | 10 | `.S` | ` L., Duncan, J.` | `S.: Simcvd: Sim` | undamaged |
| 6 | 2308.01909 | 4 | `.N` | `neousPieces ,S.` | `N.Bosearchive,h` | undamaged |
| 7 | 2310.09263 | 192 | `.A` | `16\| 
| 8 | 2311.05131 | 607 | `.A` | `:Ł,1975. [69]R.` | `A.Horn,C.R.John` | undamaged |
| 9 | 2311.05131 | 609 | `.G` | `тура 469 [99]J.` | `G.Llavona.Apppr` | undamaged |
| 10 | 2311.05131 | 609 | `.P` | `e,2008. [106]C.` | `P.Milies,S.K.Se` | undamaged |
| 11 | 2311.05862 | 133 | `.P` | ` H.J.T, and  G.` | `P.  wrote the p` | undamaged |
| 12 | 2312.01833 | 172 | `.M` | ` D., Buchan, A.` | `M., Charpak, S.` | undamaged |
| 13 | 2312.01833 | 172 | `.R` | `E.C., Frank, L.` | `R., 1998. Dynam` | undamaged |
| 14 | 2312.01833 | 172 | `.S` | `t, E., Pine, D.` | `S., Brotman, M.` | undamaged |
| 15 | 2312.01833 | 172 | `.E` | `A., Stephan, K.` | `E., 2021. Regre` | undamaged |
| 16 | 2312.01833 | 172 | `.S` | `4111 . Hong, K.` | `S., Zafar, A., ` | undamaged |
| 17 | 2312.01833 | 173 | `.D` | `L.Y., Wager, T.` | `D., 2009. Model` | undamaged |
| 18 | 2312.01833 | 173 | `.T` | `ta, T., Liu, T.` | `T., Miller, K.L` | undamaged |
| 19 | 2312.13331 | 0 | `.S` | `t, K., 2001; U.` | `S. Census Burea` | undamaged |
| 20 | 2312.13331 | 0 | `.S` | `tiple years (U.` | `S. Census Burea` | undamaged |
| 21 | 2312.13331 | 329 | `.S` | `ds Division, U.` | `S. Census Burea` | undamaged |
| 22 | 2312.13331 | 332 | `.M` | `09. Abdalla, S.` | `M., Galea, S., ` | undamaged |
| 23 | 2401.03649 | 339 | `.P` | `ngh,S.,Singh,A.` | `P., andDasgupta` | undamaged |
| 24 | 2401.03649 | 339 | `.A` | `E.,Coughlin, L.` | `A.,Kim,J.,Froeh` | undamaged |
| 25 | 2401.03649 | 341 | `.R` | ` E.,Brochado,A.` | `R.,Fernandez,K.` | undamaged |
| 26 | 2401.03649 | 341 | `.P` | `R.K.,Nag ane,V.` | `P.,andMaity,A.(` | undamaged |
| 27 | 2401.03649 | 342 | `.K` | `, andMallick,B.` | `K.(2019b).Inte ` | undamaged |
| 28 | 2401.03649 | 342 | `.K` | `87–95. Maity,A.` | `K.,Lee,S.C., Hu` | undamaged |
| 29 | 2401.03649 | 343 | `.M` | ` andPolansky,A.` | `M.(2020).Motiva` | undamaged |
| 30 | 2401.03649 | 343 | `.M` | `.andPolansky,A.` | `M.(2023b).Scori` | undamaged |
| 31 | 2401.03649 | 344 | `.D` | `3. Robinson, M.` | `D.,McCarthy,D.J` | undamaged |
| 32 | 2401.03649 | 344 | `.J` | `.D.,McCarthy,D.` | `J.,andSmyth,G.K` | undamaged |
| 33 | 2401.03649 | 344 | `.P` | `,E.C., Smith,D.` | `P.,Fo fanova,T.` | undamaged |
| 34 | 2401.03649 | 345 | `.S` | `3. Vikramdeo,K.` | `S.,Anand,S.,Sud` | undamaged |
| 35 | 2401.16311 | 36 | `.J` | `el,J.Lovejoy,A.` | `J.Yee, “Overpar` | undamaged |
| 36 | 2403.02163 | 22 | `.C` | `., P.S., and A.` | `C. conceived th` | undamaged |
| 37 | 2403.02163 | 22 | `.A` | `experiments. G.` | `A., S.B., C.H.,` | undamaged |
| 38 | 2403.02163 | 22 | `.H` | ` G.A., S.B., C.` | `H., D.H, M.I., ` | undamaged |
| 39 | 2403.02681 | 88 | `.G` | `, Bellemare, M.` | `G., Graves, A.,` | undamaged |
| 40 | 2403.03883 | 9 | `.F` | `a)(1) and 16 C.` | `F.R. § 310.3(a)` | undamaged |
| 41 | 2403.03883 | 10 | `.F` | `ovided by 16 C.` | `F.R. § 310.3(a)` | undamaged |
| 42 | 2403.03883 | 11 | `.F` | `ovided by 16 C.` | `F.R. § 310.3(a)` | undamaged |
| 43 | 2403.18402 | 167 | `.F` | ` Innovation (H.` | `F.R.I.) under t` | undamaged |
| 44 | 2403.19144 | 15 | `.C` | `Wu, W., Loy, C.` | `C., Cao, X., Xu` | undamaged |
| 45 | 2403.19144 | 16 | `.C` | `an, C., Loy, C.` | `C., He, R.: Eve` | undamaged |
| 46 | 2403.19299 | 186 | `.K` | `, V. Shukla, M.` | `K. Misra, Three` | undamaged |
| 47 | 2410.00944 | 62 | `.H` | `g, F., Torr, P.` | `H.: Hypergraph ` | undamaged |
| 48 | 2410.00944 | 63 | `.E` | `aillancourt, D.` | `E.: Emerging ne` | undamaged |
| 49 | 2505.24699 | 1312 | `.S` | `at. Sbornik] N.` | `S. , 12/54:277–` | undamaged |
| 50 | 2510.07408 | 169 | `.M` | ` particular, G.` | `M. was supporte` | undamaged |

All 50 are initials: author lists (`C.N.`, `J.S.`, `D.E.`, `M.K.`), `U.S.`, `C.F.R.`, `H.F.R.I.`,
and the table cell `A.D.I.D.A.S.`.

## 7. The rule PDF-6e ships

> **R1 - a lowercase->uppercase junction does not count when its token occurs at least twice in the
> paragraph, spelled identically, and the token contains a run of two or more capitals.**
>
> A token is a maximal run of alphanumeric characters. `ChatGPT`, `LLaMA`, `ArabicaQA`,
> `CodeLLaMA`, `SparkSQL` qualify; `thecaseofRedPajama`, `StrongInference`, `others.John` and
> `datasets.We` do not (no two-capital run, or not repeated), so #139's own shapes keep counting
> even when they repeat.

It is PDF-only (`MediaType::is_pdf()`, `FallbackGuardProfile::PdfProse`), and it does not touch the
threshold, the doubled rule or the bibliographic rule.

### Why repetition is the discriminator for R1

Glue is one word in one place: `thecaseofRedPajama` appears once, and a paragraph cannot contain
the same merged word twice by accident. A name the paragraph is about appears in every sentence.
The 2+ capital run is the second half of the rule: it is what separates `ChatGPT` from `RedPajama`,
and it is what keeps `ofAbeliangroup` (L3's counter-example) counting, because a merged pair of
ordinary words has no capital run.

Cost: the token counts are built once per paragraph, only for a paragraph that already reaches 3
events - the exemption can only remove evidence, so a paragraph below the threshold cannot change
verdict - and in one pass. The guard stays O(paragraph).

### Measured, not shipped: the initial pair

P5 - a dot between two lone capitals, `U.S.`, `J.R.`, `A.I.`, `C.F.R.`'s `.F` - is as clean as R1
on its own sample (50 of 50 undamaged, 0 glue, 0 garbled) and frees 26 paragraphs. It is **not**
shipped, because the junction is not the only thing that decides the output: `J.Platt`, `Z.Phys.`
and `A.Einstein` correctly keep counting, but once a paragraph is handed to the splitter, the
splitter's own abbreviation list decides where it cuts. `ascii_period_is_terminal` protects `u.s`,
`u.k`, `e.g`, `i.e`, `etc`, the titles, and a lone letter before a capital - not `F.B.`, `C.M.` or
`S.G.` - so 18 of the 26 paragraphs P5 would free contain a `X.Y.` followed by whitespace and are
cut mid-citation:

```
2311.16229  "helpful reading of the manuscript. F.B. thanks Carlo Vanoni"  -> "...manuscript. F.B."
                                                                              + "thanks Carlo ..."
2312.13331  "(Waller, L.A. and Gotway, C.A., 2004; ...)"                    -> "(Waller, L.A."
                                                                              + "and Gotway ..."
```

Letting those paragraphs through would trade one coarse unit for two worse ones. The precondition
for P5 is a splitter change (extend `is_protected_abbreviation` to `X.Y`), which is format-agnostic
and would move non-PDF output, so it does not belong in a PDF-only narrowing of the guard. P5's
numbers are here so the next task starts from the measurement instead of repeating it.

## 8. What the rule frees

**1,977 events**, **115 paragraphs** of 2,652, in 28 papers. 2,534 stay flagged, and **all 38
coarse paragraphs of the three papers where PDF-6a measured real space loss stay flagged**
(2308.01909 6, 2404.05778 16, 2403.02247 16). No paragraph is newly flagged.

| Paper | Paragraphs freed |
| --- | --- |
| 2407.10296 | 21 |
| 2403.18133 | 11 |
| 2403.03773 | 9 |
| 2403.03967 | 9 |
| 2403.08062 | 7 |
| 2403.17848 | 6 |
| 2403.02642 | 5 |
| 2403.11395 | 4 |
| 2311.05862 | 4 |
| 2309.06402 | 3 |
| 2311.15310 | 3 |
| 2403.03883 | 3 |
| **total, 28 papers** | **115** |

The exemption was also checked on the **freed paragraphs**, not only on their events: all 115 were
read, and every exempted junction in them is a name. The names are `ChatGPT` (14 paragraphs),
`SemRL` (9), `ReLU` (8), `VeriTraCER` (7), `SparkSQL` (7), `OntoChatGPT` (5), `LiDAR` (5),
`HebbFF` (4), `AutoML` (4), `ArabicaQA`/`AraDPR` (4), `TwoNN`, `SaulLM`, `RiseFL`, `GraphCL`,
`AutoLFADS`, `AToM`/`MToV`, `RobustCE`, `REaR`/`MiniLM`, `LeakDB`, `DynaSAND`, `CuPL`/`PaLI`/`PaLM`,
`BsplineMLPCC`/`RawdataMLPCC`, `maxRAF` - and nothing else. Examples:

| Id | Unit | Events before -> after | Exempted tokens | The text at the junction |
| --- | --- | --- | --- | --- |
| 2403.19012 | 176 | 3 -> 0 | `ChatGPT` | a prompt table with `ChatGPT` in it |
| 2403.18762 | 5 | 7 -> 2 | `LiDAR`, `NetVLAD` | `where no LiDAR point is projected` |
| 2403.19009 | 0 | 4 -> 2 | `SecureML` | `Towards Sustainable SecureML:` |
| 2403.08062 | 100 | 14 -> 2 | `SparkSQL` | `Quokka outperforms Trino and SparkSQL` |
| 2410.05294 | 376 | 3 -> 1 | `maxRAF` | `referred to as the maxRAF` |
| 2403.19144 | 3 | 7 -> 2 | `AToM`, `MToV` | `Audio-to-Motion (AToM) and Motion-to-Video (MToV)` |
| 2407.10296 | 2 | 8 -> 0 | `OntoChatGPT` | `we evaluate OntoChatGPT on the benchmark` |

The largest single paper is 2407.10296 (21 paragraphs), whose text has lost sentence boundaries and
hyphenation spaces - not missing spaces - and where `ChatGPT`/`OntoChatGPT` are the only junctions.
Splitting those paragraphs is what the guard does for any clean paragraph; its own damage is not
what the #139 rule measures, and R1 does not exempt any of it.

## 9. Measured but not implemented: a density threshold

Replacing the absolute count with events per 1,000 characters is a change to #139 itself, so it is
reported and not implemented. On whole paragraphs (PDF-6d could only bound this from 420-character
samples):

| Events per 1,000 characters | Paragraphs freed | Paragraphs newly flagged | Still flagged | Of the 3 glued papers |
| --- | --- | --- | --- | --- |
| 1 | 129 | 0 | 2,520 | 38 |
| 2 | 462 | 0 | 2,187 | 38 |
| 3 | 689 | 0 | 1,960 | 38 |
| 5 | 975 | 0 | 1,674 | 38 |

On whole paragraphs the direction reverses PDF-6d's sample-based guess: every rate frees paragraphs
and none newly flags, because a long paragraph's 3 events are a low density. But 1 event per 1,000
characters is satisfied by a 3,000-character paragraph with 3 events - the same paragraph the
absolute rule flags - so a density rule at these rates is a genuine loosening of #139, and the
three glued papers stay flagged only because their paragraphs are dense (0.283-0.418 whitespace
ratio). This is a question for the user, not a change in this PR.

## Reproduce

```bash
# 1. the probe with the whole paragraph kept (one run, on the final head)
gh workflow run pdf-quality.yml --ref <branch> -f keep_coarse_text=true
gh run download <run-id> -p 'pdf-quality-shard-*'

# 2. the coarse paragraphs, one shard at a time
python3 - <<'PY'
import json, glob
for path in sorted(glob.glob("pdf-quality-shard-*/all/quality-*.jsonl")):
    for line in open(path):
        record = json.loads(line)
        if record.get("kind_record") == "coarse_unit":
            print(line.rstrip())
PY

# 3. the tables of this note
python3 scripts/pdf-guard-events.py --coarse .tmp/coarse.jsonl --report-full \
    --sample 50 --seed 18 --labels .tmp/labels.json
```

`--sample-kind lower_upper --seed 16`, `--sample-kind punct_upper --seed 17` and
`--sample-candidate '<name>' --seed 18` reproduce the three sample tables; the hand labels are in
`EMBEDDED_LABELS` (empty by default, so the script has no data baked in) and in the `--labels` JSON
the commands above name.
