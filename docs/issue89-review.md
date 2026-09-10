# Issue #89 review and implementation decision

Reviewed on 2026-09-07. The user authorized completing review, development and
publishing a deployment package; production deployment remains the user's next
step. This report is a self-review, not an independent third-party review.

## Decision

Ship **0.4.0-rc.1** as an opt-in layout candidate. Do not replace the default PDF
backend, close #87/#89 as a deployed fix, or claim universal PDF fidelity.

The existing parser's failure is upstream of exact reading: PDF glyph extraction
can lose word spacing; title/authors/Abstract lack reliable owner boundaries; the
sentence splitter then protects identifier-like periods and receives merged text.
`read_document` faithfully reproduces the incorrect canonical range. Kafka's text
layer and paragraph layout survive extraction better, explaining why the same
splitter performs well there. Replacing only the sentence library cannot recover
missing spaces or distinguish missing structural boundaries.

## Alternatives and evidence

| Path | Evidence | Decision |
| --- | --- | --- |
| Existing lopdf + v8 structure | #87 Abstract boundaries have 16.7% internal recall; Kafka 100%; Raft 50% on frozen scopes | Keep default for compatibility, not the recommended path for #87 |
| Legacy PDF-to-Markdown | Some layout and punctuation problems remain | Reject as general solution |
| Layout-aware Markdown | #87/Kafka/Raft Abstracts expose 7/6/5 sentences; 1,579 full-document units exact-read consistently in the isolated prototype | Useful feasibility evidence, insufficient production representation |
| Structured layout JSON + native Rust blocks | Source identity, page bindings, typed coarse blocks and source-aware joins; existing Rust sentence/locator contract retained | Implement as optional candidate |
| Different splitter/model alone | Cannot restore missing PDF word boundaries or structural owners; no claim of measured superiority | Not the first implementation step |

