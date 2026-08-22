# Sentence Locator and Coverage Contract

> Status: Implemented block-aware Sentence locator/coverage contract
>
> Foundation branch: `feat/sentence-locator`
>
> Current identity branch: `feat/block-aware-text-unit-identity`
>
> Related: `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0005-block-aware-text-unit-identity.md`, `docs/paragraph-text-unit-index.md`, `docs/text-unit-enumeration-contract.md`

## 1. Goal

Sentence identity is deterministic, exact, Paragraph-owned and rebuildable from persisted canonical facts. Current eligibility uses persisted native block evidence where available and conservative fallback classification otherwise.

Current dependency chain：

```text
persisted Document / Section.content
+ optional valid normalized-block-model/v1
        ↓
Paragraph TextUnit (text-segmentation/v2)
        ↓
native/fallback Sentence eligibility
        ↓
deterministic punctuation segmentation
        ↓
SentenceTextUnit + exact NormalizedTextRange
        ↓
get_text_units / context / read / lexical search
```

`Document / Section` remain source truth. Paragraph/Sentence remain derived state.

## 2. Sentence ownership

Every emitted Sentence belongs to exactly one Paragraph and one owner Section.

```text
SentenceTextUnit
├── id
├── document_id
├── content_hash                  # raw provenance
├── normalized_document_hash      # v2
├── owner_section_id
├── paragraph_index               # 1-based within Section
├── sentence_index                # 1-based within Paragraph
├── parent_paragraph_id
├── source_order
├── normalized_range              # exact Section.content-relative range
├── text                          # exact normalized slice
└── segmentation_version          # text-segmentation/v2
```

Containing Paragraph identity is explicit; it is never inferred by snippet similarity.

## 3. Identity

Current versions：

```text
normalized-document-hash/v2
text-segmentation/v2
text-unit-id/v1
```

Sentence ID derivation includes：

```text
document_id
normalized_document_hash
owner_section_id
kind = sentence
paragraph_index
sentence_index
normalized_range [start,end)
segmentation_version
```

Raw `content_hash` remains provenance rather than an independent normalized identity input.

Historical v1 Sentence locators are stale after migration and are never reinterpreted under v2 even when ranges happen to match.

## 4. Eligibility under block-aware v2

Native evidence takes priority：

```text
native paragraph       → eligible
native blockquote      → coarse Paragraph only
native list_item       → coarse Paragraph only
native preformatted    → coarse Paragraph only
native table           → coarse Paragraph only
```

BlockQuote/ListItem remain coarse under flat `normalized-block-model/v1` because their persisted outer range may suppress nested leaf boundaries. This is evidence sufficiency, not a claim that quote/list text is inherently non-prose.

Fallback/no-native-evidence Paragraph classification：

```text
prose_or_unknown → eligible
code_block       → coarse Paragraph only
table            → coarse Paragraph only
```

Strong fallback code signals remain fenced blocks and fully tab/four-space-indented Paragraphs. Fallback Markdown table detection remains header + delimiter-row based.

Native evidence outranks heuristic appearance. A native `<p>` is sentence-eligible even if its punctuation resembles table/code text.

## 5. Sentence boundary algorithm

The deterministic punctuation algorithm remains Unicode-aware and non-LLM.

Recognized terminal punctuation：

```text
. ? ! 。 ？ ！
```

Terminal clusters and common closing punctuation remain attached to the preceding Sentence.

ASCII `?` / `!` require whitespace/end-of-Paragraph after the terminal cluster, protecting technical forms such as URL queries and `x!=0`.

ASCII period protection includes：

```text
3.14
README.md
foo.bar
./configure
e.g. / i.e.
Dr. / Prof. / Fig.
single-letter initials before uppercase continuation
```

CJK terminal punctuation does not require ASCII whitespace：

```text
第一句。第二句？第三句！
```

becomes three exact Sentence ranges.

Eligible trailing non-whitespace text without recognized punctuation becomes one final Sentence instead of disappearing.

## 6. Exact range semantics

Sentence coordinates：

```text
section-content-unicode-scalar/v1
zero-based
half-open [start,end)
Unicode scalar / Rust char
```

