# Why PDF sentences come out too long, too short, or full of page furniture (PDF-6a)

Measurement only. No product code changed, nothing compiled or run on the measuring host: the
numbers come from GitHub Actions, the corpus is the committed PDF-5a list, and the PDFs are
never committed.

- Corpus: `scripts/pdf-sweep-ids.txt`, the same 121 CC-BY / CC-BY-SA / CC0 arXiv papers PDF-5a
  measured, byte-identical (`sha256 254624fedbdb7f7675e1e8ada8efa17f5252cdf2c474d7f286bd7f43c7b73342`,
  121 entries, 4,842 pages).
- Harness: `scripts/pdf-quality.py` (new, run mode + `--aggregate`) and
  `.github/workflows/pdf-quality.yml` (new, `workflow_dispatch` plus a `[quality-run]` push
  trigger on `docs/pdf6a-**`, because a dispatch needs the workflow on the default branch). It
  reuses the PDF-5a sweep's downloader, licence re-check, capped-server helper and PyMuPDF
  reference text (`scripts/pdf-sweep.py`, `scripts/pdf-probe.py`, `scripts/pdf-corpus.py` are
  imported and unchanged); PDF-5a's own output is untouched.
- Server settings, identical to PDF-5a: release binary of the run's head, `READING_MCP_TOOLS=v1`,
  OCR off (`READING_MCP_OCR_ENABLED=false`), **default** PDF backend
  (`READING_MCP_PDF_LAYOUT_PYTHON` unset), `READING_MCP_MAX_DOCUMENT_BYTES=134217728`, every
  open inside `sudo systemd-run --scope -p MemoryMax=600M -p MemorySwapMax=0`.
- Reading: `open_document`, `get_document_structure`, then every section's
  `get_text_units` (`requested_kind=sentence`, `coverage_policy=preserve_source`) in 256-item
  batches, keeping each item's `text`, `effective_kind`, `content_class_detail` and
  `degradation`. That is the reader's own text, unit by unit.
- Reference text: PyMuPDF `page.get_text()` for every page, independent of the backend, plus
  per-page lines and the font inventory.
- Per-unit records (every long unit and every short unit, with its cause, its page, its
  evidence and its what-if split) go into the run artifact as JSONL; git holds only this note
  and the code.

## Runs

| What | Run | Head | Outcome |
| --- | --- | --- | --- |
| quality probe, the numbers below | [`44232508228`](https://github.com/liqiangcc/reading-mcp/actions/runs/44232508228) | `6fd7fbc` | 121 of 121 documents measured, 0 failed, 0 skipped |
| plain `PDF wide sweep` on the same head | RUN_SWEEP | HEAD_SWEEP | SWEEP_OUTCOME |

Both runs are on code that differs from the last commit of this branch only in this file.

## What the measurement found, in one paragraph

Of 79,553 units over 121 papers, 2,672 are longer than 800 characters and 10,841 shorter than 5.
The two populations have almost nothing in common. **92.4% of the long units are not sentences at
all**: they are whole paragraphs returned as one item because `looks_like_unreliable_fallback_text`
refused to split them (`degradation=requested_sentence_but_paragraph_is_coarse_only`), so the
800-character question is mostly a question about that guard, not about the terminal rules. The
short units are mostly page furniture that arrives as its own line and therefore its own
paragraph: a table-of-contents dot leader or a maths ellipsis becomes a one-character unit
(4,952 of them, 45.7% of all short units), a heading's own number becomes a unit (1,236), a page
number becomes a unit (881). Running headers are almost never their own unit (13 of 457
occurrences): they are the first line of a body paragraph or buried inside it, so removing them
needs the line breaks and the missing spaces fixed first. Silent loss is rare and mostly not
loss at all: the median 5-gram containment of the first 20 sentences is 0.982, 14 papers fall
below 0.9, but 12 of those 14 return *more* characters than PyMuPDF does (ratios 1.05-1.72), so
their gap is wrong code points or duplicated runs; only 2403.02163 (0.287 of the reference
characters) and 2311.05131 (0.805) return less.

## The separators the code puts between chunks and between pages

Read from `main` at `4993b1f`. "Chunk" is one `Ok` item of lopdf 0.44
`extract_text_chunks_with_limit`: a run of `Tj`/`TJ` text closed at a font change (`Tf`), with a
newline appended by `'`, `"`, `T*` and `ET` (`docs/design/pdf1c-extraction.md` §1).

| Where the text comes from | Code | Between chunks in one page | Between pages |
| --- | --- | --- | --- |
| `PageText.text` (the raw page text) | `extract_page_texts_raw` (`src/parsing/pdf.rs:306`), `normalize_pdf_text` (`:354`) | **nothing** — `text.push_str(&fragment)` per chunk, then one `trim()` | n/a |
| `PageText.chunks` (the fragments) | same function, `:316-330` | each chunk is trimmed and kept separately | n/a |
| inferred-heading sections | `build_inferred_heading_sections` (`:653`) → `PageFragments::push_line` (`:75`) → `content_and_bindings_from_fragments` (`:764`) | **`"\n"`** (one newline) | **`"\n\n"`** (blank line) |
| page-fallback sections | `build_page_sections` (`:803`) | **nothing** — the section content *is* `PageText.text` | n/a (one section per page) |
| native-TOC sections | `source_text_and_bindings_for_page_range` (`:956`) | **nothing** — it reads `PageText.text` | **`"\n\n"`** (blank line) |

Two consequences that matter for the measurements below:

- A paragraph ends only at a blank line (`paragraph_ranges`, `src/domain/segmentation.rs:727`), so
  **only a page boundary can end a paragraph** in a PDF: the heading path ends a paragraph per
  page (the `"\n\n"`), the page-fallback path has no paragraph break inside a page at all, and
  the TOC path has one per page. A page whose text has no blank line is therefore one paragraph,
  however many lines it really has. §1 measures the result: 107 of 121 papers take the heading
  path and still end up with a median of 1.32 paragraphs per page.
- The task description says the inferred-heading path joins the trimmed chunks with `"\n\n"`.
  The code joins the chunks **of one page** with a single `"\n"` and joins **pages** with
  `"\n\n"` (`PageFragments::push_line` at `:75-86` vs `content_and_bindings_from_fragments` at
  `:764-801`). Only the page boundary is a paragraph boundary.

Sentence boundaries (`src/domain/segmentation.rs`):

- `.` `!` `?` end a sentence only when the terminal cluster is followed by whitespace or the end
  of the paragraph (`ascii_terminal_is_terminal`, `:814`) — a mark followed directly by a letter
  never splits.
- `.` between two identifier characters, between two digits, before `/` or `\`, after a protected
  abbreviation (`e.g`, `i.e`, `etc`, `fig`, `no`, `vol`-free list at `:893`) or after a
  single-letter token before an uppercase word never splits (`ascii_period_is_terminal`, `:819`).
- A paragraph that `looks_like_unreliable_fallback_text` (`:1016`) — 3 or more missing-space glue
  events (`:1024`, e.g. `.Next`, `,Next`, `)provided`), a pairwise-doubled token, or a
  bibliographic masthead run — becomes `CoarseParagraphOnly`, and `get_text_units` then returns
  **the whole paragraph as one item** with
  `degradation=requested_sentence_but_paragraph_is_coarse_only`
  (`src/application/text_unit_stream.rs:331-384`). PDF paragraphs are `ProseOrUnknown` (no
  parser-native block map), so this guard applies to them.

## 1. Whitespace (H1)

**Verdict: partly refuted as a cause of long units, confirmed as a cause in three papers.**
Missing whitespace is not the general problem: the backend's whitespace share is *higher* than
PyMuPDF's (median ratio 1.122), because a chunk closed by `'`, `"`, `T*` or `ET` keeps its
newline. Only 3 of 121
papers fall below 70% of the reference — 2308.01909 (0.283), 2404.05778 (0.386) and 2403.02247
(0.418) — and those three are also where the glued long units and the glued headers come from.