[Upstream JSON output](https://pymupdf.readthedocs.io/en/latest/pymupdf4llm/index.html#as-json)
provides layout classes and bounding boxes. The integration consumes raw text
spans, not Markdown syntax. It performs local inference with versions fixed at
PyMuPDF/PyMuPDF4LLM/Layout 1.28.2.

The independent holdout was frozen before comparison: Attention Is All You Need
(15 pages) and MapReduce (13 pages), with 25 manually reviewed sentences in their
Abstracts and opening Introduction excerpts. Plain layout Markdown deleted the
real hyphen in `English-to-German`, and author-contribution notes could remain in
the Abstract owner. These failures reject direct Markdown import even when exact
reads and repeat hashes are perfect. After those failures informed changes, the
same samples are **regression samples**, no longer an untouched final holdout.

## Scope and reliability review

The worker runs in a separate Python process with isolated imports, piped PDF
bytes, bounded output, process cancellation and serialized execution. Missing or
wrong engine dependencies fail explicitly; no hidden legacy fallback is cached.
Rust validates scalar ranges and page bounds before building canonical native
blocks. Original source/hash identity is retained. Unsupported non-prose classes
remain coarse; notes are separated from the main prose stream. Source page maps
cover the canonical text, while picture/table regions remain original-PDF evidence.

A visible wrap hyphen is removed only with independent unbroken word evidence in
the document; soft hyphens can be removed as discretionary formatting. Ambiguous
printed hyphens remain. This trades polished spelling for avoiding silent source
character deletion. Flat inferred owners and conservative cross-column/page joins
are bounded implementation choices, not a complete reconstruction of the PDF's
semantic hierarchy. The optional engine does not address #88 in full.

Normalization v10 invalidates stored v9 canonical documents; clients explicitly
reopen sources and acquire new locators/cursors. The optional backend has a
separate parsed-cache namespace. The external OCR adapter only projects a
conservative subset of an existing invisible text layer; it does not generate OCR
or claim transcription/reading-order quality. Segmentation v3 permits sentence-final
`etc.` before a capitalized next sentence, while preserving interior uses and other
protected abbreviations. Exact-read contracts remain.
A sentence's exact read must equal its enumerated text and preserve its resolved
target locator. The returned locator is a character-range locator by contract;
it need not retain sentence-only ordinal fields.

The visual review also caught a pre-existing native-renderer failure: #87 produced
an entirely white PNG despite a correct page binding. Its original 2560×3300
page image exceeds the default 4-million-pixel decoded-image budget; deployment
instructions explicitly select a 16-million-pixel budget for this sample. When the optional backend
is enabled, original page rendering now uses pinned PyMuPDF through the existing
private-file worker protocol. Page, pixel, stream, image-byte and process limits
remain enforced; it never reconstructs a page from normalized text. The default
renderer remains unchanged.

## Acceptance interpretation

The proposed gates remain boundary precision ≥99%, recall ≥98%, correctly
individually returned prose scalar coverage ≥95%, metadata contamination zero,
#87 Abstract 7/7, and exact/repeat consistency 100%. Compute boundary scores only
inside frozen annotated scopes. Report coarse coverage and lexical fidelity
separately; do not hide missed prose by downgrading everything to a paragraph.
Typography-normalized matching removes whitespace/discretionary wrap differences;
it does not establish faithful spelling or glyph-perfect extraction.

Passing these small scopes cannot establish the gates across all PDFs. A new
untouched, broader independently annotated set is required before default-backend
promotion. This is why the deployment artifact is a candidate release.

## Reproduction and source rights

The repository includes self-authored JSON fixtures, Rust source-map/cache/process
checks and a reusable MCP smoke probe. Real-paper PDFs and long excerpts are not
redistributed in the package. Local research evidence is kept under
`/root/reading-mcp-research/issue89-{phase2,holdout,development}` in the development
environment; release verification results below summarize it.

| Sample | Source | SHA-256 |
| --- | --- | --- |
| #87, The World and the Machine | [Issue #87 PDF](https://users.ece.utexas.edu/~perry/education/SE-Intro/jackson-icse95.pdf) | `48c6ab4b1e17360ab32be09d0042b43630d3afcd8da7d453c17042a959ab6061` |
| Kafka | Existing research input | `4abdeba2503eb20a5d7ed84aa8e7680bcbe3088541712626315deae0b07c2821` |
| Raft | Existing research input, 16 physical pages including cover | `e6345fcba31cbc747ab41755aa62654859c4403dbb687da0021079f78181a7b5` |
| Attention | [arXiv 1706.03762v7](https://arxiv.org/pdf/1706.03762v7) | `bdfaa68d8984f0dc02beaca527b76f207d99b666d31d1da728ee0728182df697` |
| MapReduce | [Google Research PDF](https://storage.googleapis.com/gweb-research2023-media/pubtools/4449.pdf) | `ae84cc48ff0005c5a79e095039e54e91dce5c116f65746e1593e819d21830ce9` |

Public download access does not imply general redistribution rights. Attention's
paper includes a limited figure/table reuse notice; no blanket reuse permission
is assumed for the other papers. EPUB regression fixtures are self-authored.

## Release verification

Results are recorded after the final code and package checks; candidate quality
limitations above remain applicable regardless of test counts.

The development regression run enumerated and exact-read **2,104 units** across
five original PDFs and the self-authored EPUB, with zero exact-read mismatches.
The 43 annotated sentences have 36 internal gold boundaries; boundary precision,
recall and typography-normalized individually returned scalar coverage are 100%
on these scopes. None of their Abstract owners exposes extra metadata as sentences.
These are scoped regression results, not full-corpus semantic accuracy estimates.

| Scope | Individually returned gold sentences |
| --- | --- |
| #87 Abstract | 7/7 |
| Kafka Abstract | 6/6 |
| Raft Abstract | 5/5 |
| Attention Abstract + opening excerpt | 7/7 + 2/2 |
| MapReduce Abstract + opening excerpt | 8/8 + 8/8 |

Conservative hyphen preservation differs from polished dehyphenated reference
spelling; the MapReduce frozen reference also collapses a printed compound to
`userspecified`. Such lexical differences are not hidden by boundary scores, and
frozen annotations were not rewritten to improve results. Before v3, sentence-final
`etc.` merged two MapReduce Introduction sentences (6/8 correct sentences and
85.7% internal boundary recall); v3 fixes that case while testing interior uses.

The EPUB control retains 12 sentences and 3 coarse non-prose units. Reopening the
same EPUB under the new release preserves raw identity and unchanged normalized
text hash, while actual v0.3.0/v2 sentence locators and cursors return
`STALE_LOCATOR` / `STALE_CURSOR`. Source-content changes, exact reads and restart
continuations were also exercised using actual server-issued targets.

Nine Python projection/render checks pass with the pinned environment, including
visible rendered text and dimension/byte/stream failures. Missing dependencies,
page limit, image-only input and parser timeout fail explicitly; the timed-out
worker was confirmed reaped. Original-page MCP views retain source hash and target
locator for #87 page 1 and Raft physical page 2. #87's new preview was visually
reviewed after explicitly raising its decoded-image pixel budget to 16 million.

A 30-second parse timeout occurred once under concurrent compilation on the
2-GB development host. Stressed validation allows 120 seconds; this is not a
claim that every PDF fits the default 30-second budget. Final artifact smoke and
repeat results, full Rust/Clippy/format gates and immutable package identities
are recorded in the GitHub release verification assets and CI run. Publication
requires those gates to finish successfully.
