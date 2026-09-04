# Named-section Boundary / No-lookahead Scope Gate Design

> Status: bounded design for Issue #69
>
> Baseline: `main` at `31972fecaf48baa52128c3a7d991727df60ac3d8`
>
> Scope: structure-only named-section resolution and executable no-lookahead boundaries. This design does not add reading-session state, AI reasoning, lexical fallback, or a format-specific public Tool.

## 1. Problem

Paper Reading Lab needs to establish a reading scope before any body SourceUnit is revealed:

```text
planned_scope = named Section
→ resolve canonical structural boundary
→ inspect the owner/order of the next canonical unit
→ if outside scope: STOP before body reveal
```

The real Raft 2014 USENIX PDF demonstrates a capability gap. Its stable current source identity is:

```text
document_id: doc:sha256:6b910bccce5cabc0f7e14f4c131c361edc055fb5b0703b0a1aac2049a379bbdf
content_hash: sha256:e6345fcba31cbc747ab41755aa62654859c4403dbb687da0021079f78181a7b5
normalized_document_hash: sha256:bced1dc57972b784215245749745ab33d34267463a451384c9372aa8e145432f
normalization_version: reading-mcp-normalization/v6
segmentation_version: text-segmentation/v2
```

A live `get_document_structure` call returns exactly sixteen `Page N` nodes. The PDF parser currently uses native PDF TOC entries when present and otherwise promotes each page to one canonical `Section`. Therefore the canonical tree contains no `1 Introduction` structural node for this source.

Using `search_document` to compensate is invalid: search is lexical derived state and returns snippets. Boundary discovery is a control-plane operation and must not reveal future body text.

The previously contaminated Raft ReadingSession remains abandoned. This Issue repairs the capability for a future fresh session; it does not reinterpret that failed session as successful.

## 2. Use case

### UC-STRUCTURE-NAMED-BOUNDARY-01 — Resolve a named structural scope without revealing body text

Preconditions:

- the document has been opened and exists in the canonical `DocumentRepository`;
- caller retained the identity returned by `open_document`;
- the requested name is intended to identify a source structural node, not an arbitrary body occurrence.

Success flow:

```text
(document identity + "1 Introduction")
→ match only canonical Section metadata
→ return matched structural node metadata
→ return executable body-order boundary metadata
→ return zero Section.content / Paragraph / Sentence / search snippets
```

The caller can then enumerate only owner Sections admitted by the boundary. At a Section transition, the caller compares the next owner Section's canonical body order to the boundary before calling `get_text_units` / `read_document` for that owner.

Failure/degradation:

- stale identity: fail closed;
- ambiguous structural match: return `ambiguous` plus metadata-only candidates;
- no structural match: return `not_found`;
- source has only coarse/page structure and no trustworthy named headings: return `unavailable`, never invoke lexical search implicitly;
- an executable boundary cannot be represented from canonical body-order facts: return `boundary_unavailable`, never guess.

## 3. Responsibility decision

Keep the current public Tool surface. Extend `get_document_structure` additively instead of adding a new Tool.

Reason:

```text
get_document_structure = structural navigation / structural metadata
search_document        = lexical discovery
get_text_units          = body TextUnit enumeration
read_document           = explicit body reveal
```

Named-section resolution is a new mode of the existing structural-navigation responsibility, not an independent source of truth.

No `resolve_pdf_section`, `search_heading`, or Paper Reading Lab-specific Tool is introduced.

## 4. Canonical PDF structure decision

### 4.1 Evidence priority

PDF canonical section construction uses the following evidence priority:

```text
valid native PDF TOC / outline
→ use existing native-TOC structure path

else conservative deterministic heading extraction succeeds
→ build canonical heading Sections

else
→ preserve current Page N fallback
```

Native TOC remains stronger evidence than inferred headings.

### 4.2 Why parser-level heading extraction is required

A metadata overlay that leaves `Section 1` and `Section 2` inside the same `Page N` owner cannot satisfy the strict gate. `get_text_units` returns body text and its locator together. If a future heading is still inside the same canonical owner Section, the caller cannot discover that ownership crossing before the body item is revealed.

Therefore a trustworthy inferred heading boundary must become a canonical `Section` ownership boundary.

### 4.3 Conservative v1 heading inference

The first fallback is deterministic, non-LLM, format-generic, and intentionally conservative. It recognizes coherent numbered heading lines, for example:

```text
1 Introduction
2 Replicated state machines
3.1 Replicated state machines
```

A candidate must satisfy bounded structural rules, including:

- standalone/line-like text, not an arbitrary substring match;
- a short heading-sized line, not a paragraph;
- a numeric section designator followed by a non-empty title;
- valid bounded hierarchy depth;
- coherent document-level numbering evidence rather than one isolated numeric line;
- deterministic source order;
- no use of SearchIndex, BM25, snippets, LLMs, OCR, or document-specific title lists.

The implementation may reject uncertain candidates. False negative + explicit page fallback is preferred over manufacturing false source structure.

