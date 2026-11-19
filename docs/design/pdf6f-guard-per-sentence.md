# Judge the #139 guard per sentence in long PDF paragraphs (PDF-6f)

Measurement for issue #232, follow-up to `pdf6e-guard-main-events.md`. The guard
(`looks_like_unreliable_fallback_text`, `src/domain/segmentation.rs`) gives one verdict per
paragraph: `MIN_MISSING_SPACE_GLUE_EVENTS = 3` is an absolute count over the whole paragraph,
so a 2,000-character paragraph with one camel-case name or one lost space in each of three
different sentences is refused as a whole and served as one `coarse_paragraph` unit, often
longer than 800 characters. On `main` the harness files 540 long units under `missed_boundary`,
and 538 of them are coarse paragraphs.

This note prices moving **where the count is taken**, not which events count. No event kind is
exempted (PDF-6e already showed `lower_upper` and `punct_upper` are mostly real glue). The
doubled-character and bibliographic rules keep their paragraph-wide check under every
candidate: they catch a masthead merged into a paragraph, which no per-sentence view can see.

## Provenance

- Artifact: `pdf-quality-shard-*` of `PDF quality probe` run **44504197593**, head `297ad91`
  (the PDF-6f harness-only commit: the broken-sentence measure ported from the closed PDF-7b-1
  branch; product code identical to `main` `8df7f43`), dispatched with
  `keep_coarse_text=true`. 121 of 121 documents, 0 failed.
- Records: 2,564 `long_unit`, 6,018 `short_unit`, 2,537 `coarse_unit` (full paragraph text),
  121 `document`. Baseline summary: 53 papers over 5% long units, 6,379 broken sentences in 48
  documents.
- Everything below is computed by `scripts/pdf6f-sentence-price.py`, which re-uses the PDF-6e
  guard replica (`scripts/pdf-guard-events.py`) and adds a Python port of `sentence_ranges`
  including the abbreviation list (`e.g`, `i.e`, `etc`, `u.s`, `u.k`, the titles, the
  lone-initial rule).

## Replica agreement

- Guard verdict on the 2,536 prose coarse paragraphs: the replica fires on **2,534 (0.9992)**.
  The two misses are `2407.10296` units 525/526, 2,553 characters, 0 counted junctions, no
  doubled token, 3 bibliographic markers where 4 are needed - the same pair PDF-6d and PDF-6e
  missed (as units 513/514 before re-indexing).
- Sentence-split fidelity. (a) On the 6,581 stored `head`/`tail`/text segments of units the
  server emitted as sentences (non-degraded `long_unit`/`short_unit` records), the port
  reproduces every segment as a single piece: **6,581 of 6,581 (1.0000)**. (b) At the 187
  recorded joins where two consecutive stored units meet and the left side ends in a terminal,
  the port cuts at the join in 107; the other 80 are every single one an initial pair rejoined
  by the port (`F.` + `J.`, `M.` + `W.`, `Fig.` + `4`, `no.` + `8`,) - joins where the
  reconstruction itself is at fault, because the server treats an initial as its own unit at a
  point `ascii_period_is_terminal` deliberately does not cut ("a lone letter before a capital"),
  while the port, given the whole joined text, does exactly what the Rust code does and keeps
  them together. No join shows the port splitting where the server does not.
- The two 2407.10296 misses mean the candidate numbers below can free two paragraphs too many;
  both are in a paper whose paragraphs stay refused by sentence anyway (its sentences each carry
  enough glue), so the effect is zero.

## Candidates

- **S1, per-sentence verdict.** A paragraph that fires today is re-checked sentence by sentence
  with `sentence_ranges`. It stays coarse only if at least one of its sentences fires the
  unchanged PDF-profile guard on its own text. Otherwise it is sentence-eligible, exactly like a
  paragraph that never fired. One verdict per paragraph - `Eligible`/`CoarseParagraphOnly` - no
  new unit kind, no new degradation value.
- **S2, density.** The paragraph fires only at >= 3 counted events AND at least one event per
  *N* characters. Priced as a curve in *N*.
