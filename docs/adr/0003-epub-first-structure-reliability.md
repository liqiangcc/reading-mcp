# ADR 0003: EPUB-First Structure Reliability

- Status: Accepted
- Date: 2026-08-21
- Reviewed branch: `design/epub-structure-reliability`
- Reviewed against main: `97490792ed1207ff27ed795fbc5f42138dc80784`
- Related design: `docs/epub-structure-reliability-design.md`
- Related locator architecture: `docs/adr/0002-text-index-locator-identity.md`
- Implementation status: `feat/epub-navigation-map`, `feat/epub-structure-reconciliation`, and `feat/normalized-block-model` are implemented. The next EPUB reliability increment is the structure/range/coverage validator. Paragraph/Sentence TextUnit foundations remain independently versioned.

## Context

Precise Paragraph/Sentence addressing is useful only when the structural parse above it is trustworthy. EPUB parsing now separates package/manifest, spine source order, publisher navigation, canonical Section reconciliation, and persisted native body-block facts instead of treating XHTML headings alone as the final book hierarchy.

The EPUB reliability design was reviewed against the current parser and the W3C EPUB 3.3 Recommendation. The review identified the following required boundaries:

1. spine order and table-of-contents hierarchy are distinct concerns;
2. EPUB 3 navigation must be discovered from manifest metadata and parsed before generic HTML noise removal can discard `nav` elements;
3. legacy NCX is a compatibility source, not EPUB 3 navigation provenance;
4. `linear="no"` spine items remain publication/spine content but are auxiliary to the primary reading order;
5. EPUB permits XHTML and SVG top-level content, and fixed-layout/pre-paginated publications need a different reliability claim from reflowable XHTML prose;
6. fallback structure must never be presented as native publisher structure;
7. parser reliability needs measurable structural/text coverage, not a single parse-success bit.

## Decision

### 1. EPUB is the first precise-book reliability target

Reading MCP uses EPUB as the first book format on which to prove reliable machine-readable structure, Paragraph/Sentence addressability, and validator coverage.

This does not make EPUB the universal source format and does not reduce support for HTML/Markdown/DOCX/PDF. It establishes implementation priority for structured books:

```text
EPUB structured-book reliability
        ↓
other structured text formats
        ↓
PDF/layout inference reliability
```

### 2. EPUB 3.3 is the normative baseline

Implementation behavior for EPUB 3 is based on the W3C EPUB 3.3 Recommendation dated 2026-01-13. A Working Draft such as EPUB 3.4 may inform future work but does not silently redefine the baseline.

### 3. Manifest, spine, and navigation are separate planes

Reading MCP treats these as distinct facts:

```text
manifest   = publication resources
spine      = ordered top-level content references / reading-order plane
navigation = named publisher navigation hierarchy
```

The spine MUST NOT be interpreted as the TOC hierarchy.

### 4. Structural precedence

Navigation/structure evidence is applied in this order:

```text
EPUB 3 toc nav
        ↓
legacy NCX, when applicable
        ↓
XHTML heading structure
        ↓
spine-item fallback node
```

A weaker source may supplement missing detail but cannot overwrite provenance and pretend to be a stronger source.

The implemented reconciliation applies this precedence only when stronger navigation evidence maps to a real canonical Section boundary. It never creates fake Sections merely to mirror a TOC node.

### 5. EPUB 3 navigation is first-class input

The parser discovers the EPUB navigation document through package/manifest metadata, including the `nav` resource property, rather than by filename convention.

The `toc nav` hierarchy is the primary publisher navigation hierarchy. Each navigation node preserves at least:

```text
label
hierarchy/depth
href
resolved entry path
fragment
source order
provenance = epub_nav
resolution status
```

The navigation document is parsed for structure before generic HTML normalization removes `nav` elements.

### 6. Legacy NCX is compatibility provenance

When a legacy NCX is used, its nodes populate the same logical navigation contract but retain:

```text
provenance = epub_ncx
```

NCX is never labeled as EPUB 3 navigation structure.

### 7. Spine order remains authoritative for source order

The spine determines deterministic top-level content ordering.

For each spine reference, reconciliation records:

```text
spine_index
idref
linear status
manifest resource
resolved content path
media type
parse status
```

Malformed or differently ordered navigation cannot reorder canonical sibling/root source content contrary to resolved spine/document order. Such conflicts are diagnostic facts.

### 8. `linear=no` is auxiliary, not nonexistent

A non-linear spine item remains addressable publication content. It is not removed merely because it is auxiliary to the primary reading sequence.

Reading MCP distinguishes:

```text
linear=yes/omitted → primary reading sequence
linear=no          → auxiliary spine content
```

Supported `linear=no` XHTML/XML is currently parsed into the canonical Document and tagged as auxiliary in `epub-structure-reconciliation/v1`. A future traversal contract may allow primary-only navigation without deleting the source.

### 9. Navigation targets must be resolved, not merely parsed