What *is* systematically missing is the **newline**, not the space: 2403.04799 has 6.29 newlines
per page against the reference's 56.57 while its whitespace share is 1.001 of the reference.
That is the H2 finding: chunks are joined with nothing, so a page becomes one run of text.

Whitespace share = whitespace characters / all characters, backend text against PyMuPDF `page.get_text()` over the same pages.

| Measure | Median | p05 | p25 | p75 | p95 | Min | Max |
| --- | --- | --- | --- | --- | --- | --- | --- |
| backend whitespace share | 0.1771 | 0.1447 | 0.1647 | 0.2139 | 0.2720 | 0.0477 | 0.3181 |
| reference whitespace share | 0.1602 | 0.1445 | 0.1531 | 0.1750 | 0.1968 | 0.1400 | 0.2624 |
| backend / reference | 1.1220 | 0.9020 | 1.0640 | 1.2400 | 1.4690 | 0.2830 | 1.6220 |

Documents whose backend whitespace is below 70% of the reference: **3** of 121. The table lists
the ten lowest ratios, not only the three that fall below the threshold.

| Id | Backend | Reference | Ratio | Backend newlines/page | Reference newlines/page |
| --- | --- | --- | --- | --- | --- |
| 2308.01909 | 0.0477 | 0.1688 | 0.283 | 4.83 | 41.67 |
| 2404.05778 | 0.0625 | 0.1619 | 0.386 | 12.31 | 44.25 |
| 2403.02247 | 0.0606 | 0.1449 | 0.418 | 9.08 | 74.77 |
| 2405.04391 | 0.1442 | 0.2015 | 0.715 | 6.24 | 44.59 |
| 2401.14345 | 0.2035 | 0.2624 | 0.776 | 24.69 | 439.53 |
| 2401.03649 | 0.1681 | 0.1894 | 0.888 | 40.76 | 143.68 |
| 2306.09672 | 0.1576 | 0.1747 | 0.902 | 16.85 | 62.91 |
| 2309.14539 | 0.1869 | 0.1979 | 0.944 | 22.13 | 65.47 |
| 2403.04799 | 0.1447 | 0.1445 | 1.001 | 6.29 | 56.57 |
| 2406.17961 | 0.1574 | 0.1568 | 1.004 | 6.29 | 105.94 |

| Structure | Documents | Median paragraphs/page |
| --- | --- | --- |
| `headings` | 107 | 1.32 |
| `page_fallback` | 14 | 16.57 |

Paragraphs against pages (H2): 7 of 121 documents have fewer paragraphs than pages (1 has at most half).

| Id | Paragraphs | Pages | Paragraphs/page |
| --- | --- | --- | --- |
| 2403.02163 | 3 | 12 | 0.25 |
| 2311.01358 | 8 | 9 | 0.89 |
| 2306.05976 | 281 | 291 | 0.97 |
| 2312.13331 | 32 | 33 | 0.97 |
| 2604.02042 | 35 | 36 | 0.97 |
| 2311.18111 | 300 | 307 | 0.98 |
| 2305.18793 | 487 | 490 | 0.99 |
| 2312.17065 | 21 | 21 | 1.00 |
| 2309.14539 | 15 | 15 | 1.00 |
| 2401.16464 | 37 | 37 | 1.00 |
The second table is the H2 measurement hiding in this section: 107 papers take the heading path
and still end up with a median of 1.32 paragraphs per page, i.e. one paragraph per page, because
only a page boundary ends a paragraph. The 14 page-fallback papers reach 16.57 paragraphs per
page for the opposite reason: every chunk becomes its own line, and every line its own paragraph.

## 2. Long units (> 800 characters)

**Verdict: confirmed, and the dominant mechanism is the coarse-paragraph guard, not the terminal
rules.** 2,672 long units in 112 papers; 61 papers have more than 5% of their units long, which
reproduces PDF-5a's 61 exactly.

Long units: 2672 in 112 documents; 61 documents have more than 5% long units.

| Cause | Units | Documents | Share of long units | What-if: units split away | What-if: documents below 5% |
| --- | --- | --- | --- | --- | --- |
| `glued_terminal` | 277 | 15 | 0.1037 | 277 | 1 of 61 |
| `glued_words` | 583 | 10 | 0.2182 | 583 | 0 of 61 |
| `bibliography` | 586 | 92 | 0.2193 | 586 | 7 of 61 |
| `math_table` | 41 | 26 | 0.0153 | 41 | 1 of 61 |
| `missed_boundary` | 645 | 89 | 0.2414 | 645 | 32 of 61 |
| `genuine` | 540 | 24 | 0.2021 | 540 | 0 of 61 |

