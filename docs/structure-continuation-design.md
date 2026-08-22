# Reading MCP — Actionable Structure Continuation Design

> Status: proposed bounded design for Phase 1
>
> Branch: `design/structure-continuation`
>
> Baseline reviewed: `main` at `2b7eafc3609198b09622dd79a1605e3ba789657a`
>
> Scope: UC-STRUCTURE-03 only. This document designs bounded, resumable structural enumeration. It does **not** redefine whole-document body/source reading order.

## 1. Decision summary

`get_document_structure` currently returns a nested Section tree with a hard 1,000-node response budget and only:

```text
truncated: bool
```

A valid document can contain more Sections than one response can carry. Therefore the current response is bounded but not actionable: once `truncated=true`, the caller has no canonical continuation state.

The accepted Phase 1 change is additive:

```text
get_document_structure
+ optional root_section_id
+ optional max_nodes
+ optional StructureCursor
→ deterministic bounded structural preorder pages
→ complete + next_cursor
→ existing nested SectionNode page-forest projection
```

The cursor is a progress token, not a source citation.

The Tool surface remains seven.

A second source-first finding is intentionally kept separate:

```text
structural preorder
!= necessarily publication/body source reading order
```

This is observable for EPUB because navigation may reparent Sections while spine/source order remains authoritative. Structure continuation therefore does not invent a `source_order` field or claim that its traversal can drive whole-book sentence reading. A separate bounded reading-order design is required before the whole-document sentence-reading acceptance is implemented.

---

## 2. Source-first findings

### 2.1 Current application behavior

Current `GetDocumentStructureUseCase`:

```text
Document.root_sections
→ recursive outline traversal
→ hard budget = 1,000 nodes
→ nested SectionOutline
→ truncated=true when budget is exhausted
```

Traversal is preorder over the canonical Section tree:

```text
root
→ child subtree
→ next sibling
```

When the budget reaches zero, traversal stops in the middle of that preorder.

There is no cursor, start position, total stream metadata, subtree root, or caller-selected page size.

### 2.2 Current MCP contract

Current request:

```text
document_id
max_depth?
```

Current response:

```text
document_id
sections[]       # nested SectionNode tree/forest
truncated
```

### 2.3 Existing cursor/error foundations can be reused as patterns

Reading MCP already has independent cursor state machines for read and TextUnit enumeration.

The existing pattern provides:

```text
versioned cursor schema
own prefix/domain
bounded encoded size
serialized claims
checksum envelope
stale vs mismatch vs invalid distinction
```

Existing stable application errors already cover the required taxonomy:

```text
INVALID_CURSOR
STALE_CURSOR
CURSOR_TARGET_MISMATCH
CURSOR_ENCODING_FAILED
```

Structure continuation must use its own cursor schema and must not reuse a ReadCursor or TextUnitCursor.

### 2.4 Canonical tree order is not a universal body-reading order

For EPUB, persisted reconciliation facts explicitly separate:

```text
spine/source order
from
publisher navigation hierarchy
```

The parser first assigns a monotonic Section `source_order` from spine/resource order, then navigation may change canonical parentage when the new parent precedes the child in source order. The rebuilt canonical tree sorts roots and direct children by source order, but a depth-first traversal can still differ from global source order after reparenting.

Example:

```text
source order: A(0), B(1), C(2)
canonical hierarchy after valid navigation reparenting:
A
└── C
B

structural preorder: A, C, B
source order:        A, B, C
```

Therefore this design freezes only the structural traversal contract.

---

## 3. Use case

### UC-STRUCTURE-03 — Expand an oversized structure

#### Actor goal

Eventually inspect every Section node in one requested structural scope despite response limits, without hidden loss or duplicate node identity.

#### Preconditions

- the document has been opened and exists in `DocumentRepository`;
- canonical `Document.root_sections` is internally valid;
- the requested subtree root, if any, exists.

#### Trigger

The requested structure scope contains more nodes than the current page budget.

#### Main success flow

```text
request structural scope
→ flatten that scope into one deterministic structural preorder stream
→ return bounded page [start,end)
→ complete=false + next_cursor
→ continue using cursor
→ ...
→ terminal page
→ complete=true + next_cursor=null
```

