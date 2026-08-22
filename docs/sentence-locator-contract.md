# Sentence Locator and Coverage Contract

> Status: Implemented P1 locator foundation
>
> Branch: `feat/sentence-locator`
>
> Related: `docs/adr/0002-text-index-locator-identity.md`, `docs/paragraph-text-unit-index.md`, `docs/tool-contract-use-case-design.md`

## 1. Goal

This increment establishes deterministic Sentence identity and Paragraph ownership before any MCP Sentence enumeration Tool is exposed.

The dependency chain is:

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
future get_text_units / context / search handoff
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

The ASCII-period policy is deliberately conservative. It avoids boundaries for strong technical/non-terminal patterns including:

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

Sentence ranges use the already-implemented canonical coordinate space:

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

Code/table Paragraphs therefore receive zero fabricated Sentence children.

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
sentence_chars  = 0
separator_chars = 0
coarse_only_chars = paragraph_chars
sentence_count  = 0
```

This makes future source-preserving Sentence-first enumeration possible without pretending code/table content is a Sentence. The future enumeration layer can return the containing Paragraph as a coarse reading item.

## 8. Rebuildability

Given the same persisted canonical Document and `text-segmentation/v1`:

```text
sentence_text_units(document)
==
sentence_text_units(repository_round_trip(document))
```

Changing normalized text/structure changes `normalized_document_hash` and therefore Sentence identity. A stale locator must fail closed in later wire contracts; this increment does not implement fuzzy rebasing.

## 9. Persistence boundary

This increment intentionally does **not** migrate the existing Paragraph TextUnitIndex schema.

Current persisted index remains:

```text
TextUnitIndex v1 = Paragraph units
```

Sentence locator state is rebuilt from canonical Document + deterministic Paragraph units. This matches the implementation-order requirement for `feat/sentence-locator`: prove deterministic Sentence identity and coverage before deciding the pagination/enumeration storage contract.

The next `get_text_units` increment must decide, from its pagination and lookup use cases, whether Sentence rows need persistence or can be deterministically materialized through a dedicated enumeration index. It must not silently insert Sentence rows into the current Paragraph-only schema, whose uniqueness contract has no Sentence ordinal.

## 10. MCP surface

No MCP Tool or request/response contract is added in this increment.

Current runtime remains six Tools:

```text
list_documents
open_document
get_document_structure
search_document
get_context
read_document
```

`get_text_units` remains accepted design, not implemented runtime functionality.

## 11. Acceptance evidence

Tests cover:

- exact Sentence range/text equality;
- Paragraph ownership and 1-based Sentence ordinals;
- deterministic document Sentence source order;
- English and CJK terminal punctuation;
- abbreviations, decimals, paths, file/member/API punctuation;
- trailing unterminated prose fallback;
- fenced and indented code as coarse-only Paragraphs;
- Markdown table as a coarse-only Paragraph;
- factual coverage invariant;
- deterministic Sentence IDs;
- raw provenance changes not redefining otherwise identical normalized Sentence identity;
- normalized text changes invalidating Sentence IDs;
- canonical Document SQLite round-trip rebuild equality.

The repository release gate remains:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 12. Explicit non-goals

This increment does not implement:

```text
get_text_units
TextUnitCursor
Sentence persistence migration
TextLocator MCP wire DTOs
Sentence read mode
Paragraph/Sentence context
SearchHit → Sentence TextLocator
Paragraph/Sentence FTS
EPUB parser/block-type restructuring
```

The next dependency step is `feat/text-unit-enumeration-contract`.
