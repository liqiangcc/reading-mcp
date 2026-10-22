# Reading text that lopdf 0.44 does not reach (PDF-1c)

## 1. How the default backend extracts text today

`src/parsing/pdf.rs:202-232` calls lopdf 0.44 twice per page:
`extract_text_chunks_with_limit(&[page], MAX_PAGE_DECOMPRESSED_BYTES)` for the
chunks used as source fragments, and `extract_text_with_limit(...)` for the
page text. Both land in `Document::extract_text_chunks_from_page`
(`lopdf-0.44.0/src/parser_aux.rs:108-215`).

### What the two calls return

- `extract_text_with_limit` (`parser_aux.rs:64-79`) concatenates the chunks and
  returns `Result<String>`. Every `Err` chunk produced by the page walk is
  propagated with `?`, so one bad font or one over-limit stream makes the whole
  page an `Err`; `src/parsing/pdf.rs:214-230` then records a page error and keeps
  an empty `PageText.text` while still keeping the chunks that did succeed.
- `extract_text_chunks_with_limit` (`parser_aux.rs:87-106`) returns
  `Vec<Result<String>>`: one item per chunk, or one `Err` item when the page
  failed. A chunk is closed at every `Tf` that finds a non-empty current text
  (`parser_aux.rs:140-162`), and a final chunk is pushed after the operation
  loop. `Tj`/`TJ` append to the current chunk; `'` and `"` first append a
  newline; `T*` and `ET` append a newline. A font whose encoding cannot be
  resolved contributes an `Err` item and no text; text operators with no
  current encoding only log a warning. `src/parsing/pdf.rs` filters chunk errors
  out and normalises each surviving chunk.
- `extract_text_chunks_inner` (`parser_aux.rs:92-103`) maps over the requested
  page numbers with `flat_map`, so a missing page number yields one
  `Error::PageNumberNotFound` item.

### How fonts are resolved

`extract_text_chunks_from_page` calls `get_page_fonts(page_id)`
(`document.rs:691-730`). That helper collects `get_page_resources(page_id)`
(`document.rs:662-689`): the page's own `/Resources` dictionary if it is
direct, plus the `/Resources` references of the page and every `/Parent` above
it, walking the page tree with a reference-cycle guard. Each resource
dictionary's `/Font` dictionary is merged into a `BTreeMap<resource name,
&Dictionary>`; the first resource dictionary that defines a name wins, and the
page's own dictionary is visited first. The content stream's `Tf` operand is
looked up in that map by resource name. A font used only by a Form XObject is
not in this map because `extract_text_chunks_from_page` has no `Do` arm: the
form's `/Resources` is never consulted.

The encoding of each font comes from
`Dictionary::get_font_encoding_with_limit` (`object.rs:420-465`): `/Encoding`
is tried first (`object.rs:440-450`), then `/ToUnicode`, then
StandardEncoding. An unparseable `/Encoding` (for example a `/Differences`
array containing `/uni9762`, which `Glyph::from_name` does not know,
`encodings/glyphnames.rs:35-51`) turns into `InvalidEncodingDifferenceGlyph`
and the whole font falls back to StandardEncoding (`object.rs:458-463`).
`Identity-H` without `/ToUnicode` also falls back (`object.rs:540-543`).

### How page content streams are concatenated

`get_page_contents(page_id)` (`document.rs:538-580`) reads the page
`/Contents` entry, follows references (up to `DEREF_LIMIT = 128`) and collects
the stream object ids, in array order. `get_page_content_with_limit`
(`document.rs:627-660`) iterates those ids, decompresses each stream, appends
the bytes and then one `\n` after every stream. The resulting byte buffer is
passed to `Content::decode` (`parser_aux.rs:30-34`), so the page's operations
are the operations of all content streams concatenated with a newline between
them.

### How the decompression limit is enforced

`get_page_content_with_limit(page_id, max)` keeps `remaining = max -
content.len()` and calls `decompressed_content_with_limit(remaining)` on each
stream. A size-limit error is returned as `MemoryLimitExceeded`; any other
decode error falls back to the raw stream bytes, but only if those raw bytes
also fit in the remaining budget. The total decoded page content is therefore
bounded by `max_decompressed_bytes` (`MAX_PAGE_DECOMPRESSED_BYTES = 16 MiB` in
`src/parsing/pdf.rs`). Font `/ToUnicode` streams are bounded separately: each
font gets the same `max` in `get_font_encoding_with_limit`, not a share of the
page budget. `extract_text_chunks_with_limit` turns a page-level limit error
into one `Err` item; `extract_text_with_limit` returns it.

## 2. Design: an in-memory pre-pass on the lopdf document

Approach **A** is used.  `prepare_text_extraction` (`src/parsing/pdf_text_repair.rs`)
returns `None` for a document with neither a text-bearing Form XObject nor a
`uniXXXX`-named simple font, so the common case keeps using the original
document and the original code path byte for byte.  Otherwise it clones the
`lopdf::Document` once and rewrites only the affected pages and fonts on the
clone; the original document still supplies the title, TOC and metadata.

`Content::decode` / `Content::encode` round-trip is sufficient for the page
rewrite, so approach B (a second text interpreter) is not needed.

### A1: text inside Form XObjects

- A document is considered only if one of its `/Subtype /Form` streams contains
  a text-showing operator (`Tj`, `TJ`, `'`, `"`), directly or through a nested
  form.  A form is followed only when its own `/Resources` dictionary exists;
  otherwise the `Do` is left as it was.
