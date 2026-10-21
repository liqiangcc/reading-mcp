# Cache-free O(page) reads (R2c-4a)

Design note only: this task changes no code. It designs the change that makes every per-call TextUnit read cost O(page) through
indexed SQLite lookups alone, with `SectionLayoutCache` and `CachedSectionLayout` deleted and no in-process cache on the read path.
Tool responses must stay byte-identical.

*(measured)* = measured for this note with SQLite 3.46.1 (this host's Python; the server bundles 3.50.x via
`libsqlite3-sys` 0.38.1), `page_size` 4096, WAL, default `cache_size` 2 MiB — re-check against the bundled build.
*(R5a)* = PR #199. `zh-large`'s layout size is the R2c-2b `dbstat` figure; `cjk-64mib`'s is reproduced from
`scripts/memory-profile.py`'s generator and `section_layout.rs`'s codec.

## 0. Decision

Store the layout as **rowid chunks of consecutive paragraphs** (`section_layout_chunk_v2`), one small header row per section
(`section_layout_v2`) holding the summary counters and the plan error, and find the chunk for a position or a unit with **covering
index seeks on the chunk's own counter columns**. The header carries no chunk directory: a directory big enough to matter is itself
read several times per call (§3.4). Every per-call read is 4–5 indexed rows whose payload is one chunk (≤ 8 KiB) plus the text
slice.

## 1. What one tool call reads today

`SqliteTextUnitIndex` implements 9 `TextUnitIndex` methods plus 4 key lookups on `feat/v2-1-open-read`; five are reached by a single
tool call. The cost columns are cjk-64mib's one 64 MiB section.

| method | reads today | cache hit | miss | per-call callers |
| --- | --- | --- | --- | --- |
| `section_stream_summary` | doc row + `PRAGMA data_version` + `section_layout_v1` row (`section_key`, `char_len`, `paragraph_coverage_json`) + the plan | the row read: **13.0 MiB**, 7.6 of 8.4 ms *(R5a)* | + whole `layout` blob, `plan_section_stream_desc` over both kinds | `get_text_units` (always), `get_context` (unit neighbors), v2 `read` (once per section, memoized) |
| `section_stream_items` | the same preamble, the plan, the page's units | same row read + text slices | + blob decode + full plan | `get_text_units` (1–3×), `get_context` (1×), v2 `read` (1–2×) |
| `section_stream_position` | the same preamble + the plan | same row read | + blob decode + full plan | `get_text_units` (anchored), `get_context` (unit/container), v2 `read` (handle steps) |
| `section_unit` | the same preamble + slot lookup + `load_text_slice` | same row read + one text slice | + blob decode | `resolve_text_locator`: `read_document` exact, `get_source_view`, every search hit, `get_context` container, `get_text_units` anchors |
| `section_units` | the whole section: every text chunk + the whole layout | O(section) | O(section) | none in production on main; tests, `DerivedTextUnits` equivalence |

The rest are ingest (`replace_document*`) or diagnostics (`list_document`, `stored_section_layouts`, no production caller); search
adds `scan_lexical` (O(doc), not per call) and one `section_unit` per hit. Keep: `load_cached_section_layout` (`sqlite.rs:3066`)
runs `PRAGMA data_version` and the coverage query **before** checking the cache (`:3072`; callers `:2634`, `:2740`, `:2827`,
`:2875`, `:2968`, `:3321`); `items` and `position` do **not** compare `segmentation_version` while `section_units`, `section_unit`
and `summary` do; and no layout row (or a `NULL` coverage JSON) makes `summary` fail with the section-coverage message, `items` `[]`
and `position` `None`, while a paragraph without a coverage record makes all three fail with the paragraph-coverage message (but
`items`/`position` only reach the plan when the section coverage exists).

**Why the row read is the whole blob.** `section_layout_v1` is `WITHOUT ROWID` keyed by `(doc_key, section_key)` and its record
carries the blob. SQLite positions such a cursor by unpacking the cell's record at every b-tree level, so *any* column read reads
the record's whole payload once per level: *(measured)* 1 MiB blob → 0.15 ms, no I/O; 4 MiB → 4.0 MiB, 4.5 ms; 13 MiB → 13.01 MiB,
10.7 ms; even `SELECT chunk_no` of a 49 KB row reads 523 KiB. That is the 7.6 ms — the coverage JSON R5a blamed is ~250 bytes
(`StoredParagraphCoverage`), so the cost is the record. Hence: **chunk rows must be a rowid table with a unique index**, whose index
records are small (the seek compares only the key) and whose row is fetched by rowid reading only the selected columns: *(measured)*
9 KB blob → 8 KiB per call, 30 KB → 28 KiB, 2 KB → nothing, against 72–346 KiB for the same rows in a `WITHOUT ROWID` table.