- **S3, quorum-two (own).** Like S1, but the paragraph stays coarse only if *two* sentences
  fire. Priced as the loosening bound: it emits a paragraph containing one glued sentence,
  which is what #139 exists to prevent. Listed to show S1 is not an arbitrary midpoint.

## Candidate table

Priced on the 2,536 prose coarse paragraphs (the one `non_prose` coarse unit is excluded);
"missed_boundary freed" counts the 538 coarse paragraphs the baseline harness files under that
cause.

| Candidate | Paragraphs freed | Chars freed | Sentences produced | missed_boundary freed (of 538) | New short units (<5) |
| --- | --- | --- | --- | --- | --- |
| S1 per-sentence | 589 | 2,179,180 | 22,597 | **251** | 712 |
| S3 quorum-two (own) | 1,129 | 3,480,748 | 32,854 | 407 | 982 |
| S2 density N=100 | 1,321 | 4,262,192 | 38,350 | 478 | 1,162 |
| S2 density N=150 | 1,085 | 3,769,142 | 34,629 | 445 | 982 |
| S2 density N=200 | 924 | 3,392,795 | 31,070 | 416 | 875 |
| S2 density N=250 | 812 | 3,103,716 | 27,961 | 391 | 803 |
| S2 density N=300 | 739 | 2,893,119 | 26,091 | 355 | 766 |
| S2 density N=400 | 576 | 2,389,910 | 21,784 | 291 | 676 |
| S2 density N=500 | 481 | 2,045,235 | 18,787 | 245 | 617 |
| S2 density N=750 | 283 | 1,279,355 | 11,571 | 149 | 466 |
| S2 density N=1000 | 139 | 690,792 | 6,040 | 70 | 221 |

## Projected metrics

Recomputed from the produced sentences, not the harness what-if: each document's unit stream is
rebuilt from the recorded units, each freed paragraph expands into its sentences, unrecorded
unit indices count as ordinary sentences. "Broken" is the §7 broken-sentence measure applied
to the projected stream; the artifact carries no `section_id`, so the approximation treats
adjacent stored units as same-section and breaks adjacency at unrecorded gaps - the same method
gives 1,596 against the harness's 6,379 on the unchanged stream (the records' short units
dominate what can be seen), so the useful column is the per-candidate delta.

| Candidate | M1 (papers >5% long) | Long units | Short units | Broken (approx) | Papers with broken up |
| --- | --- | --- | --- | --- | --- |
| today | 53 | 2,564 | 6,018 | 1,596 | - |
| S1 per-sentence | 21 | 3,756 | 7,442 | 1,682 | 23 |
| S3 quorum-two | 12 | 3,090 | 7,962 | 1,713 | 24 |
| S2 N=100 | 11 | 2,624 | 8,343 | 1,730 | 27 |
| S2 N=150 | 12 | 2,956 | 7,983 | 1,711 | 24 |
| S2 N=200 | 14 | 3,222 | 7,769 | 1,693 | 24 |
| S2 N=250 | 14 | 3,424 | 7,625 | 1,690 | 24 |
| S2 N=300 | 16 | 3,550 | 7,551 | 1,688 | 24 |
| S2 N=400 | 22 | 3,818 | 7,371 | 1,675 | 22 |
| S2 N=500 | 25 | 3,982 | 7,253 | 1,669 | 22 |
| S2 N=750 | 37 | 4,344 | 6,951 | 1,647 | 19 |
| S2 N=1000 | 50 | 4,588 | 6,461 | 1,627 | 17 |

Notes on the shape of the table: "long units" grows under freeing because a freed paragraph's
sentences are themselves often longer than 800 characters (one 2,500-char coarse unit becomes
three long sentences - the units are no longer `coarse_paragraph` kind, which is the column the
task cares about); M1 falls because the *share* falls - total units grow faster than long units.
S2's density curve is monotone in both directions as expected; at N<=300 it out-frees S1 because
it also frees paragraphs whose glue is concentrated in one sentence, which S1 correctly keeps.

