# EPUB-First Structure Reliability Design

> Status: Draft for design review
>
> Branch: `design/epub-structure-reliability`
>
> Scope: parser/normalization reliability and validation design only. This document does not authorize implementation changes on the design branch.
>
> Related: `docs/adr/0002-text-index-locator-identity.md`, `docs/text-index-and-locator-design.md`

## 1. Goal

Reading MCP needs reliable source structure before Paragraph/Sentence TextUnits can become trustworthy reading and evidence locators.

The design goal is not merely "extract text from EPUB". It is:

> Preserve the strongest author/publisher-provided EPUB structure first, deterministically normalize it, expose provenance for every structural decision, and degrade explicitly when native structure is missing or malformed.

For precise reading, the expected chain is:

```text
EPUB container/package
        ↓
manifest + spine
        ↓
EPUB navigation structure
        ↓
XHTML block structure
        ↓
Normalized Section.content
        ↓
Paragraph TextUnits
        ↓
Sentence TextUnits
        ↓
TextLocator
```

Any claimed locator such as:

```text
§1.1 ¶4 S3
```

must be reproducible and traceable to the persisted normalized document and, where possible, to an EPUB-native target such as:

```text
EPUB entry path + fragment/anchor
```

## 2. EPUB is the priority structured-book format

For book-like documents, EPUB is the first format on which Reading MCP should prove reliable structural parsing and fine-grained addressing.

Recommended implementation/validation priority:

```text
EPUB
  ↓
HTML / Markdown / DOCX
  ↓
plain Text
  ↓
PDF layout inference
```

This is not a statement that EPUB is always semantically perfect. It means EPUB normally exposes richer machine-readable structure than a page-layout format and therefore provides the best proving ground for deterministic source-first reading.

Reading MCP must not flatten EPUB XHTML to plain text first and then try to reconstruct structure that the publication already provided.

## 3. Standards baseline

The normative implementation baseline for EPUB 3 behavior is the current W3C EPUB 3.3 Recommendation (2026-01-13). EPUB 3.4 is still a Working Draft and must not silently redefine baseline behavior.

Relevant EPUB concepts:

- the package document identifies publication resources;
- the manifest enumerates resources;
- the spine defines the default reading order;
- the EPUB navigation document provides global navigation, including the table of contents;
- XHTML content documents carry the readable structured content.

Legacy EPUB 2 publications may expose an NCX navigation document instead of an EPUB 3 navigation document. Reading MCP may support this as a compatibility path, but the provenance must identify NCX rather than pretending it is EPUB 3 `nav`.

## 4. Current implementation baseline

The current `EpubParser` already performs:

```text
META-INF/container.xml
        ↓
package document
        ↓
manifest
        ↓
spine
        ↓
resolve readable spine XHTML
        ↓
HtmlParser
        ↓
remapped Section tree
```

It also records EPUB-native locations of the form:

```text
epub:<entry-path>
epub:<entry-path>#<anchor>
```

However, current structural hierarchy is not yet EPUB-navigation-first:

1. the parser does not parse an EPUB 3 navigation document;
2. it does not parse legacy NCX;
3. the spine is used for item order but not reconciled with a publisher TOC;
4. each spine XHTML is delegated to the generic `HtmlParser`;
5. the generic HTML parser reconstructs Sections from `h1` through `h6`;
6. block content is normalized into strings and joined using blank lines;
7. HTML `nav` elements are currently removed as noise by the generic parser.

Therefore current support should be described as:

```text
EPUB package/spine aware
+ XHTML-heading-derived structure
```

not:

```text
fully EPUB TOC-structured
```

## 5. Three distinct EPUB structure planes

Reading MCP must keep these separate:

### 5.1 Resource plane — manifest

Answers:

> What resources belong to the publication, and what are their types/properties?

Required facts include at least:

```text
manifest item id
href
media type
properties
fallback, when relevant
```

### 5.2 Reading-order plane — spine

