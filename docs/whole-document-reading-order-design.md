# Whole-Document Section Body Reading Order Design

Status: accepted bounded design for whole-document sentence-first composition.

This design deliberately does not add an MCP Tool or a synthetic `document_complete` flag. It
defines the evidence and composition rules that an Agent uses with the existing seven Tools.

## 1. Problem and source-first findings

`get_document_structure` exposes a structural hierarchy. Its preorder is useful for outline
navigation, but it is not automatically the body reading order.

The current canonical facts are:

- `Document.root_sections` and each `Section.children` preserve the parser's canonical Section
  sequence.
- Markdown, text, HTML, DOCX, PDF, and OpenAPI parsers construct that sequence while consuming
  source records in source order; children and roots are rebuilt without title-based lookup.
- EPUB reconciliation has an explicit `epub-structure-reconciliation/v1` map. Every canonical
  Section has a flat spine `source_order`, while navigation hierarchy may assign a different
  parentage. Existing EPUB validation checks that these facts are complete, unique, and monotonic
  in spine order.
- `Section.content` is the body owned by that Section. It does not include descendants. Existing
  exact Section reads and TextUnit enumeration preserve this boundary.

Therefore a structural DFS is not a truthful whole-book order for EPUB. The body order must be
derived from source-order evidence and surfaced separately from structural preorder.

## 2. Body order contract

Introduce the additive `body-order/v1` evidence in the existing structure response:

```text
SectionNode.body_order: non-negative integer
StructureStreamSegment.body_order_version: "body-order/v1"
```

`body_order` is a global, zero-based order of body-owning canonical Sections in the requested
Document. It is not a structure index, navigation rank, citation offset, or TextUnit source order.
It is stable only under the existing raw and normalized document identity bindings.

The field is present only when the runtime can prove the order. If the evidence is absent or
invalid, the runtime must return the existing truthful application error/reliability degradation;
it must not synthesize an order from titles or fuzzy relocation.

The structure cursor continues to bind the document identities, structural scope, depth, and
preorder position. Body order is a projection on each returned node and does not change the
structure stream's traversal version or page boundaries.

## 3. Format-neutral reconstruction rule

The application derives a `BodySectionOrder` list containing each canonical Section exactly once.

1. For an EPUB document, decode the persisted `epub-structure-reconciliation/v1` map, require one
   fact per canonical Section, and sort facts by their validated flat `source_order`. This is the
   spine/source order, not the publisher navigation hierarchy. The validator's reliability result
   remains authoritative; an invalid or incomplete map cannot produce a completion claim.
2. For a non-EPUB document, use the canonical parser Section sequence: roots in stored order and
   each Section's own body before its stored children. This is a parser-owned source sequence, not
   a general tree traversal rule. The current parsers' source-sequence construction and existing
   format tests are the evidence for this v1 fallback.
3. Validate that the resulting list contains every canonical Section once, with no missing or
   duplicate Section identity. Section IDs, never titles, are the join key.

The order is used only to choose which Section body stream starts next. It does not change the
structural `children` relationship or reinterpret a Section's locator.

## 4. Whole-document composition state machine

For a requested document/scope, the upper layer performs:

```text
structure enumeration complete
  AND body-order Section list complete
  → for each Section in body_order:
      get_text_units(
        section_id = Section.id,
        requested_kind = sentence,
        coverage_policy = preserve_source,
        anchor/cursor as applicable
      ) until complete
  → next body_order Section
  → scope exhausted
```

The final completion claim is the conjunction of:

- structural/requested-scope enumeration is complete;
- every body-owning Section in the canonical body order was visited exactly once;
- every Section's preserve-source TextUnit stream reached `complete=true`;
- no stream reported an unsupported hidden gap for the requested completion scope;
- the document's reading-profile reliability remains truthful for the relevant format.

No state is persisted as a user reading session, and no synthetic document-complete bit is added.
The upper layer may save the last fully consumed item's `TextLocator`; after restart it reopens the
same document and resumes the exact Section stream from that locator, then uses this body order to
select the next Section after the final item.

## 5. Required edge semantics

- A Section with intro body and children is read as its own body first, then the next Section by
  `body_order`; structural descendants are never silently included in the parent's stream.
- An empty or title-only Section is still one visited body-owning Section. Its preserve-source
  stream may contain zero items but must return truthful complete/coverage evidence; it cannot be
  silently omitted.
- Parent-to-child and child-to-sibling transitions use Section IDs and body order, not duplicate
  titles or nearest text.
- Duplicate titles are ordinary; identity is `SectionId` plus the existing `TextLocator` facts.
- Native prose produces Sentence items. Native structural containers, preformatted/code, tables,
  and other regions without reliable sentence identity remain coarse Paragraph items under
  `preserve_source`.
- `eligible_only` may intentionally skip coarse regions but never participates in a whole-document
  completion claim as if the Section were source-complete.
- Unsupported EPUB publication resources remain reliability degradations. Canonical Section body
  completion must not be relabelled as publication 100% coverage.

## 6. Version, identity, and non-goals

This design adds `body-order/v1` as an evidence/schema label only. It does not bump the existing
normalization, TextUnit, locator, cursor, lexical, or EPUB validator versions. A change to the
canonical Section source order remains visible through the existing raw/normalized identity
bindings; no historical locator or cursor is rebased.

Non-goals are a new reading-session Tool, a database checkpoint, navigation-order reading, fuzzy
resume, synthetic document completion, or treating structure preorder as a universal body order.

## 7. Required implementation evidence

The implementation PR must prove:

- parent intro body followed by child body;
- empty and title-only Sections are accounted for;
- child-to-sibling transition and duplicate titles use exact IDs;
- an EPUB whose navigation order conflicts with spine order reads in spine body order;
- fallback Sections remain in canonical source order;
- native Sentence and coarse Paragraph regions preserve source accounting;
- multi-Section preserve-source traversal has no hidden gap or duplicate body region;
- final Section completion selects no further Section;
- saved TextLocator resumes the exact next item after repository/runtime restart;
- raw or normalized identity change is stale;
- stdio evidence exercises the existing seven-Tool composition.
