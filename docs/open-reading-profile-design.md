# Open Reading Profile Design

> Status: Proposed design
>
> Branch: `design/open-reading-profile`
>
> Reviewed against `main` at `70518f67f3c15725e1befc050aef0cf6ce0ccd40`
>
> Scope: use-case/application/MCP contract design only. No runtime implementation, parser change, cache migration, index change, or Tool addition is authorized by this branch.
>
> Related: `docs/tool-contract-use-case-design.md`, ADR 0003, ADR 0004, ADR 0005, `docs/epub-structure-validator-contract.md`, `docs/text-unit-enumeration-contract.md`

## 1. Why this is the next use-case gap

The accepted reading workflow is:

```text
open
  ↓
inspect capabilities / reliability / coverage
  ↓
choose structure / coarse read / Paragraph / Sentence-first / search
```

The runtime already implements the seven-Tool precise-reading surface, block-aware TextUnit identity, exact locator handoff, lexical TextUnit search, EPUB navigation/reconciliation, normalized block evidence, and a persisted EPUB validator.

However the current successful `open_document` response exposes identity/version facts only:

```text
document_id
source / title / media_type
content_hash
normalized_document_hash
normalized_document_hash_version
normalization_version
normalized_text_coordinate_space
section_count
```

An Agent therefore still has to probe later Tools or know Reading MCP internals to answer questions such as:

- Is Paragraph/Sentence-first reading truthfully available for this concrete document?
- Does Sentence-first reading contain coarse structural/non-prose regions?
- How much canonical normalized text comes from native Paragraph evidence versus deterministic fallback?
- Is the canonical normalized document complete relative to the source publication, or are some EPUB spine resources unsupported?
- Is EPUB structure publisher-native, NCX-backed, heading fallback, or spine-item fallback?
- Are there validator degradations that should qualify a later “I read the whole book” claim?

This is the unresolved part of UC-OPEN-04 and UC-RELIABILITY-01/02. It is not a reason for a new Tool: the decision is required immediately after `open_document` succeeds.

## 2. Source-first baseline

### 2.1 Canonical precise-reading evidence already exists

Current `Document::try_paragraph_text_units()` returns deterministic coverage per owner Section:

```text
owner_chars
paragraph_chars
separator_chars
paragraph_count
native_paragraph_chars
native_structural_container_chars
native_non_prose_chars
fallback_chars
```

Current `Document::try_sentence_text_units()` returns Paragraph-level Sentence coverage including:

```text
content_class
eligibility
paragraph_chars
sentence_chars
separator_chars
coarse_only_chars
sentence_count
```

Both are derived from the same canonical `Document / Section.content`, `normalized-document-hash/v2`, `text-segmentation/v2`, and optional validated `normalized-block-model/v1` evidence used by enumeration/read/context/search.

A declared invalid normalized block map already fails precise TextUnit materialization closed.

### 2.2 EPUB publication/structure evidence already exists

A successfully parsed current EPUB persists `epub-structure-validator/v1` evidence. The validator distinguishes:

```text
error
→ persisted facts contradict an invariant
→ EPUB parse fails closed

degradation
→ readable canonical content survives but source/capability coverage is incomplete
```

Its factual coverage includes independent package/spine, navigation, canonical structure, normalized-block, and current TextUnit planes.

A degraded EPUB can therefore be simultaneously true in two different senses:

```text
all canonical Section.content is precisely accounted for
AND
one source-publication spine item is unsupported
```

Those statements must never be collapsed into one synthetic percentage.

### 2.3 The missing contract is projection, not new evidence

The next capability is a compact, bounded projection of already available evidence at the open decision point.

It must not:

- become another source of truth;
- reparse ZIP/DOM state;
- rebuild a second validator;
- infer unsupported source facts from media type alone;
- assign subjective confidence scores;
- duplicate full validator findings into every open response.

## 3. Use cases

### UC-OPEN-PROFILE-01 — Choose a truthful reading workflow after open

