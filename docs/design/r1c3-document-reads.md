# What a per-call read costs without `DocumentCache` (R1c-3a)

Inventory and measurement only: this note records what each reader of
`DocumentRepository::get_with_hash` / `get` actually touches on the decoded `Document`, and
where the `NormalizedDocumentHash` it returns is consumed. No design proposals here; the
design is written from these facts.

Line numbers are `origin/main` at the start of `bench/r1c3a-document-reads` (`3d85b3d`).

The load itself (`src/infrastructure/sqlite.rs:412-474`) on a cold call does, in order:

1. `PRAGMA data_version` + cache lookup (`:422-432`);
2. one row read `SELECT document_json, normalized_document_hash FROM documents WHERE id = ?1`
   (`:433-447`) — the whole `document_json` string;
3. `serde_json::from_str::<StoredDocument>(&json)` (`:448-449`);
4. `StoredDocument::into_document` (`:450`), which for `content_store = "section_text_v2"`
   runs `load_document_section_text` (`:5087-5160`): one `text_unit_documents_v2` row, the
   `section_layout_v2` `char_len` rows of the document, and then one
   `load_section_text_unchecked` per section, i.e. **the whole section text of the document**;
5. `validate_ocr_publication` (`:452-454`);
6. the normalized hash, only when the `normalized_document_hash` column is `NULL`
   (`:455-468`) — it hashes every section's text, the normalized block projection, the
   original-source binding map and the OCR derivation (`src/domain/normalized_text.rs:169-211`).

A warm call is step 1 plus a cache hit. `DocumentCache` (`src/infrastructure/sqlite.rs:48-84`)
keeps up to `DOCUMENT_CACHE_CAPACITY` = 8 decoded documents (`:43`).

## 1. What each reader uses

### `get_text_units` — `src/application/get_text_units.rs:214-218`

- **fields**: `id` (`:241`, `:329`, `:365`, `:434`, `:449`, `:497`, `:547`), `content_hash`
  (`:224`, `:312`), `root_sections`/`children` through `find_section` (`:219-221`).
  `source`, `title`, `media_type`, `metadata` are **not** read.
- **methods**: `find_section` (`:219`), `TextLocator::for_section_with_hash` (`:330`),
  `DerivedTextUnits::new` only when the use case was built without a text-unit index
  (`:241` region; the runtime always passes `SqliteTextUnitIndex`). `flatten_sections`,
  `section_body_order`, `reading_order` are **not** used.
- **section `content`**: none on the default path. The page is read from the text-unit index
  (`section_stream_items`), not from `Document`. Content is only touched through
  `resolve_text_locator` when `anchor_locator` is set, and then only `section.normalized_text_len()`
  (a `chars().count()` over **one** section) for a section-kind anchor
  (`src/application/locator_resolution.rs:73-83`).
- **big metadata keys**: none. With `DerivedTextUnits` (tests only) `try_segmentation` reads
  `normalized_block_map` (`src/domain/segmentation.rs:371-373`).

### `get_context` — `src/application/get_context.rs:252-256`

Two entry points, `execute` (neighbour, section units) and `execute_structured`.

- **fields**: `id`, `source` (`:379`, `:640`, `:701`), `content_hash` (through
  `resolve_text_locator`, `src/application/locator_resolution.rs:47-56`), `root_sections`.
  `title`/`media_type`/`metadata` are not read.
- **methods**: `find_section` (`:283`, `:404`, `:634`, `:666`), `flatten_sections`
  (`:345`), `TextLocator::for_section_with_hash` (`:359`, `:371`), `resolve_text_locator`,
  `DerivedTextUnits::new` when no index (`:262`).
- **section `content`**: yes, but bounded per relation:
  - `execute` / `Neighbor { Section }` (`section_neighbor_result`, `:336-399`) renders
    `before + after + 1` sections shallowly (`:356` → `render_section_shallow`,
    `src/application/reading_support.rs:25-38`, which reads `section.content`), while
    `flatten_sections` walks every section's `id`/`title` only;
  - `Container { Section }` (`:628-657`) reads **one** section's content (`:637`);
  - `Structural { .. }` (`:660-717`) reads **no** content (titles only, `:684-687`);
  - paragraph/sentence neighbours and the paragraph container read text units from the index,
    not from `Document`.
