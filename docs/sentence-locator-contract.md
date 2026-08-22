# Sentence Locator and Coverage Contract

> Status: Implemented P1 locator foundation; consumed by implemented `get_text_units`
>
> Branch: `feat/sentence-locator`
>
> Follow-up implementation: `feat/text-unit-enumeration-contract`
>
> Related: `docs/adr/0002-text-index-locator-identity.md`, `docs/paragraph-text-unit-index.md`, `docs/text-unit-enumeration-contract.md`, `docs/tool-contract-use-case-design.md`

## 1. Goal

This increment established deterministic Sentence identity and Paragraph ownership before MCP Sentence enumeration was exposed. The subsequent enumeration increment now consumes this foundation through `get_text_units` without changing its identity rules.

The implemented dependency chain is:

```text
persisted canonical Document / Section.content
        ↓
Paragraph TextUnit (text-segmentation/v1)
        ↓
Sentence eligibility / content classification
        ↓
deterministic Sentence segmentation
        ↓
SentenceTextUnit + exact NormalizedTextRange
        ↓
get_text_units + TextUnitCursor
        ↓
future context / search / precise-read locator input
```

`Document / Section` remain source truth. Paragraph and Sentence units remain rebuildable derived state.

## 2. Sentence ownership

Every emitted Sentence belongs to exactly one Paragraph and one owner Section.

Implemented Sentence fields:

```text
SentenceTextUnit
├── id: TextUnitId
├── document_id
├── content_hash                  # raw-source provenance
├── normalized_document_hash
├── owner_section_id
├── paragraph_index               # 1-based within Section
├── sentence_index                # 1-based within Paragraph
├── parent_paragraph_id
├── source_order                  # deterministic Sentence-stream order
├── normalized_range              # exact Section.content-relative range
├── text                          # exact normalized slice
└── segmentation_version
```

The containing Paragraph is not inferred from snippet similarity. `parent_paragraph_id + paragraph_index + owner_section_id` are deterministic handoff facts.

## 3. Identity

Sentence IDs use the existing `text-unit-id/v1` identity namespace and are derived from:

```text
document_id
+ normalized_document_hash
+ owner_section_id
+ kind = sentence
+ paragraph_index
+ sentence_index
+ normalized_range [start, end)
+ text-segmentation/v1
```

Raw `content_hash` remains provenance rather than an additional standalone normalized-text identity input.

Paragraph IDs are unchanged by this increment.

## 4. Sentence segmentation v1

Sentence segmentation is deterministic, non-LLM, Unicode-aware, and works only from exact persisted Paragraph text.

Recognized terminal punctuation includes:

```text
. ? ! 。 ？ ！
```

Terminal clusters and common closing punctuation are kept with the preceding Sentence.

The ASCII punctuation policy is deliberately conservative. An ASCII `?` or `!` is accepted as a Sentence terminal only when its terminal/closer cluster is followed by whitespace or the end of the Paragraph. This prevents false splits in technical forms such as URL query strings and operators like `x!=0`.

ASCII period handling adds further protection for strong technical/non-terminal patterns including:

```text
3.14                 # decimal/version-like numeric punctuation
README.md             # identifier/file member punctuation
foo.bar               # API/member/domain-like punctuation
./configure            # path/command punctuation
e.g. / i.e.           # protected common abbreviations
Dr. / Prof. / Fig.    # protected common abbreviations
single-letter initials before an uppercase continuation
```

CJK terminal punctuation does not require ASCII whitespace, so:

```text
第一句。第二句？第三句！
```

forms three deterministic Sentence ranges.

If eligible prose has no recognized terminal punctuation, the remaining non-whitespace text becomes one Sentence rather than disappearing from coverage.

## 5. Exact range semantics

Sentence ranges use the canonical coordinate space:

```text
section-content-unicode-scalar/v1
zero-based
half-open [start, end)
Unicode scalar / Rust char
```

For every Sentence:

```text
sentence.text
==
owner_section.normalized_text_slice(sentence.normalized_range)
```

A Sentence range is Section-relative, not Paragraph-relative and never rendered-response-relative.

Whitespace between Sentence ranges is factual separator coverage. It is not silently attached to a neighboring Sentence merely to make ranges contiguous.

## 6. Non-prose eligibility

The current canonical Document does not yet persist parser-native block type metadata. Sentence v1 therefore uses a conservative persisted-text classifier and does not claim native block provenance.

Current content classes:

```text
prose_or_unknown
code_block
table
```

Strong persisted-text signals classified as `code_block` include:

- fenced blocks beginning/ending with triple backticks or tildes;
- Paragraphs whose non-empty lines are all tab-indented or four-space-indented.