- **Actor goal:** choose the strongest safe reading workflow for this document without probing by failure.
- **Success:** the response states which reading operations are available and exposes the evidence/coverage that may qualify their use.
- **Failure:** an unavailable operation is advertised as available, or a document-level degradation is hidden.
- **Degradation:** operation remains usable with coarse/fallback/source-gap evidence explicitly reported.
- **Required capability:** `CapabilityAdvertisement + ReliabilityInspection + CoverageInspection`.
- **Tool mapping:** additive `open_document` response only.

### UC-OPEN-PROFILE-02 — Distinguish canonical text completion from publication completion

- **Actor goal:** know whether “I consumed every canonical normalized reading region” also means “the whole source publication was representable”.
- **Success:** canonical TextUnit coverage and source-publication coverage are separate planes.
- **Failure:** unsupported EPUB spine content disappears behind 100% canonical TextUnit coverage.
- **Degradation:** publication coverage is partial while canonical normalized text remains fully addressable.
- **Required capability:** `CoverageInspection + NativeTraceability`.

### UC-OPEN-PROFILE-03 — Detect coarse Sentence-first regions before traversal

- **Actor goal:** know whether Sentence-first traversal will contain Paragraph-level coarse items.
- **Success:** current coarse-only structural/non-prose character/item evidence is visible before enumeration.
- **Failure:** `sentence_first` is presented as “all content has Sentence identity”.
- **Degradation:** Sentence-first operation is available, but some represented regions have only coarse Paragraph identity.
- **Required capability:** `CapabilityAdvertisement + CoverageInspection`.

### UC-OPEN-PROFILE-04 — Qualify EPUB structural claims

- **Actor goal:** know whether EPUB navigation/structure is native, fallback, partial, or contains unresolved/unsupported source facts.
- **Success:** compact validator/provenance/source-coverage evidence is returned.
- **Failure:** heading/spine fallback is labeled publisher-native, or validator degradations are hidden.
- **Degradation:** readable fallback remains available with explicit factual counters/reason codes.
- **Required capability:** `ReliabilityInspection + NativeTraceability`.

## 4. Success semantics

A successful open profile means:

```text
canonical Document exists
+ current normalized identity is known
+ current Paragraph/Sentence materialization is internally valid
+ the profile was projected from the same Document/evidence returned by this open
```

It does **not** mean:

```text
all source-publication resources were supported
all structure is publisher-native
every Paragraph has Sentence identity
no fallback normalization was used
```

Those are separate profile facts.

## 5. Decision: one versioned additive `reading_profile`

Add one nested response object rather than a growing set of unrelated top-level flags:

```text
reading_profile: {
  schema_version: "reading-profile/v1",
  capabilities: ...,
  canonical_text_coverage: ...,
  reliability: ...
}
```

Existing `open_document` fields keep their exact meaning.

The profile is bounded summary evidence. Full body text, full validator messages, search rows, or parser-native trees never appear here.

## 6. Capability model

### 6.1 Availability is separate from quality/coverage

A capability entry uses factual availability:

```text
availability = available | unavailable
```

Availability answers only:

> Can the current runtime truthfully execute this operation for this concrete opened document?

It does not claim native provenance or all-Sentence precision.

Current v1 profile advertises only document-dependent reading capabilities that materially affect workflow selection:

```text
structural_navigation
paragraph_enumeration
sentence_first_enumeration
exact_locator_read
locator_context
lexical_search
```

Tool discovery still tells the client what Tools exist globally. The profile tells the Agent whether the opened document can use the corresponding reading capability truthfully.

### 6.2 Capability detail

Logical shape:

```text
capabilities: {
  structural_navigation: {
    availability,
    section_count
  },
  paragraph_enumeration: {
    availability,
    segmentation_version?
  },
  sentence_first_enumeration: {
    availability,
    segmentation_version?,
    source_preserving_coarse_regions: bool
  },
  exact_locator_read: { availability },
  locator_context: { availability },
  lexical_search: { availability }
}
```