A TOC href is precise only after resolution through:

```text
href
 → archive-safe entry path
 → publication/content resource
 → fragment target, when present
 → document/DOM position
```

Resolution state distinguishes:

```text
resolved_document
resolved_fragment
missing_fragment
missing_resource
unsupported_resource
invalid_path
malformed_resource
unlinked
```

A missing fragment may degrade to a document-level target if the containing resource is still trustworthy, and the loss of precision remains visible.

A fragment that exists in the DOM but is not an existing canonical Section boundary does not fabricate a Section. The persisted normalized block model now preserves supported body-block boundaries for later validation and future explicitly versioned identity migration, but reconciliation still does not silently promote arbitrary block fragments into Sections.

### 10. EPUB precise-reading support is capability-graded

The first precise-reading profile is:

```text
reflowable XHTML EPUB
```

EPUB may also contain SVG top-level content, foreign-content fallback chains, and fixed-layout/pre-paginated items. These must not inherit the reflowable-XHTML precision claim automatically.

Until specialized handling exists:

- unsupported SVG/fixed-layout/foreign content is represented as an explicit coverage gap or coarse supported unit;
- fallback traversal, when supported, retains provenance;
- no Paragraph/Sentence precision is claimed for content whose reading order/text structure cannot be deterministically normalized.

### 11. Native block structure precedes sentence segmentation

Within supported XHTML content, meaningful block structure is preserved before Paragraph/Sentence segmentation.

The implemented `normalized-block-model/v1` persists these body-block kinds as exact Section-relative ranges:

```text
p           → paragraph
blockquote  → blockquote
li          → list_item
pre         → preformatted
table       → table
```

Heading evidence remains canonical `Section.title/id/parent/level/location` rather than a fake body block because heading label text is not part of current `Section.content`.

Block ranges are generated while rendering the same normalized text that becomes `Section.content`; they are not recovered later by text search. The current flat model uses a maximal non-overlapping body-block projection so nested selected blocks do not duplicate source text.

Per ADR 0002, persisted native block facts may affect future persistent TextUnit identity only through an explicit identity migration. Current `text-segmentation/v1` and `normalized-document-hash/v1` do not silently reinterpret existing Paragraph/Sentence locators from the new block map.

### 12. Paragraph and Sentence remain derived TextUnits

EPUB-first reliability does not change the source-truth model:

```text
Section.content                 = canonical normalized text
NormalizedBlockMap             = persisted canonical normalization evidence
Paragraph/Sentence TextUnits    = persisted/rebuildable derived state
FTS/BM25                        = rebuildable retrieval state
```

Canonical Section/Chapter text must not depend on reassembling Sentence rows. Current Paragraph/Sentence segmentation remains `text-segmentation/v1`; block-aware segmentation is a separate future migration, not part of the block-persistence increment.

### 13. Provenance is preferred over subjective confidence

Structural/boundary records use factual provenance and status rather than ungrounded numeric confidence scores.

Implemented structural provenance includes:

```text
epub_nav
epub_ncx
xhtml_heading
spine_item
xhtml_native_block
```

The reconciliation map retains original heading title/level separately from final canonical title/level/parent so the structural decision remains auditable. Normalized blocks retain native anchor/location when available.

### 14. Degradation must be visible

Fatal failures prevent a trustworthy readable Document, including essential container/package/spine failures and archive-security/resource-limit violations.

Recoverable structural failures preserve readable content but emit diagnostics/coverage, including:

```text
missing/malformed nav
missing/malformed NCX
unresolved TOC fragment
navigation target not at a Section boundary
navigation order conflicting with spine order
unmapped navigation parent
unsupported auxiliary resource
intentional sentence-split skip for non-prose
```

Reading MCP must not fabricate a clean native hierarchy to hide degraded input.

### 15. Reliability validator is part of the architecture

Precise-reading readiness requires deterministic validation of:

- package/spine resolution;
- Section ID uniqueness and acyclic parentage;
- structural/source order;
- claimed native target resolution;
- provenance correctness;
- normalized block owner/index/source-order/range invariants;
- normalized range bounds;
- `TextUnit.text == exact slice of Section.content`;
- Paragraph/Sentence ownership and source order.

`Document::validate_normalized_block_map()` already validates the block-local invariants without repairing source state. The full EPUB validator remains the next dedicated increment and will compose package/navigation/reconciliation/block/TextUnit evidence rather than hiding validation inside parsing.

### 16. Coverage is evidence

EPUB parsing must be able to report structural/textual coverage dimensions such as:

```text
spine items total / parsed
navigation nodes total / resolved / applied
fragment targets total / resolved
content documents total / parsed
normalized blocks represented
paragraph units represented
sentence units represented
non-prose units intentionally skipped
unsupported SVG/fixed-layout/foreign content
```

Coverage denominators must be well-defined and not mix prose eligibility with unsupported/non-prose content in misleading percentages.