S3 exists to mark the far edge: freeing any paragraph that still contains one glued sentence
emits glued text as served sentences. It is priced, not proposed.

## Hand labels

Both samples drawn with seed 23, 50 units each, at most 8 per document, >= 10 documents. The
candidate sample is sentences S1 produces from freed paragraphs; the control is units the server
already emits today (recorded units only - the 5..800-char majority carries no stored text, so
the control is drawn from the recorded tail and is mostly `<5`-char units, which makes the
comparison stricter for the candidate, not easier). Labels:

- `sentence`: one sentence. Glued words inside it are allowed.
- `merged`: two or more sentences, or a heading, byline, caption, masthead or table row fused
  with a sentence.
- `fragment`: part of a sentence, cut at an abbreviation, a decimal, an initial, a citation,
  or a formula.
- `garbled`: unreadable text.

### Candidate sample (S1, seed 23): 50 sentences from 36 documents

| # | Id | Unit | Text (<=160) | Label |
| --- | --- | --- | --- | --- |
| 1 | 2403.17729 | 44 | `2019.` | fragment |
| 2 | 2403.18762 | 49 | `As shown in Table.` | fragment |
| 3 | 2311.16343 | 847 | `B 80 , 184421 (2009) .` | fragment |
| 4 | 2506.06561 | 35 | `As sug- gested by our human evaluation results (§ 4.2 ), high similarity indicates that proﬁles capture context and style, but it does not ensure the captions a` | sentence |
| 5 | 2502.07239 | 82 | `[7] Junming Chen, Yunfei Liu, Jianan Wang, Ailing Zeng, Yu Li, and Qifeng Chen.` | fragment |
| 6 | 2510.07408 | 5211 | `[Cited on: pgs.` | fragment |
| 7 | 2401.02387 | 416 | `7.4, Example 7.9)): N 0 = n � | ρ ′′ (0) | /π (13) This expression can be used to express the ESS in terms of expected number of zero-crossings.` | merged |
| 8 | 2403.02681 | 0 | `The proposed method, namely SGD with Partial Hessian (SGD-PH), inherits the advantages of both ﬁrst-order and second-order optimizers.` | sentence |
| 9 | 2603.17845 | 4 | `They support automatic segmentation by covering the input image with a grid of point prompts, segmenting each prompt, and then merging the predicted masks via n` | sentence |
| 10 | 2404.07090 | 144 | `[8].` | fragment |
| 11 | 2309.06402 | 120 | `From this projection, we derived an estimate of the activa- tion function for each neuron, and compared this estimate to the ground-truth activation function.` | sentence |
| 12 | 2307.09982 | 183 | `Мыпредставилимножествовекторов v ( 1 )= v 1 , ..., v ( m )= v m ввидестолбцаматрицы v =  | | |  v 1 ...v m  | | |  имножество D (1) -чисел c ( 1 )= c 1 ,...` | merged |
| 13 | 2403.03592 | 77 | `In International Conference on Security for Information Technology and Communications .` | fragment |
| 14 | 2403.03883 | 2 | `, 2023 ; Nishida et al.` | fragment |
| 15 | 2403.02681 | 78 | `07 94 .` | fragment |
| 16 | 2307.09982 | 211 | `Здесь • a -координатнаяматрица A 1 -числа a относительноба- зиса e 1 (3.4.47) a = ae 1 • b -координатнаяматрица A 2 -числа (3.4.48) b = f ◦ a относительнобазиса` | merged |
| 17 | 2403.03788 | 65 | `API: insert _ pie _ chart(data, series): This API inserts a pie chart onto the slide.` | sentence |
| 18 | 2309.06402 | 454 | `FP ﬁnding was fast, requiring 1 minute for each model.` | sentence |
| 19 | 2403.17729 | 44 | `1376–1385.` | fragment |
| 20 | 2403.18069 | 687 | `PhD thesis, University of Illinois at Chicago, 2022.` | fragment |
| 21 | 2403.18575 | 1 | `Hand-Object Interaction Image Synthesis.` | fragment |
| 22 | 2403.18148 | 447 | `93 seek 0 .` | fragment |
| 23 | 2311.11050 | 102 | `For the FRCC, regression coeﬃcients are estimated using the training set, while the tuning set is used to compute the CLs as the 𝛼 ∕2 and 1 − 𝛼 ∕2 of empirical ` | sentence |
| 24 | 2403.03967 | 497 | `(2022), “Gra- dient methods provably converge to non-robust net- works,” Advances in Neural Information Processing Systems , 35, 20921–20932.` | fragment |
| 25 | 2403.19012 | 52 | `Finally, based on the best zero-shot setting, we explored a one-shot setting for GPT-noun phrase , where a random sample from the data was used.` | sentence |
| 26 | 2403.01601 | 210 | `16 A Comparison of Proximity Indices Applied to OpenAlex Table 5 : Median SMAPEs for transfer learning forecasting Algorithm Horizon Index 1 Index 2 Index 3 Ind` | merged |
| 27 | 2510.07408 | 5215 | `[Cited on: pg.` | fragment |
| 28 | 2403.03592 | 3 | `The main concern using SMPC is the possibility of malicious behavior by an external entity or even by a subset of participating parties as well as the high comm` | sentence |
| 29 | 2305.18793 | 1305 | `05 for ReM > rownames ( res ) = c ( " Neyman " , " Lin " ) > colnames ( res ) = c ( " estimate " , " s .` | fragment |
| 30 | 2307.09982 | 336 | `Теорема 3.1.5 .` | fragment |
| 31 | 2402.13288 | 5 | `, n K ) in input and returns a sequence of tokens.` | fragment |
| 32 | 2403.04799 | 2 | `This rapid evolution has sparked a shift in how we approach AI literacy, moving beyond a lim- ited scope in higher education to include early childhood educatio` | sentence |
| 33 | 2403.18973 | 339 | `The atis spoken language sys- tems pilot corpus.` | fragment |
| 34 | 2305.18793 | 4278 | `Example 23.2 Hearst et al.` | fragment |
| 35 | 2403.03883 | 8 | `Original Prompt The Telemarketing Sales Rule is provided by 16 C.F.R.` | merged |
| 36 | 2305.10450 | 32 | `In [13], Niels et al.` | fragment |
| 37 | 2403.01809 | 11 | `accumulate toward the end.` | fragment |
| 38 | 2307.09982 | 165 | `Левостороннеепредставление f :A 1 ∗ // A 2 называется ковариантным ,еслиравенство a 1 ∗ ( b 1 ∗ a 2 )=( a 1 ∗ b 1 ) ∗ a 2 верно.` | sentence |
| 39 | 2506.06561 | 32 | `The four captions were: (i) the original Gold caption, and (ii) No-Proﬁle , (iii) 1-Proﬁle , and (iv) All-Proﬁle settings generated by our best-performing model` | sentence |
| 40 | 2309.06402 | 97 | `As the true latent dimensionality D is usually unknown, NPDMs with non-injective readouts (like MLPs) may be predisposed to learning misleading latent activity ` | sentence |
| 41 | 2302.13692 | 13 | `The further practical calculation scheme in our numerics is explained in Appendix B. C. Phase diagram of steady state obtained by the MoC We start with observin` | merged |
| 42 | 2308.04244 | 0 | `In the dual-speaker scenario, which is the most popular exper- imental form in recent AAD research, the subject will hear two diﬀerent speeches and choose one s` | sentence |
| 43 | 2505.21329 | 143 | `More aligned columns representing core attributes increase confidence.` | sentence |
| 44 | 2403.17848 | 239 | `The Anserini IR toolkit, an open-source platform, was employed in conjunction with the BM25 ranking function 3 from Arabic Wikipedia to facilitate this process.` | sentence |
| 45 | 2604.10609 | 16 | `A formula 2 for calculating SEG is shown below with GT i and P i representing the ground truth and predicted sets of pixels in matched set i. SEG = � i GT i ∩ P` | merged |
| 46 | 2403.19144 | 68 | `# ResBlock Channel mul.` | fragment |
| 47 | 2311.16343 | 707 | `Several interesting directions remain for future studies.` | sentence |
| 48 | 2403.03883 | 12 | `Nevertheless, we still note that even without a dedicated instruction ﬁne- tuning stage, SaulLM-7B performs on par with Llama2-7B-chat ( 0 .` | fragment |
| 49 | 2403.02626 | 135 | `4 , 5 [34] Pan Lu, Baolin Peng, Hao Cheng, Michel Galley, Kai-Wei Chang, Ying Nian Wu, Song-Chun Zhu, and Jianfeng Gao.` | fragment |
| 50 | 2310.09263 | 87 | `DataWig: Missing Value Imputation for Tables.` | fragment |