| Cause | 3 samples (arXiv id, first 200 characters) |
| --- | --- |
| `glued_terminal` | `2403.02247` arXiv:2403.02247v1 [cs.CL] 4 Mar 2024 Birbal:Aneﬃcient7Binstruct-modelﬁne-tunedwith curateddatasets AshviniKumarJindal ajindal@linkedin.com LinkedInAI,USA PawanKumarRajpoot pawan.rajpoot2411@gmail.com |
| `glued_terminal` | `2403.02247` cessing and trainingcode impedes fullreproducibility, as observed in thecaseofRedPajama Computer (2023),anopenreproductionofLlama’s data. Toaddressthe lack oftransparencyinmodel trainingand dem ocrati |
| `glued_terminal` | `2403.02247` • DatasetCurationvsGeneration: ThesuccessofrecentLLM-generateddatasets likeStanfordAlpacaTaorietal.(2023)ispromising.Howev er,only open-sourcebase modelscanbe used for datasetgeneration.Wecancurateahi |
| `glued_words` | `2403.02247` Mayne,BobMcGrew, ScottMayerMcKinney,ChristineMcLeave y,PaulMcMillan,Jake McNeil,DavidMedina,AalokMehta,JacobMenick,LukeMetz, AndreyMishchenko, PamelaMishkin,VinnieMonaco,EvanMorikawa, DanielMossi ng,T |
| `glued_words` | `2401.01137` 10GUO-DONGHONGANDZILILIM theidealgenerated bythefollowingpolynomials: 4 Y i =1 b( y i ) d ( y i ) F ( y 1 ) − G ( y 1 ) − F ( y 2 )+ G ( y 2 ) − F ( y 3 )+ G ( y 3 )+ F ( y 4 ) − G ( y 4 ) (28) 8 Y i  |
| `glued_words` | `2401.01137` , whichisboundedby ∥ b f 0 ∥ l 1 ∥ b D∥ l ∞ byapplyingH¨older’sinequality. Thefollowingpropositionsaysthatthedualfunctioniscon trolledbyshorterprogressions. Proposition 5.3. Let F ( t ) , G ( t ) ∈ Q  |
| `bibliography` | `2403.19012` Reﬂection Prompt Describe what you found most interesting in to- day’s class Student Reﬂections • Nothing in particular today -> 1.0 • Despite the confusion, I did ﬁnd setting up these problems to be  |
| `bibliography` | `2403.19012` phrases to compress the reﬂections for supporting mobile applications. Magooda and Litman ( 2020 ) utilized neural models with a focus on generat- ing abstractive summaries in a low-resource con- text |
| `bibliography` | `2403.19012` ROUGE scores (R-1/R-2/R-L) ( Lin , 2004 ), which are 48.31/27.57/43.52 and 30.16/6.77/27.91 for the extractive and abstractive summarization tasks, re- spectively. These scores are slightly lower than |
| `math_table` | `2401.01137` ≤ ∥ f 2 ∥ 2 X n 2 ,n 1 ,m 1 b f 0 ( − n 1 − n 2 ) b f 0 ( − m 1 − n 2 ) b f 1 ( n 1 ) b f 1 ( m 1 ) K ( n 1 , n 2 ) K ( m 1 , n 2 ) 1 / 2 . (17) Onceagain,rearrange,applyCauchy–Schwarzinequalityin n 1 |
| `math_table` | `2305.18793` The kindergarten data are below: treatment = list ( c (1 ,1 ,0 ,0) , c (1 ,1 ,0 ,0) , c (1 ,1 ,1 ,0 ,0) , c (1 ,1 ,0 ,0) , c (1 ,1 ,0 ,0) , c (1 ,1 ,0 ,0) , c (1 ,1 ,0 ,0) , c (1 ,1 ,1 ,1 ,0 ,0) , c ( |
| `math_table` | `2305.18793` csv " ) > multicenter center n0 mean0 sd0 n1 mean1 sd1 n5 mean5 sd5 1 1 7 0.43 4.58 7 -5.43 5.53 8 -2.63 3.38 2 2 11 0.10 4.21 11 -2.59 3.95 12 -2.21 4.14 3 3 6 2.58 4.80 6 -3.94 4.25 7 1.29 7.39 4 4  |
| `missed_boundary` | `2403.02247` OpenEvalScore (weightedat1/3)and ClosedEvalScore (weighted at2/3). ethics justice , ethics commonsense , ethics virtue , ethics deontology ,and ethics utilitarianism aredenoted as ethics j , ethics c  |
| `missed_boundary` | `2403.19012` Model R-1 R-2 R-L BS EM F1 P F1 LexRank 56.96 40.45 55.10 89.95 31.33 37.16 BERTSUM-EXT (cnndm) 55.21 38.29 53.39 89.91 34.08 37.74 BERTSUM-EXT (ft. F EW S UMM A MAZON ) 55.51 38.41 53.59 89.88 33.85  |
| `missed_boundary` | `2403.19012` Model Prompt GPT-reﬂect " reflections: {{reflections}} Can you summarize the reflections, which are split by the special token ∥ _ ∥ , by extracting and selecting 5 original reflections from the split |
| `genuine` | `2403.02247` BIG-bench EM(Accuracy)Open0.330.380.0 MWR Open0.75 0.87 0.06 GSM8K EM(Accuracy)Open0.440.570.0 MWR Open0.62 0.81 0.03 BBQ EM(Accuracy)Open0.740.850.93 MWR Open0.250.56 0.75 sam sum ROUGE-2Closed0.130. |
| `genuine` | `2403.17891` (a) U 1 for normal and abnormal classes (b) U 2 for normal and abnormal classes Figure 14: U1 and U2 values comparison l soft ˆ y (ˆ y ) log f ˆ y ( ˜ x ) = l soft ˆ y (ˆ y ) log f ˆ y ( x ) − ϵ sign  |
| `genuine` | `2305.18793` xxiv Acronyms acronym full name first chapter ACE average causal effect 2 AI Abadie and Imbens (for matching estimators) 15 ANCOVA analysis of covariance 6 BMI body mass index 2 BRE Bernoulli randomiz |

Long units by the kind the server reports (a coarse paragraph is a paragraph the fallback guard refused to split):

| Unit kind | Long units | Documents |
| --- | --- | --- |
| sentence | 204 | 31 |
| coarse_paragraph | 2468 | 110 |

Most common token before a missed boundary: `` 903, `dataset` 44, `models` 35, `al` 33, `respectively` 29, `1` 29, `2` 28, `model` 28, `proof` 23, `4` 22.
Three things decide how to read this table.

1. **2,468 of the 2,672 long units (92.4%) are `coarse_paragraph`, and only 204 are `sentence`**
   (110 papers against 31). For the 2,468 the sentence splitter never ran: the paragraph was
   flagged as unreliable and handed back whole. Any change to the terminal rules is therefore
   gated on that guard; nothing else can move the 800-character population as much.
2. **The causes are what a splitter *could* do, measured on the text as it arrives.** The
   what-if column splits only at the boundaries the cause rule found and keeps every other unit,
   so it is a lower bound on the payoff: `missed_boundary` alone (a terminal mark followed by
   whitespace and a capital, `[.?!]["')\] ]?\s+(?=[A-Z])`) would take 32 of the 61 papers below
   the 5% line; `bibliography` 7; `glued_terminal` and `math_table` 1 each; `glued_words` and
   `genuine` 0.
3. **`glued_words` (583 units in 10 papers) and `glued_terminal` (277 in 15) are H1, not H3**:
   their text has lost its spaces ("Birbal:Anefficient7B…", "cessing and trainingcode impedes").
   `genuine` (540 in 24) is the residual: real long sentences, maths and legal prose, where 800
   characters is simply a long sentence.

The most common token before a missed boundary is *no token at all* (903 occurrences: the mark
follows whitespace, which is the dot-leader and maths-ellipsis shape from §3), then `dataset` 44,
`models` 35, `al` 33 (`et al.` — a protected abbreviation, so the split is suppressed by design),
`respectively` 29, `1` 29, `2` 28, `model` 28, `proof` 23, `4` 22.

## 3. Short units (< 5 characters)

**Verdict: confirmed, but only one of the six causes is a sentence-splitter mistake.** 10,841
short units in 93 papers; 29 papers have more than 10% of their units short, which reproduces
PDF-5a's 29 exactly.

Short units: 10841 in 93 documents; 29 documents have more than 10% short units.

| Cause | Units | Documents | Share of short units |
| --- | --- | --- | --- |
| `page_number` | 881 | 51 | 0.0813 |
| `marker` | 352 | 30 | 0.0325 |
| `heading_number` | 1236 | 53 | 0.1140 |
| `math` | 443 | 25 | 0.0409 |
| `hyphen_fragment` | 74 | 5 | 0.0068 |
| `other` | 7855 | 75 | 0.7246 |

| Cause | 3 samples (arXiv id, text) |
| --- | --- |
| `page_number` | `2403.17891` 3 |
| `page_number` | `2403.17891` 11 |
| `page_number` | `2403.17891` 12 |
| `marker` | `2403.17891` 5 . |
| `marker` | `2403.17891` 1 . |
| `marker` | `2403.01862` 3). |
| `heading_number` | `2403.19012` 3. |
| `heading_number` | `2403.19012` 4. |
| `heading_number` | `2403.19012` 2. |
| `math` | `2401.01137` = |
| `math` | `2312.17065` p. |
| `math` | `2305.18793` y0 ) |
| `hyphen_fragment` | `2407.10296` an |
| `hyphen_fragment` | `2407.10296` ad- |
| `hyphen_fragment` | `2407.10296` Me- |
| `other` | `2403.19012` A.2. |
| `other` | `2403.19012` B.2. |
| `other` | `2403.19012` B.3. |

