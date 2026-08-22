# ADR 0003: EPUB-First Structure Reliability

- Status: Accepted
- Date: 2026-08-21
- Reviewed branch: `design/epub-structure-reliability`
- Reviewed against main: `97490792ed1207ff27ed795fbc5f42138dc80784`
- Related design: `docs/epub-structure-reliability-design.md`
- Related locator architecture: `docs/adr/0002-text-index-locator-identity.md`
- Implementation status: navigation mapping, canonical structure reconciliation, normalized block persistence, and `epub-structure-validator/v1` are implemented. Current Paragraph/Sentence identity remains independently versioned under `text-segmentation/v1`; any block-aware identity migration is a later explicit decision.

## Context

Precise Paragraph/Sentence addressing is useful only when the structural parse above it is trustworthy. EPUB parsing now separates package/manifest, spine source order, publisher navigation, canonical Section reconciliation, persisted native body-block facts, and persisted-fact validation instead of treating XHTML headings alone as the final book hierarchy.

The EPUB reliability design identified the following required boundaries:

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

The validator now cross-checks each Section structure fact against the referenced spine row, including `spine_index` and `linear` consistency.

### 8. `linear=no` is auxiliary, not nonexistent

A non-linear spine item remains addressable publication content. It is not removed merely because it is auxiliary to the primary reading sequence.

Reading MCP distinguishes:

```text
linear=yes/omitted → primary reading sequence
linear=no          → auxiliary spine content
```

Supported `linear=no` XHTML/XML is parsed into the canonical Document and tagged as auxiliary in `epub-structure-reconciliation/v1`. Unsupported auxiliary spine content remains in the validator denominator as an explicit degradation rather than disappearing.

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

A fragment that exists in the DOM but is not an existing canonical Section boundary does not fabricate a Section. The persisted normalized block model preserves supported body-block boundaries for later validation and future explicitly versioned identity migration, but reconciliation still does not silently promote arbitrary block fragments into Sections.

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