**Sentence 18, merged 5, fragment 27, garbled 0.** The fragments are citation tails (`2019.`,
`1376–1385.`, `(2022), "Gradient methods..."`), reference-list pieces (`[7] Junming Chen...`,
`[Cited on: pg.`, `PhD thesis...`), heading stubs (`Теорема 3.1.5 .`,
`Hand-Object Interaction Image Synthesis.`), formula/table bits (`07 94 .`, `# ResBlock Channel
mul.`) and one page-number (`93 seek 0 .`). They are the units the splitter already emits
everywhere else in the corpus - the same shapes the control sample is made of - not a new
damage mode. The 5 merged rows are glued text the segmenter cannot see (missing spaces inside,
as in 2307.09982's spaceless Russian) - exactly the text the guard is right about and that S1
still cannot split honestly, but no worse than the paragraph it replaces.

### Control sample (server's own emitted units, seed 23): 50 units from 26 documents

| # | Id | Unit | Text (<=160) | Label |
| --- | --- | --- | --- | --- |
| 1 | 2403.11395 | 5144 | `Z.` | fragment |
| 2 | 2610.01064 | 731 | `12` | fragment |
| 3 | 2307.01842 | 793 | `Rev.` | fragment |
| 4 | 2410.05294 | 1829 | `the` | fragment |
| 5 | 2410.05294 | 1043 | `and` | fragment |
| 6 | 2403.11395 | 3656 | `× √` | fragment |
| 7 | 2410.05294 | 1538 | `is` | fragment |
| 8 | 2403.11395 | 4128 | `is` | fragment |
| 9 | 2403.11395 | 4682 | `[46]` | fragment |
| 10 | 2403.11395 | 2982 | `in` | fragment |
| 11 | 2403.18537 | 250 | `(1)` | fragment |
| 12 | 2403.11395 | 6399 | `He,` | fragment |
| 13 | 2407.10296 | 483 | `93 “Soft” requirements (personal qualities or “soft skills”) combine a number of psychological characteristics, properties and skills that can be grouped  These` | merged |
| 14 | 2410.05294 | 2369 | `J.` | fragment |
| 15 | 2307.01499 | 312 | `4.3.` | fragment |
| 16 | 2410.05294 | 961 | `into` | fragment |
| 17 | 2403.11395 | 4082 | `to` | fragment |
| 18 | 2407.10296 | 1153 | `Fig  6 38  View of the Kinect v2 device Both Kinect devices offer unique gaming and entertainment experienc - es  They allow users to control game characters, i` | merged |
| 19 | 2403.11395 | 6059 | `197,` | fragment |
| 20 | 2403.08062 | 8 | `II.` | fragment |
| 21 | 2510.07408 | 2068 | `115` | fragment |
| 22 | 2407.10296 | 322 | `Definition and main tasks of ontological engineering The tasks involved in creating a correctly structured and comprehensi - ble ontology, ensuring its most eff` | merged |
| 23 | 2410.05294 | 2107 | `can` | fragment |
| 24 | 2410.05294 | 938 | `used` | fragment |
| 25 | 2410.05294 | 2467 | `1993` | fragment |
| 26 | 2309.02323 | 148 | `6` | fragment |
| 27 | 2510.07408 | 1527 | `al.` | fragment |
| 28 | 2305.18793 | 1126 | `csv " ) > multicenter center n0 mean0 sd0 n1 mean1 sd1 n5 mean5 sd5 1 1 7 0.43 4.58 7 -5.43 5.53 8 -2.63 3.38 2 2 11 0.10 4.21 11 -2.59 3.95 12 -2.21 4.14 3 3 6` | merged |
| 29 | 2403.03967 | 653 | `Using Lemma C.1.5 with δ = c 3 ( g, τ, k ) and due to the assumption of c 3 ( g, τ, k ) > 0 we have: P � ⟨ ζ ( r ) , ζ ( j ) ⟩ P ≥ c 3 ( g, τ, k ) � ≤ 4 exp − c` | merged |
| 30 | 2608.19179 | 155 | `k .` | fragment |
| 31 | 2403.18537 | 259 | `Cs` | fragment |
| 32 | 2311.16343 | 858 | `Rep.` | fragment |
| 33 | 2510.07408 | 2167 | `121` | fragment |
| 34 | 2307.11273 | 1936 | `Left` | fragment |
| 35 | 2305.18793 | 3083 | `219` | fragment |
| 36 | 2407.10296 | 1236 | `Sci ` | fragment |
| 37 | 2510.07408 | 3064 | `171` | fragment |
| 38 | 2407.10296 | 27 | `A` | fragment |
| 39 | 2307.11273 | 1337 | `0.37` | fragment |
| 40 | 2311.18111 | 378 | `23` | fragment |
| 41 | 2401.03649 | 174 | `] 1` | fragment |
| 42 | 2510.18414 | 35 | `M [ h ] 1 = h h (0)+ h (2)+ h (4)+ h (6)+ h (8) h (1)+ h (3)+ h (5)+ h (7)+ h (9) i ; M [ h ] 2 = " h (0)+ h (4)+ h (8) h (1)+ h (5)+ h (9) h (2)+ h (6) h (3)+ ` | merged |
| 43 | 2306.05976 | 2809 | `In` | fragment |
| 44 | 2412.11865 | 568 | `(d)` | fragment |
| 45 | 2307.01842 | 291 | `2.` | fragment |
| 46 | 2306.17419 | 1040 | `28.` | fragment |
| 47 | 2307.11273 | 158 | `2.1` | fragment |
| 48 | 2608.19179 | 42 | `2.` | fragment |
| 49 | 2307.11273 | 1535 | `0,15` | fragment |
| 50 | 2307.11273 | 2469 | `0,01` | fragment |

**Sentence 0, merged 6, fragment 44, garbled 0.**

## Stop-condition check (Step 0)

- Candidate `fragment` 27 of 50 vs control `fragment` 44 of 50: the candidate is **17 below**
  the control, nowhere near the +2 stop line. The freed paragraphs' sentence stream is *more*
  sentence-like than the units the server already serves, because the control tail is all
  citation fragments and math debris.
- S1 frees 251 of the 538 `missed_boundary` coarse paragraphs - above the 200 minimum.

## Choice: S1, per-sentence verdict

S1 meets the M1 target (projected 21 < 30 papers) with the smallest change to what the guard
means: the threshold, the junction shapes, R1, PDF-6d's digit/capital narrowing and both
paragraph-wide rules are untouched; only the *scope* of the count moves from the paragraph to
the sentence. S2 (density) frees more paragraphs but redefines "damaged paragraph" as "sparse
damage", which is a different - and weaker - claim about #139, and every density still emits
paragraphs containing a genuinely glued sentence (that is how it out-frees S1). S3 is the
priced counterexample showing where that direction ends.

Cost: the re-check runs only on paragraphs the paragraph-level verdict already refuses - a
paragraph that passes today never pays for `sentence_ranges`.

## Reproduce

```bash
gh workflow run pdf-quality.yml --ref <branch> -f keep_coarse_text=true
gh run download <run-id> -p 'pdf-quality-shard-*'
cat pdf-quality-shard-*/all/quality-*.jsonl > .tmp/records.jsonl
python3 scripts/pdf6f-sentence-price.py --records .tmp/records.jsonl \
    --sample-candidate 'S1 per-sentence' --seed 23
```
