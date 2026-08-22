# EPUB Navigation Map Contract

> Status: Implemented P1 navigation-map foundation; consumed by the later structure-reconciliation stage
>
> Branch: `feat/epub-navigation-map`
>
> Related: `docs/epub-structure-reconciliation-contract.md`, `docs/adr/0003-epub-first-structure-reliability.md`, `docs/epub-structure-reliability-design.md`

## 1. Goal

The navigation-map stage separates publisher navigation facts from spine order and from canonical Section structure.

```text
EPUB package
├── manifest
├── spine
└── navigation
```

The map remains an independent provenance plane even though the subsequent `feat/epub-structure-reconciliation` stage now consumes it to upgrade canonical Section hierarchy at proven boundaries.

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
epub_spine_items
epub_navigation_map_version
epub_navigation_provenance
epub_navigation_source_path?
epub_navigation_nodes
epub_navigation_resolved_nodes
epub_navigation_diagnostics
epub_navigation_map
```

## 3. Package facts

The package parser records enough facts for navigation discovery and reconciliation:

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

`source_order` is deterministic pre-order within the chosen navigation source. It remains **navigation-source order**, not canonical spine/source order. The reconciliation stage is explicitly required to preserve this distinction.

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

A missing fragment may leave the publication readable while showing that the publisher navigation target lost fragment precision.

## 9. Fragment evidence

For supported XHTML/XML/HTML targets, fragment resolution checks actual `id`/legacy `name` anchors.

Spine XHTML already read for content parsing is indexed in a fragment cache so TOC validation does not re-read the same archive entry unnecessarily.

Unsupported top-level media such as SVG is currently reported as:

```text
unsupported_resource
```

This navigation stage does not claim reflowable-XHTML precision for SVG/fixed-layout/foreign content.

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

## 11. Relationship to canonical Section reconciliation

The navigation-map stage itself only produces navigation facts. The now-implemented reconciliation stage consumes them under an explicit boundary:

```text
spine / XHTML
→ existing canonical Section boundaries

navigation map
→ publisher labels / parent hierarchy / target evidence

reconciliation
→ apply stronger nav/NCX evidence only when it maps to a proven Section boundary
→ keep canonical sibling/root order in spine/source order
```

Consequences:

- `resolved_fragment` at an existing Section anchor can upgrade title/parentage;
- a DOM fragment that is not a Section boundary does not fabricate a Section;
- duplicate TOC targets remain aliases instead of duplicate text;
- unresolved/unsupported nodes remain diagnostic facts;
- navigation order never becomes source order by implication.

The canonical result is documented by `epub-structure-reconciliation/v1`.

## 12. Parsed Cache / normalization history

Navigation-map output originally advanced parser/cache policy to:

```text
reading-mcp-normalization/v2
```

because old Parsed Documents lacked navigation metadata.

The subsequent structure-reconciliation stage advances policy again to:

```text
reading-mcp-normalization/v3
```

because reconciliation may change addressing-relevant canonical Section facts.

The normalized hash contract remains:

```text
normalized-document-hash/v1
```

Its existing Section inputs naturally produce a different hash when reconciliation changes title/parent/level/order. The navigation map itself is metadata/provenance and is not directly injected into the hash.

## 13. Acceptance evidence

Navigation-map tests cover:

- EPUB 3 package version and `properties=nav` discovery;
- nested TOC hierarchy and deterministic navigation source order;
- resolved and missing fragments;
- malformed EPUB 3 nav → explicit NCX fallback;
- invalid archive-root escape;
- missing manifest target;
- unsupported SVG target;
- no-navigation readable degradation.

The later reconciliation tests additionally prove that the same map can upgrade canonical heading Sections at proven boundaries without conflating navigation order with spine order.

Release gate remains:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

## 14. Navigation-map non-goals

The map itself does not define:

```text
normalized XHTML block boundaries
Paragraph/Sentence segmentation
EPUB validator / complete coverage report
SVG/fixed-layout precise reading
SearchIndex policy
MCP Tool surface
```

Canonical reconciliation is a separate consumer and remains separately versioned from `epub-navigation-map/v1`.

## 15. Current downstream dependency

`feat/epub-structure-reconciliation` is now implemented. The next ADR 0003 dependency is:

```text
feat/normalized-block-model
```

Publisher/native block evidence must become persisted, addressing-relevant normalized facts before Paragraph/Sentence identity may depend on transient XHTML block boundaries.