Invariant：

```text
sentence.text
==
owner_section.normalized_text_slice(sentence.normalized_range)
```

A Sentence range is Section-relative, never rendered-response-relative. No Sentence crosses a Paragraph boundary.

Whitespace between Sentences is factual separator coverage and is not attached merely to make ranges contiguous.

## 7. Coverage semantics

Per-Paragraph Sentence coverage：

```text
paragraph_chars
sentence_chars
separator_chars
coarse_only_chars
sentence_count
content_class
eligibility
```

Invariant：

```text
paragraph_chars
=
sentence_chars + separator_chars + coarse_only_chars
```

Eligible Paragraph：

```text
coarse_only_chars = 0
```

Coarse structural/non-prose Paragraph：

```text
sentence_chars    = 0
separator_chars   = 0
coarse_only_chars = paragraph_chars
sentence_count    = 0
```

Generated Sentence ranges must stay inside the containing Paragraph; coverage calculation fails rather than hiding range-accounting bugs.

## 8. Enumeration semantics

`get_text_units(requested_kind=sentence, coverage_policy=preserve_source)` returns：

- exact Sentence items for eligible Paragraphs;
- coarse Paragraph items for BlockQuote/ListItem/Preformatted/Table/fallback code-table regions;
- no fabricated Sentence identity.

Structural coarse degradation：

```text
flat_native_container_no_nested_textunit_evidence
```

`eligible_only` may omit all coarse items and therefore cannot claim all-source `source_complete/section_complete`.

Coverage distinguishes coarse structural and non-prose counts separately.

## 9. Fallible materialization

Domain exposes：

```text
try_sentence_text_units()
```

A declared invalid/corrupt block map returns an error. Enumeration, lexical indexing and shared locator resolution propagate the failure rather than silently switching to fallback or panicking.

Absent block evidence remains supported deterministic fallback.

## 10. Rebuildability and persistence

Given the same persisted canonical Document and current v2 policy：

```text
sentence_text_units(document)
==
sentence_text_units(repository_round_trip(document))
```

Sentence SQLite rows are still not required for correctness.

TextUnitCursor binds normalized identity + segmentation version. Continuation after DocumentRepository reopen reconstructs the same Sentence stream without Sentence persistence.

Sentence persistence remains：

```text
optional performance optimization
≠ source truth
≠ correctness dependency
```

## 11. Search/read/context handoff

Current precise flows：

```text
eligible Sentence
→ TextLocator
   ├→ read_document exact target
   ├→ get_context
   └→ lexical-search-index/v3 candidate
```

Coarse structural/non-prose Paragraphs remain Paragraph search candidates only.

Old `text-segmentation/v1` Paragraph/Sentence locators fail `STALE_LOCATOR`; old TextUnitCursor state fails `STALE_CURSOR`. No fuzzy rebase.

## 12. Acceptance evidence

Tests cover：

- exact Sentence range/text equality;
- Paragraph ownership and 1-based Sentence ordinals;
- deterministic Sentence source order;
- English/CJK punctuation;
- abbreviation/decimal/path/file/API protections;
- URL-query/operator protections;
- trailing unterminated prose fallback;
- native Paragraph exact Sentence eligibility;
- native BlockQuote/ListItem zero fabricated Sentences;
- native Preformatted/Table zero fabricated Sentences;
- fallback fenced/indented code and Markdown table coarse behavior;
- source-preserving coarse enumeration;
- eligible-only no false source-completion claim;
- factual coverage invariant;
- deterministic IDs under hash-v2/segmentation-v2;
- old v1 locator/cursor stale migration;
- repository reopen continuation without Sentence rows;
- direct search/read/context handoff.

CI #876 passed the implementation head before final docs-only synchronization：

```text
Format  success
Clippy  success
Test    success
```

## 13. Current non-goals

```text
Sentence SQLite persistence
nested/leaf Sentence identity inside flat BlockQuote/ListItem evidence
anchor-based TextUnit before/after start
SVG/fixed-layout precise blocks
fuzzy locator rebasing
```