This is deliberately not a general typography/OCR heading classifier. Future richer font/layout evidence requires a separate design when justified by real sources.

### 4.4 Multi-column PDFs

The fallback operates on deterministic parser text lines and structural numbering, not lexical search ranking. Real Raft evidence is mandatory because it is a multi-column conference PDF. If the current extraction stream cannot produce coherent heading lines for Raft without unsafe heuristics, implementation must stop with a blocker rather than special-case Raft.

## 5. Normalization and migration

Canonical `Section` identity/title/content/parentage participates in `normalized_document_hash`. Changing a no-TOC PDF from page-owned Sections to heading-owned Sections is therefore an addressing-relevant normalization change.

Implementation must:

```text
reading-mcp-normalization/v6
→ reading-mcp-normalization/v7
```

The normalized-document hash algorithm/version may remain `normalized-document-hash/v2`; the normalized content being hashed changes because the canonical Document changes.

Consequences:

- Parsed Cache keys change through `normalization_version`;
- a fresh open reparses affected PDFs;
- document_id/raw content_hash remain tied to the same source bytes;
- normalized_document_hash changes where canonical structure changes;
- old TextLocator / cursor identity fails closed against the new normalized hash;
- no fuzzy or ordinal/snippet rebase is permitted.

## 6. Named-section resolution

### 6.1 Request evolution

Add optional fields to `get_document_structure`:

```text
named_section_query?
expected_content_hash?
expected_normalized_document_hash?
expected_structure_resolution_version?
```

Rules:

- historical structure enumeration calls remain unchanged;
- when `named_section_query` is supplied, `expected_content_hash` and `expected_normalized_document_hash` are required;
- mismatched expected identity is a stale structural request and fails closed;
- a supplied unsupported structure-resolution version is stale/unsupported, not fuzzily upgraded.

Version:

```text
named-section-resolution/v1
```

### 6.2 Matching source

Resolution examines only canonical `Section` metadata already loaded from `DocumentRepository`:

```text
section_id
parent_id
title
level
location / section_path
body_order
```

It MUST NOT inspect `Section.content`, TextUnit text, SearchIndex rows, search snippets, or body previews to decide a match.

### 6.3 Deterministic normalization

Comparison normalization is deliberately small and versioned by the resolver contract:

- Unicode-aware trim/collapse internal whitespace;
- ASCII case-fold for the literal optional prefix `Section`;
- normalize common separator punctuation around section number/title (`-`, `–`, `—`, `:`) to spacing;
- preserve title words otherwise; no synonym/fuzzy similarity.

Supported query intents:

1. exact structural title: `1 Introduction`;
2. optional display prefix: `Section 1 Introduction`;
3. title-only: `Introduction`, matched after stripping one leading numeric section designator from a canonical title.

Match precedence:

```text
exact normalized full title
> normalized optional-Section-prefix full title
> title-only normalized match
```

Only candidates in the strongest non-empty class participate. If more than one candidate remains, result is `ambiguous`; the resolver never picks one by body order, search score, or proximity.

### 6.4 Result shape

Add metadata-only resolution state to the structure response:

```text
resolution? {
  version
  status: resolved | ambiguous | not_found | unavailable | boundary_unavailable
  query
  match_kind?
  matched? {
    section_id
    parent_id?
    title
    level
    location
    body_order
    start_locator       # Section-level TextLocator; no normalized body range
  }
  candidates[]          # same structural metadata, no body text
  boundary? {
    version: named-section-boundary/v1
    body_order_version
    intervals[]         # zero-based half-open body-order ranges [start,end)
    end_exclusive?      # metadata for first body owner after scope when unambiguous
  }
  degradation?
}
```

The normal `sections[]` structure page remains available for compatibility. A caller that only needs named resolution may request a small `max_nodes`; resolution itself does not depend on lexical search.

No response field contains Section body content or a lexical snippet.

## 7. Executable boundary semantics

A named structural node scopes that node plus its canonical descendant subtree.

Map all scope member Sections to `body-order/v1`, sort their body-order indexes, and compress them into one or more half-open intervals:

```text
[start_0, end_0)
[start_1, end_1)
...
```

This representation is executable even when canonical hierarchy preorder and body source order differ (notably EPUB): the caller can test a prospective owner Section's `body_order` against the intervals before revealing its body.

For the common contiguous case:

```text
Section 1 scope body orders = [k, m)
next owner body order       = m
→ out of scope
→ STOP before get_text_units(next_owner)
```

`end_exclusive`, when present, is metadata-only information for the first body owner immediately after the final interval. It is not a body preview.

Resource rule: if the interval representation would exceed the existing bounded structure-response budget, return `boundary_unavailable` rather than truncating an executable boundary silently.

## 8. Source identity returned with the artifact

A successful or ambiguous resolution response binds/returns enough audit identity to reconstruct what was resolved:

```text
document_id
content_hash
normalized_document_hash
normalized_document_hash_version
normalization_version
segmentation_version
structure traversal version
body_order_version
named-section-resolution/v1
named-section-boundary/v1 (when executable)
```

