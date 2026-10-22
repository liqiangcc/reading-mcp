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