A Markdown-style table is recognized only when the persisted text has a header row plus a pipe-separated delimiter row whose cells are dash/colon delimiters.

Anything without a strong signal is `prose_or_unknown`. This is deliberately different from asserting that the parser proved the content is prose.

Eligibility:

```text
prose_or_unknown → eligible for deterministic fallback Sentence segmentation
code_block       → coarse_paragraph_only
table            → coarse_paragraph_only
```

Code/table Paragraphs receive zero fabricated Sentence children.

## 7. Coverage semantics

Coverage is reported per Paragraph:

```text
paragraph_chars
sentence_chars
separator_chars
coarse_only_chars
sentence_count
content_class
eligibility
```

Required invariant:

```text
paragraph_chars
=
sentence_chars + separator_chars + coarse_only_chars
```

For eligible Paragraphs:

```text
coarse_only_chars = 0
```

For recognized non-prose Paragraphs:

```text
sentence_chars    = 0
separator_chars   = 0
coarse_only_chars = paragraph_chars
sentence_count    = 0
```

Generated eligible ranges are required to remain inside the containing Paragraph; coverage calculation fails rather than silently hiding a range-accounting bug.

The implemented `get_text_units(requested_kind=sentence, coverage_policy=preserve_source)` uses this data to return recognized code/table Paragraphs as explicit coarse Paragraph reading items. It never fabricates Sentence identity.

`eligible_only` may omit coarse regions, but the enumeration contract therefore never claims all-source `source_complete/section_complete` under that policy.

## 8. Rebuildability

Given the same persisted canonical Document and `text-segmentation/v1`:

```text
sentence_text_units(document)
==
sentence_text_units(repository_round_trip(document))
```

Changing normalized text/structure changes `normalized_document_hash` and therefore Sentence identity. TextUnitCursor binds this normalized identity and fails closed after changes; no fuzzy rebasing is performed.

## 9. Persistence boundary

The existing persisted index remains:

```text
TextUnitIndex v1 = Paragraph units
```

The enumeration implementation deliberately chose **not** to add Sentence SQLite rows. Sentence locator state and the declared Sentence stream are rebuilt from canonical Document + deterministic Paragraph/Sentence segmentation.

This is now backed by continuation evidence:

```text
open/save canonical Document
→ get_text_units page 1
→ persist/reopen SqliteDocumentRepository
→ continue with TextUnitCursor
→ same deterministic remaining Sentence stream
```

Therefore Sentence persistence is currently:

```text
optional future performance optimization
≠ correctness dependency
≠ source truth
```

A future persistence migration requires measured performance evidence and must remain fully rebuildable.

## 10. MCP surface

The subsequent enumeration increment added the seventh Tool:

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

`get_text_units` emits Paragraph/Sentence TextLocator output, supports forward/backward source-order pagination and TextUnitCursor continuation, and carries non-prose/coverage semantics.

Current v1 begins at a Section boundary. Anchor-based `before/after(locator)` start and TextLocator input to read/context/search are later extensions.

See [TextUnit Enumeration Contract](text-unit-enumeration-contract.md).

## 11. Acceptance evidence

Sentence foundation tests cover:

- exact Sentence range/text equality;
- Paragraph ownership and 1-based Sentence ordinals;
- deterministic document Sentence source order;
- English and CJK terminal punctuation;
- abbreviations, decimals, paths, file/member/API punctuation;
- ASCII URL-query/operator `?` / `!` protection;
- trailing unterminated prose fallback;
- fenced and indented code as coarse-only Paragraphs;
- Markdown table as a coarse-only Paragraph;
- factual coverage invariant;
- deterministic Sentence IDs;
- raw provenance changes not redefining otherwise identical normalized Sentence identity;
- normalized text changes invalidating Sentence IDs;
- canonical Document SQLite round-trip rebuild equality.

Enumeration follow-up tests additionally cover:

- source-preserving coarse non-prose output;
- eligible-only no all-source completion claim;
- forward/backward no-gap/no-overlap pagination;
- TextUnitCursor stale/mismatch behavior;
- continuation after persisted Document repository reopen without Sentence rows.

Release gate remains:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 12. Current non-goals / next dependency

Still not implemented:

```text
Sentence persistence migration
anchor-based TextUnit before/after start
TextLocator input to read_document
Paragraph/Sentence context
SearchHit → Sentence TextLocator
Paragraph/Sentence FTS
EPUB parser/block-type restructuring
```

The next dependency step is `feat/context-granularity`, followed by search locator handoff. Sentence persistence remains conditional on performance evidence.