Most common `other` short units: `.` 4952, `of` 178, `and` 170, `the` 159, `Rev.` 106, `to` 91, `in` 87, `is` 61, `•` 60, `that` 52.

| Id | Unit | Before | After |
| --- | --- | --- | --- |
| 2403.17891 | `.` | `Assume we have a dataset D = { ( x n , y` | `.` |
| 2407.10296 | `of` | `development` | `information` |
| 2407.10296 | `and` | `rendering` | `informatization` |
| 2407.10296 | `the` | `finally,` | `use` |

Page-number units whose page could not be inferred from the reference: 420.
Reading the `other` bucket, which is 72% of the population:

- **`.` is 4,952 of the 7,855 `other` units, and 3,732 of those are in five papers** (2510.18203
  2,103, 2306.05976 505, 2305.18793 424, 2311.18111 350, 2505.24699 350). The neighbours say what
  they are: in 2510.18203 the unit before and after a `.` is another `.` — a table-of-contents dot
  leader, drawn as one glyph per chunk and separated by spaces — and in 2306.05976 the context is
  `… 𝑖, 𝑗 = 1 , . . . , 𝑁 𝑐` — a maths ellipsis written as three spaced periods. A period with
  whitespace on both sides is a terminal under `ascii_terminal_is_terminal`, so each becomes a
  one-character sentence. This is the one real splitter bug in the short population.
- The rest of `other` is headings that arrive as their own line: `A.2.`, `B.2.`, `Rev.` (106,
  a reference abbreviation), `of`/`and`/`the` (178/170/159, all in 2407.10296, where the whole
  passage arrives one word per unit — the neighbours of `of` are `development` and
  `information`).
- `heading_number` (1,236 units, 53 papers) is the same shape one step earlier: the heading
  number `3.` and its title are separate lines, so the number is its own paragraph.
- `page_number` (881 units, 51 papers) is genuine page furniture that arrives as its own line;
  420 of them could not be confirmed against the inferred page, so that count is a lower bound.

## 4. Running headers and footers (H5)

**Verdict: confirmed that headers and footers reach the reader, refuted that they can be removed
at unit level as things stand.** They arrive glued to body text, not as units of their own.

Two populations, because the threshold decides the answer: the task's tier (a line on >= 80% of the pages, a page-number line on >= 50%) and PDF-5a's looser 30% proxy, which is where most real running headers sit.

### Tier `spec` (>= 80% / >= 50%)

Documents: 30; candidate lines: 45.

| Where the line lands | Occurrences |
| --- | --- |
| `own_unit` | 13 |
| `prefix` | 217 |
| `suffix` | 0 |
| `inside` | 130 |
| `glued` | 75 |
| `not_found` | 22 |
| `skipped_short` | 0 |

| Where the line sits on its page | Occurrences |
| --- | --- |
| `start` | 291 |
| `end` | 16 |
| `both` | 0 |
| `middle` | 23 |

| Id | Line | Kind | Pages | Cases | Position |
| --- | --- | --- | --- | --- | --- |
| 2310.09263 | `Conference'17, July 2017, Washington, DC, USA` | repeated_line | 47 | inside=27, prefix=26 | middle=1, start=46 |
| 2403.01601 | `A Comparison of Proximity Indices Applied to OpenAlex` | repeated_line | 26 | inside=16, prefix=15 | start=26 |
| 2401.16311 | `1` | page_number | 25 | not_found=1 |  |
| 2401.16311 | `2` | page_number | 25 | not_found=1 |  |
| 2403.03592 | `Proceedings on Privacy Enhancing Technologies YYYY(X)` | repeated_line | 20 | inside=13, prefix=13 | start=20 |
| 2604.02042 | `0` | page_number | 19 | not_found=1 |  |
| 2604.10609 | `A PREPRINT` | repeated_line | 18 | glued=32 | middle=1, start=17 |
| 2401.03649 | `1` | page_number | 18 | not_found=1 |  |
| 2604.10609 | `Self-supervised Pretraining of Cell Segmentation Models` | repeated_line | 17 | prefix=31 | start=17 |
| 2311.11050 | `Kulahci et al.: Preprint submitted to Elsevier` | repeated_line | 17 | inside=15, prefix=2 | end=16, middle=1 |
| 2311.11050 | `Functional neural network control chart` | repeated_line | 17 | own_unit=1, prefix=16 | start=17 |
| 2502.07239 | `MM '25, October 27-31, 2025, Dublin, Ireland` | repeated_line | 16 | inside=18, prefix=11 | start=16 |
| 2401.02387 | `PREPRINT` | repeated_line | 14 | glued=20 | middle=1, start=13 |
| 2402.13288 | `Training Table Question Answering via SQL Query Decompositio` | repeated_line | 14 | prefix=22 | start=14 |
| 2402.13288 | `PREPRINT` | repeated_line | 14 | glued=23 | start=14 |