Answers:

> In what default order does the reader progress through content documents?

The spine is the authoritative default reading-order source. It is not itself the TOC hierarchy.

Important facts include:

```text
spine ordinal
idref
linear status, when declared
resolved manifest item
resolved content path
```

### 5.3 Navigation plane — EPUB nav / legacy NCX

Answers:

> What named hierarchical structure did the publisher expose for navigation?

For EPUB 3, the primary source is the navigation document's TOC `nav` element. For legacy EPUB 2 compatibility, NCX may be used when appropriate.

The navigation plane must not be reconstructed from the spine alone.

## 6. Structure-source precedence

Structural extraction uses the strongest available evidence in this order:

```text
EPUB 3 navigation TOC
        ↓
legacy NCX navigation
        ↓
XHTML heading structure
        ↓
spine-item fallback structure
```

This means:

- EPUB nav hierarchy wins over a conflicting `h1/h2` hierarchy for navigation structure;
- NCX is a legacy compatibility source, not equivalent provenance to EPUB 3 nav;
- XHTML headings can supplement missing intra-document structure or serve as deterministic fallback;
- a readable spine item with no usable navigation or heading information still remains addressable as a coarse structural node rather than disappearing.

No fallback source may be labeled as publisher-native TOC structure.

## 7. Provenance is required

Every normalized structural decision that may affect addressing must expose deterministic provenance.

Conceptual provenance classes:

```text
StructureProvenance
├── epub_nav
├── epub_ncx
├── xhtml_heading
├── spine_item
└── normalized_fallback
```

For text/block boundaries:

```text
BoundaryProvenance
├── xhtml_native_block
├── normalized_block
└── inferred
```

Sentence boundaries remain governed separately by the versioned segmentation policy from ADR 0002.

Do not invent subjective floating-point confidence scores. Prefer factual provenance and resolution state:

```text
source = epub_nav
status = resolved | partial | unresolved | fallback
```

## 8. EPUB navigation extraction

### 8.1 EPUB 3 navigation document discovery

The parser should discover the EPUB navigation document from package metadata/manifest properties rather than by filename guesses.

The navigation parser must extract the TOC hierarchy while preserving:

```text
label/title
hierarchy/depth
href
resolved entry path
fragment, if any
source order
```

### 8.2 Legacy NCX

For publications requiring EPUB 2 compatibility, the package/spine metadata may identify an NCX resource.

If implemented, NCX extraction must produce the same logical navigation-node contract but retain:

```text
provenance = epub_ncx
```

### 8.3 Navigation labels and XHTML headings may differ

A publisher navigation label does not have to equal the visible XHTML heading text.

Therefore:

```text
nav label != heading text
```

is not automatically a parse error.

The navigation label is the navigation-plane title. The visible heading may be retained as content/diagnostic metadata.

## 9. TOC target resolution

A TOC node is not considered fully resolved merely because its `href` string was parsed.

Resolution must establish, when possible:

```text
TOC href
  ↓
archive-safe resolved entry path
  ↓
manifest/content document
  ↓
fragment target inside XHTML, if present
  ↓
DOM/source position
```

Target resolution states should distinguish:

```text
resolved_document
resolved_fragment
missing_fragment
missing_resource
unsupported_resource
invalid_path
```

A missing fragment should not cause the entire book to disappear. The node may degrade to its containing content document while diagnostics record the lost precision.

Archive path resolution must continue to reject traversal outside the EPUB container root.

## 10. Mapping navigation structure to content ranges

Navigation hierarchy and readable content order must be reconciled deterministically.

For a resolved navigation target, the target identifies a section start. A section's content range may extend until the next relevant resolved navigation boundary in document order, subject to parent/child hierarchy and spine transitions.

Key requirements:

1. source order must never be reversed to match a malformed TOC;
2. multiple navigation nodes targeting the same position must be represented deterministically, not randomly deduplicated;
3. an intra-document fragment target may define a finer section boundary than the spine item;
4. a whole-document target begins at that content document's readable start;
5. unresolved nodes remain visible in diagnostics and must not silently claim precise ranges;
6. headings may supplement boundaries only under explicit fallback/supplement provenance.

Exact range-building algorithms are deferred to implementation pre-research, but the above semantics are required.

## 11. Spine semantics

The spine remains the default reading-order authority even when TOC hierarchy is available.

The implementation must record enough spine metadata to explain order and degradation:

```text
spine_index
idref
linear?
manifest href
resolved path
media type
parse status
```

If a spine item is unsupported or cannot be parsed, coverage must report the gap.

The parser must not silently compress:

```text
spine item 4 failed
spine item 5 succeeded
```

into a result that appears to have complete continuous coverage.

## 12. XHTML block preservation comes before Paragraph/Sentence segmentation

For EPUB precise reading, the parser must preserve meaningful XHTML block boundaries before TextUnit segmentation.

Relevant block-like structures include at least:

```text
p
pre
blockquote
li
table
headings
```

They are not all equivalent prose Paragraphs.

The current generic HTML normalization collapses whitespace and joins block strings. That is suitable for coarse reading, but it is insufficient as the sole source of future reliable paragraph provenance because block type/boundary information is discarded.

Before parser-native paragraph boundaries can influence persistent TextUnit identity, addressing-relevant boundary metadata must be materialized into the canonical persisted normalized document, as required by ADR 0002.

A future canonical block representation may conceptually contain:

```text
NormalizedBlock
├── block_kind
├── owner_section_id
├── normalized_range
├── native_location/anchor?
├── source_order
└── provenance
```

This design does not yet fix the physical schema.

## 13. Paragraph policy for EPUB

Paragraph TextUnits should prefer preserved XHTML-native prose blocks when the boundary metadata is part of canonical persisted state.

Recommended behavior:

```text
<p> prose                 → paragraph candidate
<blockquote> prose block  → paragraph/block candidate
<li>                      → preserve list-item nature; do not pretend source was <p>
<pre>                     → non-prose block by default
<table>                   → structured/non-prose block by default
```

If canonical block metadata has not yet been introduced, Paragraph v1 must continue to derive deterministically from exact persisted `Section.content`, not from transient DOM state that cannot be reproduced after reopening the persisted Document.

## 14. Sentence policy for EPUB

Sentence segmentation runs only after normalized Paragraph/prose boundaries exist.

Requirements from ADR 0002 remain:

- deterministic;
- non-LLM;
- versioned;
- exact normalized ranges;
- stable for the same normalized-document hash and segmentation version.

Non-prose blocks must not be force-split into sentences merely to maximize sentence coverage.

Therefore:

```text
sentence coverage < 100%
```

can be correct when the uncovered content is code, tables, formulas, navigation structures, or other intentionally non-prose blocks.

## 15. Canonical text is not rebuilt from sentences

EPUB reliability does not change the source-truth rule:

```text
Section.content = canonical normalized text
Paragraph/Sentence = persisted, rebuildable TextUnits
```

Sentence-first reading is encouraged. Sentence-first source storage is not.

The invariant remains:

```text
TextUnit.text
==
exact slice of owner Section.content at TextUnit.normalized_range
```

A Section or Chapter must not depend on concatenating Sentence rows to reconstruct canonical text.

## 16. Reliability and degradation policy

The parser should distinguish fatal parse failures from degradable structural failures.

### 16.1 Fatal examples

A publication cannot produce a trustworthy readable Document when essential container/reading-order state is unavailable, for example:

```text
invalid ZIP/OCF container
missing/unreadable container.xml
missing package rootfile
invalid/unreadable package document
empty spine
no readable spine content after supported fallback handling
archive path escapes container root
resource limits exceeded
```

### 16.2 Degradable examples

The publication may remain readable while exposing reduced structural precision:

