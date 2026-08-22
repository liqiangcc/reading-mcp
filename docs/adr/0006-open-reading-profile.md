# ADR 0006: Open Reading Profile at the Workflow Decision Point

- Status: Proposed
- Date: 2026-08-23
- Design branch: `design/open-reading-profile`
- Reviewed against main: `70518f67f3c15725e1befc050aef0cf6ce0ccd40`
- Related: ADR 0003, ADR 0004, ADR 0005, `docs/open-reading-profile-design.md`, `docs/tool-contract-use-case-design.md`

## Context

The current runtime already exposes the seven accepted reading Tools and has precise identity/continuation/search/context foundations:

```text
normalized-document-hash/v2
text-segmentation/v2
normalized-block-model/v1
TextLocator / ReadCursor / TextUnitCursor
lexical-search-index/v3
EPUB navigation/reconciliation/validator evidence
```

But `open_document` still returns only source/version identity plus `section_count`. The accepted use-case workflow requires an Agent to inspect capabilities, reliability, and coverage immediately after open, before deciding whether to use structure navigation, Paragraph reading, Sentence-first reading, search, or a coarser workflow.

The missing capability is therefore not another Tool. It is a bounded decision profile projected from the same canonical evidence already used by precise reading.

A second problem must be made explicit: canonical normalized-text completion is not equivalent to publication completion. For example, an EPUB may have every persisted `Section.content` character accounted for by current Paragraph/Sentence rules while one spine item is unsupported and never became canonical text.

## Decision

### 1. `open_document` gains one additive versioned reading profile

Logical response evolution:

```text
reading_profile: {
  schema_version: "reading-profile/v1",
  capabilities,
  canonical_text_coverage,
  reliability
}
```

Existing `open_document` request/response fields keep their current meaning.

The profile is a bounded projection, not a new source of truth.

### 2. Capability availability is factual and separate from coverage quality

The v1 profile advertises document-dependent workflow capabilities:

```text
structural_navigation
paragraph_enumeration
sentence_first_enumeration
exact_locator_read
locator_context
lexical_search
```

Each capability answers only whether the operation can be executed truthfully for the opened document.

Sentence-first availability does not mean every source region has Sentence identity. Actual coarse regions are disclosed by canonical coverage.

### 3. Canonical precise-reading coverage is Domain-derived

Aggregate current fallible TextUnit coverage from the canonical `Document`:

```text
owner_chars
paragraph_chars
paragraph_separator_chars
paragraph_count
native_paragraph_chars
native_structural_container_chars
native_non_prose_chars
fallback_chars
sentence_eligible_paragraphs
coarse_paragraphs
sentence_count
sentence_chars
sentence_separator_chars
sentence_coarse_only_chars
```

This is the authoritative open-time summary for the canonical normalized text plane.

No confidence percentage is introduced.

### 4. Publication/source reliability is a separate plane

Format-specific persisted validators may contribute a neutral reliability projection containing:

```text
evidence kind/schema/integrity
degradation_count + bounded degradation codes
publication source-unit coverage
structure provenance counts
navigation resolution counts
```

For current EPUB v1 projection:

```text
source unit = spine item
native navigation = epub_nav
legacy navigation = epub_ncx
heading fallback = xhtml_heading
source-item fallback = spine_item
```

This keeps unsupported/missing source resources visible even when canonical TextUnit coverage is internally complete.

### 5. Application does not import EPUB validator types

Rejected:

```text
OpenDocumentUseCase
→ parsing::epub_validator::EpubValidationReport
```

Accepted boundary:

```text
OpenDocumentUseCase
→ application port: DocumentReliabilityInspector
← format/reliability adapter implementation
```

The inspector consumes only the already parsed `Document` and persisted evidence. It performs no source retrieval or reparse.

Application owns neutral summary meanings; format-specific modules own mapping from their validator facts.

### 6. Required evidence fails closed

For a format whose current parser contract guarantees validator evidence, malformed/missing required evidence is an invariant failure. It is not relabeled `not_applicable` and it does not produce an optimistic profile.