`source_preserving_coarse_regions=true` is derived from actual coarse Sentence coverage, not from format or tag guesses.

### 6.3 Fail closed

If current precise TextUnit evidence cannot be materialized because persisted canonical block facts are invalid, precise capability advertisement must not silently fall back to an optimistic profile.

The existing open flow already treats that as a TextUnit/index integrity failure. The profile follows the same failure boundary.

## 7. Canonical normalized-text coverage

### 7.1 Why this plane is format-neutral

This plane answers:

> Within the canonical normalized `Document`, what exact text is represented at Paragraph/Sentence granularity, and what is coarse/fallback/separator evidence?

It is derived from Domain TextUnit materialization, not parser-specific validator types.

### 7.2 v1 aggregate fields

Aggregate per-Section Paragraph coverage into:

```text
canonical_text_coverage: {
  owner_chars,
  paragraph_chars,
  paragraph_separator_chars,
  paragraph_count,

  native_paragraph_chars,
  native_structural_container_chars,
  native_non_prose_chars,
  fallback_chars,

  sentence_eligible_paragraphs,
  coarse_paragraphs,
  sentence_count,
  sentence_chars,
  sentence_separator_chars,
  sentence_coarse_only_chars
}
```

Interpretation:

- `owner_chars` is total Unicode-scalar length of canonical owner Section content across the document;
- `paragraph_chars + paragraph_separator_chars == owner_chars` for current materialized canonical text;
- `native_paragraph_chars` has persisted exact native Paragraph evidence;
- `native_structural_container_chars` is current coarse BlockQuote/ListItem evidence under flat block-model/v1;
- `native_non_prose_chars` is current native Preformatted/Table evidence;
- `fallback_chars` was materialized without a native block boundary;
- `sentence_coarse_only_chars` is readable canonical content intentionally not given Sentence identity;
- Sentence separator characters are not missing source content.

No percentage is necessary. Consumers may calculate a ratio for presentation, but Reading MCP does not assign a confidence meaning to it.

### 7.3 Canonical coverage is not publication coverage

A canonical partition can be exact while a source publication still has an unsupported resource that never became `Section.content`.

Therefore this plane must never expose a field named simply `document_complete` or `book_complete`.

## 8. Reliability evidence

### 8.1 Neutral evidence envelope

`open_document` must not depend on EPUB validator Rust types. Application owns a neutral reliability projection contract; format-specific modules can contribute evidence through an application port.

Logical shape:

```text
reliability: {
  evidence[]: {
    kind,
    schema_version,
    integrity,
    degradation_count,
    degradation_codes[]
  },
  publication_coverage?: {
    source_units_total,
    source_units_represented,
    source_units_missing,
    source_units_unsupported
  },
  structure_provenance?: {
    native_navigation_sections,
    legacy_navigation_sections,
    heading_fallback_sections,
    source_item_fallback_sections
  },
  navigation_resolution?: {
    targets_total,
    targets_resolved,
    targets_unresolved_or_unsupported
  }
}
```

The field names are format-neutral at the MCP/application boundary. An EPUB reliability inspector maps persisted validator facts into these meanings:

```text
source unit             = spine item
native navigation       = epub_nav
legacy navigation       = epub_ncx
heading fallback        = xhtml_heading
source-item fallback    = spine_item
```

A future format may contribute a different validator while preserving the same neutral concepts where they genuinely apply. It must not fake absent concepts merely to fill fields.

### 8.2 Evidence integrity states

Use a small factual enum:

```text
integrity = valid | invalid | not_applicable
```

For a successful current EPUB open, persisted validator `invalid` should already have failed parsing. If the reliability inspector observes missing/malformed/contradictory evidence where the parser contract requires it, profile construction fails closed as an internal/canonical invariant error rather than relabeling it `not_applicable`.

For non-EPUB formats without a format validator, absence is `not_applicable`, not `valid` and not a degradation.

### 8.3 Degradation codes, not messages

