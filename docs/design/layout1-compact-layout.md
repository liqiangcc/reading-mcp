# LAYOUT-1: the stored section layout

`section_layout_v2` and `section_layout_chunk_v2` are what make a section's units readable without
re-parsing the book: one header row per section with its counters and its coverages, and one row per
8 KiB chunk of unit boundaries. They are also the largest part of the store that is not text.

The disk target is "the store is at most 1x the book's text". `zh-large` (宋史) was at 1.02x, so the
layout had to give back at least 0.29 MB for the target to be met.

## What is stored today

From `src/infrastructure/sqlite.rs`:

- `section_layout_v2`, one row per section: `section_id` (the full id text), `char_len`, five JSON
  text columns (`paragraph_coverage_json` and `coverage_{paragraph,sentence}_{preserve,eligible}_json`),
  four `total_items_*` counters, `paragraph_count`, `sentence_count`, `chunk_count`, `sorted_flags`
  and `plan_error`. It carries `UNIQUE(doc_key, section_id)`, whose index stores every section's
  261-byte average id, and `idx_section_layout_v2_section_key`.
- `section_layout_chunk_v2`, one row per chunk of at most `LAYOUT_CHUNK_BYTES` (8192) of encoded
  boundaries: 11 integer columns plus the blob, `UNIQUE(doc_key, section_key, chunk_no)` and five
  secondary indexes, one per entry column a read ranges over.

## The measurement

