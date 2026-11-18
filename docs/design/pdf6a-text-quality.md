# Why PDF sentences come out too long, too short, or full of page furniture (PDF-6a)

Measurement only. No product code changed, nothing compiled or run on the measuring host: the
numbers come from GitHub Actions, the corpus is the committed PDF-5a list, and the PDFs are
never committed.

- Corpus: `scripts/pdf-sweep-ids.txt`, the same 121 CC-BY / CC-BY-SA / CC0 arXiv papers PDF-5a
  measured, byte-identical (`sha256 254624fedbdb7f7675e1e8ada8efa17f5252cdf2c474d7f286bd7f43c7b73342`,
  121 entries, 4,842 pages).
- Harness: `scripts/pdf-quality.py` (new, run mode + `--aggregate`) and
  `.github/workflows/pdf-quality.yml` (new, `workflow_dispatch` only). It reuses the PDF-5a
  sweep's downloader, licence re-check, capped-server helper and PyMuPDF reference text
  (`scripts/pdf-sweep.py`, `scripts/pdf-probe.py`, `scripts/pdf-corpus.py` are imported and
  unchanged); PDF-5a's own output is untouched.
- Server settings, identical to PDF-5a: release binary of the run's head, `READING_MCP_TOOLS=v1`,
  OCR off (`READING_MCP_OCR_ENABLED=false`), **default** PDF backend
  (`READING_MCP_PDF_LAYOUT_PYTHON` unset), `READING_MCP_MAX_DOCUMENT_BYTES=134217728`, every
  open inside `sudo systemd-run --scope -p MemoryMax=600M -p MemorySwapMax=0`.
- Reading: `open_document`, `get_document_structure`, then every section's
  `get_text_units` (`requested_kind=sentence`, `coverage_policy=preserve_source`) in 256-item
  batches, keeping each item's `text`, `effective_kind`, `content_class_detail` and
  `degradation`. That is the reader's own text, unit by unit.
- Reference text: PyMuPDF `page.get_text()` for every page, independent of the backend, plus
  per-page lines and the font inventory.
- Per-unit records (every long unit and every short unit, with its cause, its page, its
  evidence and its what-if split) go into the run artifact as JSONL; git holds only this note
  and the code.

## Runs

| What | Run | Head | Outcome |
| --- | --- | --- | --- |
| quality probe | _pending_ | _pending_ | _pending_ |
| plain `PDF wide sweep` on the same head | _pending_ | _pending_ | _pending_ |

## The separators the code puts between chunks and between pages

Read from `main` at `4993b1f`. "Chunk" is one `Ok` item of lopdf 0.44
`extract_text_chunks_with_limit`: a run of `Tj`/`TJ` text closed at a font change (`Tf`), with a
newline appended by `'`, `"`, `T*` and `ET` (`docs/design/pdf1c-extraction.md` §1).

| Where the text comes from | Code | Between chunks in one page | Between pages |
| --- | --- | --- | --- |
| `PageText.text` (the raw page text) | `extract_page_texts_raw` (`src/parsing/pdf.rs:306`), `normalize_pdf_text` (`:354`) | **nothing** — `text.push_str(&fragment)` per chunk, then one `trim()` | n/a |
| `PageText.chunks` (the fragments) | same function, `:316-330` | each chunk is trimmed and kept separately | n/a |
| inferred-heading sections | `build_inferred_heading_sections` (`:653`) → `PageFragments::push_line` (`:75`) → `content_and_bindings_from_fragments` (`:764`) | **`"\n"`** (one newline) | **`"\n\n"`** (blank line) |
| page-fallback sections | `build_page_sections` (`:803`) | **nothing** — the section content *is* `PageText.text` | n/a (one section per page) |
| native-TOC sections | `source_text_and_bindings_for_page_range` (`:956`) | **nothing** — it reads `PageText.text` | **`"\n\n"`** (blank line) |

Two consequences that matter for the measurements below:

- A paragraph ends only at a blank line (`paragraph_ranges`, `src/domain/segmentation.rs:727`), so
  **only a page boundary can end a paragraph** in a PDF: the heading path ends a paragraph per
  page (the `"\n\n"`), the page-fallback path has no paragraph break inside a page at all, and
  the TOC path has one per page. A page whose text has no blank line is therefore one paragraph,
  however many lines it really has.
- The task description says the inferred-heading path joins the trimmed chunks with `"\n\n"`.
  The code joins the chunks **of one page** with a single `"\n"` and joins **pages** with
  `"\n\n"` (`PageFragments::push_line` at `:75-86` vs `content_and_bindings_from_fragments` at
  `:764-801`). Only the page boundary is a paragraph boundary.

Sentence boundaries (`src/domain/segmentation.rs`):

- `.` `!` `?` end a sentence only when the terminal cluster is followed by whitespace or the end
  of the paragraph (`ascii_terminal_is_terminal`, `:814`) — a mark followed directly by a letter
  never splits.
- `.` between two identifier characters, between two digits, before `/` or `\`, after a protected
  abbreviation (`e.g`, `i.e`, `etc`, `fig`, `no`, `vol`-free list at `:893`) or after a
  single-letter token before an uppercase word never splits (`ascii_period_is_terminal`, `:819`).
- A paragraph that `looks_like_unreliable_fallback_text` (`:1016`) — 3 or more missing-space glue
  events (`:1024`, e.g. `.Next`, `,Next`, `)provided`), a pairwise-doubled token, or a
  bibliographic masthead run — becomes `CoarseParagraphOnly`, and `get_text_units` then returns
  **the whole paragraph as one item** with
  `degradation=requested_sentence_but_paragraph_is_coarse_only`
  (`src/application/text_unit_stream.rs:331-384`). PDF paragraphs are `ProseOrUnknown` (no
  parser-native block map), so this guard applies to them.

## 1. Whitespace (H1)

_pending measurement_

## 2. Long units (> 800 characters)

_pending measurement_

## 3. Short units (< 5 characters)

_pending measurement_

## 4. Running headers and footers (H5)

_pending measurement_

## 5. Silent loss and bad characters (H6)

_pending measurement_

## Verdicts on the hypotheses

_pending measurement_

## Ranked fix candidates

_pending measurement_

## What did not run, and why

_pending measurement_