- **big metadata keys**: none.

### `read_document` — `src/application/read_document.rs:189-193`

- **fields**: `id`, `source` (`:237`, `:302`), `content_hash` (cursor claims, `:231`), `title`
  never; `root_sections` through `find_section` (`:99`, `:118`, `:278`).
- **methods**: `find_section`, `render_section_tree` (`:215`, `:120`;
  `src/application/reading_support.rs:19-23`), `normalized_text_slice` (`:281`),
  `TextLocator::for_section_with_hash` / `for_character_range_with_hash`, `DerivedTextUnits::new`
  when no index (`:199`).
- **section `content`**: yes.
  - `execute` / `continue_read`: `render_section_tree(section)` renders the section **and its
    whole subtree** — every section in that subtree, i.e. up to the whole document for a root
    section;
  - `read_exact` / `continue_exact`: `section.normalized_text_slice(range)` — **one** section.
- **big metadata keys**: none.

### `read_reading` (v2 `read`) — `src/application/read_reading.rs:147-151`

- **fields**: `id` (`:152`, `:170`, `:358`, `:366`), `root_sections`/`children` through
  `reading_order` and `find_section` (`:366`, `:806`). `content_hash`, `source`, `title`,
  `media_type` are not read; `title` of a *section* is (`:806`).
- **methods**: `reading_order` (`:385-390`) → `section_body_order`
  (`src/application/body_order.rs:12-38`), `find_section` (`:366`, `:806`).
- **section `content`**: none — every item comes from the text-unit index stream.
- **big metadata keys**: **`epub_structure_map`** for every EPUB document
  (`src/application/body_order.rs:66-72`), deserialized and validated on every call. That is the
  largest metadata value for 宋史 / zh-large.

### `search_document` — `src/application/search_document.rs:86-90`

- **fields**: `id`, `source` (`:135`, `:193`), `content_hash` (through `resolve_text_locator`).
  `title`/`media_type`/`metadata` are not read.
- **methods**: `find_section` (through `resolve_text_locator` and the scan), `DerivedTextUnits::new`
  when no index (`:94`), and the search index itself
  (`src/infrastructure/sqlite_search_index.rs:349-351` calls `get_with_hash` again for the scan).
- **section `content`**: only for hits whose locator resolves to a section-kind unit
  (`normalized_text_len` on that section, `src/application/locator_resolution.rs:73-83`), i.e. at
  most `limit` sections (default 10). Paragraph/sentence hits read the index, not the document.
  The lexical scan (`sqlite_search_index.rs:150-196`) reads section **titles** and gets text from
  `section_text_v2` via `scan_lexical`, not from `Document`.
- **big metadata keys**: none.

### `source_view` — `src/application/source_view.rs:133-137`

- **fields**: `id`, `source` (`:167`, `:181`), `content_hash` (`:186`), `media_type` (`:158`).
- **methods**: `find_section`, `resolve_text_locator` (range validation on one section,
  `locator_resolution.rs:85-95`), `original_source_binding_map` (`:225`),
  `original_source_target_for_range`, `DerivedTextUnits::new` when no index (`:140`).
- **section `content`**: one section, only to validate the normalized range.
- **big metadata keys**: **`original_source_binding_map`** — deserialized (`serde_json`) and
  validated on every call (`src/domain/original_source.rs:67-77`); it is also serialized again to
  compute the SHA-256 of the map. `ocr_evidence_blob` is read only through
  `validate_ocr_publication` in the load itself.

### `get_document_structure` — two sites

- `resolve_named_section`, `src/application/get_document_structure.rs:172-176`;
- `execute_command`, `src/application/get_document_structure.rs:202-206`.

Both: **fields** `id`, `content_hash`, `title` (`:270`), `root_sections`; **methods**
`section_body_order` (`:177`, `:207`) and `find_section` (named-section path);
**section `content`**: none; **big metadata keys**: **`epub_structure_map`** through
`section_body_order` for EPUB, plus `pdf_structure_provenance` on the named-section path
(`:505-512`).

### `open_reading` (v2 `open`) — `src/application/open_reading.rs:115`