- For every page with an `/XObject` in its own or an inherited `/Resources`,
  the page content is decoded with `Content::decode`.  A `Do` naming a Form
  XObject is replaced by `q`, the form's `/Matrix` as `cm` when it is a
  6-element array, the form's decoded operations, and `Q`.  Nested text forms
  are inlined the same way, up to depth 8; an object id already on the current
  path is not followed again, so cycles terminate.
- The total decoded bytes inlined per page are bounded by
  `MAX_PAGE_DECOMPRESSED_BYTES` (16 MiB).  A form that would exceed the budget,
  fails to decode, has no resources, or has a `Tf` whose font cannot be
  resolved aborts the inline of that form and leaves the original `Do` in
  place.
- The form's `/Font` entries are copied into a fresh page `/Resources`
  dictionary under collision-free names (`Pdf1cF1`, ...); every `Tf` in the
  inlined content is rewritten to the fresh name.  The page's own resource
  dictionary is cloned, so a shared indirect resource dictionary is never
  mutated.
- A font is only copied when lopdf can actually decode it: a `Type0` font
  whose encoding falls back to StandardEncoding (no `/ToUnicode`, or a
  `/ToUnicode` that lopdf cannot parse) is not inlined, and a
  `SimpleEncoding` (an unimplemented predefined CMap) aborts the form.  This
  keeps the old "no text" result instead of turning it into Latin garbage.
- A page whose content stream is not changed is not re-encoded.  Only pages
  that actually inline a text form get a new content stream.

### A2: `/Encoding` `/Differences` with `uniXXXX` glyph names

- Only `Type1` and `TrueType` fonts whose `/Encoding` is a dictionary with
  `/Differences` containing at least one `uniXXXX` or `uXXXX`/`uXXXXXX` name
  are touched.
- If the font already has a usable `/ToUnicode` stream, the shadowing
  `/Encoding` is removed and the existing `/ToUnicode` is kept.
- Otherwise a `/ToUnicode` CMap is synthesised.  For every byte code 0-255 it
  uses the encoding lopdf would have built from the same `/Differences` with
  the `uniXXXX` names removed (so Adobe glyph names and the base encoding keep
  exactly their old meaning), and it overrides the codes named by `uniXXXX`
  with the Unicode scalar of the glyph name.  The CMap is added as a new
  stream and the font's `/Encoding` is removed.

## 3. Version identity

- New `PDF_TEXT_EXTRACTION_VERSION = "pdf-text-extraction/v1"` in
  `src/parsing/pdf.rs`, re-exported from `src/parsing/mod.rs`.
- Default-backend PDF parsed-cache key: `src/infrastructure/cache.rs` appends
  the version to the normalization namespace for PDFs **without** a
  `pdf_namespace` (the layout backend keeps its own namespace and is
  unaffected).
- Open-result fingerprint: `src/runtime/mod.rs` adds `|pdf-text=<version>` to
  `ingest_fingerprint`, so a stored open result cannot survive a change in the
  default PDF text.
- `NORMALIZATION_VERSION` is **not** bumped: it is shared by every media type,
  and only PDF text changed.
- `TEXT_UNIT_STORE_VERSION` is **not** bumped: the store is keyed by
  `document_id` and checked against `normalized_document_hash`, which changes
  when the section text changes (`section_text_is_stored` in
  `src/infrastructure/sqlite.rs`), so a changed PDF cannot reuse the old
  section text.
- No existing version-string assertion changes; the version string tests in
  `tests/normalized_text_range.rs`, `tests/raft_named_section_evidence.rs`,
  `tests/section_text_content_store.rs` and `tests/named_section_boundary_mcp.rs`
  still pin the unchanged normalization version.

## 4. Tests

`src/parsing/pdf.rs` unit tests build small lopdf documents and cover: a form
with its own font and `/Matrix`; two forms (order follows the content stream);
nested forms; a cyclic form; an image-only form; a `Type1` `/Differences`
`uniXXXX` font without `/ToUnicode`; the same with `/ToUnicode`; a `Type0`
form font with `/ToUnicode`; Adobe glyph names keeping their old decoding; and
a regression over `tests/fixtures/scanned_pdf/pdf/F01-F14` (plus
`F04-form.pdf`) asserting old and new page texts and chunks are identical.