## 2. What the stream plan needs

`plan_section_stream_desc` (`text_unit_stream.rs:222`) builds, per section and kind:

- **local to one paragraph**: whether it contributes one entry (kind `paragraph`), one *ineligible* entry with
  `degradation = coarse_degradation(class)` (kind `sentence`, coarse paragraph), or one entry per sentence; plus that
  entry's `content_class`, `char_count` (`end - start`) and `eligible`.
- **section-wide**: `SectionCoverageDesc` (`owner_chars`, `separator_chars`, `paragraph_count`) and the counters over
  all paragraphs and coverage records: `sentence_eligible_paragraphs`, `non_prose_paragraphs`,
  `coarse_structural_paragraphs`, `sentence_separator_chars`, `represented_paragraphs`, `represented_sentences`,
  `eligible_represented_*`, `coarse_non_prose_items`, `coarse_structural_items`, `intentionally_skipped`
  (`unsupported_gaps` and `source_complete` are constants).
- **errors**: `paragraph coverage is unavailable for section {id}` when the section coverage is missing;
  `sentence coverage is unavailable for paragraph {p} in section {id}` when a paragraph has no coverage record.

**Entry order.** The planner iterates `layout.paragraphs` in *slice order* and, per paragraph, the sentence slots of
`sentences_by_paragraph`, built by iterating `layout.sentences` in slice order. Entry order is therefore paragraph-array order with
each paragraph's sentences in sentence-array order — array order, not index order, with no sort or de-duplication first; the chunker
preserves that by cutting the paragraph array into runs.

**Can `paragraphs_sorted` / `sentences_sorted` be false?** They gate the binary-search slot lookups in `CachedSectionLayout::new`.
The only writers are `SectionLayoutSource::Segmentation` (production) and `::Units` (tests, port default).
`collect_section_segmentation` (`domain/segmentation.rs:417`) assigns `paragraph_index = offset + 1` over
`section_paragraph_candidates`, which walks the section's blocks in stored order and the gaps in ascending offset, and pushes
sentences per paragraph with `sentence_index = sentence_offset + 1`; `materialize()` and `build_section_layout` keep that order. So
both flags are always true for rows this code writes and can only be false for a hand-written or corrupted row. If false, the code
keeps today's behaviour — a linear scan returning the *first* match (scan chunk rows in `chunk_no` order) — and the writer stores a
non-strictly-increasing section as a **single chunk**, so in-chunk semantics are exactly today's. That fallback is O(section) and
unreachable for data this code writes.

## 3. Storage choice

### 3.1 (A) Chunked layout

```
section_layout_v2   one row per section (rowid table, unique (doc_key, section_id)): char_len,
  paragraph_coverage_json, plan_error, paragraph_count, sentence_count, chunk_count, sorted_flags,
  total_items[2 kinds x 2 policies], coverage[preserve_source, eligible_only]
section_layout_chunk_v2   one row per chunk (rowid table)
  doc_key, section_key, chunk_no, first_paragraph_index, first_sentence_index,
  paragraph_slot, sentence_slot,             -- global array slots of that start
  paragraph_entry, sentence_entry,           -- PreserveSource stream start per kind
  eligible_paragraph_entry, eligible_sentence_entry,  -- EligibleOnly stream start per kind
  paragraph_count, sentence_count, blob
  unique (doc_key, section_key, chunk_no)
  index (doc_key, section_key, {paragraph,sentence}_entry) and (…, eligible_{paragraph,sentence}_entry)
  index (doc_key, section_key, first_paragraph_index, first_sentence_index)
```

The blob is `SectionLayout::encode` restricted to the chunk's paragraph run, with delta bases reset per chunk and paragraph indices
relative to `first_paragraph_index`; a chunk holding a sentence range of one paragraph carries that paragraph's coverage record and
only its sentences. Chunk size: stop at **8 KiB of payload** (≈ 570 sentence entries, ≈ 95 paragraphs), so a 256-item
`get_text_units` page touches at most two chunks.

### 3.2 (B) Materialized stream chunks

