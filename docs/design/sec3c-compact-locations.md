# SEC-3c: a compact section location

`document_sections_v1` keeps one row per section, and its `location_json` column stores the verbatim
`StoredLocation` JSON:

```json
{"page":…,"chapter":…,"section_path":[…],"anchor":…,"paragraph":…,"char_start":…,"char_end":…,"native_location":…}
```

Every one of 宋史's 543 rows repeats the eight key names and the JSON punctuation. The row payload
is 621,709 B, of which 257,019 B is this column; the table costs 847,872 B of pages for that
payload, and its two indexes cost another 430,080 B.

## The measurement

The numbers below come from the `zh-large` state database (宋史, 543 section rows) that the
ingest-parity gate wrote at `3c7c5ea`, read with `sqlite3`:

```python
rows = conn.execute("SELECT location_json FROM document_sections_v1")
```

`dbstat` for the same file: `document_sections_v1` 847,872 B, `idx_document_sections_v1_id`
315,392 B, `idx_document_sections_v1_parent` 57,344 B,
`sqlite_autoindex_document_sections_v1_1` 57,344 B, whole file 15,060,992 B.

## Encoding comparison (all 543 rows, bytes of the location column)

| encoding | bytes | bytes/row | vs verbatim |
| --- | ---: | ---: | ---: |
| verbatim `StoredLocation` JSON | 257,019 | 473.3 | 1.00 |
| (a) positional JSON array, no key names | 205,434 | 378.3 | 0.80 |
| (a) positional binary: presence byte, varint lengths | 189,671 | 349.3 | 0.74 |
| (a) positional binary + `section_path == [title]` elision | **46,548** | **85.7** | **0.18** |
| (b) zlib(verbatim JSON), level 6 | 197,126 | 363.0 | 0.77 |
| (b) zlib(positional JSON), level 6 | 160,089 | 294.8 | 0.62 |
| (b) zlib(positional binary + elision), level 6 | 46,927 | 86.4 | 0.18 |

The elision is the decisive part: in 403 of the 543 rows `section_path` is exactly `[title]`, and
the title of those rows is the long text the path repeats. Dropping the repeated path from the
location removes 143,123 B of the column. zlib on the same bytes then *grows* the blob by 379 B,
so the raw positional binary is the smaller of the two and the read path never inflates.

Table pages, measured by rebuilding the table from the same rows into a copy of the state database:

| location column | `document_sections_v1` pages | bytes |
| --- | ---: | ---: |
| verbatim JSON (today) | 207 | 847,872 |
| zlib(verbatim JSON) | 186 | 761,856 |
| zlib(positional JSON) | 173 | 708,608 |
| positional binary + elision | 137 | 561,152 |

## What the column alone does not fix

Compacting the location saves 286,720 B of the whole file (15,060,992 → 14,774,272 B), but the
pre-SEC-1 state was 14,479,360 B: the gate needs 581,632 B. The rest of the row overhead is not in
the location column at all:

- `idx_document_sections_v1_id` is 315,392 B for 543 entries because every entry repeats the
  71-byte `document_id` and the 261-byte average `section_id`. The section id is itself a slug of
  the title path, so the table stores the same text three times (title, id, and — before the
  elision — section path).
- `title` is 223,009 B of the 621,709 B payload and 77,687 B of it is repeated across rows.

So this change also:

- replaces the `(document_id, section_id)` index with a non-unique `(document_id, id_hash)` index
  over a 64-bit FNV-1a hash of the section id, and verifies the full id after the lookup. The
  bucket is one row in practice and every id is still compared exactly, so `section_by_id` stays
  O(1) rows and answers the same value, collisions included.
- stores `title` as a one-byte-tagged blob: `0` + UTF-8 when zlib does not make it at least 5 %
  smaller, `1` + zlib otherwise. Decoding returns the exact title bytes.

Measured on the same 宋史 state, rebuilding the section table with both changes:

| build | section table | id index | parent index | key autoindex | whole file |
| --- | ---: | ---: | ---: | ---: | ---: |
| today | 847,872 | 315,392 | 57,344 | 57,344 | 15,060,992 |
| compact location only | 561,152 | 315,392 | 57,344 | 57,344 | 14,774,272 |
| compact location + id hash index | 565,248 | 61,440 | 57,344 | 57,344 | 14,516,224 |
| + compressed title | 462,848 | 61,440 | 57,344 | 57,344 | **14,409,728** |

## The location encoding

Byte 0 is a flag byte:

| bit | meaning |
| --- | --- |
| 0 | `page` present |
| 1 | `chapter` present |
| 2 | `anchor` present |
| 3 | `paragraph` present |
| 4 | `char_start` present |
| 5 | `char_end` present |
| 6 | `native_location` present |
| 7 | `section_path` elided because it equals `[title]` |

Then, in this fixed order: `page` (varint), `chapter` (varint length + UTF-8), `section_path`
unless elided (varint count, then each element as varint length + UTF-8), `anchor`, `paragraph`,
`char_start`, `char_end`, `native_location`. Numbers are unsigned LEB128; `char_start`/`char_end`
are 64-bit so `usize::MAX` round-trips. A present empty string is distinguishable from an absent
one by its flag bit, and an empty `section_path` is a count of 0, so every `Location` round-trips
exactly, including defaults and `Some(0)`.

## Version

`TEXT_UNIT_STORE_VERSION` goes `text-unit-store/v10` → `text-unit-store/v11`: the section rows
change shape (a new `id_hash` column, a different id index, a blob `title` and `location_json`), so
every book re-ingests once on its next open. `SqliteDocumentRepository::open` drops and recreates a
`document_sections_v1` written by the previous version in the same open that carries the bump,
following the `document_body_order_rows_v1` precedent; the header that remains makes a stale
document an explicit `StaleDocument`, never an empty book.

## What does not change

Every tool, error, cursor and locator: the decoded `Location` and title are the saved ones byte for
byte, and `section_by_id` / `section_by_key` / `sections_in_key_range` / `section_children` /
`section_ancestors` still answer from one row read each, with no join and no cache.
