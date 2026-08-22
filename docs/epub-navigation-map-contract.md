# EPUB Navigation Map Contract

> Status: Implemented P1 navigation-map foundation
>
> Branch: `feat/epub-navigation-map`
>
> Related: `docs/adr/0003-epub-first-structure-reliability.md`, `docs/epub-structure-reliability-design.md`

## 1. Goal

The first EPUB reliability increment separates publisher navigation facts from spine order and from the current XHTML-heading-derived Section hierarchy.

```text
EPUB package
├── manifest
├── spine
└── navigation
```

This increment maps navigation faithfully enough for later structural reconciliation, but deliberately does not rewrite canonical `Document.root_sections` yet.

## 2. Versioned map

Current persisted parser fact:

```text
epub-navigation-map/v1
```

The map is serialized into `Document.metadata["epub_navigation_map"]` together with summary metadata. It is persisted through DocumentRepository as parser output and is not a second source store.

Current summary keys:

```text
epub_package_path
epub_package_version
epub_manifest_items
epub_spine_items_total
epub_spine_items                  # historical parsed-readable count
epub_navigation_map_version
epub_navigation_provenance
epub_navigation_source_path?
epub_navigation_nodes
epub_navigation_resolved_nodes
epub_navigation_diagnostics
epub_navigation_map
```

## 3. Package facts

The package parser records enough facts for navigation discovery and the next reconciliation increment:

```text
package version
manifest item id
manifest href
manifest media-type
manifest properties
manifest fallback?
spine itemref idref
spine toc reference?
```

Manifest, spine and navigation are not collapsed into one hierarchy.

## 4. EPUB 3 navigation discovery

EPUB 3 navigation is discovered from manifest metadata:

```text
manifest item
properties contains "nav"
```

No filename convention such as `toc.xhtml` is required.

The navigation document is read and parsed before generic HTML normalization can discard the `nav` element.

For a valid EPUB 3 navigation document, the parser selects the `toc` navigation and preserves nested ordered-list hierarchy.

Current provenance:

```text
epub_nav
```

If multiple manifest resources declare `nav`, the condition is diagnosed and the first valid TOC in deterministic manifest order is used.

## 5. Legacy NCX compatibility

If EPUB 3 TOC navigation is absent or unusable, the parser may fall back to legacy NCX.

NCX selection precedence:

```text
spine@toc manifest reference
↓
manifest item with media-type application/x-dtbncx+xml
```

NCX nodes use the same logical navigation-map shape but preserve:

```text
provenance = epub_ncx
```

Malformed EPUB 3 nav followed by valid NCX is a recoverable degradation, not a reason to relabel NCX as EPUB 3 navigation.

## 6. Navigation node contract

Each mapped navigation node contains:

```text
label
depth
href?
resolved_entry_path?
fragment?
source_order
provenance
resolution_status
diagnostic?
children[]
```

`source_order` is deterministic pre-order within the chosen navigation source. It is navigation-source order only; it is not yet canonical Section/source reading order.

Labels are constructed only from actual text nodes so XML element/text APIs cannot duplicate navigation labels.

## 7. Target resolution

Navigation targets are resolved through:

```text
href
→ navigation-document-relative archive path
→ archive-safe normalized entry path
→ package manifest resource
→ supported content profile
→ optional fragment lookup
```

Archive path handling rejects:

```text
absolute archive paths
backslash paths
archive-root escape via ..
percent-encoded archive-root escape
percent-decoded path separators
invalid percent escapes
```

External URI targets are outside the in-archive precise profile and are reported as unsupported rather than fetched.

## 8. Resolution states

Current v1 states:

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

The first six cover the minimum ADR 0003 resolution contract. `malformed_resource` and `unlinked` make additional degradation explicit instead of collapsing it into a generic parse failure.

A missing fragment may leave the publication readable while showing that the publisher navigation target lost fragment precision.

## 9. Fragment evidence

For supported XHTML/XML/HTML targets, fragment resolution checks actual `id`/legacy `name` anchors.

Spine XHTML already read for content parsing is indexed in a fragment cache so TOC validation does not re-read the same archive entry unnecessarily.

Unsupported top-level media such as SVG is currently reported as:

```text
unsupported_resource
```

This increment does not claim reflowable-XHTML precision for SVG/fixed-layout/foreign content.

## 10. Fatal vs recoverable behavior

Still fatal:

```text
invalid ZIP/container/package essential structure
empty spine
archive security/resource-budget violation
no readable supported spine content
```

Recoverable navigation degradation includes:

```text
missing/malformed EPUB 3 nav
missing/malformed NCX
missing fragment
missing navigation resource
unsupported target media
invalid navigation target path
navigation unavailable
```

Resource-limit failures remain fatal even when encountered while reading navigation or fragment evidence; reliability code cannot bypass archive budgets.

## 11. Canonical Section boundary in this increment

This PR intentionally preserves the historical Section construction:

```text
spine order
→ readable XHTML/XML resource
→ HtmlParser heading hierarchy
→ remapped EPUB Section ids/native locations
```

The publisher navigation map is persisted in metadata but does not yet modify:

```text
Document.root_sections
Section ids/parentage/order/content
Paragraph/Sentence TextUnits
TextLocator identity
normalized-document-hash/v1
```

That separation prevents this navigation-extraction increment from silently performing the next architectural step.

## 12. Parsed Cache invalidation

Adding the navigation map changes persisted Parser output even though Section addressing facts remain unchanged.

Therefore the parser/cache diagnostic version is bumped to:

```text
reading-mcp-normalization/v2
```

Parsed Cache identity remains:

```text
final_source + raw_sha256 + normalization_version
```

Old v1 Parsed Documents become cache misses, ensuring previously cached EPUBs are reparsed and receive navigation metadata.

The normalized source hash remains:

```text
normalized-document-hash/v1
```

because this increment does not change addressing-relevant Section facts. A future reconciliation that changes canonical Section hierarchy will naturally change normalized facts/hash outputs; a future persisted addressing-relevant block model may require a hash-contract version bump.

## 13. Acceptance evidence

Tests cover:

- EPUB 3 package version and `properties=nav` discovery;
- nested TOC hierarchy and deterministic source order;
- resolved and missing fragments;
- malformed EPUB 3 nav → explicit NCX fallback;
- invalid archive-root escape;
- missing manifest target;
- unsupported SVG target;
- no-navigation readable degradation;
- heading-derived Section hierarchy remains unchanged in this increment;
- prior normalization-version Parsed Cache entries miss after the v2 bump;
- existing EPUB/general format acceptance remains green.

Release gate:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 14. Explicit non-goals

This increment does not implement:

```text
nav/spine → canonical Section reconciliation
linear=yes/no traversal semantics
heading/spine fallback provenance on canonical Sections
persisted normalized XHTML block boundaries
EPUB reliability validator / complete coverage report
SVG/fixed-layout precise reading
Paragraph/Sentence segmentation changes
SearchIndex changes
new MCP Tools
```

## 15. Next dependency

The next independent increment is:

```text
feat/epub-structure-reconciliation
```

It may consume `epub-navigation-map/v1` and spine facts to establish canonical structural precedence:

```text
EPUB 3 toc nav
→ legacy NCX
→ XHTML heading fallback
→ spine-item fallback
```

That next increment must also make structural provenance and `linear=no` semantics explicit. It must not silently treat navigation order as spine/source reading order.