The open profile may return bounded unique degradation **codes** because an Agent needs to know why a workflow is qualified. It must not copy unbounded validator messages.

For current EPUB evidence examples include:

```text
spine_unsupported_media
navigation_target_missing_fragment
navigation_target_unsupported_resource
navigation_missing_fragment_document_fallback
```

The exact list is validator-owned. The profile deduplicates and caps the list; `degradation_count` remains the factual full count.

### 8.4 Publication coverage

Publication coverage answers a different question from canonical TextUnit coverage:

```text
source_units_total
source_units_represented
source_units_missing
source_units_unsupported
```

For EPUB v1 projection:

```text
source_units_total       = spine_items_total
source_units_represented = spine_items_parsed
source_units_missing     = spine_items_missing_manifest
source_units_unsupported = spine_items_unsupported_media
```

This preserves the denominator even when unsupported content cannot enter the canonical normalized Document.

### 8.5 Structure provenance

For EPUB:

```text
native_navigation_sections   = sections_epub_nav
legacy_navigation_sections   = sections_epub_ncx
heading_fallback_sections    = sections_xhtml_heading
source_item_fallback_sections= sections_spine_item
```

These are factual provenance counters. They do not produce a subjective `structure_confidence` score.

### 8.6 Navigation resolution

For EPUB:

```text
targets_total
resolved targets
unresolved/unsupported targets
```

may be projected from validator navigation counters. Detailed target records remain a structural-navigation concern, not an open-response payload.

## 9. Architecture boundary

### 9.1 Rejected coupling

Do not implement:

```text
application/open_document.rs
  → import parsing::epub_validator::EpubValidationReport
  → serde_json::from_str(document.metadata["epub_validation_report"])
```

That makes an application use case change for every parser-specific evidence schema.

### 9.2 Accepted dependency shape

Introduce an application port with neutral result types, conceptually:

```text
DocumentReliabilityInspector
  inspect(&Document)
    → ReliabilitySummary
```

Dependency direction remains:

```text
open_document application use case
        ↓ depends on port
DocumentReliabilityInspector
        ↑ implemented by format/reliability adapter
EPUB persisted-validator projection
```

The inspector:

- consumes only the already parsed canonical `Document` and persisted evidence;
- performs no retrieval or source reparse;
- maps format-specific evidence into the neutral summary;
- returns `not_applicable` when no validator is defined for the format;
- fails if evidence required by the concrete parser contract is malformed/internally inconsistent.

This preserves:

```text
application = workflow
parsing/validator = format evidence semantics
MCP = DTO mapping only
```

### 9.3 Text coverage remains Domain-derived

Canonical TextUnit coverage does not go through the format inspector. It is derived directly from current fallible Domain TextUnit materialization because that is the canonical identity/coverage source used by reading/search.

## 10. State machine

```text
source
  ↓
retrieve / parse
  ↓
canonical Document
  ↓
validate current Paragraph/Sentence materialization
  ↓
project canonical text coverage
  ↓
inspect persisted format reliability evidence
  ↓
persist canonical Document + derived indexes
  ↓
return identity + reading_profile
```

The profile must describe the same Document version that was persisted/indexed.

If profile projection detects an invariant contradiction, the open does not return a successful optimistic profile.

## 11. MCP contract

Current fields remain unchanged and additive response becomes:

```text
OpenDocumentResponse {
  ...existing fields,
  reading_profile: ReadingProfileDto
}
```

Logical DTO:

```text
ReadingProfileDto {
  schema_version: "reading-profile/v1",
  capabilities: ReadingCapabilitiesDto,
  canonical_text_coverage: CanonicalTextCoverageDto,
  reliability: ReliabilitySummaryDto
}
```

This branch intentionally does not freeze exact Rust enum names or serde layout beyond the semantic requirements above. Implementation tests must lock the actual wire schema chosen.

## 12. Compatibility and versioning

### 12.1 Additive MCP evolution