### Tier `loose` (>= 30% (PDF-5a's proxy))

Documents: 79; candidate lines: 255.

| Where the line lands | Occurrences |
| --- | --- |
| `own_unit` | 79 |
| `prefix` | 577 |
| `suffix` | 36 |
| `inside` | 474 |
| `glued` | 221 |
| `not_found` | 111 |
| `skipped_short` | 44 |

| Where the line sits on its page | Occurrences |
| --- | --- |
| `start` | 1075 |
| `end` | 17 |
| `both` | 1 |
| `middle` | 270 |

| Id | Line | Kind | Pages | Cases | Position |
| --- | --- | --- | --- | --- | --- |
| 2510.18203 | `0` | page_number | 174 | not_found=1 |  |
| 2510.18203 | `CHAPTER 5. SOLUTIONS TO SELECTED EXERCISES` | repeated_line | 172 | not_found=1 | start=172 |
| 2510.18203 | `1` | page_number | 167 | not_found=1 |  |
| 2510.18203 | `2` | page_number | 153 | not_found=1 |  |
| 2306.05976 | `1` | page_number | 135 | own_unit=1 |  |
| 2407.10296 | `S. Kotlyk. MODERN INFORMATION TECHNOLOGIES IN SCIENTIFIC RES` | repeated_line | 134 | not_found=1 | start=134 |
| 2311.18111 | `2` | page_number | 121 | not_found=1 |  |
| 2311.18111 | `1` | page_number | 121 | not_found=1 |  |
| 2306.05976 | `2` | page_number | 110 | not_found=1 |  |
| 2310.09263 | `Conference'17, July 2017, Washington, DC, USA` | repeated_line | 47 | inside=27, prefix=26 | middle=1, start=46 |
| 2310.09263 | `Prompt:` | repeated_line | 29 | skipped_short=1 |  |
| 2310.09263 | `Completion:` | repeated_line | 28 | inside=36 | middle=28 |
| 2403.01601 | `A Comparison of Proximity Indices Applied to OpenAlex` | repeated_line | 26 | inside=16, prefix=15 | start=26 |
| 2401.16311 | `1` | page_number | 25 | not_found=1 |  |
| 2401.16311 | `2` | page_number | 25 | not_found=1 |  |

### The most repeated line of every document

| Id | Lines | Most repeated line (pages, share of pages) |
| --- | --- | --- |
| 2403.02247 | 914 | `0.64` (4, 0.308) |
| 2401.01137 | 1001 | `GUO-DONG HONG AND ZI LI LIM` (8, 0.471) |
| 2401.16311 | 2704 | `1` (25, 0.694) |
| 2302.13692 | 1485 | `2` (7, 0.500) |
| 2308.04244 | 911 | `Conference acronym 'XX, June 03-05, 2018, Woodstock, NY` (9, 1.000) |
| 2309.14539 | 851 | `F. FRICK AND Z. WELLNER` (7, 0.467) |
| 2604.10609 | 811 | `A PREPRINT` (18, 1.000) |
| 2403.04799 | 372 | `Published as a conference paper at the Global AI Cultures Wo` (7, 1.000) |
| 2403.18158 | 1116 | `Preprint - The effects of short video-sharing services on vi` (7, 0.875) |
| 2403.17992 | 848 | `1` (3, 0.300) |
| 2403.19009 | 768 | `of Robustness` (2, 0.333) |
| 2401.16464 | 1793 | `Ali et al.` (18, 0.486) |
| 2406.06945 | 942 | `1` (13, 0.867) |
| 2307.01842 | 2172 | `1` (13, 0.448) |
| 2401.03649 | 2706 | `1` (18, 0.529) |
| 2403.05685 | 1115 | `0` (6, 0.600) |
| 2407.10296 | 10496 | `S. Kotlyk. MODERN INFORMATION TECHNOLOGIES IN SCIENTIFIC RES` (134, 0.489) |
| 2306.09672 | 1680 | `YU ZHAO` (18, 0.529) |
| 2502.13422 | 1978 | `Notes:` (6, 0.353) |
| 2403.02163 | 884 | `0` (5, 0.417) |
| 2403.18133 | 838 | `AE SemRL: Learning Semantic Association Rules with Autoencod` (11, 0.917) |
| 2311.15310 | 1895 | `Yizheng Zhu, Yuncheng Wu, Zhaojing Luo, Beng Chin Ooi, and X` (7, 0.467) |
| 2510.18414 | 600 | `1` (6, 0.667) |
| 2311.16229 | 1285 | `0` (7, 0.583) |
| 2405.16182 | 1245 | `1` (12, 0.522) |
| 2307.01499 | 674 | `COMPARING DENDRITIC TREES WITH ACTUAL TREES` (7, 0.500) |
| 2311.16375 | 975 | `2` (13, 0.481) |
| 2304.03209 | 911 | `C. You et al.` (6, 0.462) |
| 2510.18203 | 21453 | `0` (174, 0.478) |
| 2609.25703 | 1848 | `1` (10, 0.303) |
| 2306.11843 | 1295 | `WebTables` (4, 0.333) |
| 2403.03883 | 1356 | `arXiv preprint` (4, 0.308) |
| 2403.04923 | 1029 | `PROTEINS` (3, 0.375) |
| 2403.17571 | 760 | `Cl0p` (4, 0.444) |
| 2312.16335 | 2003 | `2` (8, 0.320) |
| 2603.26932 | 2017 | `1` (9, 0.333) |
| 2402.18422 | 1064 | `0` (4, 0.364) |
| 2311.16343 | 2470 | `1` (13, 0.619) |
| 2410.00944 | 614 | `F. Nerrise et al.` (6, 0.500) |
| 2311.11050 | 1022 | `Functional neural network control chart` (17, 1.000) |
| 2307.03827 | 759 | `Ghazvanchahi Maralani Moody Khademi` (9, 0.500) |
| 2609.37159 | 139 | `LORENZO CARLUCCI AND ANDREA D'AMICO` (3, 0.750) |
| 2310.09263 | 3464 | `Conference'17, July 2017, Washington, DC, USA` (47, 1.000) |
| 2403.17848 | 1473 | `SIGIR '24, July 14-18, 2024, Washington, USA` (11, 1.000) |
| 2403.02681 | 1025 | `Y. Sun et al.` (8, 0.500) |
| 2403.03592 | 2615 | `Proceedings on Privacy Enhancing Technologies YYYY(X)` (20, 0.952) |
| 2312.10273 | 1470 | `Unpublished working draft.` (11, 1.000) |
| 2403.08062 | 1440 | `1` (5, 0.385) |
| 2311.01358 | 447 | `The Universal Statistical Structure and Scaling Laws of Chao` (8, 0.889) |
| 2312.01833 | 468 | `2` (4, 0.444) |
| 2312.13331 | 1912 | `1` (10, 0.303) |
| 2511.09857 | 1394 | `XIAOKANG WANG` (15, 0.517) |
| 2506.06561 | 1262 | `ROUGE` (5, 0.333) |
| 2403.17856 | 563 | `intransitive` (2, 0.333) |
| 2312.14388 | 1196 | `2` (6, 0.667) |
| 2403.09706 | 990 | `MTSQL` (3, 0.300) |
| 2308.14580 | 514 | `RITIK JAIN, HAN-BOM MOON, AND PETER WU` (7, 0.583) |
| 2307.11273 | 2286 | `Right` (10, 0.345) |
| 2404.07090 | 1469 | `2` (9, 0.643) |
| 2307.02625 | 779 | `-∥ci -cj∥2` (2, 0.333) |
| 2311.18111 | 14893 | `2` (121, 0.394) |
| 2604.02042 | 1730 | `0` (19, 0.528) |
| 2412.16346 | 902 | `IEEE ROBOTICS AND AUTOMATION LETTERS. PREPRINT VERSION. ACCE` (5, 0.625) |
| 2403.03967 | 2658 | `1` (15, 0.484) |
| 2403.01601 | 1188 | `A Comparison of Proximity Indices Applied to OpenAlex` (26, 0.963) |
| 2301.07141 | 1756 | `0` (8, 0.381) |
| 2403.18069 | 1716 | `0` (10, 0.333) |
| 2401.02387 | 1018 | `PREPRINT` (14, 1.000) |
| 2306.17419 | 708 | `2` (8, 0.571) |
| 2610.01064 | 1446 | `1` (5, 0.417) |
| 2502.07239 | 1740 | `MM '25, October 27-31, 2025, Dublin, Ireland` (16, 0.941) |
| 2403.18973 | 1650 | `1` (8, 0.381) |
| 2403.19144 | 1156 | `S. Kim et al.` (16, 0.500) |
| 2403.17729 | 1687 | `Conference'17, July 2017, Washington, DC, USA` (11, 1.000) |
| 2403.01809 | 1035 | `K. Wolsing et al.` (10, 0.476) |
| 2402.13288 | 1025 | `PREPRINT` (14, 0.933) |
| 2608.19179 | 932 | `mod Zp` (7, 0.333) |
| 2306.13140 | 1209 | `0` (13, 0.448) |
| 2401.14345 | 8240 | `1` (16, 0.444) |
| 2403.05969 | 577 | `2` (10, 0.500) |
| 2306.05976 | 14930 | `1` (135, 0.464) |
| 2603.17845 | 1347 | `Archit Pape` (16, 0.500) |

