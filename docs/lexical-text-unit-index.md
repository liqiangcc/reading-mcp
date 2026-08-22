# Lexical TextUnit Index Contract

> Status: Implemented P1 lexical-precision foundation
>
> Branch: `feat/lexical-text-unit-index`
>
> Related: `docs/search-locator-contract.md`, `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0004-use-case-first-tool-contracts.md`

## 1. Goal

Upgrade lexical retrieval from Section-only canonical handoff to truthful Section / Paragraph / Sentence candidates without changing source identity or adding an MCP Tool.

```text
canonical Document / Section
        ↓
Paragraph + Sentence TextUnits
        ↓
versioned lexical projection
        ↓
SearchHit(candidate_kind + TextLocator)
        ├→ read_document
        └→ get_context
```

Search remains derived retrieval state. Document/Section remain source truth.

## 2. Candidate identity

The accepted candidate kinds are now implemented:

```text
section
paragraph
sentence
```

### Section candidate

Every structural Section contributes one title candidate, including empty-body Sections.

```text
candidate_kind = section
text_locator   = Section TextLocator
```

A title-only hit never receives fabricated Paragraph/Sentence identity.

### Paragraph candidate

Every canonical Paragraph TextUnit contributes one lexical candidate with its exact Paragraph TextLocator.

### Sentence candidate

Every deterministic eligible Sentence TextUnit contributes one lexical candidate with its exact Sentence TextLocator.

Recognized non-prose Paragraphs do not generate fake Sentence candidates. They remain searchable as Paragraph candidates.

## 3. Segmentation and tokenizer versions are independent

```text
normalized_document_hash
+ text-segmentation/v1
→ TextUnit identity

lexical-tokenizer/v1
→ lexical projection / query matching
```

Changing `lexical-tokenizer/v1` requires lexical-index rebuild but must not renumber Paragraph/Sentence TextUnits or change their TextLocator identity.

The current SearchIndex contract exposes its tokenizer version to the application layer. Search hits returned by an adapter must declare the same tokenizer version or fail as index inconsistency.

## 4. `lexical-tokenizer/v1`

The tokenizer is deterministic and non-LLM.

### Latin / technical identifiers

For technical identifiers it preserves a normalized full token when useful and also emits alphanumeric/underscore components.

Examples:

```text
read-cursor/v2
→ read-cursor/v2, read, cursor, v2

std::sync::Arc
→ std::sync::arc, std, sync, arc

x86_64
→ x86_64
```

### CJK / mixed technical text

Han, Hiragana, Katakana and Hangul runs emit deterministic character unigrams and adjacent bigrams.

Example:

```text
虚拟内存机制
→ 虚, 拟, 内, 存, 机, 制,
   虚拟, 拟内, 内存, 存机, 机制
```

This allows bounded substring-style lexical queries without depending on whitespace segmentation.

Mixed text such as `JVM内存` independently emits the Latin component and CJK unigrams/bigrams.

## 5. Shared candidate builder

InMemory and SQLite indexes consume the same candidate builder and tokenizer policy. They must not maintain independent Paragraph/Sentence splitting rules.

The builder derives candidates only from:

```text
persisted canonical Document
+ deterministic Paragraph/Sentence TextUnits
+ lexical-tokenizer/v1
```

Legacy search-unit boundaries are not used as canonical identity.

## 6. SQLite lexical index v2

Current precise persistent search uses:

```text
lexical-search-index/v2
```

The FTS rows persist:

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

Only encoded lexical terms participate in FTS tokenization. Each logical tokenizer token is encoded before insertion/query so SQLite FTS cannot reinterpret technical punctuation or CJK boundaries.

The canonical TextLocator stored in the derived row is still validated against the current Document through the shared locator resolver before being returned to an Agent.

## 7. Index migration and rebuild

SQLite metadata records:

```text
lexical_search_index_version = lexical-search-index/v2
lexical_tokenizer_version    = lexical-tokenizer/v1
```

If either version is incompatible, only rebuildable lexical v2 state is discarded. DocumentRepository and TextUnit source facts are untouched.

If a persisted Document exists but its lexical derived rows are absent, `SearchDocumentUseCase` may rebuild the lexical index from that canonical Document and retry the query. This rebuild:

- does not retrieve the source;
- does not reparse bytes;
- does not change Document identity;
- does not fuzzy-repair stale locators.

The historical SQLite search adapter remains available only through a hidden compatibility alias; the runtime `SqliteSearchIndex` name points to lexical v2.

## 8. Search result validation

`SearchDocumentUseCase` validates every precise hit before returning it:

```text
source == canonical Document.source
tokenizer_version == runtime tokenizer version
candidate_kind matches resolved TextLocator kind
section_id == locator.owner_section_id
shared TextLocator resolver accepts current identity/range
```

Any inconsistency fails as index corruption/staleness; SearchIndex never becomes source truth.

## 9. Legacy response compatibility

The existing `search_document(document_id, query, limit)` request is unchanged.

Existing SearchHit preview fields remain:

```text
section_id
title
source
snippet
score
location
```

Precise fields continue to be:

```text
candidate_kind
text_locator
```

For Paragraph/Sentence candidates, legacy `location` keeps a `search-unit` provenance marker for compatibility. Canonical source identity is always `text_locator`, never that marker.

## 10. Acceptance evidence

Tests cover:

- Section-title candidate preservation;
- canonical Paragraph candidate identity;
- canonical Sentence candidate identity;
- CJK substring retrieval;
- mixed technical identifier retrieval;
- non-prose Paragraph search without fake Sentence candidates;
- SQLite precise locator persistence and reopen;
- missing derived-index rebuild from persisted canonical Document;
- direct SearchHit Sentence locator → exact read;
- direct SearchHit Sentence locator → Sentence neighbor context;
- existing legacy SearchIndex projection remains available;
- telemetry records query length, not query content.

Release gate:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 11. Non-goals

This increment does not implement:

```text
vector / semantic retrieval
LLM tokenization
ranking tuning based on speculative heuristics
anchor-based get_text_units before/after(locator)
Sentence persistence as source truth
EPUB parser/navigation changes
```

Future ranking changes remain evidence-driven and must not alter TextLocator identity.