No current request field or response field changes meaning. Clients that ignore unknown response fields continue working.

Runtime Tool count remains seven.

### 12.2 Source identity is unchanged

The profile is evidence/decision metadata, not normalized source identity.

This design does not change:

```text
normalized-document-hash/v2
text-segmentation/v2
text-unit-id/v1
text-unit-cursor/v1
read-cursor/v2
lexical-search-index/v3
lexical-tokenizer/v1
```

Changing a degradation count or validator message must not renumber TextUnits or stale a locator unless an actual existing identity input changes.

### 12.3 Parser/cache version decision is implementation-gated

The design does not automatically bump `reading-mcp-normalization/v6`.

If implementation can project the profile entirely from facts already guaranteed in current v6 parsed Documents, no Parsed Cache migration is required.

If implementation introduces new persisted parser facts required to construct the profile, then Parsed Cache policy must advance in that implementation PR. The reason must be the changed persisted parser output, not the MCP DTO addition itself.

## 13. Open outcome is explicitly separate

The accepted use-case design also proposed:

```text
open_outcome = opened_new | reused | refreshed | changed
```

Current `OpenDocumentUseCase` does not yet own enough explicit prior/current version-state evidence to classify those outcomes without guessing about cache/retriever internals.

Therefore `open_outcome` is **not** bundled into `reading-profile/v1`.

It remains a separate freshness/version-resolution increment derived from UC-OPEN-02/03.

## 14. Acceptance criteria for implementation

Implementation must prove at least:

1. clean blockless/non-EPUB Document returns a valid format-neutral profile without pretending EPUB validation applies;
2. native Paragraph evidence contributes to native Paragraph coverage;
3. fallback Paragraph evidence is counted separately;
4. BlockQuote/ListItem coarse chars make Sentence-first coarse regions observable before traversal;
5. Preformatted/Table coarse-only chars are not counted as missing Sentence source;
6. canonical Paragraph partition obeys exact owner-char invariants;
7. canonical Sentence counters equal current deterministic `text-segmentation/v2` materialization;
8. clean EPUB maps validator v1 to zero-degradation reliability and full represented spine denominator;
9. degraded EPUB exposes unsupported/missing source units plus bounded degradation reason codes while open remains successful;
10. EPUB structural provenance counts distinguish nav/NCX/heading/spine fallback;
11. malformed required reliability evidence fails closed rather than becoming `not_applicable`;
12. profile changes do not affect normalized hash/TextUnit locator identity;
13. no parser-specific validator type leaks into MCP contracts or `OpenDocumentUseCase`;
14. no retrieval/reparse occurs merely to construct the profile;
15. stdio `open_document` returns the additive profile while Tool count remains seven;
16. Format / Clippy / full tests remain green.

## 15. Explicit non-goals

This design does not implement or decide:

- `StructureCursor` or `get_document_structure` pagination/subtree continuation;
- `DiscoveryCursor` / `list_documents` continuation;
- search pagination or ranking changes;
- a dedicated reliability/inspection MCP Tool;
- a confidence score;
- full validator finding messages in open responses;
- source refresh or historical version retention;
- `open_outcome` freshness classification;
- new TextUnit kinds;
- nested/leaf block identity;
- Sentence SQLite persistence;
- normalized hash/segmentation/tokenizer/index version changes;
- fuzzy locator/source repair.

## 16. Next implementation boundary

After this design is accepted and merged, the bounded implementation branch should be:

```text
feat/open-reading-profile
```

Recommended implementation order:

```text
neutral application profile types
→ DocumentReliabilityInspector port
→ format-neutral canonical TextUnit coverage projector
→ current EPUB persisted-validator inspector
→ OpenDocumentResult additive reading_profile
→ MCP DTO mapping/schema
→ unit + EPUB degradation + stdio acceptance
→ independent architecture/diff review
```

Only after this increment is complete should the next use-case gap be selected independently. `get_document_structure` actionable continuation remains a known high-priority candidate, but it is not part of this change.