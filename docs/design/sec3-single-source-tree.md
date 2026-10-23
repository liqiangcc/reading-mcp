# SEC-3: one source for the section tree and the body order

SEC-1 (#221) added `document_header_v1`, `document_sections_v1` and
`document_body_order_rows_v1` next to the old copies. SEC-2 (#223) moved the hot readers onto the
rows. This change removes the old copies, so the tree and the body order live only in their rows:

- `documents.document_json` keeps the light document (id, source, title, media type, content hash,
  light metadata) and, for a row whose section text is in `section_text_v3`, an **empty**
  `root_sections` plus `"section_tree_store": "document_sections_v1"`;
- `document_body_order_v1` is dropped; a failed order's exact error text lives in the nullable
  `document_header_v1.body_order_error` column.

## The one call that rebuilds a document

`DocumentRepository::get_rebuilt_with_hash(id, with_text)` is the single call the remaining
full-document paths use.

- Default: `get_with_hash` when `with_text`, `get_light_with_hash` otherwise, so a store that
  implements only one of the existing reads still answers correctly.
- SQLite: `get_with_hash` (`with_text = true`) and `get_light_with_hash` (`false`). Both decode the
  `documents` row and, when `section_tree_store` says so, rebuild `root_sections` from
  `document_sections_v1` (`src/infrastructure/sqlite.rs`, `load_section_tree` /
  `build_section_children`). The full form then fills `content` from `section_text_v3` in
  depth-first order, exactly as before; the light form leaves `content` empty.
- `ObservedDocumentRepository` forwards it (`src/infrastructure/observability.rs`), so the observed
  wrapper still counts every read.

The rebuild validates `document_header_v1.section_count` against the row count. A marker with a
missing or short set of rows is a `StaleDocument` error, never an empty book.

### Call sites that rebuild a full document

| path | line | `with_text` | why |
| --- | --- | --- | --- |
| v1 `source_view`, stored text-unit index | `src/application/source_view.rs:143` | `false` | renders the tree; text and lengths come from the repository |
| v1 `source_view`, no stored index (`DerivedTextUnits`) | `src/application/source_view.rs:148` | `true` | `DerivedTextUnits` segments the document's own text |
| v1 `get_document_structure` (both entry points) | `src/application/get_document_structure.rs:177`, `:222` | `false` | walks titles/levels/locations and the stored body order |
| v1 `search_document`, stored index | `src/application/search_document.rs:91` | `false` | the scan walks the tree, text comes from `section_text_v3` |
| v1 `search_document`, no stored index | `src/application/search_document.rs:96` | `true` | `DerivedTextUnits` segments the document's own text |
| v1 `get_context`, no stored index | `src/application/get_context.rs:254` | `true` | `DerivedTextUnits` segments the document's own text |
| v1 `read_document`, no stored index | `src/application/read_document.rs:252` | `true` | `DerivedTextUnits` segments the document's own text |
| standalone lexical scan entry point | `src/infrastructure/sqlite_search_index.rs:386` | `false` | the scan reads ids/titles and text from `section_text_v3` |

Ingest (`src/application/open_document.rs` and the parsers) builds a fresh `Document` and never
reads one back, so it is unchanged.

### Paths that read rows only (unchanged from SEC-2)

v2 `open`/`read`/`outline`/`search`/`cite`, v1 `get_text_units`, v1 `get_context` and v1
`read_document` with a stored text-unit index, and v1 `source_view`/`get_document_structure` after
their one rebuild: they never walk a `Document` tree page by page. Their section questions are the
SEC-1 point queries and `body_order_position`, so a page stays O(page).

## Deviation: `get_light_with_hash` keeps returning a tree

The task allowed `get_light_with_hash` to answer from the header or be replaced by the header call.
It cannot be made treeless without changing existing assertions: `tests/document_meta_store.rs`
asserts that the light read's `tree_shape` equals the full document's, and several tests read
section ids/locations from the light form. So `get_light_with_hash` now returns the tree rebuilt
from the rows with no section text — never a treeless document, never an empty book. The hot
readers no longer call it; the explicit `get_rebuilt_with_hash` is what the full-document paths
use. The one remaining `get_light_with_hash` call is `DocumentView::load_with_hash`'s fallback for
a store whose header carries no normalized hash, which reads only the hash.

## Body order: the error lives in the header

`document_body_order_v1` held one row per document: a JSON array of every section id, or the exact
error text. Both copies are gone.

- A successful order is `document_body_order_rows_v1` `(ord, section_key)`; `body_order()` joins
  the rows to `document_sections_v1` to return the ids in `ord` order.
- A failed order writes no order rows; its exact text goes in
  `document_header_v1.body_order_error` (nullable). The header is written in the same transaction
  as the section rows, so the error and the tree it failed to order are one row apart, and
  `body_order`, `body_order_len`, `body_order_window`, `body_order_at` and `body_order_position`
  report the same variant and message as before. A one-row table was the alternative; the header
  column avoids a second per-document row and a second lookup on the error path.
- A document saved before the rows existed (no `document_header_v1` row) still computes its order
  from `document_json`, exactly as the trait default does.
- `get_document_structure` reads the order as **section keys**
  (`DocumentRepository::body_order_keys`, one sequential scan of the order rows) and maps them to
  ids through the tree it already rebuilt, instead of `body_order`, which would join every order row
  to `document_sections_v1` — one random index seek per section. That keeps a structure page's
  order read proportional to the order rows, not to a join.
- `document_body_order_rows_v1` is a `WITHOUT ROWID` table with `PRIMARY KEY(document_id, ord)`.
  The primary key carries `section_key`, and the `(document_id, section_key)` index carries the
  primary key, so `body_order_at` and `body_order_position` are both answered from an index with no
  table lookup. The old rowid shape read a table page per point query. `SqliteDocumentRepository::open`
  recreates the old shape once (`ensure_body_order_rows_shape`), and the store-version bump
  re-ingests every book into the new shape.
- The v2 `read` walk resolves a section key to a section id, and a section id to a key, through
  `document_sections_v1` (`section_by_key` / `section_by_id`) rather than the text-unit layout
  table: both stores number sections by the same depth-first key, and the section rows are the
  smaller table. A section's title is memoized with its position, so the banner never repeats the
  row read.

## Migration and store version

`TEXT_UNIT_STORE_VERSION` goes from `text-unit-store/v9` to `text-unit-store/v10`. The open
fingerprint includes it (`src/application/open_document.rs`), so every book stored by the previous
version re-ingests once on its next open and is re-saved with the marker, the empty tree and the
body-order rows. That is the chosen contract: **re-ingest once**, not a dual-read store.

`SqliteTextUnitIndex::open` runs the migration (idempotent `DROP TABLE IF EXISTS
document_body_order_v1`) in the same open that carries the version bump. `SqliteDocumentRepository::open`
no longer creates the table and adds `document_header_v1.body_order_error` with a guarded
`ALTER TABLE` when an older header table lacks it.

A save replaces the document in one transaction: `document_json`, both metadata stores, the header,
the section rows and the body-order rows are all deleted/replaced together, so a re-save of a
smaller document leaves no orphan rows in any of the three tables. `document_body_order_v1` no
longer exists to leave rows in.

## What did not change

- Every v1/v2 tool's bytes, every error, cursor and locator: the rebuilt document is byte-identical
  to the saved one, and the body-order answers are the same, including the error case.
- No new dependency, no cache. A page read still touches only its rows.
- `documents` still carries the light metadata inline and the heavy entries in `document_meta_v2`.

## Standalone repository rows

A `SqliteDocumentRepository` used on its own (no text-unit index) stores the section text inline in
`document_json`, because no `section_text_v3` row exists to hold it. Such a row keeps its tree
inline too and has no `section_tree_store` marker: dropping the tree would drop the only copy of
the text. In the production store the text is always in `section_text_v3` first, so the tree is
dropped and the disk saving is realised.
