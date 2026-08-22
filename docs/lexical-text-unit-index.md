# Lexical TextUnit Index Contract

> Status: Implemented precise lexical retrieval contract
>
> Foundation branch: `feat/lexical-text-unit-index`
>
> Current identity migration: `feat/block-aware-text-unit-identity`
>
> Related: `docs/search-locator-contract.md`, `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0005-block-aware-text-unit-identity.md`

## 1. Goal

Provide truthful Section / Paragraph / eligible-Sentence lexical candidates whose canonical handoff always uses the current TextUnit identity.

```text
canonical persisted Document
        ↓
block-aware Paragraph / eligible Sentence TextUnits
        ↓
versioned lexical projection
        ↓
SearchHit(candidate_kind + TextLocator)
        ├→ read_document
        └→ get_context
```

Search remains rebuildable derived state. Document/Section remain source truth.

## 2. Candidate identity

Implemented candidate kinds：

```text
section
paragraph
sentence
```

### Section

Every structural Section contributes a title candidate, including empty-body Sections：

```text
candidate_kind = section
text_locator   = Section TextLocator
```

A title-only hit never receives fabricated Paragraph/Sentence identity.

### Paragraph

Every current Paragraph TextUnit contributes one exact Paragraph candidate.

This includes coarse BlockQuote/ListItem/Preformatted/Table/fallback code-table regions; they remain truthfully Paragraph-addressable/searchable.

### Sentence

Only deterministic eligible Sentence TextUnits contribute Sentence candidates.

Under current flat block evidence, BlockQuote/ListItem/Preformatted/Table regions do not generate fake Sentence candidates.

## 3. Identity and tokenizer versions are independent

Current source identity：

```text
normalized-document-hash/v2
+ text-segmentation/v2
→ Paragraph / Sentence / TextLocator identity
```

Current lexical projection：

```text
lexical-tokenizer/v1
```

Tokenizer changes require lexical rebuild but must never renumber TextUnits or alter canonical locator identity.

## 4. `lexical-tokenizer/v1`

Tokenizer remains deterministic and non-LLM.

### Latin / technical identifiers

It retains useful normalized full tokens plus components：

```text
read-cursor/v2
→ read-cursor/v2, read, cursor, v2

std::sync::Arc
→ std::sync::arc, std, sync, arc

x86_64
→ x86_64
```

### CJK / mixed text

Han, Hiragana, Katakana and Hangul runs emit deterministic unigrams + adjacent bigrams：

```text
虚拟内存机制
→ 虚, 拟, 内, 存, 机, 制,
   虚拟, 拟内, 内存, 存机, 机制
```

This provides bounded lexical substring retrieval without whitespace-only assumptions.

## 5. Shared fallible candidate builder

InMemory and SQLite indexes consume one candidate builder and tokenizer policy. Neither maintains its own Paragraph/Sentence splitting logic.

Current builder inputs：

```text
persisted canonical Document
+ try_paragraph_text_units()
+ try_sentence_text_units()
+ lexical-tokenizer/v1
```

A declared invalid/corrupt block map causes lexical rebuild to fail closed with an index error. It never panics, silently switches identity, or uses legacy search-unit boundaries as canonical locators.

## 6. Current persistent lexical index

Current semantic version：

```text
lexical-search-index/v3
```

The v3 change is an **identity migration**, not a tokenizer/schema-shape feature. Existing physical SQLite tables can retain their implementation names because semantic metadata gates compatibility.

Derived rows persist：

```text
document_id
candidate_kind
owner section id
title / snippet preview
legacy Location preview
canonical TextLocator
tokenizer_version
source_order
encoded lexical terms
```

Only encoded logical lexemes participate in FTS matching so SQLite does not redefine CJK/technical token boundaries.

## 7. v2 → v3 migration

SQLite metadata now records：

```text
lexical_search_index_version = lexical-search-index/v3
lexical_tokenizer_version    = lexical-tokenizer/v1
```

Why v3 is required： persisted lexical rows contain Paragraph/Sentence TextLocators. After normalized hash/segmentation identity moved to v2, lexical v2 rows could not remain current precise state.

When old semantic v2 metadata is opened：

```text
old derived lexical rows
→ discarded
→ no canonical Document mutation
→ rebuild from persisted canonical Document when needed
```

The migration does not：

- retrieve source bytes;
- reparse the source;
- change tokenizer policy;
- fuzzy-rebase old locators;
- make SearchIndex source truth.

A dedicated migration test seeds semantic v2 state, proves old rows disappear, rebuilds current candidates, and verifies metadata becomes v3 while tokenizer remains v1.

## 8. Search result validation

`SearchDocumentUseCase` validates every precise hit against current canonical facts：

```text
source == canonical Document.source
tokenizer_version == lexical-tokenizer/v1
candidate_kind == resolved locator kind
section_id == locator.owner_section_id
shared TextLocator resolver accepts current hash/range/segmentation identity
```

Any mismatch fails as index inconsistency/staleness.

The shared resolver itself uses fallible block-aware TextUnit materialization for Paragraph/Sentence locators.

## 9. Missing derived-state rebuild

If a canonical persisted Document exists but lexical rows are missing/incompatible：

```text
DocumentRepository current Document
→ rebuild lexical-search-index/v3
→ retry search
```

No source retrieval/reparse is necessary.

Historical/custom SearchIndex adapters without the precise lexical contract remain compatible through truthful Section-level fallback.

## 10. Response compatibility

Request remains：

```text
search_document(document_id, query, limit)
```

Historical preview fields remain：

```text
section_id
title
source
snippet
score
location
```

Canonical handoff remains：

```text
candidate_kind
text_locator
```

Legacy `location/search-unit` markers are preview/provenance only; precise source identity is always `text_locator`.

## 11. Acceptance evidence

Tests cover：

- Section-title candidate preservation;
- current Paragraph candidate identity;
- current eligible Sentence identity;
- coarse structural/non-prose Paragraph search with zero fake Sentence candidate;
- CJK substring retrieval;
- mixed technical identifiers;
- SQLite precise locator persistence/reopen;
- missing derived-index rebuild from canonical persisted Document;
- semantic lexical v2 → v3 invalidation/rebuild;
- tokenizer remaining v1 through identity migration;
- invalid native block evidence fail-closed indexing;
- direct Sentence SearchHit → exact read;
- direct Sentence SearchHit → context;
- legacy adapter Section fallback;
- telemetry query-length-only behavior.

CI #876 passed the implementation head before final docs-only synchronization：

```text
Format  success
Clippy  success
Test    success
```

## 12. Non-goals

```text
vector / semantic retrieval
LLM tokenization
speculative ranking tuning
Sentence persistence as source truth
nested quote/list Sentence fabrication
fuzzy locator rebasing
new MCP Tools
```
