# SEC-2: the hot readers answer from the point-query rows

This document is the inventory the SEC-2 change is built on. It lists, per hot path, every use of a
`Document` or `Section` value reachable from that path and says which SEC-1 point-query call (or
which existing per-section call) now serves it, which uses still need the whole document and why,
and where the tree is rendered or walked (and therefore must be rebuilt from a key range before it
is rendered).

Reference points: `src/application/ports.rs` holds the nine SEC-1 calls
(`document_header`, `section_by_id`, `section_by_key`, `sections_in_key_range`, `section_children`,
`section_ancestors`, `body_order_window`, `body_order_at`, `body_order_len`) and
`src/application/section_rows.rs` is the flattening the trait defaults and the SQLite fallback share.

## The view layer

`src/application/section_view.rs` adds:

- `DocumentView`: `document_header` plus a handle to the same `Arc<dyn DocumentRepository>` the
  reader already holds. It exposes `section_by_id`, `section_by_key`, `children`, `ancestors`,
  `key_range`, `subtree` (one `sections_in_key_range(key, subtree_end_key)` read) and
  `subtree_section` (that read, nested back into a `Section` tree).
- `carrier_document(&DocumentHeader) -> Document` and `carrier_section(&SectionRow) -> Section`.

**Carrier choice.** The design allowed either row-based `TextLocator` constructors or a
`Document`/`Section` carrier with empty `content` and empty `children`. This change uses the
carrier: every `TextLocator` constructor (`src/domain/text_locator.rs:25`, `:47`, `:66`, `:88`,
`:104`) reads exactly `document.id`, `document.content_hash`, the normalized hash, `section.id`,
`section.location.section_path` and `section.location.native_location`, all of which a header and a
`SectionRow` carry, so a carrier reproduces the existing bytes without touching the domain type or
adding a second constructor that could drift. The rule that makes the carrier safe is: **a carrier
`Section` is never rendered, walked or searched.** A path that renders a tree loads the rows of the
subtree first (`DocumentView::subtree_section`), which is listed per path below.

`ObservedDocumentRepository` already forwards the nine calls (`src/infrastructure/observability.rs`
`:470`–`:552`), and every migrated reader goes through the `Arc<dyn DocumentRepository>` trait
object, so the observed wrapper still counts every read.

## Path: v2 `open` (`src/application/open_reading.rs`)

| use | line | now served by |
| --- | --- | --- |
| `document.id` (section-key lookup, error text) | `:132`, `:139` | `document_header` |
| `document.root_sections.len()` → `more_top` | `:98` | `document_header.root_count` |
| `document.root_sections` iteration (titles) | `:129`, `:147` | `section_children(None, MAX_TOP_SECTIONS)` → `SectionRow.title` |
| section `id` (section-key lookup) | `:132` | `SectionRow.id` |

`open` needs no section text and no tree; the light document load disappears entirely.
`document_format` keeps reading `opened.media_type`, which the open use case already returns.

## Path: v2 `read` (`src/application/read_reading.rs`)

| use | line | now served by |
| --- | --- | --- |
| `document.id` + normalized hash (handle checksum) | `:152` | `document_header` (hash falls back to `get_light_with_hash` only when the header has none) |
| `document.id` (stream keys, section-key lookup) | `:170`, `:358` | `document_header.id` |
| `reading_order` → `Vec<SectionId>` | `:159`, `:384`–`:399` | `body_order_len`, `body_order_at`, `body_order_window` |
| `document.find_section(&section_id).is_none()` | `:366` | `section_by_id` |
| `document.find_section(...).title` for the section banner | `:816`–`:818` | `section_by_key(...).title` |

The `Walk` is rewritten from "index into `Vec<SectionId>`" to "body-order position":
`body_order_at(id, ord)` turns a position into a `section_key`, `body_order_window(id, key, 0, 0)`
turns a `section_key` into its `ord` (and gives `len`), and `section_by_key` gives the row behind a
position. `body_order_len` replaces the eager `Vec<SectionId>` and still reports the stored
body-order error with the same variant and message, because it is the same stored error.

## Path: v2 `outline` (`src/application/outline_reading.rs`)

| use | line | now served by |
| --- | --- | --- |
| `document.id` (section keys, char lengths) | `:142`, `:162` | `document_header` |
| `document.root_sections` preorder | `:107`, `:191` | `section_children` per visited section, bounded by `depth` |
| section scope `children` | `:112` | `section_children(Some(scope_key), …)` |
| `section.id`, `title`, `level` | `:142`, `:156`, `:157` | `SectionRow` |

The preorder is produced lazily (an explicit stack), so a page costs one `section_children` per
listed node and never builds the whole tree. `has_more` is decided by asking for one node past the
page, and a cursor position past the end of the scope is still `stale_handle`.

## Path: v2 `search` (`src/application/search_reading.rs`)

| use | line | now served by |
| --- | --- | --- |
| `resolved.document.id` | `:90`, `:112` | `document_header` |
| `resolved.handle` | `:121` | unchanged |

v2 `search` resolves its handle from the header and then calls `SearchDocumentUseCase`, which still
loads the light document: the lexical scan is a whole-document scan by definition
(`src/infrastructure/sqlite_search_index.rs:73`–`:110` walks `root_sections` and resolves each hit
section). That load is the search itself, not a per-call tree decode in front of a point lookup.

## Path: v2 `cite` (`src/application/cite_reading.rs`)