For formats with no validator contract, reliability evidence is `not_applicable`, which is neither a success claim nor a degradation.

### 7. Degradation codes may be exposed, validator messages may not

The profile can return a bounded deduplicated list of stable degradation codes so the Agent knows why a claim must be qualified.

Full free-form validator messages remain outside the open-response summary to keep the response bounded and avoid duplicating the validator report.

### 8. Profile facts do not participate in source identity

This ADR does not change:

```text
normalized-document-hash/v2
text-segmentation/v2
text-unit-id/v1
text-unit-cursor/v1
read-cursor/v2
lexical-search-index/v3
lexical-tokenizer/v1
```

Validator/degradation/coverage facts remain evidence. Changing those facts alone cannot redefine a TextLocator unless an existing identity-bearing source fact also changes.

### 9. Parsed Cache version changes only if parser output changes

Adding the MCP/application profile alone does not justify a `reading-mcp-normalization/v6` bump.

If implementation requires newly persisted parser facts, Parsed Cache policy must advance in that implementation. If all profile evidence is projected from facts already guaranteed in v6 Documents, no cache migration is required.

### 10. `open_outcome` is deferred as a separate use case increment

The prior accepted Tool design proposed:

```text
opened_new | reused | refreshed | changed
```

Current application state does not expose enough explicit prior/current version evidence to classify this without coupling to cache/retriever implementation details. The reading profile therefore does not guess it.

UC-OPEN-02/03 freshness/version outcomes remain separately scoped.

## Consequences

Positive:

- an Agent can choose a safe reading workflow immediately after open;
- Sentence-first coarse regions are visible before traversal;
- canonical normalized-text coverage cannot hide source-publication gaps;
- EPUB native/fallback structure and unsupported spine evidence can be qualified without a new Tool;
- no confidence score or fabricated precision is introduced;
- application remains independent of EPUB validator implementation types;
- profile facts remain outside source identity.

Costs:

- `open_document` response grows by one structured object;
- a neutral reliability-inspection application port and format projection are required;
- implementation must define/test compact cross-format meanings instead of simply serializing parser metadata;
- successful open may fail if required persisted reliability evidence is internally corrupt.

## Rejected alternatives

### Dedicated `get_reliability` Tool

Rejected for v1 because capability/reliability is required at the open workflow decision point and currently has no independent user workflow that justifies another Tool.

### Return the full EPUB validator report from `open_document`

Rejected because it leaks format-specific schema into the application/MCP surface, increases response size, and duplicates detailed diagnostics instead of projecting the decision evidence the Agent needs.

### One `confidence` percentage

Rejected because canonical text coverage, publication source coverage, navigation resolution, native/fallback structure, and Sentence granularity have different denominators and meanings.

### Infer capability from media type

Rejected because capability is a property of the actual opened document plus runtime evidence, not merely `application/epub+zip` versus `text/html`.

### Let the Agent probe later Tools and interpret failures

Rejected because UC-OPEN-04 explicitly requires safe workflow selection before precise operations and probing does not expose hidden source-publication gaps.

## Implementation gate

After design acceptance, implement on:

```text
feat/open-reading-profile
```

Required evidence includes:

- non-EPUB `not_applicable` reliability without false degradation;
- native/fallback/coarse TextUnit aggregate coverage;
- clean and degraded EPUB reliability projection;
- unsupported/missing spine denominator preserved;
- nav/NCX/heading/spine provenance counters;
- bounded degradation codes;
- malformed required evidence fail closed;
- no parser type leakage into application/MCP DTOs;
- no identity/version changes unless separately justified;
- real stdio additive response;
- runtime Tool count remains seven;
- Format, Clippy, and full Test green.

## Non-goals

- structure/discovery/search cursors;
- a reliability Tool;
- `open_outcome` freshness classification;
- source refresh/historical version retention;
- full validator messages in open response;
- confidence scoring;
- TextUnit identity changes;
- new TextUnit kinds;
- Sentence persistence;
- fuzzy repair.