`Book profile` run [44251449600](https://github.com/liqiangcc/reading-mcp/actions/runs/44251449600)
builds `main`'s binary and the branch's, opens the same book with both, and reads the resulting state
databases. The numbers below are that run's `dbstat` output, per input and per side.

### Bytes per table and per index

| input | side | store B | layout B | layout share | `section_layout_v2` | `UNIQUE` autoindex | `idx_..._id` | `idx_..._section_key` | `section_layout_chunk_v2` |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `zh-large` | main | 14,450,688 | 5,009,408 | 34.67% | 1,699,840 | 237,568 | 0 | 12,288 | 2,961,408 |
| `zh-large` | branch | **12,771,328** | **3,330,048** | **26.07%** | **241,664** | 0 | 16,384 | 12,288 | 2,961,408 |
| `md-2000` | main | 6,406,144 | 4,714,496 | 73.59% | 4,112,384 | 135,168 | 0 | 28,672 | 241,664 |
| `md-2000` | branch | **2,568,192** | **876,544** | **34.13%** | **360,448** | 0 | 49,152 | 28,672 | 241,664 |
| `en-novel` | main | 3,829,760 | 1,548,288 | 40.43% | 798,720 | 20,480 | 0 | 4,096 | 651,264 |
| `en-novel` | branch | **3,088,384** | **806,912** | **26.13%** | **65,536** | 0 | 12,288 | 4,096 | 651,264 |
| `zh-classic` | main | 1,486,848 | 225,280 | 15.15% | 69,632 | 4,096 | 0 | 4,096 | 122,880 |
| `zh-classic` | branch | **1,429,504** | **167,936** | **11.75%** | **12,288** | 0 | 4,096 | 4,096 | 122,880 |
| `cjk-64mib` | main | 54,259,712 | 14,548,992 | 26.81% | 4,096 | 4,096 | 0 | 4,096 | 14,225,408 |
| `cjk-64mib` | branch | 54,259,712 | 14,548,992 | 26.81% | 4,096 | 0 | 4,096 | 4,096 | 14,225,408 |

Two shapes fall out of the table:

- On a prose-heavy book the chunk table is the layout: `cjk-64mib` keeps 14,225,408 B of boundaries
  and one page of headers, because it has one section with 3,195 chunks.
- On a document with many short sections the **header** is the layout: `md-2000` (2,001 sections)
  spent 4,112,384 B on `section_layout_v2` — 64% of its whole store — and only 241,664 B on
  boundaries. `zh-large` sits between the two at 1,699,840 B of headers for 543 sections, 3,130 B a
  row for a row whose numbers are a few dozen bytes.

### What the header row holds

`SUM(length(col))` over every row of `section_layout_v2`, and the same after the change:

| column | `zh-large` main | `zh-large` branch | `md-2000` main | `md-2000` branch |
| --- | ---: | ---: | ---: | ---: |
| `section_id` | 141,681 | 141,681 | 100,931 | 100,931 |
| `paragraph_coverage` | 262,052 | 7,735 | 533,141 | 26,010 |
| `coverage_paragraph_preserve` | 201,400 | 9,458 | 730,363 | 32,015 |
| `coverage_paragraph_eligible` | 201,943 | 9,458 | 732,364 | 32,015 |
| `coverage_sentence_preserve` | 201,677 | 9,696 | 730,363 | 32,015 |
| `coverage_sentence_eligible` | 202,220 | 9,696 | 732,364 | 32,015 |
| the five coverage columns | 1,069,292 | **46,043** | 3,458,595 | **154,070** |
| `id_hash` | — | 10,525 | — | 39,128 |
| `char_len` | 2,198 | 2,198 | 6,001 | 6,001 |
| `chunk_count`, `paragraph_count`, `sentence_count`, `sorted_flags`, four `total_items_*` | 8,949 | 8,949 | 16,008 | 16,008 |

The five JSON columns were 1,069,292 B of a 1,699,840 B table on `zh-large`: 63% of the header was
key names, JSON punctuation and a repeated section id, for 5 × 9–14 numbers. The same values cost
46,043 B as a version byte and a varint list — 4.3% of the table.

### Are the coverages duplicated?

A row can store one value four times if the four stream coverages agree. They do not: in every input
`all_four_equal_rows=0`, `paragraph_pair_equal_rows=0` and `sentence_pair_equal_rows=0`, and every
row has a paragraph coverage (`paragraph_coverage_null_rows=0`). Deduplicating equal values would
save nothing on any input measured, so that candidate is out on the numbers rather than on taste.

### The chunk table's shape

| input | chunk rows | sections | single-chunk sections | max chunks | blob B | sentences | blob B/sentence |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `zh-large` | 858 | 543 | 359 (66%) | 5 | 2,445,773 | 218,533 | 11.19 |
| `cjk-64mib` | 3,195 | 1 | 0 | 3,195 | 11,739,229 | 1,112,340 | 10.55 |
| `md-2000` | 2,001 | 2,001 | 2,001 (100%) | 1 | 177,960 | 6,000 | 29.66 |
| `en-novel` | 388 | 388 | 388 (100%) | 1 | 500,975 | 33,522 | 14.94 |

The chunk rows are the boundary payload itself — 11.19 B per sentence on `zh-large` — not a copy of
something the header already knows: the header has counts, not offsets, so a single-chunk section
still needs its chunk row. The 11 integer columns beside the blob cost 30–40 B a row: 34,320 B over
`zh-large`'s 858 rows.

### Every layout read query

`EXPLAIN QUERY PLAN` for each statement `sqlite.rs` runs against either table, with `zh-large`'s
document key, section key and id hash bound (`scripts/sqlite-facts.py --layout-detail` prints these
for both sides of every input):

| statement | main | branch |
| --- | --- | --- |
| `header_char_len_by_key` | SEARCH `idx_..._section_key` (doc_key, section_key) | same |
| `header_char_lens_by_doc` | SEARCH `idx_..._section_key` (doc_key) | same |
| `header_section_keys` | SEARCH `idx_..._section_key` (doc_key) | same |
| `header_sections` | SEARCH `idx_..._section_key` (doc_key) | same |
| `document_exists_with_layout` | SEARCH `text_unit_documents_v2` (document_id) + SEARCH `section_layout_v2` covering `idx_..._section_key` (doc_key) | + SEARCH `section_layout_v2` covering `idx_..._id` (doc_key) |
| `header_by_section_id` | SEARCH `sqlite_autoindex_section_layout_v2_1` (doc_key, section_id) | SEARCH `idx_..._id` (doc_key, id_hash) |
| `header_keys_and_ids` | SEARCH `idx_..._section_key` (doc_key) | same |
| `section_key_of_id` | SEARCH `d` covering (document_id) + SEARCH `l` `sqlite_autoindex_section_layout_v2_1` (doc_key, section_id) | SEARCH `idx_..._id` (doc_key, id_hash) |
| `section_id_of_key` | SEARCH `d` covering (document_id) + SEARCH `l` `idx_..._section_key` (doc_key, section_key) | SEARCH `idx_..._section_key` (doc_key, section_key) |
| `chunk_at_paragraph` | SEARCH `idx_..._chunk_v2_first` (doc_key, section_key, first_paragraph_index) | same |
| `chunk_at_sentence` | SEARCH `idx_..._chunk_v2_first` | same |
| `chunks_from` | SEARCH `sqlite_autoindex_section_layout_chunk_v2_1` (doc_key, section_key, chunk_no) | same |
| `chunk_at_no` | SEARCH `sqlite_autoindex_section_layout_chunk_v2_1` (doc_key, section_key, chunk_no) | same |
| `stream_position_paragraph_preserve` | SEARCH `idx_..._paragraph_entry` (doc_key, section_key, paragraph_entry) | same |
| `stream_position_paragraph_eligible` | SEARCH `idx_..._eligible_paragraph_entry` | same |
| `stream_position_sentence_preserve` | SEARCH `idx_..._sentence_entry` | same |
| `stream_position_sentence_eligible` | SEARCH `idx_..._eligible_sentence_entry` | same |

Every statement seeks an index on both sides; no statement scans a layout table. The four
`*_entry` indexes and `idx_..._first` are each used by at least one statement, so none of the five is
dead weight.

The one thing the plans did not settle by themselves is the two id lookups. On the v11 store the
planner answered them with a join of two seeks (`d` by `document_id`, then `l` by the id index). That
is the right plan, but it is the planner's choice: with the same SQL and a store whose statistics
differ, it scans `section_layout_v2` instead. The branch resolves the document key first and then
seeks one layout row, so each statement's plan is a seek by construction.

## Design

| candidate | `zh-large` bytes saved | read cost |
| --- | ---: | --- |
| (a) the five coverage columns as varint blobs instead of JSON text | 1,023,249 of column bytes; the table itself 1,699,840 → 241,664 | none: the same values, decoded from fewer bytes |
| (b) elide the owner section id the paragraph coverage repeats | included in (a): `paragraph_coverage` 262,052 → 7,735 | none |
| (c) look a section up by a 64-bit hash of its id, replacing `UNIQUE(doc_key, section_id)` | 221,184 (237,568 → 16,384) | none; each lookup is one seek, stated rather than planner-chosen |
| (d) `WITHOUT ROWID` on `section_layout_chunk_v2` | 16,384 (its autoindex) | none |
| (e) drop a chunk index no query uses | 0 — all five are used | — |
| (f) drop chunk columns derivable from the blob | ≤ 34,320 over 858 rows | the blob is read anyway, but the saving is the whole row's integers |
| (g) store the coverage once when the four agree | 0 — no row in any input has two equal coverages | none |

Only (a) and (b) clear the 0.3 MiB bar on their own: they are one change, they take 63% of the
header row away, and they cost a read nothing but a varint decode of the bytes it already loaded.
(c) is below the bar alone (0.21 MiB), but it is the same row shape, the same single migration and
the same single store version bump, it replaces an index rather than adding one (−221,184 B), and
the header lookup needs an id index either way. (d) is 16 KiB and (f) is 34 KiB: not worth a second
store version. (e) and (g) are zero, measured.

## What was implemented

`section_layout_v2` keeps its coverages as blobs, and looks a section up by a hash of its id:

- `paragraph_coverage BLOB` — a version byte, a flag byte and eight varints. The flag says the
  coverage's `owner_section_id` is the row's own `section_id`, so the id text is not written again
  (the decoder rebuilds it from the row); a coverage owned by another section keeps the id, length
  prefixed. `zh-large`: 482 B a row → 14.2 B a row.
- `coverage_{paragraph,sentence}_{preserve,eligible} BLOB` — a version byte, 13 varints and the
  `source_complete` flag. `zh-large`: 371–372 B a row → 17.4–17.9 B a row.
- `id_hash INTEGER` and `idx_section_layout_v2_id (doc_key, id_hash)` replace
  `UNIQUE(doc_key, section_id)`. The lookup compares the full `section_id` after the seek, so a hash
  collision reads as "not stored" instead of answering with another section.
- `stored_section_key` and `stored_section_id_at_key` resolve `doc_key` with one statement and seek
  one layout row with the next, instead of joining the layout table to `text_unit_documents_v2`.

The codec lives in `src/infrastructure/section_layout.rs` beside the boundary codec: unsigned LEB128
varints, a version byte per value so a future change can be told apart from a truncated one, and an
error for trailing bytes, an unknown version, an unknown flag or a length past the input.

`TEXT_UNIT_STORE_VERSION` goes from `text-unit-store/v11` to `text-unit-store/v12`, and the open that
carries the bump drops both layout tables when `section_layout_v2` has no `id_hash` column — the same
idempotent-drop path v10 and v11 used. A document stored by v11 therefore reads as "not stored" until
its next open re-ingests it, which is what makes the caller open the book again instead of reading
rows of another shape.

The writer refuses two sections of one document with the same id itself: the `UNIQUE(doc_key,
section_id)` index used to make that a failed insert, and the id lookup index does not.

### What the change does not touch

`section_text_v3` and its encoding, the chunk rows and their five indexes, the boundary codec, and
every tool response. The section text, the section tree and the stored units are byte-identical: the
`Ingest parity` gate compares them column by column and reports `same` for every input.

## Verification

- `CI` on the head: format, clippy `-D warnings`, and the whole test suite, including
  `tests/layout1_compact_layout.rs`:
  - the stored coverages decode to the planner's, field for field, and cost at most 34 B (paragraph)
    and 24 B (stream) a row;
  - a database rewritten as `text-unit-store/v11` is dropped on open, reads as not stored, and after
    re-ingesting the same document answers byte-identical layouts and summaries;
  - every layout statement's plan names the index it seeks and none scans a layout table.
- `Ingest parity` (`base=main`): every input and every column `same`, including `zh-large`'s 543
  section rows and the coverage column rendered back to v11 JSON.
- `Book profile`: the store shrinks on every input that has headers, `mismatches=0`.
- `Doc read profile` (`base=main`, two runs): no tool slower than `main` by more than 5%.
- `Section scale`: rchar flatness ≤ 1.3 and every check passes.

### The disk result

| input | text B | main store B | branch store B | main ratio | branch ratio |
| --- | ---: | ---: | ---: | ---: | ---: |
| `zh-large` | 14,152,894 | 14,450,688 | 12,771,328 | 1.021x | **0.902x** |
| `md-2000` | 757,358 | 6,406,144 | 2,568,192 | 8.459x | 3.391x |
| `en-novel` | 3,265,485 | 3,829,760 | 3,088,384 | 1.173x | 0.946x |
| `zh-classic` | 2,332,026 | 1,486,848 | 1,429,504 | 0.638x | 0.613x |
| `cjk-64mib` | 67,108,864 | 54,259,712 | 54,259,712 | 0.809x | 0.809x |

`zh-large` gives back 1,679,360 B (1.60 MiB) and is at 0.902x of its text, inside the 1x target.
`cjk-64mib` does not move: it has one header row and 14.2 MB of boundaries, which this change does not
touch.