Every Section in the requested stream appears exactly once across the sequence of pages.

#### Failure flow

Fail closed when:

- document does not exist;
- requested subtree root does not exist;
- cursor cannot be decoded or has an invalid checksum/position;
- cursor belongs to another document/scope/depth mode;
- referenced source/normalized document identity changed;
- cursor schema/traversal contract is obsolete.

#### Degradation flow

Source/publication reliability degradation remains separate from structural response pagination. Structure continuation does not repair unresolved EPUB navigation targets or manufacture source nodes.

#### Success result

The Agent can prove the requested structural scope has been exhausted by:

```text
complete = true
next_cursor = null
```

and can reconstruct every returned node by `section_id` + `parent_id` without duplicates.

---

## 4. Structural scope

The request gains an optional subtree boundary:

```text
root_section_id?
```

### Whole-document scope

When omitted:

```text
scope roots = Document.root_sections
```

### Subtree scope

When present:

```text
scope root = exactly that Section
```

The selected root itself is the first item in the subtree stream.

A subtree request never implicitly includes siblings or ancestors outside that subtree.

---

## 5. Depth semantics

`max_depth` remains optional and keeps the existing whole-document meaning.

Depth is **relative to the requested structural scope**:

```text
whole document:
root Sections = depth 1

subtree request:
selected root = depth 1
```

This makes subtree expansion self-contained.

Compatibility rule for historical `max_depth=0`:

```text
0 is normalized to effective depth 1
```

This preserves the current observable result where root nodes are still returned and children are omitted.

The cursor binds the **effective** depth value.

Nodes deeper than the effective `max_depth` are outside the requested stream. Their omission is therefore not response truncation.

---

## 6. Traversal contract

Introduce a versioned traversal name:

```text
structure-preorder/v1
```

For the requested scope:

```text
visit node
→ recursively visit included children in canonical stored child order
→ next sibling
```

For whole-document scope, roots are visited in canonical `Document.root_sections` order.

This order is deterministic for one canonical Document and requested depth/scope.

It is a **structural enumeration order only**.

Never describe `structure-preorder/v1` as:

```text
publication reading order
EPUB spine order
body source order
TextUnit order across Sections
```

---

## 7. Page representation: page forest

The existing response field remains:

```text
sections: Vec<SectionNode>
```

To avoid a breaking response shape, pagination uses a **page-forest projection** of one contiguous preorder slice.

### 7.1 Flatten first, project second

Conceptually:

```text
requested structural scope
→ flat preorder entries [0..N)
→ select page [start,end)
→ project only those selected entries back into a nested page forest
```

A Section identity is serialized only once in the entire continuation sequence.

### 7.2 Parent inside the same page

If a node's parent is also inside the selected page slice, the node appears in that parent's `children`.

### 7.3 Parent was returned on an earlier page

If the selected page starts at a descendant whose parent is outside the current slice:

```text
that descendant becomes a top-level page-forest entry
```

but its real canonical:

```text
parent_id
```

is preserved.

This is not a claim that the node became a document root. It is only a page-serialization boundary.

The caller can reconstruct the complete tree across pages from stable `section_id` / `parent_id` identity.

### 7.4 No repeated ancestor scaffolding

Rejected design:

```text
repeat all ancestors on every continuation page
```

because it makes “every required node returned exactly once” false and forces clients to distinguish repeated context from newly enumerated nodes.

Page-forest projection keeps progress semantics exact.

---

## 8. Child completeness

Add an additive field to `SectionNode`:

```text
children_complete: bool
```

Meaning:

> All direct children of this node that belong to the **requested structural scope** are present in this page node's `children` array.

Examples:

```text
leaf node
→ true

max_depth stops below this node
→ true
because deeper nodes are outside requested scope

page ends before one or more in-scope direct children
→ false

continuation page starts at child C whose parent P was on prior page
→ C completeness is evaluated for C's own children
```

This field prevents a partial nested page from being mistaken for a complete subtree.

---

## 9. Request evolution

Additive logical request:

```text
document_id
root_section_id?
max_depth?
max_nodes?          # page budget
cursor?
```

### 9.1 `max_nodes`

Default:

```text
1000
```

Server hard maximum remains:

```text
1000
```

Rules:

