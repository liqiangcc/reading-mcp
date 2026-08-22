# Search Locator Handoff Contract

> Status: Implemented P1 search-handoff foundation
>
> Branch: `feat/search-locator`
>
> Related: `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0004-use-case-first-tool-contracts.md`, `docs/precise-read-locator-contract.md`, `docs/context-granularity-contract.md`

## 1. Goal

Close the direct search handoff without pretending the current lexical index has finer source identity than it actually stores:

```text
search_document
    ↓
SearchHit.text_locator
   ├→ read_document(target_locator)
   └→ get_context(target_locator, relation)
```

Snippet text remains preview. It is never copied and re-searched to recover source identity.

## 2. Current SearchIndex evidence

The current InMemory and SQLite search implementations index paragraph-like retrieval units for ranking/snippets, but those rows do not use the canonical Paragraph TextUnit contract.

Current retrieval rows have facts such as:

```text
document_id
section_id
title
snippet/body
legacy Location / search-unit marker
score
```

They do **not** persist enough evidence for a canonical Paragraph/Sentence locator:

```text
normalized_document_hash-bound Paragraph identity
canonical normalized_range
text-segmentation/v1
canonical paragraph_index proven against Paragraph TextUnit
sentence_index
```

In addition, the InMemory and SQLite retrieval split rules historically differ from the canonical Paragraph segmentation rules. Therefore a paragraph-like search row is not proof of a Paragraph TextUnit.

## 3. Strongest truthful locator policy

Current SearchHit enrichment is deliberately:

```text
candidate_kind = section
text_locator   = canonical owning Section TextLocator
```

for every current SearchIndex hit.

This is true even when the ranking/snippet row represents a smaller paragraph-like retrieval unit.

Legacy preview facts are preserved independently:

```text
section_id
title
source
snippet
score
location
```

The legacy `location` may contain a narrower search-unit marker. It is retrieval provenance/preview metadata and is not silently promoted into canonical Paragraph identity.

## 4. Title-only hits

A title-only Section candidate remains:

```text
candidate_kind = section
```

with a Section TextLocator. No Paragraph or Sentence ordinal/range is fabricated merely to unify the response schema.

## 5. Canonical enrichment boundary

`SearchIndex` remains derived retrieval state and does not become a source-of-truth store.

`SearchDocumentUseCase` performs two stages:

```text
SearchIndex.search(...)
    ↓
ranked bounded retrieval hits
    ↓
DocumentRepository.get(document_id)
    ↓
validate owning Section/source
    ↓
construct canonical Section TextLocator
```

If an index hit references a missing canonical Section or inconsistent source, the request fails as an index inconsistency rather than inventing a locator.

## 6. Shared TextLocator resolution

Before SearchHit became a third locator participant, read/context duplicated locator-validation logic. This increment consolidates resolution into one application-level resolver used by exact read and structured context.

It owns:

```text
document_id validation
raw content_hash freshness
normalized_document_hash freshness
owner Section existence
Section shape
CharacterRange validation
Paragraph ordinal/range/segmentation validation
Sentence ordinal/range/segmentation validation
INVALID_LOCATOR / STALE_LOCATOR rules
```

Capability-specific consumers still decide which valid locator kinds they accept. For example, exact read accepts CharacterRange while current context deliberately rejects CharacterRange as an unsupported context anchor instead of calling it malformed.

## 7. MCP response evolution

Existing SearchHit fields retain their meaning. Additive fields are:

```text
candidate_kind: section | paragraph | sentence
text_locator: TextLocator
```

Current implementation emits only `section` candidate kind. The enum reserves the accepted contract surface for a later lexical TextUnit index, but the runtime must never emit `paragraph` or `sentence` before the index can prove those identities.

## 8. Direct handoff

A returned current Section locator can immediately be used as:

```text
read_document(document_id, target_locator)
→ exact owning Section.content
```

or:

```text
get_context(document_id, target_locator, relation)
→ bounded context/structure around owning Section
```

No snippet search or title lookup is needed.

## 9. Backward compatibility

The request remains:

```text
search_document(document_id, query, limit)
```

Existing response fields remain present. `candidate_kind` and `text_locator` are additive.

No SearchIndex SQLite schema migration is performed in this increment.

## 10. Acceptance evidence

Tests must prove:

- paragraph-like retrieval rows return only Section candidate identity today;
- title-only hits remain Section-level;
- Section TextLocator is bound to current raw/normalized canonical identity;
- legacy narrower Location remains preview/provenance and does not become canonical range identity;
- search locator flows directly into exact read;
- search locator flows directly into structured context;
- exact read and context still share one resolver for overlapping locator kinds;
- current InMemory/SQLite SearchIndex implementation/schema remains unchanged.

Release gate remains:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 11. Next dependency

After direct handoff is correct, the next independent increment is `feat/lexical-text-unit-index`.

That increment must decide and prove:

```text
canonical Section title candidates preserved
canonical Paragraph candidates
canonical Sentence candidates
CJK/mixed technical tokenizer policy
tokenizer_version independent of segmentation_version
index rebuild/migration semantics
candidate_kind precision backed by stored canonical locator facts
```

Only then may `search_document` truthfully emit `candidate_kind=paragraph` or `candidate_kind=sentence`.