Per kind, chunks of N consecutive entries (`unit ref, content_class, degradation, char_count, eligible`), plus the layout kept as
today for the search scan and `section_units`; to rebuild an item without the layout each entry must also carry the unit's range,
source order and parent range.

### 3.3 On-disk size, and rows and bytes per Next call

| | layout on disk, zh-large | layout on disk, cjk-64mib | rows / payload / read per call |
| --- | ---: | ---: | --- |
| today | 3.7 MiB (543 rows, `dbstat`) | 13.80 MiB (1 row) | 3–4 / 13.8 MiB + 49 KB / 13.0 MiB layout + 0.67 MiB text *(measured)* |
| (A) | ~3.1 MiB (543 headers + ~220 chunks) | 11.14 MiB payload, 11.58 MiB table + 0.18 MiB indexes *(measured)* | 5 / 8 KiB + 49 KB / 8–16 KiB layout + 0.67 MiB text |
| (B) | ~3.7 + ~10 MiB | ~13.8 + ~18 MiB | 5 / 8 KiB + 49 KB / 8–16 KiB streams + 0.67 MiB text |

(A) is smaller than today because a chunk's paragraph indices are 1–2 byte deltas from its first paragraph instead of 2–3 byte
absolutes; (B) keeps the layout *and* the streams (~14 bytes per sentence entry, 1.11 M ≈ 15 MiB; ~12 per paragraph entry, 186 k ≈ 2
MiB). The text part is identical in all three: `load_text_slice` reads the 16,384-char `section_text_v1` chunks covering the page —
49 KB of payload per chunk for CJK, amplified ~13× by the same `WITHOUT ROWID` defect *(measured)*: 666 KiB and 0.31 ms per slice.
Out of scope here (§8), but it becomes the dominant per-call I/O once the layout read is gone.

### 3.4 Handle or anchor → position

`section_id_for_key` (one JOIN seek on the header) is memoized per v2 walk, so a handle costs one header seek, one chunk index seek
and one chunk fetch. An anchored `get_text_units` call runs `section_unit` (chunk fetch + text slice), `section_stream_position`
(chunk fetch) and `section_stream_items(position, 1)` (same or next chunk). A unit → position lookup binary-searches the
`(first_paragraph_index, first_sentence_index)` index, walks the chunk's paragraphs and sentences, and adds the local entry index to
the chunk's `*_entry` start; for `EligibleOnly` the rank is `eligible_*_entry + eligible entries before the unit inside the chunk`.
That is why the directory is the chunk's own columns behind covering indexes instead of a header blob: a 1,947-chunk section would
need a 16–20 KiB directory, which as a `WITHOUT ROWID` header record costs 4–5× that per call *(measured)* and as a plain blob
column still 16–20 KiB. Two index seeks cost ~8 KiB and are O(log chunks).

### 3.5 How the search scan and `section_units` read it

`scan_stored_sections` (`:1773`) and `section_units` need the whole section, so they read every chunk row (ordered by `chunk_no`) and
decode the runs into one `SectionLayout` — the same three vectors they hold today, one row per chunk instead of per section. Both are already O(section) and neither is on the Next path. `stored_sections` (`:2074`) and the
v2 key lookups point at the header table, which keeps `(doc_key, section_key, section_id, char_len)`, so `section_text_is_stored`
(`:773`) and `section_char_lens` (`:836`) only rename.

### 3.6 One paragraph with 100k sentences

The chunker walks paragraphs; when one paragraph's entries exceed the 8 KiB budget, the chunk is cut at sentence boundaries: its
first chunk carries the paragraph record, the coverage record and sentences `[0, n1)`, each following chunk the coverage record and
the next sentence run, with `first_paragraph_index = p`, `first_sentence_index = n_i` and no paragraph record. The paragraph-kind
entry lives in the first chunk, so later chunks repeat the same cumulative `paragraph_entry`; a 100k-sentence paragraph becomes ~175
chunks and reading any one of its sentences costs one chunk. In (B) each entry's range still comes from the layout record holding
all 100k sentences, so (B) keeps the O(paragraph) decode here.

### 3.7 Pick (A)

Both are O(page) per call. (A) is ~2.7× smaller on disk than (B) and smaller than today (11.8 vs 13.9 MiB on cjk-64mib), needs no
second representation to keep in sync with the layout the scan and `section_units` still read, and adds no ingest work beyond the one
counter pass of §5. (B)'s only advantage — skipping the in-chunk decode — is microseconds against an 8 KiB read, and it answers the
same questions twice (eligibility, coarse degradation): the drift the byte-identical requirement forbids.