```text
max_nodes = 0
→ INVALID_REQUEST

max_nodes > 1000
→ clamp to 1000
```

This matches the existing response-budget principle: callers may request a smaller page but cannot increase the server-owned hard response cap.

`max_nodes` is a page-size budget, not stream identity, and is therefore not bound into StructureCursor claims. A caller may use a different valid page size on continuation.

### 9.2 Initial request

Without cursor, `root_section_id` and `max_depth` define the structural stream.

### 9.3 Continuation request

With cursor:

- `document_id` remains required;
- `cursor` defines stored root/depth/traversal progress;
- `root_section_id` and `max_depth` may be omitted;
- if supplied, their normalized/effective values must match cursor claims;
- `max_nodes` may change within server limits.

This avoids requiring the Agent to manually reproduce every scope parameter while still failing closed on contradictory parameters.

---

## 10. Response evolution

Additive logical response:

```text
document_id
sections[]
truncated
complete
next_cursor?
stream {
  traversal_version
  root_section_id?
  max_depth?
  start_index
  end_index
  total_nodes
}
```

### 10.1 Completion

```text
complete = end_index == total_nodes
```

Invariant:

```text
complete == true
iff
next_cursor == null
```

### 10.2 `truncated` compatibility

Retain the historical field.

Define:

```text
truncated = !complete
```

relative to the requested structural scope.

A `max_depth` boundary does not by itself make the response truncated because excluded deeper nodes are outside the declared request scope.

For old requests that omit all new fields:

```text
max_nodes defaults to 1000
root scope = whole document
```

so existing behavior is preserved, except that an oversized response now additionally provides actionable `next_cursor`/`complete` metadata.

### 10.3 Stream indexes

Use zero-based half-open structural stream positions:

```text
[start_index, end_index)
```

Invariant across forward pages:

```text
page[n].end_index == page[n+1].start_index
```

No reverse structural traversal is introduced in v1.

---

## 11. StructureCursor

Introduce an independent cursor schema:

```text
structure-cursor/v1
```

Recommended transport prefix/domain pattern:

```text
sc1.
reading-mcp/structure-cursor-checksum/v1\0
```

Use the same bounded envelope/checksum discipline as existing cursor implementations, but no code/type is shared in a way that conflates stream contracts.

### 11.1 Required claims

```text
schema_version
structure_traversal_version = structure-preorder/v1
document_id
content_hash
normalized_document_hash
root_section_id?
effective_max_depth?
next_index
total_nodes
```

### 11.2 Why raw + normalized identity are both bound

```text
content_hash
= raw-source provenance/version evidence

normalized_document_hash
= addressing-relevant canonical normalized identity
```

Continuation must not splice structural pages across either changed source bytes or changed normalized structure.

### 11.3 What is not bound

Do not bind:

```text
max_nodes
```

because page size may safely change without redefining the requested stream.

Do not bind:

```text
MCP response serialization details
```

unless they later become part of structural stream identity.

---

## 12. Cursor validation and errors

Validation order should distinguish identity/state failures as follows.

### Invalid cursor

```text
bad prefix
malformed payload
checksum mismatch
next_index > total_nodes
total_nodes inconsistent with recomputed stream while source identity still matches
otherwise impossible cursor state
```

→ `INVALID_CURSOR`

### Stale cursor

```text
cursor schema version unsupported
structure traversal version unsupported
raw content_hash changed
normalized_document_hash changed
```

→ `STALE_CURSOR`

### Target/scope mismatch

```text
request document_id != cursor document_id
supplied root_section_id != cursor scope root
supplied effective max_depth != cursor effective max_depth
```

→ `CURSOR_TARGET_MISMATCH`

### Missing initial target

Initial `root_section_id` not found:

```text
SECTION_NOT_FOUND
```

No title matching or fuzzy subtree relocation is allowed.

---

## 13. Deterministic implementation model

A straightforward bounded implementation is acceptable because the parser already bounds total Section count.

Per request:

```text
load canonical Document
→ resolve requested scope
→ flatten scope refs into structure-preorder/v1 vector
→ compute current normalized document identity
→ decode/validate cursor when present
→ select [start,end) using effective max_nodes
→ project selected slice to page forest
→ compute per-node children_complete
→ emit next StructureCursor when needed
```