(The run artifact lists this line for all 121 documents; the rows above are the documents where it covers at least 30% of the pages.)
How to read it:

- **A running header is almost never its own unit.** In the task's tier, 13 of 457 occurrences are
  `own_unit`; 217 are the *prefix* of a body unit, 130 are *inside* one, 75 are present only
  without their spaces (`glued`) and 22 are not in the reader's text at all. The 30% tier says the
  same at scale: 79 of 1,506.
- **Where they sit** decides the removal: 291 of the 330 page occurrences in the task's tier are at
  the start of the page, 16 at the end, 23 in the middle. A start-of-page line is exactly what the
  line reconstruction would separate — if it kept the line break.
- **The "page-number line on >= 50% of pages" rule cannot find a page number.** A real page number
  differs on every page, so it can never be a repeated line; the four candidates that rule does
  find (`1`, `2`, `0`, `1`) are constant digits inside tables, and none of them is a unit. The real
  page numbers are the 881 `page_number` units of §3.
- **The 30% tier is noisier than the 80% tier**: it catches table values (`0.57`, `0.49`, `0.64` in
  2403.02247) and labels (`Prompt:`, `Completion:` in 2310.09263) as well as real furniture. The
  per-document table above lists each paper's most repeated line with the share of pages it covers,
  which is the honest way to see the population: the real headers sit at 30-49% of pages (2407.10296
  0.489, 2401.01137 0.471, 2309.14539 0.467, 2311.15310 0.467), and 82 of 121 papers have a line on
  at least 30% of their pages.

## 5. Silent loss and bad characters (H6)

**Verdict: confirmed for 6 papers, each with a different mechanism, and PDF-1c's CJK-only form
inlining is involved in exactly one of them.** Loss is not widespread, and most of what looks like
loss is damage. The median 5-gram containment of the first 20 sentences is 0.982; 14 papers fall
below 0.9, and 12 of those 14 return more characters than the reference (ratios 1.05-1.72), so
their gap is wrong code points, duplicated runs or reordering rather than missing text. The 14 are
2403.02163 0.244, 2311.05131 0.328, 2305.18793 0.708, 2306.05976 0.787, 2406.17961 0.839,
2309.14539 0.841, 2311.18111 0.857, 2311.01358 0.867, 2608.19179 0.869, 2401.03649 0.882,
2407.10296 0.885, 2405.04391 0.888, 2510.18414 0.894, 2401.16311 0.900; the six measured in detail
below are the ones with a mechanism or over 1% bad characters.

Documents measured in detail here: 6 (containment below 0.8 or over 1% U+FFFD/private-use). Over all 121 documents:

| Measure | Median | p05 | p25 | p75 | p95 | Min | Max | Documents below 0.9 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 5-gram containment of the first 20 sentences (whitespace-insensitive) | 0.9817 | 0.8566 | 0.9634 | 0.9994 | 1.0000 | 0.2440 | 1.0000 | 14 |
| U+FFFD + private-use share | 0.0001 | 0.0000 | 0.0000 | 0.0008 | 0.0070 | 0.0000 | 0.0145 | - |