## Implementation sequence / status

The EPUB reliability work remains separated into short-lived increments:

```text
P1 feat/epub-navigation-map                    ✓
   - package version/properties
   - EPUB 3 nav discovery/hierarchy
   - archive-safe target/fragment resolution diagnostics
   - legacy NCX fallback provenance
   - persisted epub-navigation-map/v1 parser fact

P1 feat/epub-structure-reconciliation          ✓
   - nav/NCX → proven Section boundary mapping
   - spine-authoritative canonical source order
   - XHTML heading/spine fallback
   - structural provenance
   - linear/non-linear semantics
   - persisted epub-structure-reconciliation/v1 facts

P1 feat/normalized-block-model                 ✓
   - persisted normalized-block-model/v1 body-block kinds/ranges
   - exact Section.content slice ownership
   - HTML/EPUB native location provenance
   - nested non-overlap projection
   - SQLite DocumentRepository reopen evidence
   - no silent TextUnit identity migration

P1 feat/text-unit-index                        ✓
   - deterministic Paragraph units from persisted canonical state

P1 feat/sentence-locator                       ✓
   - deterministic Sentence segmentation

P1 feat/epub-structure-validator               next
   - package/navigation/reconciliation/block structural validators
   - range/provenance/TextUnit invariants
   - coverage diagnostics
```

Parsed Cache policy advanced in evidence-gated parser increments:

```text
v2 → navigation-map parser output
v3 → canonical structure reconciliation
v4 → persisted normalized block output / HTML text normalization correction
```

`normalized-document-hash/v1` remains the current hash contract. Reconciliation changes its existing Section inputs naturally; block metadata alone does not alter the current hash because `text-segmentation/v1` does not consume block identity yet. A future block-aware segmentation migration must explicitly version its identity inputs rather than silently changing existing locators.

SVG/fixed-layout precise-reading support is a separate capability increment unless pre-research proves it can share the XHTML reliability model without weakening invariants.

## Acceptance invariants

An implementation conforms only if all are true:

1. spine order is never conflated with TOC hierarchy;
2. EPUB 3 `toc nav` is used as primary navigation structure when available and valid;
3. legacy NCX/heading/spine fallback provenance is explicit;
4. navigation href/fragment resolution status is testable;
5. `linear=no` content is not silently lost;
6. unsupported SVG/fixed-layout/foreign content is visible in coverage;
7. reflowable XHTML precision is not falsely generalized to all EPUB content;
8. native body-block facts are persisted as exact canonical Section-relative ranges before any future identity contract depends on them;
9. current Paragraph/Sentence TextUnits rebuild deterministically under their existing version;
10. every precise TextUnit is an exact canonical normalized slice;
11. non-prose blocks are not force-split to inflate sentence coverage;
12. parser degradation is observable and reproducible;
13. validator/coverage evidence exists before claiming precise-reading reliability;
14. navigation can change canonical hierarchy only at proven Section boundaries;
15. navigation order cannot reorder canonical sibling/root source order against the spine;
16. reconciliation does not duplicate canonical Section text for TOC aliases/wrappers;
17. normalized block ranges are exact owner slices and do not overlap/reorder within an owner;
18. nested native block selection does not duplicate the same source text;
19. block source order remains parser/spine source order and is not conflated with reconciled Section-tree traversal;
20. block persistence alone does not silently mutate `text-segmentation/v1` / existing locator identity.

## Consequences

Positive:

- publisher-provided EPUB hierarchy contributes directly to canonical Sections when targets are provable;
- source ordering remains auditable and independent from publisher TOC ordering;
- native XHTML body-block evidence now survives parser and repository recreation;
- exact block ranges can be validated without reconstructing offsets from snippets;
- precise Sentence locators inherit a stronger structural foundation without an implicit identity migration;
- malformed EPUBs can remain readable without creating false precision;
- EPUB2 compatibility, CJK prose, auxiliary content, and unsupported media can be tested explicitly;
- PDF can later adopt the same provenance/coverage model while acknowledging its greater inference burden.

Costs:

- EPUB/HTML parsing is more than a thin text flattener;
- canonical structural facts can change across normalization versions and therefore intentionally stale prior locators;
- the v1 block projection is flat; nested block-tree identity is intentionally deferred;
- current Paragraph/Sentence segmentation does not yet exploit persisted block evidence;
- validator and complete coverage remain dedicated follow-up work;
- precise-reading claims remain capability-specific rather than a single boolean.

## Review outcome

Accepted. Navigation mapping, canonical reconciliation, and normalized body-block persistence are implemented as separate evidence-gated stages. Publisher hierarchy only changes proven Section boundaries; spine/source ordering remains authoritative; native blocks are persisted as exact ranges without silently redefining current TextUnit identity. The next step is the EPUB structure/range/provenance/coverage validator, followed by any separately versioned block-aware TextUnit identity migration only when validator evidence supports it.
