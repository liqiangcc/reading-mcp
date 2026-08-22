# EPUB Structure Reconciliation Contract

> Status: Implemented P1 structure-reconciliation foundation
>
> Branch: `feat/epub-structure-reconciliation`
>
> Related: `docs/epub-navigation-map-contract.md`, `docs/adr/0003-epub-first-structure-reliability.md`

## 1. Goal

This increment turns the already-extracted EPUB navigation plane into canonical Section structure without conflating publisher navigation order with EPUB spine/source order.

```text
manifest        = publication resources
spine           = source / reading-order plane
EPUB nav / NCX  = publisher hierarchy / labels
XHTML headings  = fallback structural boundaries
```

The reconciliation result must preserve canonical readable text while selecting the strongest structural evidence that can be proven against an existing source boundary.

## 2. Structural precedence

The accepted precedence is now implemented:

```text
EPUB 3 toc nav
        ↓
legacy NCX
        ↓
XHTML heading
        ↓
spine-item fallback
```

Precedence means a stronger source may supply canonical title/parentage for a Section when it resolves to a real Section boundary. It does **not** mean navigation order replaces spine/source order.

## 3. Spine remains authoritative for source order

Canonical Section ordering is derived from parsed spine order and source order inside each spine resource.

```text
spine item 1 Sections
→ spine item 2 Sections
→ ...
```

Navigation can alter hierarchy/labels but cannot reverse canonical sibling/root ordering to match a malformed or differently ordered TOC.

A conflict is recorded as:

```text
navigation_order_conflicts_spine_order
```

The canonical tree still uses spine/source order.

## 4. Navigation maps only to proven Section boundaries

A navigation node may become structural authority only when its resolved target maps to an existing canonical Section boundary.

### Resolved heading fragment

```text
nav href → XHTML fragment
fragment == existing Section.location.anchor
→ navigation may supply canonical title/parentage
```

### Whole-document target

A `resolved_document` target maps to the first canonical Section in that content document.

### Missing fragment

A `missing_fragment` may degrade to the first canonical Section in the resolved document, with:

```text
navigation_missing_fragment_document_fallback
```

The lost fragment precision remains visible.

### Non-heading fragment

A fragment that exists in the XHTML DOM but does not correspond to an existing Section boundary does not fabricate a new Section:

```text
navigation_fragment_not_section_boundary
```

XHTML heading structure remains the canonical fallback until the later normalized-block model can represent finer native boundaries reproducibly.

## 5. No TOC wrapper Sections or duplicated text

The reconciliation layer does not create empty Sections just to mirror publisher navigation containers.

It also does not duplicate `Section.content` when multiple navigation nodes point at the same source target.

The first navigation node targeting a Section is the structural owner. Later nodes targeting the same Section are retained as navigation aliases in the structure map:

```text
navigation_aliases[]
duplicate_navigation_target
```

Canonical source text remains present exactly once.

## 6. Publisher labels and hierarchy

When a mapped nav/NCX node has a non-empty label, that label becomes the canonical Section title.

Navigation parentage may replace weaker heading parentage only when the mapped parent occurs before the child in canonical source order.

If a navigation parent would violate source ordering, reconciliation keeps the source-derived parentage and records:

```text
navigation_parent_conflicts_source_order
```

If a navigation parent/ancestor cannot be mapped to a canonical Section boundary, source heading parentage remains and the degradation is explicit:

```text
navigation_parent_unmapped
```

After successful navigation reparenting, `Section.location.section_path` and `Section.level` are recomputed from the final canonical tree. Original heading title/level remain available in structure provenance facts.

## 7. Structural provenance

Each reconciled Section has one effective provenance:

```text
epub_nav
| epub_ncx
| xhtml_heading
| spine_item
```

The structure map also retains:

```text
source_title
source_level
canonical_title
canonical_level
canonical_parent_id
navigation_source_order?
navigation_resolution_status?
navigation_aliases[]
```

This separates the original XHTML evidence from the final canonical structural decision.

## 8. `linear=no` semantics

Every spine item records:

```text
spine_index
idref
linear
manifest_href?
resolved_entry_path?
media_type?
parse_status
```

Current parse statuses:

```text
parsed
missing_manifest
unsupported_media
```

`linear=no` means auxiliary reading-order content; it does **not** mean nonexistent content.

Supported XHTML/XML with `linear=no` is parsed and remains addressable/searchable/readable in the canonical Document. The structure map records its `linear=false` status so future traversal policies can distinguish primary and auxiliary sequences without deleting source material.

## 9. Fallback behavior

### XHTML heading fallback

A parsed heading Section not upgraded by nav/NCX keeps:

```text
provenance = xhtml_heading
```

### Spine-item fallback

A supported XHTML resource with no heading still remains addressable through the generic HTML document Section and is recorded as:

```text
provenance = spine_item
```

This prevents readable spine content from disappearing solely because it lacks a heading hierarchy.

## 10. Versioned structure map

Current parser fact:

```text
epub-structure-reconciliation/v1
```

It is serialized into:

```text
Document.metadata["epub_structure_map"]
```

Summary metadata includes:

```text
epub_structure_map_version
epub_structure_sections
epub_structure_applied_navigation_nodes
epub_linear_spine_items
epub_non_linear_spine_items
epub_structure_diagnostics
```

The map is provenance/diagnostic state derived during parsing; canonical `Document.root_sections` remains the source-facing normalized structure.

## 11. Normalization and identity

This increment can change addressing-relevant canonical facts:

```text
Section.title
Section.parent_id
Section.level
Section.location.section_path
children/root hierarchy
```

Therefore Parsed Cache policy advances:

```text
reading-mcp-normalization/v2
→ reading-mcp-normalization/v3
```

Parsed-cache identity remains:

```text
final_source + raw_sha256 + normalization_version
```

The normalized hash algorithm/version remains:

```text
normalized-document-hash/v1
```

No new hash algorithm is needed because hash v1 already includes the canonical Section facts changed by reconciliation. If reconciliation changes title/parent/order/level, the resulting normalized hash changes naturally and old precise locators/cursors fail closed as stale.

## 12. Acceptance evidence

Tests cover:

- EPUB 3 publisher labels replacing XHTML heading titles only at proven boundaries;
- publisher navigation hierarchy reparenting Sections across spine documents;
- source text preserved without TOC wrapper/duplicate content;
- canonical root/sibling order remains spine order when navigation order conflicts;
- `linear=no` supported XHTML remains parsed and addressable;
- `linear` and spine parse status persisted in the structure map;
- non-heading DOM fragments do not fabricate canonical Sections;
- headingless readable XHTML gets `spine_item` fallback provenance;
- NCX can supply canonical structure while retaining `epub_ncx` provenance;
- normalization v2 Parsed Cache entries miss after the v3 upgrade;
- existing precise-read/TextUnit/search invariants remain governed by the resulting canonical Document.

Release gate:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 13. Explicit non-goals

This increment does not implement:

```text
persisted normalized XHTML block model
native block-kind/range identity
Paragraph/Sentence segmentation changes
EPUB full structural/range validator
complete EPUB coverage report
SVG/fixed-layout precise reading
primary-only traversal Tool/mode
SearchIndex changes
new MCP Tools
```

## 14. Next dependency

The next ADR 0003 increment is:

```text
feat/normalized-block-model
```

Now that EPUB Section hierarchy has a reliable source/provenance foundation, block evidence such as `p`, `blockquote`, `li`, `pre`, `table`, and headings can be persisted as addressing-relevant normalized facts before EPUB-native Paragraph/Sentence boundaries are allowed to depend on parser-native block structure.
