# STORAGE-1: prune stale open results and unreachable documents

## The state this removes

`open_results_v1` is keyed by `document_id`, but a state directory written by more than one store
version can hold rows of a document that no current open can ever hit: the open-result fingerprint
embeds `TEXT_UNIT_STORE_VERSION`, so a row written under `text-unit-store/v8` is a guaranteed miss
after the store is at `text-unit-store/v11`. The row is dead weight, and it keeps the `documents`
row and every per-document row behind it alive even when a newer open of the same source produced a
different `document_id` (an OCR variant, for example).

Host evidence (2026-10-24, before the hand cleanup): three `text-unit-store/v8` open results next
to the current ones, and three `documents` rows no `open_results_v1` row referenced any more (the
old `naturebp.pdf` "ok" record and two older `naturebp-ocr.*` records), with their
`section_text_v3` / `section_layout_v2` rows still present.

## Rule

1. **One row per document in `open_results_v1`.** `put` deletes every row of the document first and
   then inserts the new one, in the same transaction. The surviving row is always the row the store
   just wrote, whatever the table shape of an older database (the table is created
   `document_id PRIMARY KEY` today, but a state directory can predate that). Rows removed whose
   fingerprint differs from the new one are reported as stale; replacing a row with the same
   fingerprint is an ordinary refresh and is not.

2. **Unreachable documents are deleted whole.** A `documents` row is unreachable when no
   `open_results_v1` row references its id. It is deleted together with:
   - by `document_id`: `document_header_v1`, `document_sections_v1`,
     `document_body_order_rows_v1`, `document_meta_v1`, `document_meta_v2`,
     `text_unit_documents_v2`, `text_units`;
   - by the `doc_key` read from `text_unit_documents_v2`: `section_text_v3`,
     `section_layout_v2`, `section_layout_chunk_v2`.
   These are every table the schema creates that carries a `document_id` or a `doc_key`. A
   `documents` row without a `document_header_v1` row is **not** an orphan: a document saved before
   SEC-1 has no header and is still readable through the port's default implementations, so the
   header is never part of the reachability test. The only reachability test is the open result.

3. **Stale open results are found at startup by store version.** The startup sweep deletes rows
   whose fingerprint does not end in the current `TEXT_UNIT_STORE_VERSION`, up to the same bound.
   This is what removes the rows of a document that is never re-opened, and it is what makes the
   documents behind them unreachable. A row of another store version can never hit, so deleting it
   changes no read.

## When it runs

- **In `put`.** The delete of the document's old rows, the insert of the new row, and the
  unreachable check for that document id share one `BEGIN IMMEDIATE` transaction. The check can only
  look at the document id the put just wrote (which is referenced by the new row), so it costs one
  indexed `EXISTS` and never scans the database. This is the "cheap, only the document's rows" half.
- **Once at startup.** `SqliteOpenResultStore::open` is followed by a bounded sweep from
  `runtime::build_state_components`, before any request is served: delete the stale open results of
  at most `STARTUP_PRUNE_DOCUMENT_LIMIT` (64) documents, then delete at most the same number of
  now-unreachable documents. A state directory that is never re-saved still converges over starts.
  The bound is reported in the telemetry event. The sweep is best effort: a failure is a startup
  warning, never a failed start.
- **Never on a read.** `get` is unchanged and reads one row.

## Transaction boundaries

- `put`: one `BEGIN IMMEDIATE`; stale-row delete, insert, and the unreachable check commit or roll
  back together. A crash leaves the previous open result or the new one, never a document without
  its rows.
- Startup sweep: one `BEGIN IMMEDIATE` for the whole bounded batch. Stale open-result rows and the
  document rows they orphaned are deleted in the same transaction, so a crash leaves the old state
  or the new state, and never a `documents` row whose sections were already deleted.
- Deleting twice is a no-op: every delete is by primary key, and an unreachable check that finds
  nothing deletes nothing.

## Telemetry

`emit_storage_prune` reports `trigger` (`put` or `startup`), `limit` (the startup bound), and the
counts `open_results_removed`, `documents_removed`, `rows_removed` (every row deleted, including the
open results and the `documents` rows). A `put` that removes nothing emits nothing; the startup
sweep always emits, so the next migration is visible instead of silent.

## Tests

`tests/storage_prune.rs` seeds the host shape: a current document with a current open result and an
older-version one, an unreachable document with complete rows, and a pre-SEC-1 document (no header
row) that a current open result references. The startup sweep removes the old result and the
unreachable document with all its rows, leaves the current document and the pre-SEC-1 document
untouched, and the latter still answers every read through the fallback. Re-running changes nothing,
and a sweep with a bound removes at most that many documents.