| Id | Pages | Containment | Backend chars | Reference chars | Char ratio | U+FFFD | Private use | Worst page | Page reference chars | Page backend chars | Page content KiB | Page text ops | Form text ops on the page |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 2305.18793 | 490 | 0.708 | 825222 | 627153 | 1.328 | 0.0020 | 0.0001 | 301 | 7414 | 1005 | 6.7 | 50 | 6571 |
| 2403.02163 | 12 | 0.244 | 11881 | 41423 | 0.287 | 0.0000 | 0.0000 | 3 | 5023 | 0 | 12.8 | 64 | 0 |
| 2510.18203 | 364 | 0.963 | 747837 | 526087 | 1.437 | 0.0139 | 0.0006 | 279 | 1490 | 613 | 20.4 | 521 | 0 |
| 2312.01833 | 9 | 0.993 | 36070 | 27132 | 1.336 | 0.0059 | 0.0064 | 9 | 3293 | 0 | 9.9 | 82 | 0 |
| 2311.05131 | 495 | 0.328 | 708498 | 880869 | 0.805 | 0.0000 | 0.0000 | 101 | 3572 | 0 | 49.7 | 394 | 0 |
| 2306.05976 | 291 | 0.787 | 559777 | 430824 | 1.310 | 0.0068 | 0.0002 | 4 | 2680 | 0 | 7.8 | 66 | 0 |

Font objects in the file, and the forms a page sample reaches:

| Id | Fonts by subtype | Fonts without /ToUnicode | Type3 fonts | Pages sampled | Forms with text | Text ops in forms | ... with a CJK font (PDF-1c inlines those) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 2305.18793 | Type1 120, TrueType 5 | Type1 73, TrueType 3 | 0 | 31 | 1 | 15 | 0 |
| 2403.02163 | Type1 9, Type3 4 | Type3 4, Type1 1 | 4 | 12 | 4 | 262 | 0 |
| 2510.18203 | Type1 249, Type3 1 | Type1 2, Type3 1 | 1 | 31 | 5 | 146 | 0 |
| 2312.01833 | Type1 13, TrueType 1 | Type1 1 | 0 | 9 | 1 | 12 | 0 |
| 2311.05131 | Type1 68 | Type1 37 | 0 | 31 | 0 | 0 | 0 |
| 2306.05976 | Type1 231, Type0 35, CIDFontType0 35 | Type1 180, CIDFontType0 35 | 0 | 33 | 4 | 357 | 0 |