## 4. Exact equality

- **`summary`**: `total_items` and `coverage` are the header's stored values, produced at ingest by the same counter
  code the planner uses (§5), so they are equal by construction. `plan_error` is stored verbatim and returned as
  `InvalidRequest(plan_error)`; a missing header returns today's `paragraph coverage is unavailable for section {id}`.
- **`items` / `position`**: same position arithmetic (`Forward` `[p, p+limit)`, `Backward` `[p-limit, p)`), same policy
  mapping (EligibleOnly position k = k-th eligible entry), same min/max range, same single `load_text_slice`, same
  `build_owned_unit`, same check order (no header → `None`/`[]`; `NULL` coverage → `None`/`[]`; unit missing → `None`;
  `plan_error` → `Err`; else the answer), same "first entry of a unit wins", same `EligibleOnly` → `None` for an
  ineligible entry. The page's entries come from the chunks it touches in `chunk_no` order, the planner's order; global
  `UnitRef` slots are `chunk.paragraph_slot + local`, so the missing-descriptor message keeps its slot number.
- **`section_unit`, `section_units`, `list_document`**: same slot lookup (binary search or linear fallback), same text
  slice, same `rebuild_*_unit`, same `StaleDocument` when the header or section is missing; every chunk decoded into
  one `SectionLayout`, so `SectionTextUnits` is identical. `stored_section_layouts` re-encodes the whole section.

Tests:

- `tests/text_unit_stream_sql.rs`: keep `sqlite_streams_match_the_default_planner_for_every_section_kind_and_policy`
  (every page, both kinds and policies, byte-for-byte against the in-memory planner) and
  `sqlite_stream_windows_match_the_default_planner_for_every_section_kind_and_policy`; extend `compare_positions` to
  call `section_stream_position` for **every** paragraph and sentence under both policies and compare with a linear
  scan of the planned stream; `large_section_pages_preserve_positions_and_eligible_only_is_dense` must span many
  chunks; `missing_section_layout_is_reported_as_stale` unchanged. New: a section spanning ≥ 3 chunks with sentences
  straddling chunk boundaries, checked through `section_units`, `section_unit` and every page; a one-paragraph
  100k-sentence section; a hand-written layout with shuffled paragraphs and sentences checked against a linear scan
  (the unsorted fallback); a `NULL`-coverage section and a paragraph without coverage, checked for the exact errors.
- `tests/sql_section_text_units.rs`: keep `sqlite_section_units_match_from_sets_for_every_section`,
  `get_text_units_is_identical_for_in_memory_and_sqlite_indexes` and
  `sqlite_section_units_read_long_sections_across_chunks`; rewrite
  `replacing_sets_twice_invalidates_the_section_layout_cache` as "replacing twice replaces the chunk rows".
- `scripts/ingest-parity.py`: add a `reads` block — on the same server after `open_document`, run a fixed sequence and
  compare every payload (wall-clock keys are already dropped): `get_text_units` forward from 0, the middle and the end,
  `max_items` 1 and 256, both kinds, both policies, one anchored call and one `next_cursor` continuation;
  `get_context` for a paragraph, a sentence and a container; `read_document` by section and by exact locator;
  `search_document` for a fixed query. The `layouts` block needs a ~40-line uvarint/zigzag decoder to reassemble the
  head's chunk rows into the whole-section blob the base still stores.

## 5. Migration

- `TEXT_UNIT_STORE_VERSION` becomes `text-unit-store/v4`. It is part of
  `OpenDocumentUseCase::open_result_fingerprint` (`open_document.rs:121`), so every stored open result misses on the
  next open and the document is re-ingested exactly once, then stored under the new fingerprint. `stored_unit_hash`
  keeps returning `None` for dropped rows, so a read before that re-open returns today's `StaleDocument`.
- `SqliteTextUnitIndex::open` drops `section_layout_v1` (as it already drops the v1/v2 tables) and creates
  `section_layout_v2` + `section_layout_chunk_v2`. Dropping is idempotent, so an old database opens, keeps its
  `documents` rows and `section_text_v1` text, reports its documents as not stored for text units, and re-ingests them
  on their next open; `section_text_is_stored` reads the new header table, so text still stays out of `document_json`.