The validator classifies these source/capability gaps as degradations when the persisted facts remain internally consistent.

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
ValidationReport               = persisted/rebuildable evidence
```

Canonical Section/Chapter text must not depend on reassembling Sentence rows. Current Paragraph/Sentence segmentation remains `text-segmentation/v1`; block-aware segmentation is a separate future migration.

### 13. Provenance is preferred over subjective confidence

Structural/boundary records use factual provenance and status rather than ungrounded numeric confidence scores.

Implemented provenance includes:

```text
epub_nav
epub_ncx
xhtml_heading
spine_item
xhtml_native_block
```

The reconciliation map retains original heading title/level separately from final canonical title/level/parent so the structural decision remains auditable. Normalized blocks retain native anchor/location when available.

### 14. Degradation must be visible

Fatal failures prevent a trustworthy readable Document, including essential container/package/spine failures, archive-security/resource-limit violations, and validator-detected internal contradictions among persisted facts.

Recoverable structural/capability failures preserve readable content and emit degradation evidence, including:

```text
missing/malformed nav
missing/malformed NCX
unresolved TOC fragment
navigation target not at a Section boundary
navigation order conflicting with spine order
unmapped navigation parent
unsupported auxiliary resource
nonempty Section without a preserved native block
native block boundaries differing from current text-segmentation/v1 Paragraphs
current v1 Sentence units overlapping native pre/table evidence
```

Reading MCP must not fabricate a clean native hierarchy or fake complete coverage to hide degraded input.

### 15. Reliability validator is part of the architecture

`epub-structure-validator/v1` is implemented and validates only persisted facts. It does not reopen the ZIP or reparse transient DOM state.

It composes deterministic evidence from:

```text
canonical Document / Sections
epub-navigation-map/v1
epub-structure-reconciliation/v1
normalized-block-model/v1
current deterministic Paragraph/Sentence materialization
```

It validates at least:

- navigation schema/order/depth/provenance/resolution evidence;
- spine indices, resolution facts, linear/non-linear semantics and denominators;
- Section ID uniqueness, valid parentage and acyclicity;
- structure-map vs canonical title/level/parent/native/spine facts;
- canonical source ordering and parent-before-child source order;
- normalized block owner/index/source-order/range invariants;
- EPUB-native block location provenance;
- `TextUnit.text == exact slice of Section.content`;
- Paragraph/Sentence ordinal, ownership and coverage partition invariants.

Findings use two factual severities:

```text
error       → persisted facts contradict a claimed invariant → integrity invalid → parser fails closed
degradation → source/capability coverage incomplete but facts truthful → readable Document survives
```

Validators report violations; they do not silently repair, rebase, reorder or search for replacement source text.

### 16. Coverage is evidence

The implemented validator persists factual counts rather than a vague confidence score. Coverage planes include:

```text
package / spine
navigation
canonical structure
normalized blocks
current Paragraph/Sentence TextUnits
```

Examples of denominators/counts:

```text
spine_items_total / parsed / missing_manifest / unsupported_media
navigation nodes by resolution status
fragment_targets_total / resolved
Sections by structural provenance
blocks by native kind
nonempty Sections with / without normalized blocks
Section content chars / block chars / separator-or-unmodeled chars
native blocks exact / non-exact with current Paragraph ranges
Paragraph/Sentence units and character partitions
native pre/table blocks overlapped by current v1 Sentence units
```

Coverage denominators remain separate; the implementation does not collapse these dimensions into a misleading single percentage.

## Implementation sequence / status

The EPUB reliability work remains separated into short-lived increments:

```text
P1 feat/epub-navigation-map                    ✓
P1 feat/epub-structure-reconciliation          ✓
P1 feat/normalized-block-model                 ✓
P1 feat/text-unit-index                        ✓
P1 feat/sentence-locator                       ✓
P1 feat/epub-structure-validator               ✓
```

Persisted/cache policy history:

```text
v2 → navigation-map parser output
v3 → canonical structure reconciliation
v4 → persisted normalized block output / HTML text normalization correction
v5 → persisted EPUB validation report / coverage evidence
```

`normalized-document-hash/v1` remains the current hash contract. Reconciliation changes its existing Section inputs naturally; block/validation metadata alone does not alter the current hash because current TextUnit identity remains `text-segmentation/v1`.

Any future block-aware segmentation migration must explicitly version identity inputs rather than silently changing existing locators.

SVG/fixed-layout precise-reading support remains a separate capability increment unless evidence proves it can share the XHTML reliability model without weakening invariants.

## Acceptance invariants

An implementation conforms only if all are true:

1. spine order is never conflated with TOC hierarchy;
2. EPUB 3 `toc nav` is used as primary navigation structure when available and valid;
3. legacy NCX/heading/spine fallback provenance is explicit;
4. navigation href/fragment resolution status is testable;
5. `linear=no` content is not silently lost;
6. unsupported SVG/fixed-layout/foreign content is visible in coverage;
7. reflowable XHTML precision is not falsely generalized to all EPUB content;
8. native body-block facts are persisted as exact canonical Section-relative ranges before future identity depends on them;
9. current Paragraph/Sentence TextUnits rebuild deterministically under their existing version;
10. every precise TextUnit is an exact canonical normalized slice;
11. non-prose/source gaps are not hidden to inflate sentence coverage;
12. parser degradation is observable and reproducible;
13. validator errors and degradations are distinct factual states;
14. validator can reproduce evidence from a persisted Document without source reparse;
15. navigation can change canonical hierarchy only at proven Section boundaries;
16. navigation order cannot reorder canonical sibling/root source order against the spine;
17. reconciliation does not duplicate canonical Section text for TOC aliases/wrappers;
18. normalized block ranges are exact owner slices and do not overlap/reorder within an owner;
19. nested native block selection does not duplicate the same source text;
20. block source order remains parser/spine source order and is not conflated with reconciled Section-tree traversal;
21. block/validation persistence alone does not silently mutate `text-segmentation/v1` / existing locator identity.

## Consequences

Positive:

- publisher EPUB structure, native body blocks and current TextUnit facts are now cross-validated from persisted evidence;
- source ordering remains auditable and independent from publisher TOC ordering;
- unsupported/malformed source areas remain visible in factual denominators;
- exact block/TextUnit ranges can be audited after repository recreation without source reparse;
- validator evidence identifies concrete mismatches to resolve before a block-aware TextUnit migration;
- malformed EPUBs can remain readable when degradation is truthful, while internal contradictions fail closed;
- PDF and other formats can later adopt the same evidence-vs-confidence model.

Costs:

- EPUB parsing includes a deterministic validation pass over persisted facts and current derived TextUnits;
- validation reports add parser metadata/cache-version churn without becoming source identity;
- coverage is multi-dimensional rather than a convenient but misleading single score;
- current Paragraph/Sentence segmentation still does not exploit persisted block evidence;
- a later identity migration requires explicit compatibility/staleness design.

## Review outcome

Accepted. Navigation mapping, canonical reconciliation, normalized block persistence, and persisted-fact validation are implemented as separate evidence-gated stages. The validator distinguishes internal integrity errors from readable source degradation and produces reproducible coverage without reparsing source. Any next block-aware TextUnit identity migration must be separately versioned and justified by this evidence rather than silently reinterpreting existing v1 locators.