```text
EPUB 3 navigation document missing/malformed
legacy NCX missing/malformed
TOC fragment unresolved
some TOC entries unresolved
XHTML heading hierarchy malformed
one nonessential navigation/resource item unsupported
sentence segmentation intentionally skipped for non-prose blocks
```

For degradable conditions:

```text
readable content survives
+ diagnostics record loss
+ provenance changes to fallback where used
+ coverage reports incompleteness
```

Never fabricate a clean native hierarchy to hide malformed input.

## 17. Parser reliability validator

Opening an EPUB should eventually run or make available deterministic structural validation before precise TextUnits are treated as trustworthy.

### 17.1 Package/spine invariants

Validate at least:

- package rootfile resolves inside archive;
- manifest IDs used by spine resolve;
- supported spine content paths resolve inside archive;
- spine ordering is deterministic;
- duplicate/invalid identifiers are handled deterministically;
- parse failures are surfaced in coverage diagnostics.

### 17.2 Structural invariants

Validate at least:

- normalized Section IDs are unique;
- parent references are valid and acyclic;
- children preserve deterministic source/navigation order;
- native EPUB target, when declared resolved, actually maps to the intended resource/fragment;
- fallback provenance is never mislabeled as native EPUB navigation.

### 17.3 Text-range invariants

Once normalized ranges/TextUnits exist, validate:

```text
0 <= start <= end <= owner Section.content length
TextUnit.text == exact Section.content slice
Paragraph/Sentence ranges preserve source order
Sentence belongs to exactly one Paragraph
Paragraph belongs to exactly one owner Section
```

No validator may "repair" content silently. Repair is parser/normalizer policy and must be versioned.

## 18. Coverage report

`parse success` is not enough for precise reading. EPUB parsing should be able to produce a diagnostic coverage summary.

Conceptual coverage dimensions:

```text
EPUB coverage
├── package
├── manifest
├── spine
├── navigation
├── content documents
├── structural targets
├── normalized blocks
├── paragraphs
└── sentences
```

Useful metrics include:

```text
spine_items_total
spine_items_readable
spine_items_parsed
nav_nodes_total
nav_targets_resolved
nav_fragments_resolved
content_documents_parsed
blocks_preserved
paragraph_units_created
sentence_units_created
non_prose_units_skipped_for_sentence_split
```

Coverage is evidence, not a vague confidence score.

Example:

```text
spine:      42 / 42 parsed
nav:        118 / 120 targets resolved
paragraphs: 100% of eligible prose blocks represented
sentences:  97.4% of prose text represented
skipped:    31 code blocks, 9 tables
```

The exact external MCP exposure of these diagnostics is deferred. Internal validation and test assertions come first.

## 19. Native location preservation

EPUB native location remains distinct from normalized TextLocator identity.

Useful native facts include:

```text
package path
spine index
manifest id/href
content entry path
fragment/anchor
navigation provenance
```

A human-readable locator may eventually render as:

```text
§1.1 ¶4 S3
```

while machine traceability retains:

```text
epub:OEBPS/ch01.xhtml#processes
```

Neither display form replaces `normalized_document_hash + TextLocator` identity.

## 20. Search implications

Reliable EPUB structure strengthens the lexical index from ADR 0002:

```text
Section title candidate
Paragraph candidate
Sentence candidate
```

Publisher-native navigation labels may become Section-title search candidates when they are the normalized Section title.

A heading or TOC label must not be duplicated as a fake Sentence merely to make it searchable.

Search results must resolve back to the same Section/TextLocator and provenance chain used by read/context.

## 21. EPUB-first implementation sequence

This design must not be pulled into `feat/read-continuation`.

After the P0 continuation/normalized-range foundation is stable, EPUB reliability can proceed in short-lived units:

```text
P1 feat/epub-navigation-map
   - package version/properties
   - EPUB 3 nav discovery + hierarchy
   - optional legacy NCX compatibility
   - TOC target resolution diagnostics

P1 feat/epub-structure-reconciliation
   - reconcile nav hierarchy with spine order and XHTML positions
   - deterministic fallback to headings/spine nodes
   - structure provenance

P1 feat/normalized-block-model
   - persist addressing-relevant block boundaries/kinds
   - normalized block provenance

P1 feat/text-unit-index
   - Paragraph TextUnits derived only from persisted canonical state

P1 feat/sentence-locator
   - deterministic Sentence segmentation

P1 feat/epub-structure-validator
   - structural/range invariants
   - coverage diagnostics
```

Branch names are illustrative; each implementation unit should remain short-lived and testable.

## 22. Test corpus requirements

Reliable EPUB parsing cannot be validated with one happy-path fixture.

The fixture/corpus matrix should include at least:

```text
EPUB 3 with nav + fragments
EPUB 3 nav labels differing from XHTML headings
EPUB 3 with multiple sections in one XHTML file
EPUB 3 with one section per XHTML file
EPUB 3 navigation document present in spine
EPUB with unresolved TOC fragment
EPUB with malformed heading levels but valid nav
EPUB without usable nav but readable headings
legacy EPUB 2 + NCX
mixed prose/list/pre/table content
CJK EPUB
mixed CJK/Latin technical EPUB
Unicode punctuation/emoji
non-linear or optional spine items where supported
unsupported spine resource/fallback cases
archive path traversal attempts
resource-limit violations
```

For every accepted fixture, tests should assert not only text extraction but structural provenance and coverage.

## 23. Reliability gates

An EPUB implementation is not considered precise-reading ready unless:

1. reading order comes from a validated spine, not filesystem/archive iteration order;
2. EPUB 3 TOC hierarchy comes from the navigation document when available and valid;
3. legacy NCX, heading, and spine fallbacks are explicitly distinguishable by provenance;
4. every claimed resolved navigation target is verifiable;
5. missing structure degrades visibly rather than being fabricated;
6. Paragraph/Sentence boundaries are reproducible from persisted canonical state;
7. every TextUnit range is an exact slice of owner `Section.content`;
8. canonical Section text does not depend on reconstructing sentences;
9. non-prose blocks are not force-segmented into fake sentences;
10. structural and textual coverage can be measured;
11. CJK/mixed technical content is part of the fixture matrix;
12. changing parser/normalization behavior changes `normalized_document_hash` when canonical normalized facts change.

## 24. Hard design invariants

Unless superseded by a later ADR:

1. **EPUB spine is reading order, not TOC hierarchy.**
2. **EPUB nav/NCX structure is stronger evidence than inferred XHTML heading structure.**
3. **Fallback structure must retain provenance.**
4. **Uncertainty is represented as degradation/coverage, not fabricated precision.**
5. **Native EPUB structure must be preserved before generic text flattening.**
6. **Paragraph/Sentence TextUnits remain derived and rebuildable.**
7. **Transient DOM/parser boundaries cannot define persistent locator identity unless materialized into canonical persisted state.**
8. **Sentence is the primary fine-grained reading/evidence unit, not the canonical source store.**
9. **Canonical normalized text cannot be reconstructed solely from Sentence rows.**
10. **Validator evidence is required before a parser can claim precise-reading reliability.**

## 25. Questions deferred to implementation pre-research

These require prototypes/fixtures and do not block the architecture:

- exact DOM-range representation used while reconciling nav targets with XHTML blocks;
- whether EPUB 2 NCX support lands in the first EPUB reliability increment or a following compatibility increment;
- exact canonical `NormalizedBlock` domain/storage shape;
- handling of duplicate TOC targets and navigation aliases;
- precise policy for non-linear spine items;
- foreign-content fallback traversal breadth;
- whether navigation diagnostics become part of `open_document` response or remain an internal validation endpoint/report first;
- exact coverage percentage denominators for prose vs all content;
- physical SQLite schema and indexes for block/TextUnit persistence.