- The writer computes the counters and the plan error in the pass that cuts the chunks, with the counter accumulation
  factored out of `plan_section_stream_desc` into an accumulator both call, and a test asserting the streaming
  counters equal the planner's `TextUnitEnumerationCoverage` for every fixture section and kind. This avoids
  materializing the plan at ingest: a sentence-kind plan is 1.1 M × 24 B ≈ 27 MB for cjk-64mib, which would show in
  the open peak. `TEXT_UNIT_STREAM_VERSION` stays: the stream's shape does not change.

## 6. Cache removal

Deleted: `SectionLayoutCache` (`:337`) with `sync_version`/`get`/`insert`/`remove_document`, `SECTION_LAYOUT_CACHE_CAPACITY`
(`:89`), `CachedSectionLayout` (`:120`) with its `paragraphs`, `sentences`, `coverage`, `section_coverage`, the two `OnceLock`
plans, the two sortedness flags and `new`/`plan`/`planned`/`paragraph_slot`/`sentence_slot`, `PlannedStream`,
`PlannedStreamFailure`, `build_planned_stream` (`:92`–`:332`), `load_cached_section_layout` (`:3066`), the `cache` field (`:978`),
the `&Mutex<SectionLayoutCache>` parameters of `write_document_sections` (`:2299`) and `paragraph_units_from_layout` (`:3298`), the
five read methods' `Arc::clone(&self.cache)`, `cache.remove_document(doc_key)` (`:2406`) and the `PRAGMA data_version` at `:3073`;
the one in `get_with_hash` (`:708`) stays, because `DocumentCache` stays. What else holds memory per call:

- `DocumentCache` (8 decoded documents, with section text and block map): every `get_text_units`, `get_context` and
  `read_document` call loads the whole document through `get_with_hash` first — a hit is one `PRAGMA data_version` and
  no SQLite read, a miss is O(document). So O(page) holds for every call after the first per document in a process.
  This task deletes the layout cache; the document cache is the next thing that would have to go.
- The #194 scan text cache (`ScanTextStore`, 48 MiB default) — **not affected**: it caches `section_text_v1` text for
  `scan_lexical`, keyed by the document meta, and never held a layout. It stays, as approved separately.
- Per call, transiently: the header row, one chunk's decoded vectors, the page's text and the response payload.

## 7. Measurement plan

Deep Next at 0/50/99% of the section stream, for cjk-64mib (one 64 MiB section) and zh-large (543 sections, measured in its largest
section), release binary, fresh state directory:

1. open outside the measurement, seek with large untimed pages, then run 2,000 forward `get_text_units` calls
   (`sentence`, `preserve_source`, `max_items` 1) following `next_cursor`, reporting mean/median/p95/p99 and the
   first-5%/last-5% split, as `scripts/deep-next-probe.py` does;
2. add `--io-bytes`: `/proc/<server pid>/io` `rchar` before and after the timed loop, as bytes per call (this is what
   showed 13.0 MiB today), plus the same for a `max_items` 256 page and an anchored call;
3. repeat for both kinds and policies, and for `get_context` and an exact `read_document`, which use
   `section_stream_position` and `section_unit`.

Target: **≤ 2 ms per call, flat across positions**, layout bytes per call ≤ 16 KiB (down from 13.0 MiB). Today: 8.4 ms at 50% of
cjk-64mib *(R5a)*, 0.4–0.9 ms on zh-large. Re-measure zh-large's open (10.8 s) to keep the extra ingest pass honest.

## 8. Risks and open questions

- **`section_text_v1` has the same defect and becomes the largest per-call read**: 49 KB chunks in a `WITHOUT ROWID`
  table read 666 KiB per slice *(measured)*; as a rowid table the same read is 47.6 KiB and 0.18 instead of 0.31 ms.
  No semantic effect, but it needs its own table name and drop-and-re-ingest, and dropping it makes `document_json`
  rows that keep their text there unreadable until re-open. Recommend a separate task; the target is met without it.
- **Ingest cost**: one extra pass cutting chunks and accumulating counters, expected +50–200 ms on cjk-64mib's 3.0 s
  open and less on zh-large; keep the counters streaming so the open peak does not grow.
- **`WITHOUT ROWID` behaviour** was measured with 3.46.1, not the bundled 3.50.x: re-run the byte measurement with the
  bundled build. **Merge conflicts**: `feat/v2-1-open-read` touches the same `sqlite.rs` region and `docs/`; rebase.
- **A page spanning two chunks** must merge entries from both in `chunk_no` order (at most two with 8 KiB chunks and `max_items` ≤ 256);
  a test must force the boundary case.