The Section-level `start_locator` reuses canonical `TextLocator`; it does not create a second text identity.

## 9. Error semantics

Introduce an explicit stale structure-resolution error code rather than mislabeling a structure request as a stale text locator:

```text
STALE_STRUCTURE
```

Use it for expected raw/normalized identity or resolution-version mismatch.

Ambiguous/not-found/unavailable are normal resolution outcomes, not transport errors, because the caller needs structured metadata/degradation without guessing.

Existing `StructureCursor` stale/mismatch behavior remains unchanged.

## 10. No-body-leak invariant

Automated tests must serialize the structure-only response and prove it does not contain known body-only sentinel strings from any matched/future Section.

Implementation rule:

```text
named resolution path
→ Document Section metadata only
→ never call SearchIndex
→ never call TextUnitIndex
→ never copy Section.content into resolution DTO
```

Body remains explicit:

```text
get_text_units / read_document
→ body may appear
```

## 11. Required tests

Automated coverage:

1. exact named section match;
2. normalized heading match;
3. `Section` prefix + number + title;
4. title-only lookup;
5. ambiguous title-only lookup;
6. not found;
7. page-only document returns explicit unavailable/not-found structural result without lexical fallback;
8. stale raw identity;
9. stale normalized identity;
10. stale resolution version;
11. native-TOC PDF regression;
12. no-TOC PDF with coherent numbered headings becomes heading-owned canonical Sections;
13. uncertain/no-heading PDF preserves Page N fallback;
14. multi-column real Raft source;
15. serialized no-body-leakage sentinels;
16. scope interval proves last Section-1 item → Section-2 owner is out of scope before Section-2 body call;
17. existing EPUB/HTML/Markdown structure navigation regression;
18. existing get_document_structure continuation/cursor regression.

## 12. Real Raft acceptance

Using the existing URL (no Web search):

```text
https://www.usenix.org/system/files/conference/atc14/atc14-paper-ongaro.pdf
```

Evidence must prove:

### A — source open

Stable raw identity is recorded; after normalization v7 the normalized hash may intentionally differ from the v6 baseline and must be recorded exactly.

### B — Section 1 resolve

`1 Introduction`, `Section 1 Introduction`, and `Introduction` resolve structurally to the same canonical node without body snippets.

### C — scope gate

The final allowed SourceUnit owner is within the Section 1 boundary intervals. The next canonical body owner is outside them. The test/driver stops before calling a body-reveal API for Section 2.

### D — explicit body reveal

Structure lookup output contains no future body. Body text appears only after an explicit `get_text_units`/`read_document` call made for an allowed owner in a separate evidence step.

### E — stale identity

The v6 normalized hash or another wrong expected identity fails with `STALE_STRUCTURE`; no rebase occurs.

### F — regression

Kafka PDF plus representative EPUB/HTML/Markdown/native-TOC PDF structure flows remain green.

The Evidence log must not paste Raft future body text into Issue comments. Record metadata, IDs, hashes, statuses, body-order intervals, and PASS/FAIL only.

## 13. Rejected alternatives

### Reduce `search_document` snippets

Rejected. Search remains lexical derived state and cannot become structural truth.

### Resolve headings from arbitrary body text in the application use case

Rejected. That creates a second parser/structure interpretation above canonical Document facts.

### Metadata-only heading overlay while body ownership stays Page N

Rejected for strict no-lookahead. A future heading inside the same Page owner can only be discovered after a body-bearing TextUnit is enumerated.

### Raft-specific heading rules

Rejected. The parser must be generic and conservative.

### New `resolve_structure` MCP Tool

Rejected for v1. Existing `get_document_structure` already owns structural navigation and can evolve additively.

### Fuzzy title matching

Rejected. Ambiguity/not-found must be explicit and reproducible.

## 14. Implementation sequence

After this design PR is reviewed and merged:

```text
fresh feat/issue-69-named-section-boundary from latest main
→ normalization v7 decision in code/docs
→ conservative PDF heading fallback
→ metadata-only named resolver + executable boundary
→ MCP DTO/error mapping
→ unit/integration/regression tests
→ real Raft Action evidence
→ Format + Clippy -D warnings + full Test
→ independent diff review
→ merge exact CI-green head
→ deploy reviewed main according to repository deployment protocol
→ production reading-mcp real Raft structure-only acceptance
→ Issue #69 [EXECUTION REPORT]
→ paper-reading-lab#1 recovery note + [SESSION HANDOFF]
→ STOP; Raft strict reading restarts only in a fresh conversation
```

## 15. Completion gate

Issue #69 is complete only when all are true:

```text
named section resolves from canonical structure
+ no structure response body leakage
+ executable body-order boundary
+ pre-reveal scope crossing proof
+ identity/version binding and stale fail-closed
+ real Raft 2014 Evidence PASS
+ existing format/navigation regression PASS
+ docs updated
+ reviewed/merged/deployed GitHub evidence
```