| Id | Page | Text by font (characters) | The font the page's text uses (subtype, /Encoding, /ToUnicode CMap) | Forms reached by Do |
| --- | --- | --- | --- | --- |
| 2305.18793 | 301 | F1 6395, F75 652, F24 420 | F1 (not in /Resources/Font, enc `none`, no ToUnicode not decompressible); F75 (Type1, enc `null`, ToUnicode, bfchar 1 bfrange 1) | Im30 (6571 ops, no CJK font) |
| 2305.18793 | 299 | F1 5981, F24 407, F2 74 | F1 (not in /Resources/Font, enc `none`, no ToUnicode not decompressible); F24 (Type1, enc `null`, ToUnicode, bfchar 1 bfrange 1) | Im29 (6054 ops, no CJK font) |
| 2403.02163 | 3 | F170 5696, F164 74 | F170 (Type1, enc `indirect 480 0 R`, ToUnicode, bfchar 1 bfrange 1); F164 (Type1, enc `indirect 480 0 R`, ToUnicode, bfchar 1 bfrange 1) | - |
| 2403.02163 | 2 | F170 4971, F164 20, F30 16 | F170 (Type1, enc `indirect 480 0 R`, ToUnicode, bfchar 1 bfrange 1); F164 (Type1, enc `indirect 480 0 R`, ToUnicode, bfchar 1 bfrange 1) | - |
| 2510.18203 | 279 | F96 1230, F146 207, F174 170 | F96 (Type1, enc `indirect 306 0 R`, ToUnicode, bfchar 1 bfrange 1); F146 (Type1, enc `indirect 306 0 R`, ToUnicode, bfchar 1 bfrange 1) | - |
| 2510.18203 | 187 | F96 533, F146 105, f-0-0 77 | F96 (Type1, enc `indirect 306 0 R`, ToUnicode, bfchar 1 bfrange 1); F146 (Type1, enc `indirect 306 0 R`, ToUnicode, bfchar 1 bfrange 1) | Im98 (11 ops, no CJK font); Im99 (11 ops, no CJK font); Im100 (11 ops, no CJK fo |
| 2312.01833 | 9 | F146 3279, F142 419, F155 9 | F146 (Type1, enc `indirect 399 0 R`, ToUnicode, bfchar 1 bfrange 1); F142 (Type1, enc `null`, ToUnicode, bfchar 1 bfrange 1) | - |
| 2312.01833 | 7 | F146 1212, F164 38, F251 23 | F146 (Type1, enc `indirect 399 0 R`, ToUnicode, bfchar 1 bfrange 1); F164 (Type1, enc `indirect 399 0 R`, ToUnicode, bfchar 1 bfrange 1) | - |
| 2311.05131 | 101 | R353 2437, R769 724, R791 278 | R353 (Type1, enc `indirect 9335 0 R`, ToUnicode, bfchar 0 bfrange 2); R769 (Type1, enc `indirect 9378 0 R`, ToUnicode, bfchar 0 bfrange 2) | - |
| 2311.05131 | 495 | R353 4604, R477 36, R405 33 | R353 (Type1, enc `indirect 9335 0 R`, ToUnicode, bfchar 0 bfrange 2); R477 (Type1, enc `indirect 9374 0 R`, ToUnicode, bfchar 0 bfrange 1) | - |
| 2306.05976 | 4 | F39 2681, F81 391 | F39 (Type1, enc `indirect 5730 0 R`, ToUnicode, bfchar 1 bfrange 1); F81 (Type1, enc `indirect 5730 0 R`, ToUnicode, bfchar 1 bfrange 1) | - |
| 2306.05976 | 3 | F39 2310, F81 82, F75 10 | F39 (Type1, enc `indirect 5730 0 R`, ToUnicode, bfchar 1 bfrange 1); F81 (Type1, enc `indirect 5730 0 R`, ToUnicode, bfchar 1 bfrange 1) | - |
Per document, with the evidence that decides it:

- **2305.18793** (490 pages, containment 0.708). Page 301's 7,414 reference characters are 6,571
  text operators inside the form `Im30` — a Form XObject with **no CJK font**, which is exactly
  the case `pdf1c-extraction` §1 says the backend skips. The document has 1 text-bearing form in
  the 31 sampled pages, and 0 of the forms carry a CJK font, so PDF-1c's form inlining never fires
  here.
- **2403.02163** (12 pages, containment 0.244, the worst in the corpus). The text is *not* in a
  form: page 3 has 0 form text operators and 5,023 reference characters, 5,696 of them in the font
  `F170`, a Type1 font with `/ToUnicode` (`beginbfchar` 1, `beginbfrange` 1, `/Encoding` an
  indirect object) that *is* in the page's `/Resources/Font`. The page content is 12.8 KiB, far
  under the 16 MiB decompression limit, so neither the limit nor a missing resource explains it:
  the text is lost while `F170` is decoded. The file also has 4 Type3 fonts and all 4 lack
  `/ToUnicode`.
- **2311.05131** (495 pages, containment 0.328 with a character ratio of 0.805). The text is
  mostly there but garbled: 68 Type1 fonts, **37 of them without `/ToUnicode`**, and no text in
  any form over the 31 sampled pages. A font without `/ToUnicode` falls back to
  `StandardEncoding`, which turns the maths and the accents of a Ghostscript-converted book into
  the wrong characters while keeping roughly the right character count.
- **2312.01833** (containment 0.993, 0.59% U+FFFD + 0.64% private-use) and **2510.18203**
  (0.963, 1.39% U+FFFD) are not losing text: they return *more* characters than the reference
  (ratios 1.336 and 1.437) with the wrong code points, so the damage is in the CMap decode, not in
  the text layer. 2510.18203 has 249 Type1 fonts, 1 Type3 font, and 3 fonts without `/ToUnicode`.
- **2306.05976** (containment 0.787, 0.68% U+FFFD) is the same shape: 231 Type1 + 35 Type0 fonts,
  180 Type1 without `/ToUnicode`, 4 text-bearing forms in 33 sampled pages and 0 with a CJK font.

One caveat that applies to every per-page number in this section: the backend characters of a page
are attributed by matching each unit's opening characters against that page's reference text, so a
page whose characters are all wrong (private-use glyphs) looks empty. The document totals are
exact; the per-page split is an estimate.

## Verdicts on the hypotheses

| Hypothesis | Verdict | The number that decides it |
| --- | --- | --- |
| H1 the chunk separator loses spaces | partly, in 3 of 121 papers | whitespace ratio median 1.122 of the reference; 3 papers below 0.7; 860 glued long units in 16 papers |
| H2 the paragraph is the page | confirmed | 107 heading-path papers at a median 1.32 paragraphs/page, 14 page-fallback papers at 16.57 |
| H3 long units are splitter failures | refuted as stated | 2,468 of 2,672 long units (92.4%) are `coarse_paragraph`: the splitter never ran |
| H3a the guard is what makes them long | confirmed | lifting it is a precondition for the `missed_boundary` what-if, which alone fixes 32 of 61 papers |
| H4 short units are splitter failures | partly | 4,952 of 10,841 (45.7%) are a one-character `.`; the other causes are furniture arriving as its own line |
| H5 headers and footers reach the reader | confirmed | 45 candidate lines in 30 papers at 80%/50%; 255 in 79 papers at 30% |
| H5a they can be removed as units | refuted | 13 of 457 occurrences are `own_unit`; 217 prefix, 130 inside, 75 glued, 22 absent |
| H6 text is silently lost | confirmed, narrowly | median containment 0.982; 14 of 121 below 0.9, of which 2 return less text than the reference; 6 have a mechanism, 1 of them a non-CJK form |

## Ranked fix candidates

By measured payoff, not by size of the change. Nothing here is implemented: this note is
measurement only.

1. **A period with whitespace before it must not end a sentence** (`ascii_period_is_terminal`,
   `src/domain/segmentation.rs:819`). Removes 4,952 one-character units — 45.7% of every short
   unit in the corpus — in 5 papers, and 903 of the missed-boundary tokens in §2 are the same
   shape. It cannot make any other unit worse: a period that follows whitespace is a dot leader,
   an ellipsis dot or a maths punctuation mark, never a sentence end.
2. **Narrow `looks_like_unreliable_fallback_text` for PDF prose** (`:1016`). 2,468 of 2,672 long
   units are whole paragraphs the guard refused to split, in 110 papers. With the guard out of the
   way, the existing terminal rules plus the missed-boundary shapes take 32 of the 61 offending
   papers below the 5% line. The guard exists for a reason (H1 is real in 3 papers), so the
   narrowing has to be measured against those three: it should keep firing where the glue density
   is high and stop firing on a paragraph that is merely long.
3. **Keep the chunk separator** (H1): 860 long units are glued text in 16 papers, 75 header
   occurrences are only present without their spaces, and 74 short units are hyphen fragments.
   The 3 papers below 70% whitespace are where it shows; a separator between chunks of the same
   page would also give the line breaks H5 needs.
4. **Merge a heading's number with its heading** (`heading_number`, 1,236 units in 53 papers).
   The number and the title are separate lines and so separate paragraphs; this is the same
   line-reconstruction fix as 3, not a sentence rule.
5. **Split citation lists** (`bibliography`, 586 long units in 92 papers). High unit count, low
   document payoff: 7 of 61 papers fall below 5% with it alone. A line-per-entry splitter, not a
   sentence fix.
6. **Remove page furniture** (H5): 45 lines in 30 papers at the task's threshold. Last, because
   2.8% of the occurrences are units of their own today; it needs 3 (line breaks) and then 1
   (spaces) before it can work at paragraph level.

## What did not run, and why

- **Nothing was compiled or tested on this host.** `/dev/vda2` had 3.7 GB free, under the VM's
  5 GB stop line, so `cargo build`/`cargo test` were not run at all; GitHub Actions was the only
  gate, which is why every number above comes from a CI run and every intermediate run is in the
  table of runs.
- **The Python layout backend was not measured** (`READING_MCP_PDF_LAYOUT_PYTHON`). The task fixes
  the default backend; a comparison would need a second full sweep.
- **No new corpus.** Only the committed 121-paper list, so nothing here says anything about scans,
  CJK or non-arXiv publishers.
- **The per-page character split is an estimate** (§5 caveat), and the form census samples up to 30
  pages per document rather than all of them (2311.05131 has 130k images; a full walk was not worth
  the run time).
- **H5 was measured at two thresholds.** The task's 80%/50% tier finds 45 lines in 30 papers;
  PDF-5a's 30% proxy finds 255 in 79. The 30% tier is the one that matches what a reader sees, and
  it is also the noisier one; both are reported rather than picking one.
- **No production access, no exchange or TDX bytes, no OCR**: the sweep is arXiv-only, and OCR is
  off in every run.