| use | line | now served by |
| --- | --- | --- |
| `document.id`, `content_hash`, `media_type`, normalized hash | `:82`–`:84`, `:103` | `document_header` |
| `find_section` for a section/paragraph/sentence locator | `:122`–`:123` (via `handle_reading.rs:78`) | `section_by_id` |
| `section_body_order` for a document handle | `:114` | `body_order_at(id, 0)` + `section_by_key` |

`source_locator` (the `source_view` page binding) asks for body-order position 0 instead of building
the whole order, which is exactly the first section in body order.

## Path: v2 `source_view` (`src/application/source_view.rs`)

Keeps `get_light_with_hash` (`:143`, `:146`): the page binding renders the section tree
(`:167`, `:264`) and is the one v2 reader whose page coordinates are tree coordinates. The header is
not enough, and rewriting the page binding is outside SEC-2.

## Path: v1 `get_text_units` (`src/application/get_text_units.rs`)

| use | line | now served by |
| --- | --- | --- |
| document `id`, `content_hash`, normalized hash | `:217`, `:225`, `:231`, `:314`–`:317`, `:332` | `document_header` |
| `document.find_section(&command.section_id)` | `:221` | `section_by_id` |
| section `id` (stream keys, locator) | `:242`, `:317`, `:369` | `SectionRow.id` |
| `section.location` (locators) | via `TextLocator::for_section_with_hash` | `SectionRow.location` |

## Path: v1 `get_context` (`src/application/get_context.rs`)

| use | line | now served by |
| --- | --- | --- |
| document `id`, `source` | `:412`, `:502`, `:588`, `:671`, `:734` | `document_header` |
| `flatten_sections(&document.root_sections)` for section neighbours | `:371` | `section_by_id` for the owner key, then `sections_in_key_range(owner-before, owner+after)` |
| `find_section(owner_section_id)` | `:306`, `:439`, `:532`, `:664`, `:699` | `section_by_id` |
| `owner.location`, `section.title` | `:416`, `:396`, `:814` | `SectionRow` |
| ancestors (`parent_id` chain) | `:755` | `section_ancestors` |
| siblings (`parent.children` / `root_sections`) | `:774`–`:787` | `section_children(parent_key)` |
| children | `:706` | `section_children(Some(key))` |

Section neighbours are depth-first order, which is exactly `section_key` order, so the window is one
key-range read and the roles are decided by comparing keys with the owner key.

## Path: v1 `read_document` (`src/application/read_document.rs`)

| use | line | now served by |
| --- | --- | --- |
| document `id`, `source`, `content_hash`, normalized hash | `:263`–`:276`, `:332`–`:336`, `:447` | `document_header` |
| `find_section(&command.section_id)` | `:100`, `:128` | `section_by_id` |
| **section subtree render** | `:102`, `:130` (`render_subtree`) | `sections_in_key_range(key, subtree_end_key)` → `subtree_section`, then the existing renderer |
| `find_section(owner_section_id)` for exact reads | `:308` | `section_by_id` |

`render_subtree` is the trap: a `Section` built from one row has no `children`, so rendering it
would silently truncate. It loads the whole subtree as one key range first, exactly as it loads the
whole subtree's text today.

## Path: `search_document` (`src/application/search_document.rs`)

| use | line | now served by |
| --- | --- | --- |
| document load | `:90`–`:93` | unchanged: the lexical scan needs the tree |
| `find_section` for legacy hits | `:218` | `section_by_id` |
| `resolve_text_locator` section lookup | `locator_resolution.rs:62` | `section_by_id` |

`sqlite_search_index.rs:385` is the standalone `search_lexical` entry point; `search_document` calls
`search_lexical_in`, so a `search_document` call still loads the document exactly once.

## Path: `resolve_text_locator` (`src/application/locator_resolution.rs`)

| use | line | now served by |
| --- | --- | --- |
| `document.id`, `content_hash` (identity checks) | `:37`, `:43` | `document_header` |
| `document.find_section(&locator.owner_section_id)` | `:62` | `section_by_id` (the caller resolves the row and passes the carrier) |

The function now takes the resolved section instead of looking it up, so every caller decides how
the section is fetched (a row on the migrated paths, the tree on the `source_view` path).

## Uses that still need the whole document

1. **`DerivedTextUnits` fallback.** `get_context`, `read_document` and `search_document` build their
   units from the document itself when no stored text-unit index is configured
   (`get_context.rs:262`–`:278`, `read_document.rs:216`–`:241`, `search_document.rs:88`–`:97`). That
   path segments the document's own text, so it keeps `get_with_hash`.
2. **`source_view`** (`source_view.rs:143`–`:167`, `:264`): the page binding renders the tree and
   needs section lengths for every section it draws.
3. **The lexical search scan** (`sqlite_search_index.rs:73`–`:110`): a scan walks every section.
4. **Ingest** (`open_document.rs` and the parsers): builds the document, never reads a stored one.

## Where the tree is rendered or walked

- `read_document.rs:102` / `:130` → `render_section_tree` over the subtree: must use
  `sections_in_key_range(key, subtree_end_key)`.
- `read_document.rs:130` continuation re-renders the same subtree: same key-range read.
- `source_view.rs:167` page binding: keeps the light document (its tree is the document's).
- `search_document` precise hits: the scan's own walk, unchanged.
- `get_context` section neighbours walk depth-first order: replaced by a key range (no render).
- `outline` walks the tree to `depth`: replaced by `section_children` per listed node (no render).

## What this change does not do

- It does not delete `document_json`'s tree or `document_body_order_v1` (SEC-3).
- It does not add a table, an index or a store-version bump: every query it issues already exists.
- It does not change the v1 `get_document_structure` tool, whose body order and boundary metadata
  read the whole tree by contract.