The implementation does not need to persist structure cursor rows.

StructureCursor is fully reconstructible from:

```text
canonical Document
+ deterministic structural traversal contract
```

---

## 14. No-gap / no-overlap proof

Automated acceptance must prove stream-level identity rather than merely count pages.

For each continuation sequence:

```text
concatenate section_id values in stream order across all pages
==
expected requested structural preorder exactly once
```

Required cases:

1. more than 1,000 root Sections;
2. page boundary inside a deep child subtree;
3. continuation page starting at a node whose parent was returned previously;
4. subtree-root scope;
5. `max_depth` boundary;
6. changing `max_nodes` between continuation requests;
7. terminal page with `complete=true` and no cursor;
8. stale raw/normalized identity;
9. wrong document/root/depth scope;
10. malformed/tampered/impossible cursor;
11. repository close/reopen with unchanged canonical Document;
12. real stdio MCP continuation;
13. runtime Tool count remains seven.

Also assert `children_complete` on both sides of a page boundary.

---

## 15. Compatibility

No existing request field changes meaning.

Existing clients may continue to call:

```text
get_document_structure(document_id, max_depth?)
```

They receive the same first-page nested structure shape and `truncated` semantics, plus additive fields.

No existing Section ID, parent ID, title, level, location, normalized identity, or parser fact changes are required by StructureCursor itself.

Therefore this design does not authorize:

```text
normalization version bump
normalized-document-hash version bump
Parsed Cache migration
DocumentRepository schema migration
new MCP Tool
```

---

## 16. Explicit separation from whole-document sentence reading

The sentence-reading closure requires a different question:

> After one owner Section's source-preserving TextUnit stream completes, which Section owns the next body content in canonical publication/source order?

StructureCursor does not answer that question.

### Why tree preorder is insufficient

EPUB can preserve:

```text
Section source order: A, B, C
```

while navigation evidence creates valid canonical hierarchy:

```text
A
└── C
B
```

A structural preorder page correctly returns:

```text
A, C, B
```

for hierarchy inspection, but a publication/source reading workflow must not silently infer that as:

```text
A body → C body → B body
```

when persisted EPUB evidence says source order is:

```text
A → B → C
```

### Required follow-up design

Before implementing the whole-document sentence-reading production acceptance, create a separate bounded design decision for a format-neutral Section reading-order projection.

That design must answer at least:

```text
what is the neutral application-level reading-order evidence?
how is it derived for non-EPUB formats?
how does EPUB project persisted spine/source-order facts without leaking parser types into Application?
how are linear=no / auxiliary resources represented?
how are unsupported source gaps kept in order?
what version identifies the reading-order policy?
how does the Agent transition from final item of Section N to the next readable Section?
```

Do not add `source_order` opportunistically to the StructureCursor DTO before this separate use case is designed.

---

## 17. Rejected alternatives

### Keep `truncated=true` only

Rejected because bounded output is not actionable continuation.

### Use Section ID as the continuation cursor

Rejected because a Section locator identifies source structure, not progress through a depth/scope/version-bound enumeration stream.

### Repeat ancestors on every page

Rejected because it duplicates enumerated Section identities and makes exact no-gap/no-overlap completion harder to prove.

### Flatten `sections` permanently and break nested clients

Rejected because the existing nested response remains useful and compatibility can be preserved with page-forest projection.

### Use EPUB `source_order` as StructureCursor ordering

Rejected because `get_document_structure` is structural navigation across all formats. Publisher/source body order and canonical hierarchy are separate responsibilities.

### Add `get_structure_page` as an eighth Tool

Rejected because this is continuation of the existing structural-navigation use case, not an independent responsibility.

### Persist cursor/server session state

Rejected because deterministic canonical Document state is sufficient to reconstruct traversal.

---

## 18. Implementation gate

After this design is accepted and merged, implementation should occur on a new short-lived branch, for example:

```text
feat/structure-continuation
```

Implementation is complete only when:

```text
Format success
Clippy -D warnings success
full Test success
real stdio continuation success
branch behind main = 0
final diff review passes
```

The implementation must remain limited to actionable structural continuation. The Section reading-order follow-up remains a separate design/implementation responsibility.