- **fields**: `id`, `root_sections` (`:129-146`); the title, media type and section count in the
  response come from the stored open result (`opened`), not from the `Document`.
- **methods**: none beyond field access; `section_key` comes from the text-unit index.
- **section `content`**: none.
- **big metadata keys**: none.
- **note**: `Arc::unwrap_or_clone(document)` (`:116`) **clones the whole decoded document**
  whenever the cache holds another reference (the normal case: the cache owns an `Arc`), so a
  warm hit here is a full deep copy of `Document`, section text included.

### `SqliteSearchIndex::search_lexical_with_options` — `src/infrastructure/sqlite_search_index.rs:349-351`

- **fields**: `id`, `root_sections` (titles and ids only, `:73-83`, `:150-196`).
- **methods**: `find_section` (`:110`).
- **section `content`**: none from `Document`; text comes from `section_text_v2` through
  `scan_lexical`.
- **big metadata keys**: none.
- It is a second `get_with_hash` inside a `search_document` call, so a `search_document` call
  loads the document twice on a cold cache (once in the use case, once in the scan).

## 2. What the hash is used for

`NormalizedDocumentHash` returned by `get_with_hash` is the document's normalized-text identity.
Consumers:

| consumer | use | site |
| --- | --- | --- |
| `get_text_units` | stale-cursor check against `claims.normalized_document_hash`; embedded in every returned `TextLocator` and in the next cursor | `get_text_units.rs:227-233`, `:330-334`, `:311-325` |
| `get_context` | passed to `resolve_text_locator` (stale-locator check) and into every item/anchor locator | `get_context.rs:252-256`, `locator_resolution.rs:58-63` |
| `read_document` | cursor identity check and every locator in the response | `read_document.rs:164`, `:236-246`, `:300-310` |
| `read_reading` | `handle_checksum(document.id, normalized_hash)` — the v2 handle checksum | `read_reading.rs:152` |
| `search_document` | `resolve_text_locator` for every hit; result locators | `search_document.rs:154-160` |
| `source_view` | `resolve_text_locator`, then returned as `normalized_document_hash` in the response | `source_view.rs:141-146`, `:204-206` |
| `get_document_structure` | stale-cursor check and response field | `get_document_structure.rs:208-218`, `:266-268` |
| `open_reading` | `handle_checksum` of the document handle (through `repository.get`, which discards the hash and recomputes it from `opened`) | `open_reading.rs:91-94` |
| `SqliteSearchIndex` | passed to `scan_lexical` to pick the stored text of exactly that version | `sqlite_search_index.rs:360-370` |
| telemetry | `repository_get` duration/hit only; the hash itself is not logged | `observability.rs:388-403` |

Recomputing it (`document.normalized_document_hash()`) hashes every section's `id`, `title` and
`content`, the normalized block projection, the binding map and the OCR derivation
(`src/domain/normalized_text.rs:169-211`); the load only does so when the
`normalized_document_hash` column is `NULL`.

## 3. R1c-3c: how each text use is served

Section 1 lists what each reader touches on the decoded `Document`. This section is the exhaustive
list of the places that read a section's **text** (or a text length) that is reachable from the
readers R1c-3c switches to `DocumentRepository::get_light_with_hash` — v1 `get_text_units`,
`get_context`, `read_document`, `search_document`, v2 `read` (`read_reading`), v2 `open`
(`open_reading`), v2 `outline`, v2 `search`, v2 `cite`, and
`SqliteSearchIndex::search_lexical_with_options` — and how each one is served once the document
carries no section `content`. Line numbers are `origin/main` at the start of
`perf/r1c3c-light-readers` (`3867ee8`).

