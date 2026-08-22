# Search Locator Handoff Contract

> Status: Implemented P1 direct handoff; precise lexical candidate follow-up implemented by `feat/lexical-text-unit-index`
>
> Branch: `feat/search-locator`
>
> Follow-up: `docs/lexical-text-unit-index.md`
>
> Related: `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0004-use-case-first-tool-contracts.md`, `docs/precise-read-locator-contract.md`, `docs/context-granularity-contract.md`

## 1. Goal

Close the direct search handoff while requiring every SearchHit to carry the strongest source identity the active SearchIndex can actually prove:

```text
search_document
    ↓
SearchHit.text_locator
   ├→ read_document(target_locator)
   └→ get_context(target_locator, relation)
```

Snippet text remains preview. It is never copied and re-searched to recover source identity.

## 2. Original handoff boundary

At the time `feat/search-locator` landed, the historical InMemory/SQLite search rows were paragraph-like retrieval units whose split/location facts were not canonical Paragraph TextUnits. They did not persist normalized range + segmentation identity.

The strongest truthful handoff was therefore:

```text
candidate_kind = section
text_locator   = canonical owning Section TextLocator
```

This prevented a legacy search-unit marker from being silently promoted into Paragraph/Sentence source identity.

## 3. Precise lexical follow-up

`feat/lexical-text-unit-index` replaced the runtime lexical projection with candidates derived from canonical TextUnits:

```text
Section title → Section TextLocator
Paragraph     → canonical Paragraph TextLocator
Sentence      → canonical Sentence TextLocator
```

Therefore the runtime may now truthfully emit all accepted candidate kinds:

```text
section | paragraph | sentence
```

A title-only Section still remains Section-level. Recognized non-prose remains searchable as Paragraph and does not gain fake Sentence identity.

The detailed tokenizer/index/rebuild contract is `docs/lexical-text-unit-index.md`.

## 4. Canonical validation boundary

`SearchIndex` is derived retrieval state and never source truth.

Current flow:

```text
SearchIndex.search_lexical(...)
    ↓
ranked candidate + canonical TextLocator
    ↓
DocumentRepository.get(document_id)
    ↓
shared TextLocator resolver
    ↓
verify candidate_kind == resolved locator kind
    ↓
SearchHit handoff
```

Validation includes:

```text
source matches canonical Document
runtime/search-hit tokenizer versions agree
owner Section agrees
TextLocator raw/normalized identity is current
Paragraph/Sentence ordinal/range/segmentation is current
candidate_kind matches the resolved locator kind
```

Any inconsistency fails as index inconsistency instead of fabricating a locator.

## 5. Shared TextLocator resolution

Read and context use the same application-level resolver that validates:

```text
document_id
raw content_hash
normalized_document_hash
owner Section
Section / CharacterRange / Paragraph / Sentence shape
Paragraph/Sentence ordinal
segmentation version
normalized range
INVALID_LOCATOR / STALE_LOCATOR rules
```

Search precise hits are validated through the same resolver before reaching an Agent.

Capability-specific consumers still choose which valid kinds they accept. Exact read accepts CharacterRange; current context does not.

## 6. MCP response

Existing SearchHit fields retain their meaning:

```text
section_id
title
source
snippet
score
location
```

Additive precise fields remain:

```text
candidate_kind: section | paragraph | sentence
text_locator: TextLocator
```

Legacy `location` is preview/provenance. For Paragraph/Sentence candidates it may retain a `search-unit` marker for compatibility, but canonical identity is always `text_locator`.

## 7. Direct handoff semantics

### Section hit

```text
read_document(target_locator)
→ exact owner Section.content
```

### Paragraph hit

```text
read_document(target_locator)
→ exact Paragraph slice
```

### Sentence hit

```text
read_document(target_locator)
→ exact Sentence slice

get_context(target_locator, neighbor(sentence, ...))
→ bounded Sentence-first context
```

No snippet search or title lookup is needed.

## 8. Backward compatibility

The request remains:

```text
search_document(document_id, query, limit)
```

Existing response fields remain present. Candidate precision may become finer as the index becomes more capable; the locator always states the exact candidate identity.

The historical SQLite search adapter is retained only through a hidden compatibility alias. The runtime `SqliteSearchIndex` points to the versioned lexical TextUnit index.

## 9. Acceptance evidence

Current tests cover:

- title-only hit stays Section-level;
- Paragraph/Sentence candidates carry canonical TextLocators;
- CJK and technical identifier lexical matching;
- non-prose Paragraph without fake Sentence candidate;
- search locator flows directly into exact read/context;
- shared resolver validates search locator identity;
- SQLite lexical state survives reopen;
- missing derived lexical state rebuilds from persisted canonical Document.

Release gate remains:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 10. Remaining non-goals

```text
vector / semantic search
LLM-derived tokenizer identity
speculative ranking tuning
anchor-based get_text_units before/after(locator)
Sentence persistence as source truth
EPUB parser/navigation restructuring
```

Ranking may evolve independently, but candidate TextLocator identity must remain canonical and fail closed when inconsistent.
