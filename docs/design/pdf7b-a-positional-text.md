# A positional text layer for the default PDF backend (PDF-7b-A)

Design only. **No product code changed, nothing compiled or run on this host.** Every number
below is either read from a file (with its line), from a design note, from a run artifact, or is
labelled an **estimate** with the method that produced it. The task's own stop lines were
respected: `/dev/vda2` had 5.2 GB free at the start (`df -h /`), nothing was built, installed or
started, and the only download was the `lopdf-0.44.0` crate, deleted at the end.

The question this note answers is narrow: **can the default backend know where every glyph is,
and what would it do with that knowledge?** The alternative (B, the PyMuPDF layout backend) is
measured by PDF-7a (PR #238, still running) and is not measured here. The choice between A and B
is made against criteria the user has already fixed; §6 states what A would score on them.

## Provenance

| Tag | What it is | Where |
| --- | --- | --- |
| `[Q7A]` | `PDF quality probe` run [44259190443](https://github.com/liqiangcc/reading-mcp/actions/runs/44259190443), head `ca39a50`, default backend, 121 of 121 documents, 0 failed, 0 not run. The baseline column of every table below. | artifact `pdf-quality-final` (`final/pdf-quality-summary.json`, `all/quality-0..7.jsonl`), downloaded and re-aggregated for this note |
| `[6A]` | `docs/design/pdf6a-text-quality.md` (merged), run [44234764166](https://github.com/liqiangcc/reading-mcp/actions/runs/44234764166) | the per-cause and what-if tables |
| `[6D]` | `docs/design/pdf6d-guard-false-glue.md` (merged), run [44236511100](https://github.com/liqiangcc/reading-mcp/actions/runs/44236511100) | which guard events decide the coarse verdicts |
| `[2A]` | `docs/design/pdf-open-time-and-cmap.md`, run [39775360134](https://github.com/liqiangcc/reading-mcp/actions/runs/39775360134) | open phases, CMap re-parse cost |
| `[2C]` | `docs/design/pdf2c-open-memory.md` | the 135.8 MiB peak and where it comes from |
| `[1A]` | `docs/design/pdf-backend-data.md`, run [39634287777](https://github.com/liqiangcc/reading-mcp/actions/runs/39634287777) | the layout backend's 12 of 30 opens |
| `[232]` | issue #232 and its root-cause comment (2026-11-18) | the five reproductions, especially D and E |
| `[L44]` | `lopdf-0.44.0` from `static.crates.io`, unpacked under `.tmp/` and deleted | every `src/...:line` below is that crate |

`[Q7A]` is the PDF-7a branch's **default-backend** run on the same 121 papers, and its aggregate
reproduces `[6A]`'s population exactly (2,672 long units, 2,468 coarse paragraphs, 61 papers over
5%, 79,553 units, 10,841 short, 1,236 `heading_number`). It is used as "today" because it is the
newest measurement of the same harness; PDF-6c (#235), PDF-6d (#237) and PDF-6e (#242) may have
moved the baseline a little since, and stage 1 of §7 re-measures it before anything is claimed.

## 0. The baseline A has to move, and the bar B has to clear

| # | Criterion measure | Today `[Q7A]` | Where it comes from |
| --- | --- | --- | --- |
| M1 | papers with more than 5% long units (>800 chars) | **61 of 121** | `documents_over_5pct_long`, 61 ids |
| M2 | share of header/footer occurrences that are units of their own | **13 / 457 = 0.028** (spec tier), **79 / 1542 = 0.051** (loose tier) | `header_cases`; `[6A]` §4 prints 1,506 for the loose tier but its own case table sums to 1,542, which is the artifact's number |
| M3 | `heading_number` units | **1,236 in 53 documents** | `short_by_cause` |
| M4 | papers below 0.7 of the reference's whitespace share | **3** (2308.01909 0.283, 2404.05778 0.386, 2403.02247 0.418) | `whitespace_below_70pct` |
| — | 5-gram containment median (whitespace-insensitive) | **0.9817** | recomputed from the per-document `quality.containment_first20` |
| — | peak server RSS | median **113.4 MiB**, p95 and max **307.5 MiB** (13 records report exactly 314,880 KiB) | `peak_server_kib` |
| — | open time | median **0.037 s**, p95 **0.395 s**, max **1.374 s** | `open_s` |

B's bar, restated so A's numbers can be put beside it: B is chosen only if it opens ≥95% of the
121 papers, is clearly better on ≥3 of M1–M4, keeps containment ≥ the default's − 0.01, peaks at
≤600 MB p95 and ≤800 MB max, opens ≤15 s p95 on papers of ≤30 pages, and needs ≤1 GB of Python.
The only published layout-backend open rate is 12 of 30 (`[1A]` §2), 40%, with 18 refusals of the
form *mixed page contains raster content without source coverage*; PDF-7a's own run on the 121
papers is the figure that counts and it is not in yet.

## 1. What lopdf gives us

**Short answer: everything except the positions.** lopdf 0.44 decodes the content stream, resolves
every font's encoding (including `/ToUnicode`) and turns a shown string into characters. It does
not track a text matrix, a graphics state, a glyph width or a form matrix, and it has no width
table of any kind. `grep -rn 'Widths\|"DW"\|MissingWidth\|FontMatrix' src/` over `[L44]` returns
**nothing**: there is no `/Widths`, no CID `/W`, no `/DW`, no `/FontMatrix` anywhere in the crate.

### 1.1 The API we can use as-is

| Need | API | What it is |
| --- | --- | --- |
| page content, bounded | `Document::get_page_content_with_limit(page_id, limit)` `[L44]` `document.rs:627` | decoded, filter-applied content bytes for one page |
| operator list | `Content::decode(&bytes)` `[L44]` `parser_aux.rs:19` (and `Stream::decode_content` `:31`) | `Vec<Operation>` of `{ operator: String, operands: Vec<Object> }` |
| a page's fonts, inherited resources included | `Document::get_page_fonts(page_id)` `[L44]` `document.rs:691` | `BTreeMap<Vec<u8>, &Dictionary>`: the `/Resources/Font` names, walking the `/Parent` chain through `get_page_resources` (`:662`) |
| a font's encoding | `Dictionary::get_font_encoding_with_limit(doc, max)` `[L44]` `object.rs:420` | `Encoding` = `OneByteEncoding` / `SimpleEncoding` / `UnicodeMapEncoding(ToUnicodeCMap)` / `Differences` (`encodings/mod.rs:46`) |
| per-code decoding | `Encoding::write_to_string(&[code], &mut out)` `[L44]` `encodings/mod.rs:74` | one code at a time is legal; for a CMap the caller must pass exactly one code's bytes |
| CMap lookup and code length | `ToUnicodeCMap::get(code, len)` `[L44]` `encodings/cmap.rs:148`, public `bf_ranges: [RangeInclusiveMap<SourceCode, BfRangeTarget>; 4]` `:16` | the four maps are one per code length 1..4, so the code length is discoverable |
| form content | `Stream::decompressed_content_with_limit(limit)` | what `pdf_text_repair.rs:435` already uses |
| glyph names, when a font has no `/ToUnicode` | `Encoding::Differences` (`object.rs:534-582`, `Glyph::from_name`) | PDF-6b's material, not this design's |

### 1.2 The operators: who reads them today

`extract_text_chunks_from_page` `[L44]` `parser_aux.rs:108-260` is the whole of lopdf's text
extraction. It reads **`Tf`, `Tj`, `TJ`, `'`, `"`, `T*`, `ET`** and nothing else. It never looks at
`Tm`, `Td`, `TD`, `Tc`, `Tw`, `Tz`, `TL`, `Ts`, `cm`, `q`, `Q` or `Do`. Three consequences are the
whole reason this design exists:

- A **chunk is a font run**, not a line: `Tf` closes the current chunk (`:142-155`) and text
  accumulates across `Tj`/`TJ`/`Td`/`Tm` in between. `'`, `"`, `T*` and `ET` insert a `"\n"`
  *inside* the chunk (`:171-215`). So a heading number is its own chunk exactly when a font change
  separates it from the title — which is why `heading_number` units exist at all (§3.6).
- **A space is a threshold on one number**: `collect_text` (`:363-370`) inserts a space only for a
  `TJ` adjustment below −100 thousandths, and after a nested array. It does not know the font size,
  so −100/1000 em is a fixed guess; a producer that positions words with `Td` or `Tm` gets no
  space at all. That is `Forcomparison` and `includesthe` (`[232]` D).
- **A glyph advance is never computed**, so nothing can be said about the gap *between* two
  runs, and `Tm`/`Td` are ignored, so a page's text has no geometry.

### 1.3 What we already own

`src/parsing/pdf_layout.rs` (1,030 lines, merged) is already a partial positional walker:
`extract_text_fragment_evidence` (`:101-292`) decodes each page's content, tracks
`Tf` (font name + size, `:157-170`), `Tm` (x, y, `:172-177`), `Td`/`TD` (`:178-192`), `TL`
(`:193-200`), `T*` (`:201`), and emits one `PdfTextFragmentEvidence { page, sequence_index, text,
font_resource, font_size, x, y }` per shown string (`:476-500`). It also carries
`FontEncodingMemo` (`:57-99`), which resolves each distinct font once and removes the 39,000-parse
CMap cost that `[2A]` §2.4 measured.

Its four limits are exactly the four things this design adds:

1. `x`,`y` are the **line origin**, not the pen: after a string is shown the pen advances and the
   struct does not know it, so two strings on one line both report the line's start.
2. **No `cm`/`q`/`Q`**: the CTM is not tracked, so a form's `/Matrix` and any scaled or rotated
   `cm` are invisible, and `Do` is not followed at all.
3. **No `Tc`/`Tw`/`Tz`/`Ts`**: character, word, horizontal-scale and rise state are ignored, so a
   superscript looks like an ordinary glyph on the same baseline.
4. `T*` moves only when `TL` was set (`move_to_next_line`, `:470-474`); without `TL` the y does not
   move, which is a silent misplacement rather than a failure.

### 1.4 What is exact, what would be approximate

| Piece | Source | Status |
| --- | --- | --- |
| `Tm`, `Td`, `TD`, `T*`, `TL` | operands, `[L44]` `Content::decode` | **exact** (f32 rounding only) |
| `Tc`, `Tw`, `Tz`, `Ts` | operands | **exact** |
| CTM from `cm` and the `q`/`Q` stack | operands | **exact** |
| form `/Matrix` and nested `Do` | our recursion, `[L44]` `Stream::decompressed_content_with_limit` | **exact** for `/Matrix`, if we recurse ourselves |
| glyph advance, simple font | `/Widths` + `/FirstChar` + `/FontDescriptor/MissingWidth` | **exact when `/Widths` exists** — most arXiv PDFs |
| glyph advance, CID font | descendant `/W` (both the `c [w…]` and the `cfirst clast w` forms) + `/DW` | **exact when `/W` exists**; `/DW` alone is exact by the spec but only as good as the producer's default |
| glyph advance, simple font **without** `/Widths` (Standard 14) | — | **approximate**: we would need the 14 AFM width tables (≈200 numbers each) or a 0.5 em guess. Not planned for the first stages |
| Type 3 advance | `/Widths` × `/FontMatrix[0] × 1000` | **exact** for the usual `[0.001 0 0 0.001 0 0]`; **approximate** for a skewed or non-uniform matrix |
| code length of a CID string | `bf_ranges` lengths, else 2 | **exact** for Identity-H/V and for CMaps whose ranges are in one length; **approximate** for a CMap mixing lengths where a code is not mapped |
| characters per code | lopdf's `Encoding` | **exact for the decode lopdf does**, which is the decode the product already publishes; PDF-6b/PDF-2 §4.2 change *what* it decodes, not how many codes there are |
| a ligature glyph decoded to two characters | — | **approximate**: one advance, two characters; the second character's x is interpolated |
| `ActualText` / `/Replacement` marked content | not read | **approximate**: we position glyphs, not the replacement text |
| vertical writing (`Identity-V`, `WMode 1`) | not handled | **unsupported** (fall back to stream order) |
| a Type 3 glyph that draws away from its origin | `/FontMatrix` | **approximate** |

The honest summary: **positions are ours to compute, and for the fonts that carry `/Widths` or
`/W` the arithmetic is exact; the only genuinely approximate piece in the common case is the
character split inside a ligature.** Nothing here needs a lopdf fork.

## 2. Data model

Five levels, each a plain struct in a new `src/parsing/pdf_text_layout.rs`; nothing is stored on
the `Document` except the finished page text and a small evidence record.

```text
Glyph   { code: u32, chars: Range<u32>, x: f32, y: f32, advance: f32, size: f32, rise: f32,
          flags: u8 }                    // 32 B; chars index into the page's text buffer
Run     { font: u32, size: f32, rise: f32, rotation: u8, glyphs: Range<u32> }
Word    { run_range, x0, x1, baseline, size, gap_before: Gap }   // Gap = None|Space|Wide
Line    { baseline, x0, x1, size, rotation, words: Range<u32>, block: u32 }
Block   { lines: Range<u32>, x0, x1, y0, y1, column: u8, kind: BlockKind }
          BlockKind = Body | Heading | ListItem | TableCell | Caption | Footnote | Furniture
PageLayout { lines, blocks, words, columns: u8, header_band, footer_band, bailed_out: bool }
```

Rules, in the order they are applied:

1. **Glyph.** The pen is a point in text space; `Trm = [Tfs·Th 0 0 Tfs 0 Ts] × Tm × CTM`; the glyph
   origin is `(0,0)` through `Trm`; the advance is
   `tx = ((w0 − Tj/1000)·Tfs + Tc + Tw)·Th` with `Tw` counted only for the single-byte code 32.
   `w0` is `w/1000` for most fonts and `w·FontMatrix[0]` for Type 3. Rotation is
   `atan2(Trm.b, Trm.a)` bucketed to 0/90/180/270 ± 1°.
2. **Word.** Two adjacent glyphs in the same run are in the same word unless the gap
   `next.x0 − (prev.x0 + prev.advance)` reaches the space threshold
   `max(0.12·size, 0.60·space_advance)` where `space_advance` is `/Widths[32]` when the font has
   one. A `TJ` adjustment ≥ 0.12 em and a `Td`/`Tm` jump ≥ 0.12 em both count as a gap; a jump
   ≥ 1.0 em is `Wide` and only used by the table/TOC rules. The thresholds are **calibration
   targets, not measurements** (§8).
3. **Line.** Glyphs whose baseline differs by ≤ 0.35·size and whose x-intervals are not separated
   by more than 2.0·size belong to one line, in the same rotation bucket. Lines are sorted by
   `(rotation, −baseline, x0)`.
4. **Block.** A new block starts when any of these holds between consecutive lines of one column:
   the baseline gap exceeds 1.35× the column's modal leading; the first line is indented by
   ≥ 0.8× the modal indent while the previous line ended short; the font size changes by >20%;
   the previous line ends with no terminal mark and is short (a heading followed by body); the
   line is a footnote or a furniture line (§3.5, §3.8). Otherwise lines join.
5. **Column.** A vertical gutter is a band with no word crossing it, at least 0.03× the page width
   wide and present over ≥60% of the page's text height. Exactly one such gutter, with ≥30% of the
   words on each side, means two columns; otherwise the page keeps the content-stream order.
6. **Reading order.** Blocks are ordered by `(column, −y0)`; a block that spans the gutter is a
   separator: everything above it in both columns, then it, then everything below.

Nothing above is stored after the page is turned into text; only `PageLayout`'s block boundaries
and a per-page header/footer summary survive long enough for §3.5.

## 3. One algorithm per problem

Each subsection gives the rule, the failure modes, and the `[6A]` measure it is meant to move.
"Today" is `[Q7A]`.

### 3.1 Inter-word spaces

**Rule.** Emit one space between two words of a line when the measured gap reaches the threshold
of §2.2, and only there. Never emit a space across a code that decoded to no characters (a ligature
mapped to nothing) — that is a decode gap, not a word gap; PDF-6b owns it. Never emit two spaces.
The space goes into the line's text, so the gap becomes visible to `whitespace_share`,
`missing_space_glue_events` and the containment 5-grams (which strip whitespace, so they cannot be
hurt by an added space).

**Failure modes.** A font with no `/Widths` makes every advance a guess, and the gaps become noise
— the layer must then *not* use gaps for that run and keep lopdf's `TJ` rule for it (fallback per
run, not per page). A justified line's inter-word gaps vary from 0.15 to 0.6 em, so the threshold
must sit below the smallest real space and above the largest kerning pair; the two populations
overlap on tightly-set maths. A run of `Wide` gaps inside a table becomes one space per gap, which
is right for prose and wrong for a table cell — accepted, the table is still one line.

**Measure.** M4 (3 → 0 expected) and the `glued_words`/`glued_terminal` half of M1. `[6A]` §1: the
three papers below 0.7 whitespace are 0.283, 0.386 and 0.418 of the reference; a page of ~2,900
characters (14,201,399 backend characters / 4,842 pages, `[Q7A]`) needs one space per ~6 characters
to reach the reference's 0.16 share, so restoring the missing gaps alone is enough to cross 0.7.

### 3.2 Line breaks inside a page

**Rule.** One `"\n"` between lines of the same block, one `"\n\n"` between blocks. This is the
change that replaces "chunks joined with nothing" (`pdf.rs:328`) and "each chunk is a line"
(`PageFragments::push_line`, `pdf.rs:81`).

**Failure modes.** A superscript or a footnote marker that the baseline rule does not group becomes
its own line (§3.7). A rotated table header creates a second rotation bucket whose lines are not
interleaved with the body — acceptable. A page whose layer bailed out keeps the old behaviour.

**Measure.** M2 directly, and the paragraph count that M1 rides on. `[Q7A]`: the backend has a
median of 25.5 newlines per page against the reference's 86.9; the heading path produces a median
of 1.32 paragraphs per page (`[6A]` §1). Lines alone take that to the page-fallback regime's 16.57
paragraphs per page for the 14 papers that already behave that way.

### 3.3 Paragraph ends

**Rule.** §2.4's block rules, which are the three 6a fix-3 shapes: a first-line indent, a baseline
gap above 1.35× the modal leading, and a short last line followed by a full-width first line. The
block is the paragraph; `"\n\n"` goes between blocks.

**Failure modes.** A list with hanging indents looks like a paragraph start at every item
(intended: an item is a paragraph). A figure caption separated by a 1.2× gap is joined to the body
— the gap threshold is the only defence and it is calibrated, not measured. A paragraph that
continues over a page break is *not* joined here (§3.9, §4.4).

**Measure.** M1. `[6A]` §2: 2,468 of the 2,672 long units are whole paragraphs the #139 guard
refused to split, in 110 papers. Two independent anchors for what real paragraphs do: the 14
page-fallback papers, whose units are already one-per-line, have a **median long-unit share of
0.0016 and 3 of 14 over 5%** (`[Q7A]`, recomputed from the per-document records), and the
`missed_boundary` what-if alone takes 32 of the 61 papers below 5% (`[6A]` §2).

### 3.4 Two-column reading order

**Rule.** §2.5–2.6. Reorder only when a gutter is proven; otherwise keep the content-stream order,
which for most producers is already column-major.

**Failure modes.** A full-width figure or table crossing the gutter suppresses the split, so a page
with a figure keeps the stream order (usually correct). A three-column layout is not detected and
keeps the stream order. A page whose columns are drawn interleaved (some LaTeX converters emit
line 1 of column 1, line 1 of column 2, …) is *fixed* by this rule and is the case worth having.
The dangerous case is a false gutter down the middle of a wide table.

**Measure.** No 6a measure covers it. It protects containment and the paragraph rules: a wrong
order merges the two columns' lines into one block and makes every line pair look like a paragraph
break. `[1A]` shows the corpus is two-column-heavy (2610.28567, 2610.28615 are cs.CV papers with
two-column bodies), so this is not optional for M1.

### 3.5 Running headers and footers

**Rule.** A line whose baseline sits in the top 10% or bottom 10% of the text area, whose squashed
text repeats on ≥30% of the pages (or is a page-number line within ±1 of the page number, or
`Page N of M`), and which is not the only line on its page, is furniture: it is removed from the
page's text and recorded in `PageLayout` as `Furniture`. The 30% threshold is used because
`[6A]` §4 shows the real headers sit at 30–49% of pages (2407.10296 0.489, 2401.01137 0.471), and
the margin band is what keeps the 30% tier's table cells out (the false positives `[6A]` lists —
`0.64`, `Prompt:`, `Completion:` — are all mid-page).

**Failure modes.** A header that only appears on right-hand pages is at 50% and is still caught; a
header that changes with the section is not. A page number written as a word (`three`) is not
caught. Removing a line that is also a real sentence (a chapter title repeated as a running head)
loses text: the reference has it, so containment pays for it (§6). A footer rule (a drawn line) is
not text and is ignored.

**Measure.** M2. Arithmetic on `[Q7A]`: of 330 page occurrences in the spec tier, 291 are at the
page start and 16 at the end; per candidate, `min(start+end, occurrences found in the reader's
text)` sums to 242 of 457 = 0.53 after line breaks and 286 of 457 = 0.63 after line breaks plus
spaces. The same sum on the loose tier is 603 of 1,542 = 0.39 and 748 of 1,542 = 0.49.

**A trap in the measure, stated plainly.** `header_placement` (`scripts/pdf-quality.py:462-527`)
counts an occurrence as `own_unit` only if a *unit* equals the line, and as `not_found` if the line
is gone. So header **removal** moves occurrences from `prefix`/`inside`/`glued` to `not_found` and
does **not** raise M2 — it lowers it toward 0. M2 therefore measures "the line is no longer glued
into body prose", which is what the criterion intends, and it is maximised by §3.2–3.3, not by
removal. §6 reports both numbers.

### 3.6 Heading number + heading on one line

**Rule.** Within a line, a leading `N.`/`N.N.`/`A.`/`N)` word whose size is ≥1.0× the following
word's and whose gap is ≥0.5 em is joined to the rest of the line with one space. The line then
becomes a `Heading` block if its size is ≥1.1× the page's modal body size, and its text is
`"3. Introduction"`.

**Failure modes.** A numbered *list item* is the same shape and gets the same treatment (correct:
the item's text belongs with its number). An equation number `(3)` at the right margin is not
leading and is untouched. A heading whose title is on the next line stays two lines and the
fragment `"3."` is still rejected as a heading candidate by `parse_numbered_heading`
(`pdf.rs:594`, it rejects any fragment containing `"\n"`) — but it is no longer a *unit* either,
because the number's line is joined to the title's block only if the block rules say so.

**Measure.** M3. `[6A]` §3: 1,236 units in 53 papers, shapes `d.` 596, `A.` 357, `d.d` 280, `d)`
3 (`[Q7A]` recomputation of the 1,236 records). The mechanism argument is strong: a
`heading_number` unit is a whole chunk, and a chunk closes only at `Tf` (`[L44]`
`parser_aux.rs:142-155`) or at the page's end, so the number was separated from its title by a
**font change**, which is a same-line event. Positions therefore see the two words on one baseline
and §3.6 merges them. Estimate: 1,236 → 40–250, the residual being numbers that really are alone
on their line.

### 3.7 Superscript and subscript (`10^−12`)

**Rule.** A run whose `Ts` rise is ≥0.15× the base size, or whose baseline is offset by ≥0.15×size
while its size is ≤0.8× the base size, is attached to the *previous* glyph's position instead of
starting a new line, and is emitted **with no space and no marker**: `10` + `−12` becomes `10−12`.
The `−` itself is a decode problem (`[232]` D: CMSY's minus at code 0x00) and belongs to PDF-6b;
the layer's job is that the superscript is not a new line and not separated by a space.

**Failure modes.** A footnote marker (a raised `3`) would be attached to the body word and look
like part of it (`[232]` D's "gauge 3"); the footnote rule (§3.8) is what separates it, and a
marker whose rise is small stays inline — which is where it is in the reference too. A whole
line set in a smaller size (a table's header row) must not be mistaken for a superscript: the rule
requires a *previous glyph on the same line*.

**Measure.** No criterion measure; it is `[232]`'s semantic inversion. The alternative of emitting
`^{…}` was considered and rejected for the first stages: containment is 5-gram over
whitespace-stripped text, so inserting `^` and `{}` breaks grams the reference has (`10^−12` would
contribute `10^−1`). A marker is a separate decision with its own measurement.

### 3.8 Footnote regions

**Rule.** A block in the bottom band whose size is ≤0.9× the body size and which either follows a
drawn rule (a `re`/`l` fill in the content stream, or a 1.5× leading gap) or begins with a raised
marker is a `Footnote`. It is ordered last on the page and marked, so that §3.9 can tell that the
body ended above it.

**Failure modes.** A two-column paper puts footnotes under each column (the block's column is
kept). A paper whose footnotes are at the *top* of the next page (some journals) is not detected.
A small-font table at the bottom is misclassified — the marker test is what separates them.

**Measure.** `[232]` E only. `[6A]` has no footnote count; the synthetic two-column input
`scripts/pdf-gen-inputs.py` (`add_page_furniture`, `:261-286`) writes one footnote with a rule per
page and returns the exact strings, so the rule is measurable on a committed input.

### 3.9 A sentence continuing across a page break (#232 E)

**Rule.** Nothing is joined in the stored text. A page's last body block gets
`continues_on_next_page = true` when its last line is within 1.0 em of the column's right edge and
has no terminal mark, and the next page's first body block gets `continues_from_previous_page`
when it starts lowercase or with an unmatched opening bracket. The evidence is recorded per
paragraph and reaches the reader as metadata; `get_text_units` and `cite` can then report that a
unit is half of a sentence instead of silently returning a fragment. The footnote between them is
already ordered out of the way (§3.8), which is what `[232]` E's "footnote 1 sits between the two
halves" needs.

**Failure modes.** A paragraph that ends without punctuation by accident (a heading, a caption)
claims a continuation. A lowercase start is also how a new sentence after an abbreviation begins.

**Measure.** `[232]` E. Not a 6a measure. **This is the one algorithm that cannot be done inside
the text layer** — see §4.4 for why merging the pages would break `cite`.

### 3.10 Hyphenation at line ends

**Rule.** A line ending in `-` whose next line starts lowercase, in the same block, is joined
without the hyphen when the joined token is not a compound the page uses elsewhere; a line ending
in `-` followed by an uppercase start keeps the hyphen. `[6A]`'s `hyphen_fragment` count (74 units
in 5 papers) is the population this removes; the harness's own `neighbour_word` test is the
reference behaviour to copy.

**Failure modes.** An em dash or a minus at a line end (`-` in the decoded text) is joined into a
word. A compound broken at its own hyphen (`state-of-the-\nart`) loses a real hyphen.

**Measure.** 74 short units → ~0, in 5 papers. Small, but it is the cheapest correctness win and it
needs no new evidence beyond §3.2.

### 3.11 Bibliography entries, one per line

**Rule.** Inside a block whose section title matches `References|Bibliography|Literature Cited`
(from the inferred headings or the native TOC) or whose text has ≥6 bibliographic markers
(`[6A]`'s `BIB_MARKER_RES`), each entry starts at a line whose x0 equals the region's modal x0 and
whose previous line is short or ends a year/`]`/`.`; continuation lines are indented. Each entry
becomes its own block.

**Failure modes.** An entry that wraps without an indent is split mid-entry. A two-column reference
list must be column-scoped first (§3.4). A paper whose references are numbered `[1]` at the margin
is the easy case; an author-year list with no indent is the hard one.

**Measure.** M1. `[6A]` §2: 586 `bibliography` long units in 92 papers; the what-if takes **7 of
61** papers below the 5% line on its own, and `[Q7A]` recomputation gives 37 of 61 for
`missed_boundary`+`bibliography` and 38 of 61 for all five fixable causes together.

## 4. Integration

### 4.1 What replaces `PageText.text` and `PageText.chunks`

`PageText` (`pdf.rs:53-58`) keeps its shape; only the producer changes.

- `chunks` becomes the **block list** of the page: each item is one paragraph, its lines joined
  with `"\n"`, trimmed. The field keeps its name and its consumers, so
  `source_fragments` (`pdf.rs:578`) and `parse_numbered_heading` keep working; what changes is
  that a chunk is a paragraph instead of a font run.
- `text` becomes the page's text with `"\n"` between lines and `"\n\n"` between blocks, still
  `normalize_pdf_text`-trimmed. That is what the page-fallback and TOC builders already insert
  verbatim (`pdf.rs:809-844`, `:962-1000`), so both gain real paragraphs for free.
- `extract_page_texts_raw` (`pdf.rs:312-358`) keeps its error contract: the layer returns a
  `PageLayout` or `bailed_out`, and a bailed-out page falls back to
  `extract_text_chunks_with_limit` exactly as today. The `MAX_PAGE_DECOMPRESSED_BYTES` limit
  (16 MiB, `pdf.rs:26`) is passed through to `get_page_content_with_limit`, so the bomb guard is
  unchanged.
- The layer runs on the **repaired** tree (`pdf_text_repair::prepare_text_extraction`), which is
  already the copy the page text comes from (`pdf.rs:143-153`), so PDF-1c's form inlining gives
  the layer its form content with the `q`/`cm`/`Q` wrapper (`pdf_text_repair.rs:563-575`) and the
  renamed fonts. One tree, one walk, no second parse.

### 4.2 Ranges and bindings stay exact, by construction

`cite` and `source_view` work because every binding is computed from `content.chars().count()`
immediately before and after a fragment is appended (`pdf.rs:770-801` for the heading path,
`:962-1000` for the TOC path, `:809-844` for the page path). The positional layer changes **what**
the fragment's text is, never **how** the range is measured, so:

- a page's text is still one contiguous insertion → one binding per page, `start`/`end` still in
  the section's scalar count;
- paragraph breaks inside a page are `"\n\n"`, which `paragraph_ranges`
  (`src/domain/segmentation.rs:733-765`) turns into paragraphs that are **inside** the page's
  binding, so `OriginalSourceBindingMap::target_for_range`
  (`src/domain/original_source.rs:124-150`) still finds exactly one containing binding and does not
  return `AmbiguousTarget`;
- `Document::validate_original_source_binding_map` (`:110-119`) checks ranges against
  `normalized_text_len()`, which is the same scalar count, so nothing else moves;
- `PDF_TEXT_EXTRACTION_VERSION` (`pdf.rs:25`, today `pdf-text-extraction/v3`) must become `v4`:
  the same bytes now segment differently, and both the parsed cache (`CachingParser::cache_key`)
  and the stored open results (`ingest_fingerprint`) key on it.

### 4.3 The three section builders

| Builder | Change |
| --- | --- |
| inferred headings (`pdf.rs:659-768`) | `push_line` (`:81`) becomes `push_block`: blocks of one page join with `"\n\n"` (they already join with `"\n"` today because a chunk was a line), pages join with `"\n\n"` as now. `content_and_bindings_from_fragments` (`:770`) is unchanged except that a fragment can contain `"\n"`. Heading candidates improve: `parse_numbered_heading` now sees `"3. Introduction"` where it saw `"3."` and `"Introduction"` separately, so `infer_numbered_headings` (`:515`) accepts headings it rejects today — this *changes the structure provenance for some documents* and is the largest behavioural risk of the whole design (§8). |
| page fallback (`pdf.rs:809-844`) | no code change: `page.text` now has paragraph breaks, so the 14 papers stop being one paragraph per page. |
| native TOC (`pdf.rs:846-960`) | no code change for the same reason; `source_text_and_bindings_for_page_range` inserts page text between `"\n\n"`. |

Two callers read the text structurally and must be kept honest:

- `pdf_front_matter::split_abstract_line_ranges` (`src/parsing/pdf_front_matter.rs:132-172`)
  requires a line whose trimmed text is exactly `Abstract`. The layer must keep that line its own
  line and must not join it to the paragraph below (a heading line is a block of its own by size
  and gap). A test with a two-line "Abstract" + body fixture is required.
- `pdf_layout::infer_abstract_heading` (`:354`) reads fragment evidence; the layer's line/block
  structure is a superset of what it needs, so the two passes should be merged into one walk
  (§5.3) rather than left as two.

### 4.4 The one thing that cannot be done inside the layer

Merging a sentence across a page break in the stored text would create a paragraph whose range
spans **two** page bindings. `target_for_range` counts overlapping bindings and returns
`AmbiguousTarget` when more than one contains the range (`original_source.rs:136-149`), so `cite`
would fail on exactly the paragraphs #232 E is about. Therefore: page boundaries stay paragraph
boundaries, and the continuation is *evidence* (§3.9), not a merge. If a later task wants a
one-sentence unit across the break, it must change the binding model (for example "the binding of
the range's start") — that is a domain decision, not a text-layer one.

### 4.5 Coexistence with PDF-6b, PDF-6c and the #139 guard

- **PDF-6b** (glyph-name → Unicode for Type 1 fonts without `/ToUnicode`) is orthogonal and must
  land **first**: it changes characters, the layer changes positions. The layer must survive a code
  that decodes to nothing (§3.1: no space across a decode gap) and a code that decodes to two
  characters (ligature: one advance, interpolated second position).
- **PDF-6c** (spaced dot runs, PR #235) is a segmentation rule. The layer's contribution is the
  evidence: a TOC leader is a line with ≥3 `.` words separated by `Wide` gaps on one baseline, and
  a maths ellipsis is the same shape in a `math` block. The layer marks the line; 6c keeps deciding.
- **The #139 guard** (`looks_like_unreliable_fallback_text`, `src/domain/segmentation.rs:1046`,
  profile `FallbackGuardProfile::PdfProse` at `:1025`) counts glue events **per paragraph**. Once a
  paragraph is a paragraph and not a page, three things change:
  1. the count is over ~250 characters instead of ~2,900, so the same density trips the guard far
     less often — this is the mechanism behind most of M1's expected move, and it happens *without
     touching the guard*;
  2. the layer can replace the guess with a **measurement**: `reconstructed_spaces` (how many
     spaces it inserted from a gap) and `zero_gap_word_junctions` (how many word pairs it had to
     join with no gap at all). A paragraph with ≥1 reconstructed space is not glued, whatever its
     characters look like; a paragraph with `zero_gap_word_junctions > 0` is glued, whatever
     `[6D]`'s five junction kinds say. That is a strictly better signal than `missing_space_glue_events`,
     and it is PDF-only by construction;
  3. the bibliographic-masthead rule can be replaced by position: a masthead is the top band of
     page 1, which the layer knows. `[6D]` §1 counts that rule at 66 paragraphs; the layer removes
     the guess for them.

  None of this loosens #139 for other formats: the evidence would be an extra optional input to
  the PDF path only, and the existing guard stays the fallback when the layer bailed out.

## 5. Cost

All three numbers are **estimates**; the method is stated for each. The measured baseline is
`[Q7A]` (peak server RSS median 113.4 MiB, p95/max 307.5 MiB; open median 0.037 s, p95 0.395 s,
max 1.374 s) and `[2C]` (135.8 MiB peak on `2401.14345`, a 45.4 MiB file whose peak is the file plus
its 90 MiB object tree, `[2C]` §"The floor is now the load").

### 5.1 Memory

| Piece | Size | How it was made |
| --- | --- | --- |
| glyph buffer, one page | 2,933 chars/page × 32 B ≈ **92 KB** (measured chars/page from `[Q7A]`: 14,201,399 / 4,842) | `[Q7A]` character count × the 32 B struct of §2 |
| words + lines + blocks, one page | ≈ 2–4 KB (≈ 500 words × 16 B + 60 lines × 64 B + 6 blocks) | 2,933 chars ≈ 480 words at 6.1 chars/word |
| per-document font metric cache | **0.5–3 MiB typical**, worst case ~20 MiB | one `/Widths` (≤256 u16) or `/W` per font; 2510.18203 has 249 fonts (`[6A]` §5), so 249 × ~8 KB worst case |
| per-document retained after a page | header/footer candidates only: 4 lines × ~80 B × pages ≈ **32 KB per 100 pages** | §3.5 keeps text + band, nothing else |
| peak delta | **+2 to +10 MiB** | the glyph buffer is one page and is built while the page's decoded content (≤16 MiB by `MAX_PAGE_DECOMPRESSED_BYTES`) and the object tree are already live |

Expected peak on the 121 papers: median 113 → **118–130 MiB**, max 307.5 → **315–340 MiB**. Both are
far under the 600 MB / 800 MB bar and under the host's 1.9 GB. The one unbounded case is a page
whose content stream shows millions of codes: 4 M codes × 32 B = 128 MB. The layer therefore caps
its glyph buffer (proposal: 200,000 glyphs per page, ≈ 6.4 MB) and bails out to the chunk path for
that page, which is exactly the "no worse than today" fallback the other repairs already use.

### 5.2 Open time

| Piece | Cost | How it was made |
| --- | --- | --- |
| the walk itself | ~20 ns per glyph × 2,933 ≈ **60 µs/page**, ≈ 0.24 s over the whole 4,842-page corpus | one pass over an operator list lopdf already decodes; the arithmetic is 6 f32 multiplies per glyph |
| width tables | **0.1–1 ms per document** | parsed once per font into the cache; a `/W` array of 1,000 entries is a few µs |
| line/word/block building | ~**50 µs/page** (sorting ~500 words and ~60 lines) | comparison-count × log n |
| total | **+3 to +10% of the parse** | the parse is 99.8% of an open (`[2A]` §2.1) and the EN parse is 2.3–2.5 ms/page (`[2A]` §2.3), so +110 µs/page is +4.5% |

Expected open on the 121 papers: p95 0.395 → **0.40–0.43 s**, max 1.374 → **1.40–1.50 s**. There
is one large win available in the same change: the main extraction path still resolves every font's
encoding per page (`[2A]` §2.4, 39,000 CMap parses at 1.36 ms each for a 300-page CJK document),
while `pdf_layout.rs` already has `FontEncodingMemo` (`:57-99`). Threading that memo through the
layer's walk is required anyway, and it makes CJK-heavy documents *faster* than today (the same
document at 100 pages, which never enters the second pass, opens in 0.80 s, `[2A]` §2.1). None of
the 121 papers is that shape — the worst is 1.374 s — so this is a robustness gain, not a corpus
gain.

### 5.3 Code size

| Piece | Lines (estimate) | Basis |
| --- | --- | --- |
| extend the existing walker: CTM stack, `q`/`Q`/`cm`, `Tc`/`Tw`/`Tz`/`Ts`, pen advance, `Do` recursion | 350–450 | `pdf_layout.rs:101-292` is 190 lines for the state machine it has; the missing half is comparable, plus the form recursion `pdf_text_repair.rs:275-470` already demonstrates |
| font metrics: `/Widths`, `/W`, `/DW`, `/FontMatrix`, cache | 200–300 | two array forms and three dictionary keys |
| words, lines, blocks, columns, reading order | 350–450 | §2's six rules |
| the eleven algorithms of §3 | 500–700 | ~50 lines each |
| integration in `pdf.rs` (`PageText`, `push_block`, metadata, version bump) | 150–250 | 5 call sites |
| **product total** | **1,550–2,150** | for scale: `pdf.rs` is 1,877, `pdf_layout.rs` 1,030, `pdf_text_repair.rs` 781 |
| tests | 1,200–1,800 | the existing `pdf.rs` test module is ~480 lines for far less machinery |

## 6. Expected effect on the criteria

Baseline column is `[Q7A]`; every expected value is an **estimate**, and the method column says
what it was made from. "Measured" means a number taken from a run artifact; "assumption" means it
rests on the mechanism and would have to be confirmed by the stage that implements it.

| Algorithm | M1 papers >5% | M2 own-unit share (spec) | M3 `heading_number` | M4 papers <0.7 ws | containment median | Basis |
| --- | --- | --- | --- | --- | --- | --- |
| today | 61 | 0.028 | 1,236 | 3 | 0.9817 | measured |
| §3.2 lines | 61 → **25–35** | 0.028 → **0.53** | — | 3 → **3** | 0.9817 | M2: 242/457 arithmetic on measured positions; M1: the 14 page-fallback papers, 3 of 14 over 5% (`[Q7A]`), applied to 121 |
| §3.1 spaces | → **18–28** | 0.028 → **0.63** | — | 3 → **0** | 0.9817 | M2: 286/457; M4: the three papers' deficit *is* the inter-word space (§3.1); containment strips whitespace, so no change (assumption, strong) |
| §3.3 paragraphs | → **10–20** | 0.63 | — | 0 | 0.9817 | the union what-if leaves 23 of 61 with the *unchanged* unit count (`[Q7A]` recomputation: `missed_boundary` 32, `+bibliography` 37, all five 38 of 61 fixed); the line regime's median share is 0.0016 |
| §3.4 columns | protects M1 (no change if the gutter is right) | — | — | — | ±0.005 | assumption: a wrong order would merge columns and lose grams |
| §3.5 furniture removal | −0 to −3 papers | **0.53–0.63 before removal, → 0 after** (§3.5's trap) | — | — | **−0.005 to −0.015** | assumption: the reference contains the header line, so removing it can only lose grams; bounded by ≤1 of the first 20 reference sentences per document |
| §3.6 heading merge | small | — | 1,236 → **40–250** | — | — | M3: the `Tf`-closes-the-chunk argument (§3.6) |
| §3.7 super/subscript | small | — | — | — | ±0.002 | assumption: joining a superscript to its base restores grams `[232]` D lost |
| §3.8 footnotes | small | — | — | — | ±0.002 | assumption |
| §3.9 cross-page sentence | none | — | — | — | none | evidence only (§4.4) |
| §3.10 hyphenation | small | — | — | — | +0.001 | 74 short units (`[6A]` §3) |
| §3.11 bibliography | 10–20 → **8–16** | — | — | — | — | `[6A]` §2 what-if: 7 of 61 on its own |
| **all stages** | **61 → 10–20 (point 15)** | **0.028 → 0.53 (lines) / 0.63 (+spaces)** | **1,236 → 40–250 (point 150)** | **3 → 0** | **0.977–0.982** | — |

What this means against B's bar, stated without pretending to know B's numbers: A is expected to
be **clearly better on M1, M2, M3 and M4** (all four, not three), to hold containment inside the
−0.01 band, to peak at 315–340 MiB worst case against a 600 MB / 800 MB bar, to open at ~0.4 s p95
and ~1.5 s worst against a 15 s bar, and to need **0 bytes of Python**. The open rate is not at
risk: A keeps the default backend, which opens 121 of 121 today.

Two claims above are **assumptions, not measurements**, and the note says so where it matters:
(a) that the space threshold separates real spaces from kerning on the corpus's fonts, and (b) that
the block rules find the reference's paragraphs. Both are the prototype's job (§8).

## 7. A staged PR plan

Every stage is measured by `PDF quality probe` on the 121 papers (`.github/workflows/pdf-quality.yml`,
`workflow_dispatch`), reports the M1–M4 and containment numbers of §6, and carries its own unit
tests. Each stage also re-runs the committed synthetic inputs (`scripts/pdf-gen-inputs.py`:
`paper-2col-en`, `paper-2col-cjk`, `text-long-*`), which have ground truth for headers, footers,
footnotes and columns. A stage that does not move its measure is not merged; a stage that trips its
stop condition stops the line and sends the decision back to B.

| # | Stage | What it does | Measure it must move | Stop condition |
| --- | --- | --- | --- | --- |
| 1 | **lines only** (the cheapest proof the positional data is usable) | extend the walker with the pen advance and the CTM; `"\n"` between lines, one paragraph per line; no space insertion, no block rules, no removal | newline share per page, units per page, M2 (0.028 → ≥0.4), M1 (→ ≤45) | containment median < 0.972; or the newline ratio stays < 0.5 of the reference's; or M2 < 0.30 |
| 2 | **spaces from advances** | `/Widths`, `/W`, `/DW`, `/FontMatrix`; the §3.1 threshold with the per-run fallback | M4 (3 → 0), `glued_words` + `glued_terminal` long units | whitespace ratio median > 1.3 of the reference (over-spacing), or any paper's containment drops > 0.02, or a paper's char ratio falls below 0.95 |
| 3 | **paragraphs and the guard's evidence** | §3.3 block rules; `reconstructed_spaces` / `zero_gap_word_junctions` offered to the PDF guard path | M1 (→ ≤30), paragraphs/page | M1 > 45; or paragraphs/page median > 2× the reference's line-per-paragraph count (over-splitting); or `page_fallback` count rises above 20 |
| 4 | **header/footer removal** | §3.5, 30% tier, margin band, page numbers | M2 after removal (→ `not_found`), M1 (small) | containment median < 0.972; or any single paper loses > 2% containment |
| 5 | **headings and super/subscripts** | §3.6, §3.7, §3.10 | M3 (→ ≤400), `hyphen_fragment` (74 → ≤10), `[232]` D's `10−12` present | M3 > 700; or `pdf_structure_provenance` changes for more than 10 documents without a matching containment gain |
| 6 | **two columns** | §3.4 gutter detection and reading order | containment on the two-column papers; M1 | any paper's containment drops > 0.01; or the order changes on a page with no gutter |
| 7 | **footnotes, bibliography, cross-page evidence** | §3.8, §3.11, §3.9 (metadata only) | M1 (→ ≤25), bibliography long units | bibliography splits raise short units by > 5%; or the continuation flag fires on > 20% of page ends |

Stage 1 is deliberately the cheapest thing that can falsify the whole design: if a real line
structure on 121 papers does not raise M2 and does not raise the newline share, then the positions
are not usable and B is the answer. Stage 3 is the one that decides M1, and its stop condition is
the one that would send the choice to B even if stages 1–2 looked good.

## 8. Risks and unknowns, and what a prototype has to show

1. **No committed fixture exercises the text state.** A `zlib` walk over the committed PDFs in
   `tests/fixtures/scanned_pdf/pdf` (F01, F03, F05, F12, F04-form; stdlib only, no new dependency)
   finds content streams that use `q BT Tm Tf [Tr] TJ ET Q` and nothing else; `Do` appears only in
   `F04-form`. There is **no** `Td`, `T*`, `TL`, `Tc`, `Tw`, `Tz`, `Ts` or `cm` in any of them, and
   their `TJ` arrays carry no adjustment numbers (a line is one `TJ` of CID codes with explicit
   space glyphs, e.g. `1 0 0 1 48 770 Tm /goldeng 12 Tf [<0037…>]TJ`). The prototype must add
   hand-built content streams for: a `TJ` word gap, a `Td`-positioned word, a `Ts` superscript, a
   `cm`-scaled form, a rotated line, and a `Tz`-condensed run. That is cheap and is the first thing
   to write.
2. **The space threshold is uncalibrated.** The prototype must produce a gap histogram: for the 3
   glued papers and 3 normal papers, the distribution of inter-glyph gaps against the font's space
   advance. If the two populations overlap, §3.1 needs a per-font threshold or a different signal,
   and M4's 3 → 0 is at risk.
3. **Line and paragraph reconstruction is unverified against the reference.** The prototype must
   print, for 10 papers, the layer's lines and blocks beside PyMuPDF's `get_text("dict")` lines and
   blocks: line count per page, block count per page, and the share of lines whose text is
   identical. Without that, M1's 61 → 10–20 is a mechanism argument, not a measurement.
4. **The heading-inference side effect.** Merging a number with its title changes what
   `infer_numbered_headings` (`pdf.rs:515`) accepts, which changes `pdf_structure_provenance` for
   some documents and therefore their whole section tree. The prototype must count how many of the
   121 change provenance under stage 5 *before* that stage is merged, and stage 5's stop condition
   is about exactly this.
5. **Decode quality caps the payoff.** `[2A]` §3.1 shows a whole CMap rejected by one
   non-conformant `bfrange` line, and `[6A]` §5 shows 6 papers whose text is damaged by decode
   (Type 3 without `/ToUnicode`, Type 1 without `/ToUnicode`). Positions do not fix characters;
   papers 2403.02163 (containment 0.244) and 2311.05131 (0.328) stay bad until PDF-6b and the CMap
   repair land. Expect the M1 gain to be uneven across the corpus for that reason.
6. **Two-column detection has no measurement yet.** `[1A]`'s corpus is two-column-heavy, but no
   artifact records gutters. A false gutter merges nothing (it only reorders), but a *missed*
   gutter leaves interleaved columns, which is the one failure that makes M1 worse. The prototype
   must report, per page, whether a gutter was found and how the block order changed.
7. **The glyph-buffer cap.** The 200,000-glyph bail-out is a guess; the prototype must measure the
   largest glyph count per page over the 121 papers (the 495-page `2311.05131` and the 490-page
   `2305.18793` are the candidates) before the cap is fixed.
8. **`ActualText`, vertical writing and Type 3 with a non-uniform matrix** are out of scope and are
   the cases where the layer must fall back rather than guess. Each needs a bail-out path and a
   metadata counter, so a probe can see how often it happened.

A prototype in CI is a later task's job. This note only says what it must show first.

## Appendix A: where every number comes from

| Number | Source |
| --- | --- |
| 121 documents, 4,842 pages, 79,553 units, 2,672 long, 10,841 short, 61 over 5%, 3 below 0.7 ws, 1,236 `heading_number` in 53 documents | `[Q7A]` `pdf-quality-summary.json`; identical to `[6A]` |
| 457 / 1,542 header occurrences, 13 / 79 `own_unit`, 330 page occurrences (291 start, 16 end, 23 middle) | `[Q7A]` `header_cases`, `header_positions`, per-candidate `positions` |
| 0.53 / 0.63 (spec) and 0.39 / 0.49 (loose) expected own-unit share | arithmetic on `[Q7A]`: per candidate `min(start+end, own+prefix+inside)` and `min(start+end, own+prefix+inside+glued)`, summed, over 457 / 1,542 |
| 32, 37, 38 of 61 | `[Q7A]` per-document `what_if`, aggregated with the harness's own formula (`scripts/pdf-quality.py:1520-1531`) |
| 14 page-fallback papers, median long share 0.0016, 3 over 5%, median 16.57 paragraphs/page | `[Q7A]` per-document records grouped by `structure_mode` |
| 25.5 vs 86.9 newlines per page | `[Q7A]` `whitespace.backend_newlines_per_page` / `reference_newlines_per_page`, medians |
| 14,201,399 characters / 4,842 pages = 2,933 per page | `[Q7A]` `quality.backend_chars` |
| containment 0.9817, peak RSS 113.4 / 307.5 MiB, open 0.037 / 0.395 / 1.374 s | `[Q7A]` `quality.containment_first20`, `peak_server_kib`, `open_s` |
| 135.8 MiB peak, 45.4 MiB file, 90 MiB tree | `[2C]` §1 and §"The floor is now the load" |
| 53.22 s CJK open, 39,000 CMap parses at 1.36 ms, 99.8% of the open is `parse` | `[2A]` §2.1, §2.4 |
| 12 of 30 layout opens, 18 refusals | `[1A]` §2 |
| 2,468 of 2,672 long units are coarse paragraphs; 860 glued long units in 16 papers; 74 `hyphen_fragment`; 7 of 61 for `bibliography` | `[6A]` §2, §3 |
| 17,409 guard events, `digit_upper` 2.0%, 66 bibliographic | `[6D]` §1 |
| every `lopdf` line reference | `[L44]`, unpacked from `https://static.crates.io/crates/lopdf/lopdf-0.44.0.crate` |

Every other number in this note is an estimate and is marked as one where it appears.

## Appendix B: what the operators look like in a committed fixture

`tests/fixtures/scanned_pdf/pdf/F01.pdf`, page content stream inflated with Python's `zlib`
(stdlib only):

```
q
BT
1 0 0 1 48 770 Tm
/goldeng 12 Tf [<0037004b0048000300520045…>]TJ
ET
Q
```

One `Tm`, one `Tf`, one `TJ` per line, CID codes with an explicit space glyph (`0003`), and no
`TJ` adjustment numbers — which is why these fixtures prove the line machinery and cannot calibrate
the space threshold (risk 1). The generator that can is `scripts/pdf-gen-inputs.py`, which writes
`paper-2col-en` / `paper-2col-cjk` with two columns, a header, a footer and one footnote with a
rule per page (`:261-286`), and returns those strings as ground truth.