| # | site | what it reads | served by |
| --- | --- | --- | --- |
| 1 | `reading_support.rs:25-38` `render_section_shallow`, reached from `get_context.rs:356` (section neighbour: `before + after + 1` sections) and `get_context.rs:637` (section container: one section) | `section.content` | one `repository.load_section_text(id, section_id)` per rendered section (`SectionTexts::text`); `render_section_shallow` now takes that text as a parameter |
| 2 | `reading_support.rs:19-23` `render_section_tree` / `:83-94` `render_tree_into`, reached from `read_document.rs:120` (`continue_read`) and `read_document.rs:215` (`execute` → `read_section_segment`) | `section.content` of every section in the rendered **subtree** | `SectionTexts::fill_subtree` loads `load_section_text` for every section of the subtree in document order, then `render_section_tree` renders from that map; the cost stays O(subtree text), the output is unchanged |
| 3 | `read_document.rs:281` `section.normalized_text_slice(target.range)` (`read_exact`, `continue_exact`) | one section's text | one `load_section_text` of the owner section; the slice is taken from that string |
| 4 | `locator_resolution.rs:73-83` `section.normalized_text_len()` for a `Section`-kind locator, reached from `get_text_units` (`anchor_locator`), `get_context` (`target_locator`), `read_document` (`read_exact`/`continue_exact`), `search_document` (every section-kind hit) | the **length** of one section's normalized text | `DocumentRepository::load_section_char_len`, i.e. the stored `section_layout_v2.char_len` of that section (one indexed row). It is exactly `Section::normalized_text_len()`: `write_document_sections` stores `section.normalized_text_len()` (`sqlite.rs:3149`) and both `section_text_is_stored` (`sqlite.rs:887-893`) and the light load verify it against the text they read (`sqlite.rs:798-803`) |
| 5 | `locator_resolution.rs:85-95` `section.validate_normalized_range(range)` for a `CharacterRange`-kind locator, reached from `read_document` `read_exact`/`continue_exact` | the length of one section's normalized text | the same `load_section_char_len` (#4) |
| 6 | `outline_reading.rs:158` `section.content.chars().count()` (v2 `outline` reports `chars` per listed section) | the length of one section's normalized text | the same `load_section_char_len` (#4), one call per listed section |
| 7 | `ports.rs:326-333` the **default** `DocumentRepository::load_section_text` | `s.content` of the full document | unchanged: it is the correct-but-not-light implementation other repositories keep. `SqliteDocumentRepository` overrides it (one `section_text_v2` read) and falls back to this behaviour for a row that keeps its text inline |
| 8 | `derived_text_units.rs:32` `document.try_segmentation()` (reads every section's text and `normalized_block_map`), reached from `get_context.rs:262`, `read_document.rs:199`, `search_document.rs:94`, `source_view.rs:140` when the use case was built **without** a `TextUnitIndex` | every section's text and the block map | those use cases keep loading the full document when no text-unit index was supplied (`get_context`, `read_document`, `search_document`); the runtime always supplies one (`runtime/mod.rs:235-269`), so production reads light. `get_text_units` takes its index as a constructor argument and therefore always reads light — its `DerivedTextUnits` (tests) owns its own full document |
| 9 | `body_order.rs:66-72` `epub_structure_map` (a heavy metadata entry, up to 0.84 MB on 宋史), reached from `read_reading.rs:385-390` `reading_order` | the EPUB structure map, deserialized and validated per call | the stored `document_body_order_v1` row: one row read and one JSON array parse (`DocumentRepository::body_order`); the row is written by `save_shared` from the full document, and a document saved before this change falls back to the full computation |
| 10 | `cite_reading.rs:120` `section_body_order(&resolved.document)` (v2 `cite` → v2 `source_view`'s document-handle locator) | the EPUB structure map | left as is: it is on the `source_view` path, which R1c-3d migrates. It is reached only for a PDF, whose order is the canonical tree walk and needs no metadata |
| 11 | `get_document_structure.rs:177`, `:207` and `source_view.rs:225` (`original_source_binding_map`), `:133-137` | body order / binding map / one section's text | **not in this task**: R1c-3d |

Two more consequences of the light document are worth recording, because neither reads text but both
depend on the document being the **same** one the full load returned:

- `SqliteSearchIndex::search_lexical_with_options` (`sqlite_search_index.rs:349-351`) used to load the
  document a second time inside a `search_document` call. It now takes the light document and its
  normalized hash from the caller (`SearchIndex::search_lexical_in`) and loads light itself only when
  it is called on its own, so a `search_document` call loads the document once.
- `open_reading.rs:116` `Arc::unwrap_or_clone(document)` deep-cloned the decoded document on every
  `open`. The use case now reads the light `Arc` and never clones it.
