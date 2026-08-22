# Reading MCP Use-Case-First Tool Contract Design

> Status: Accepted design proposal
>
> Branch: `design/tool-contract-use-cases`
>
> Scope: MCP/application contract design only. This document does not authorize Rust runtime, parser, index, storage, or schema changes on this branch.

> Historical design baseline: current runtime has now implemented the accepted seventh
> Tool `get_text_units`, TextLocator handoff, Structure/Discovery continuation and
> whole-document composition. Verify current facts in `docs/requirements.md`.
>
> Related: `docs/design-principles.md`, `docs/adr/0002-text-index-locator-identity.md`, `docs/adr/0003-epub-first-structure-reliability.md`

## 1. Goals

Reading MCP is designed for an AI Agent that must complete reading tasks, not for a database administrator or parser developer. Tool success is therefore not the success criterion. A use case succeeds only when the Agent can prove that the reading goal was completed.

The design method is normative:

```text
User / Agent Goal
        ↓
Use Case
        ↓
Success / Failure / Degradation
        ↓
Required Capability
        ↓
Interaction / State Machine
        ↓
MCP Tool Contract
```

The reverse method is rejected:

```text
existing Tool
        ↓
attach scenarios to justify it
```

This design has five goals:

1. make complete, ordered, budget-safe reading possible at Section, Paragraph, and Sentence levels;
2. make every discovered passage directly transferable to read/context without copying snippets and searching again;
3. keep canonical source locators distinct from continuation/progress cursors;
4. expose structural reliability, degradation, and coverage without fabricating precision;
5. evolve the current runtime additively and preserve valid Section-based clients.

### 1.1 Source-first baseline

The design was checked against the branch at `e55fc203aa4c6e184b71125a2022140c31a4b762`.

Current runtime facts:

- six MCP Tools are exposed: `list_documents`, `open_document`, `get_document_structure`, `search_document`, `get_context`, and `read_document`;
- `read_document` accepts a `section_id`, renders that Section and descendants, and returns only `truncated` when a character budget cuts the response;
- `get_context` expands flattened neighboring Sections only;
- `get_document_structure` has a 1,000-node response bound and only a `truncated` flag;
- `search_document` returns an owning Section plus a legacy `Location`, not a version-bound fine-grained `TextLocator`;
- `open_document` returns raw `content_hash`, but not normalized-document identity, capability grading, or reliability coverage;
- existing tests verify the six-Tool coarse reading loop, response limits, persistence, and multi-format acceptance, but not continuation, TextUnit enumeration, stale locators, or precise locator handoff.

These are implementation facts, not constraints on the final Tool surface.

### 1.2 Non-goals

This design does not:

- implement `ReadCursor`, `TextUnit`, sentence segmentation, EPUB navigation, FTS changes, or SQLite schema changes;
- introduce AI summaries, explanations, tutoring, notes, claims, or semantic concepts into Reading MCP;
- define format-specific Tools;
- turn Sentence rows into canonical source storage;
- define Word or model Token as a stable source-addressing level;
- require fuzzy remapping of stale locators;
- freeze physical DTO encoding, cursor serialization, database schema, or parser algorithms before implementation prototypes.

## 2. Actors

### 2.1 Primary Actor

| Actor | Goal | Subgoal | Success condition |
|---|---|---|---|
| AI Agent / LLM client | Reliably read and reason over the same source as the user | discover, open, navigate, enumerate, read, search, expand context, cite, and detect degradation | the requested source content is consumed or located with explicit completion, stable handoff, and no silent gaps, overlap, or false precision |

The Agent is not expected to understand repository internals, SQLite rows, parser heuristics, or rendered-response offsets.

### 2.2 Supporting Actors

| Actor | Responsibility | Not a success criterion |
|---|---|---|
| Human user | supplies the reading goal or source and evaluates the Agent's answer | a Tool returning HTTP 200 or JSON-RPC success |
| MCP host/client | invokes Tools, preserves structured arguments/results, and enforces protocol behavior | choosing source identity or repairing stale locators |
| Deployment operator | configures roots, credentials, budgets, and supported capabilities | manually reconstructing missing source ranges for the Agent |
| Source publication | provides text, native structure, source order, and provenance | having a perfectly formed EPUB/HTML/PDF |

### 2.3 Goal model

```text
Read a document reliably
├── discover an allowed source
├── open one concrete document version
├── inspect structure and reliability
├── select a structural target
├── consume the target in source order
│   ├── Section stream
│   ├── Paragraph stream
│   └── Sentence-first stream
├── locate evidence through search
├── move directly from evidence to read/context
├── preserve stable citation identity
└── detect stale state and explicit degradation
```

A response such as `read_document succeeded` is not goal completion. For a long Section, success requires repeatedly continuing until the stream is complete and proving no gap or overlap.

## 3. Use Case Catalog

| Family | IDs | User/Agent goal |
|---|---|---|
| Discovery | UC-DISCOVER-01 | find currently readable documents |
| Open/version | UC-OPEN-01..04 | establish a concrete readable document version and its capability/reliability profile |
| Structure | UC-STRUCTURE-01..04 | navigate the whole book and expand bounded structure without hiding fallback |
| Ordered Section reading | UC-READ-SECTION-01..04 | consume a Section subtree completely under response budgets |
| Paragraph reading | UC-PARAGRAPH-01..03 | enumerate, read, and navigate Paragraph TextUnits |
| Sentence-first reading | UC-SENTENCE-01..06 | start at the first eligible Sentence and advance in source order until the Section is complete |
| Search | UC-SEARCH-01..06 | locate structural titles, terms, definitions, and explanations, then hand the result directly to canonical read/context |
| Context | UC-CONTEXT-01..05 | request neighbor, container, or structural context with unambiguous semantics |
| Citation | UC-CITE-01..04 | save and later resolve exact, version-bound source evidence with native provenance |
| Stale state | UC-STALE-01..03 | reject or explicitly resolve locators/cursors/hits after relevant document/index changes |
| Non-prose | UC-NONPROSE-01..04 | preserve code/table readability without fabricating Sentences or false coverage failures |
| Reliability/coverage | UC-RELIABILITY-01..04 | know whether structure and precise-reading coverage are native, fallback, partial, or unsupported |

## 4. Detailed Use Cases

Each record uses the same normative template. “Failure” means the requested contract cannot be truthfully completed. “Degradation” means useful reading survives with explicitly reduced precision.

### 4.1 Discovery

#### UC-DISCOVER-01 — Discover currently readable documents

- **Actor Goal:** identify allowed candidate documents that can be opened.
- **Preconditions:** deployment policy may expose one or more discovery scopes; discovery is not required for an already-known public URL.
- **Trigger:** the Agent has a reading goal but no concrete source.
- **Main Success Flow:** enumerate allowed candidates in deterministic order; return enough source metadata to choose one; continue when a result budget is reached.
- **Alternative Flow:** the Agent already has a source and skips discovery.
- **Failure Flow:** requested scope is outside policy, unreadable, or invalid.
- **Degradation Flow:** no discovery provider is configured; return an empty complete result with capability information rather than implying all sources are absent.
- **Success Result:** the Agent can select a concrete `source` or prove that the configured scope has no matching candidates.
- **Required Information:** path/source, display name, media type, size when available, completion state.
- **Required Capability:** `DocumentDiscovery` and bounded pagination.
- **Produced State / Locator / Cursor:** source handle and optional `DiscoveryCursor`; no `DocumentId` yet.
- **Allowed Next Operations:** open a selected source or continue discovery.
- **Acceptance Criteria:** deterministic, policy-safe enumeration; truncation is actionable; discovery does not parse/open documents.

### 4.2 Open and document version

#### UC-OPEN-01 — Open a document for the first time

- **Actor Goal:** establish one concrete document version that can be navigated and read.
- **Preconditions:** source is known and allowed; retriever/parser resources are within budget.
- **Trigger:** the Agent selects or receives a source.
- **Main Success Flow:** validate, retrieve, parse, persist canonical normalized facts, build/rebuild derived indexes, and return version identity plus capability/reliability summary.
- **Alternative Flow:** an already cached but valid raw/parsed representation is reused.
- **Failure Flow:** blocked source, retrieval failure, fatal parse failure, or resource-budget failure.
- **Degradation Flow:** readable content survives but structure or precise-reading capability is partial; return the document with explicit diagnostics.
- **Success Result:** a concrete `DocumentId` is available and the Agent knows what navigation/precise-reading claims are safe.
- **Required Information:** source, raw `content_hash`, `normalized_document_hash`, media type, title, capability and reliability summary.
- **Required Capability:** `DocumentOpenAndVersionResolution`, `ReliabilityInspection`, `CoverageInspection`.
- **Produced State / Locator / Cursor:** version-bound document identity; no reading cursor.
- **Allowed Next Operations:** structure, search, Section read, and supported precise operations.
- **Acceptance Criteria:** successful open never hides partial structure; source and normalized identity remain distinct.

#### UC-OPEN-02 — Reopen identical source/content

- **Actor Goal:** reuse the same source version without creating ambiguous duplicate identity.
- **Preconditions:** the source was opened previously and current source bytes/normalized facts are unchanged.
- **Trigger:** the Agent calls open again or resumes a workflow.
- **Main Success Flow:** revalidate according to source policy, reuse deterministic identity and valid persisted state, and report the reuse outcome.
- **Alternative Flow:** derived search indexes are rebuilt while source identity remains unchanged.
- **Failure Flow:** persisted state is corrupt and cannot be rebuilt from source/canonical facts.
- **Degradation Flow:** cached reading remains available while a nonessential derived index is temporarily unavailable; report the missing capability.
- **Success Result:** the Agent can safely reuse locators scoped to the unchanged normalized identity.
- **Required Information:** previous/current raw and normalized identities, cache/revalidation outcome.
- **Required Capability:** `DocumentOpenAndVersionResolution`, `FreshnessValidation`.
- **Produced State / Locator / Cursor:** same document version; old locators remain valid when all identity inputs match.
- **Allowed Next Operations:** any operation supported by the advertised profile.
- **Acceptance Criteria:** no silent identity fork; index rebuild alone does not renumber TextUnits.

#### UC-OPEN-03 — Same source content changes

- **Actor Goal:** open the new source without confusing it with saved evidence from the old version.
- **Preconditions:** the source was previously opened; raw bytes or addressing-relevant normalized facts now differ.
- **Trigger:** normal revalidation or `force_refresh` observes change.
- **Main Success Flow:** create/resolve the new version identity, preserve explicit provenance, and optionally report the superseded version relationship.
- **Alternative Flow:** raw bytes are unchanged but normalization changes; normalized identity changes even if the legacy raw-derived `DocumentId` policy has not yet evolved.
- **Failure Flow:** new content cannot be parsed and no truthful readable new version can be produced.
- **Degradation Flow:** new version is readable with lower structural precision; old version is not silently substituted.
- **Success Result:** new reads target the new version, while old locators cannot resolve as if they referenced it.
- **Required Information:** old/new raw hash, old/new normalized hash, normalization/segmentation versions.
- **Required Capability:** `DocumentOpenAndVersionResolution`, `FreshnessValidation`.
- **Produced State / Locator / Cursor:** new version identity; old locator/cursor state is version-scoped.
- **Allowed Next Operations:** re-navigate or re-search the new version; explicitly read an old retained version when available.
- **Acceptance Criteria:** no fuzzy or “closest sentence” remapping; version change is observable.

#### UC-OPEN-04 — Readable document with degraded precise-reading capability

- **Actor Goal:** continue useful reading while knowing which precision guarantees are unavailable.
- **Preconditions:** canonical text can be produced, but native TOC, TextUnit segmentation, block support, or coverage is incomplete.
- **Trigger:** open/validation encounters recoverable structural limitations.
- **Main Success Flow:** return readable document identity, capability grades, provenance, coverage, and permitted fallbacks.
- **Alternative Flow:** coarse Section/page reading is fully available while Sentence operations are unavailable.
- **Failure Flow:** the system labels degraded content as fully precise or fabricates native structure.
- **Degradation Flow:** use a weaker but explicit structure/text unit with the reason and uncovered regions accounted for.
- **Success Result:** the Agent can choose a safe workflow and explain limitations.
- **Required Information:** capability profile, provenance, resolution status, eligible/unsupported coverage.
- **Required Capability:** `CapabilityAdvertisement`, `ReliabilityInspection`, `CoverageInspection`.
- **Produced State / Locator / Cursor:** document identity plus graded capability state.
- **Allowed Next Operations:** supported coarse/precise calls only; inspect structure/coverage.
- **Acceptance Criteria:** degradation is machine-readable and never converted into false precision.

### 4.3 Structure navigation

#### UC-STRUCTURE-01 — View the whole-book table of contents

- **Actor Goal:** understand the publication's navigable hierarchy without loading full body text.
- **Preconditions:** document is open and structural facts exist at some reliability grade.
- **Trigger:** the Agent needs an overview or target selection.
- **Main Success Flow:** return structural nodes in deterministic source/navigation order with stable Section identity and provenance.
- **Alternative Flow:** a shallow first response is followed by bounded expansion.
- **Failure Flow:** document identity is unknown or canonical structure is internally invalid.
- **Degradation Flow:** return heading/page/spine fallback nodes with explicit provenance and unresolved targets.
- **Success Result:** the Agent can identify candidate Chapter/Section/Subsection targets and judge structure quality.
- **Required Information:** IDs, parentage, title, level, order, native location, provenance, child completeness.
- **Required Capability:** `StructuralNavigation`, `ReliabilityInspection`.
- **Produced State / Locator / Cursor:** Section locators and optional `StructureCursor`.
- **Allowed Next Operations:** expand, select, read, enumerate TextUnits, or search.
- **Acceptance Criteria:** no body-sized response; no TextUnit explosion; fallback is visible.

#### UC-STRUCTURE-02 — Locate Chapter, Section, or Subsection

- **Actor Goal:** resolve a human structural target to one stable source target.
- **Preconditions:** structure is available or a structure search hit exists.
- **Trigger:** the Agent selects a title/path such as `§1.1`.
- **Main Success Flow:** choose an exact structural node using ID/path/order and retain its location/provenance.
- **Alternative Flow:** duplicate labels are disambiguated by hierarchy and source order.
- **Failure Flow:** no exact target exists or the selected target is unresolved.
- **Degradation Flow:** target resolves only to a containing spine/resource/page node; precision loss is reported.
- **Success Result:** one Section target is ready for read/enumeration.
- **Required Information:** structure path, node ID, target resolution state, owning document version.
- **Required Capability:** `StructuralNavigation`, `LocatorHandoff`.
- **Produced State / Locator / Cursor:** Section-level locator.
- **Allowed Next Operations:** Section read, TextUnit enumeration, context, or scoped reasoning.
- **Acceptance Criteria:** duplicate titles never require guessing by snippet text.

#### UC-STRUCTURE-03 — Expand an oversized structure

- **Actor Goal:** eventually inspect all relevant structural nodes despite response limits.
- **Preconditions:** full structure exceeds depth/node budget.
- **Trigger:** response indicates incomplete children or an Agent requests expansion.
- **Main Success Flow:** continue/expand from a stable structural boundary until the requested subtree is complete.
- **Alternative Flow:** request one selected root subtree rather than paginate the whole book.
- **Failure Flow:** cursor is malformed, stale, or bound to a different document/root.
- **Degradation Flow:** unsupported nodes remain visible as coarse entries with coverage gaps.
- **Success Result:** every required node is returned once in deterministic order, or an explicit unsupported gap remains.
- **Required Information:** root identity, child completeness, continuation state, source order.
- **Required Capability:** `StructuralNavigation`, `SequentialContinuation`.
- **Produced State / Locator / Cursor:** `StructureCursor`, not a source citation.
- **Allowed Next Operations:** continue, expand another node, select/read.
- **Acceptance Criteria:** bounded responses are actionable and gap/overlap tests cover pagination.

#### UC-STRUCTURE-04 — Detect incomplete native TOC and fallback

- **Actor Goal:** know whether displayed hierarchy is publisher-native or inferred/fallback.
- **Preconditions:** EPUB/native structure extraction was attempted.
- **Trigger:** structure is requested or used for a precision claim.
- **Main Success Flow:** expose provenance and resolution status per relevant node plus aggregate coverage.
- **Alternative Flow:** native hierarchy is complete and no fallback is required.
- **Failure Flow:** fallback is mislabeled as native or unresolved targets disappear.
- **Degradation Flow:** retain readable fallback hierarchy and explicit unresolved/native gaps.
- **Success Result:** the Agent can qualify citations/navigation claims correctly.
- **Required Information:** `epub_nav`/`epub_ncx`/heading/spine provenance, resolution status, coverage counts.
- **Required Capability:** `ReliabilityInspection`, `CoverageInspection`, `NativeTraceability`.
- **Produced State / Locator / Cursor:** reliability evidence associated with structural locators.
- **Allowed Next Operations:** coarse/precise reading according to advertised capability.
- **Acceptance Criteria:** provenance is factual; no subjective confidence score is required.

### 4.4 Complete ordered Section reading

#### UC-READ-SECTION-01 — Read a short Section completely

- **Actor Goal:** consume the selected Section subtree in source order in one response.
- **Preconditions:** document and Section target are valid.
- **Trigger:** the Agent requests canonical Section reading.
- **Main Success Flow:** return the complete logical Section stream with source identity/location and `complete=true`.
- **Alternative Flow:** exact Section-only mode may be introduced separately from legacy subtree mode, but mode is explicit.
- **Failure Flow:** document/Section not found or target is stale.
- **Degradation Flow:** unsupported child resources are represented as explicit coverage gaps rather than silently omitted.
- **Success Result:** all content in the declared read mode is consumed once.
- **Required Information:** target, read mode, returned stream/range metadata, completion state.
- **Required Capability:** `PreciseRead`.
- **Produced State / Locator / Cursor:** target locator; no next cursor when complete.
- **Allowed Next Operations:** analyze, cite, request context, or choose next Section.
- **Acceptance Criteria:** completion means the logical target, not merely the returned response, is exhausted.

#### UC-READ-SECTION-02 — Read an oversized Section

- **Actor Goal:** begin reading a Section whose logical stream exceeds the response budget.
- **Preconditions:** valid target; bounded server budget.
- **Trigger:** initial read cannot return the whole stream.
- **Main Success Flow:** return the first deterministic segment plus actionable `ReadCursor` and `complete=false`.
- **Alternative Flow:** the Agent chooses a smaller budget or a finer TextUnit workflow.
- **Failure Flow:** response reports truncation without continuation.
- **Degradation Flow:** unsupported source regions are explicitly accounted for in coverage.
- **Success Result:** the Agent can resume exactly after the returned segment.
- **Required Information:** stream mode/version, target identity, returned stream boundary, next cursor.
- **Required Capability:** `PreciseRead`, `SequentialContinuation`.
- **Produced State / Locator / Cursor:** opaque version-bound `ReadCursor`; never a citation locator.
- **Allowed Next Operations:** continue the same read stream.
- **Acceptance Criteria:** cursor binding includes raw/normalized identity and read/rendering mode.

#### UC-READ-SECTION-03 — Continue until the Section subtree is consumed

- **Actor Goal:** repeatedly consume every remaining segment.
- **Preconditions:** a valid `ReadCursor` from the immediately compatible stream exists.
- **Trigger:** prior response has `complete=false`.
- **Main Success Flow:** resume, return the next non-overlapping segment, and repeat until `complete=true` with no next cursor.
- **Alternative Flow:** restart from the source target using a new stream.
- **Failure Flow:** cursor is stale, tampered with, wrong-mode, or wrong-target.
- **Degradation Flow:** none for cursor identity; mismatch fails closed. Source coverage gaps remain separately reported.
- **Success Result:** the full declared Section stream is consumed.
- **Required Information:** cursor binding and stream completion state.
- **Required Capability:** `SequentialContinuation`, `FreshnessValidation`.
- **Produced State / Locator / Cursor:** next `ReadCursor` or terminal completion.
- **Allowed Next Operations:** continue or finish/analyze.
- **Acceptance Criteria:** finite progression; no restart-from-zero requirement; stale cursor never auto-rebases.

#### UC-READ-SECTION-04 — Verify no gap or overlap

- **Actor Goal:** prove continuation preserved exact ordered coverage.
- **Preconditions:** a multi-response read was performed.
- **Trigger:** validation, testing, or high-assurance reading.
- **Main Success Flow:** compare declared stream boundaries/sequence and reconstruct the logical stream exactly once.
- **Alternative Flow:** server-side tests validate cursor invariants without exposing implementation offsets.
- **Failure Flow:** duplicated/missing content or cursor sequence disagreement.
- **Degradation Flow:** explicitly unsupported source regions are counted as known gaps, not hidden transport gaps.
- **Success Result:** complete ordered coverage is auditable.
- **Required Information:** read-stream identity, ordered segment metadata, coverage diagnostics.
- **Required Capability:** `SequentialContinuation`, `CoverageInspection`.
- **Produced State / Locator / Cursor:** validation evidence; no new source locator.
- **Allowed Next Operations:** accept completion or retry/reopen on failure.
- **Acceptance Criteria:** property tests prove concatenated segments equal the declared full stream.

### 4.5 Paragraph reading

#### UC-PARAGRAPH-01 — List Paragraphs in a Section

- **Actor Goal:** obtain source-ordered Paragraph TextUnits without loading an unbounded Section.
- **Preconditions:** Paragraph segmentation is available for the document version.
- **Trigger:** the Agent selects a Section and requests Paragraph granularity.
- **Main Success Flow:** return a bounded ordered page of exact Paragraph text plus `TextLocator` for each item and continuation/completion state.
- **Alternative Flow:** start after/before a known Paragraph locator.
- **Failure Flow:** segmentation unavailable, target stale, or cursor invalid.
- **Degradation Flow:** return a coarser readable Section/block representation only when explicitly declared; never invent Paragraph precision.
- **Success Result:** Paragraphs can be consumed in source order with actionable pagination.
- **Required Information:** owner Section, normalized identity, segmentation version, ranges, source order.
- **Required Capability:** `OrderedTextUnitEnumeration`, `LocatorHandoff`.
- **Produced State / Locator / Cursor:** Paragraph `TextLocator`s and optional `TextUnitCursor`.
- **Allowed Next Operations:** read exact Paragraph, navigate, context, cite, continue.
- **Acceptance Criteria:** every returned Paragraph is an exact owner-Section slice; pagination is gap/overlap free.

#### UC-PARAGRAPH-02 — Read a specified Paragraph

- **Actor Goal:** re-read one exact Paragraph found by enumeration/search/citation.
- **Preconditions:** a valid Paragraph locator exists.
- **Trigger:** the Agent passes the locator to canonical read.
- **Main Success Flow:** validate identity and return the exact normalized slice plus provenance.
- **Alternative Flow:** enumeration already returned the exact text, so no extra read is required unless revalidation is desired.
- **Failure Flow:** stale/invalid locator fails closed.
- **Degradation Flow:** no fuzzy replacement; a coarse fallback requires an explicit new request.
- **Success Result:** exact source passage is reproduced.
- **Required Information:** full locator identity and normalized range.
- **Required Capability:** `PreciseRead`, `FreshnessValidation`.
- **Produced State / Locator / Cursor:** same resolved Paragraph locator.
- **Allowed Next Operations:** context, cite, adjacent enumeration.
- **Acceptance Criteria:** returned text exactly equals the canonical range.

#### UC-PARAGRAPH-03 — Navigate previous/next Paragraph

- **Actor Goal:** move one or more Paragraphs relative to a known Paragraph.
- **Preconditions:** anchor Paragraph locator is valid.
- **Trigger:** the Agent requests before/after traversal.
- **Main Success Flow:** enumerate from the anchor in the requested direction; return items in source order with boundary/completion state.
- **Alternative Flow:** request neighbor context instead of traversal when only a local window is needed.
- **Failure Flow:** anchor stale or not part of the target Section.
- **Degradation Flow:** at document/Section boundary return an empty complete result, not an error.
- **Success Result:** the Agent knows the adjacent Paragraph and whether a boundary was reached.
- **Required Information:** anchor, direction, owner Section, order.
- **Required Capability:** `OrderedTextUnitEnumeration`.
- **Produced State / Locator / Cursor:** adjacent Paragraph locators and optional cursor.
- **Allowed Next Operations:** continue, read, context, cite.
- **Acceptance Criteria:** previous/next is deterministic and does not depend on text search.

### 4.6 Sentence-first precise reading

#### UC-SENTENCE-01 — Start from the first eligible Sentence

- **Actor Goal:** begin precise reading at the first prose Sentence of a selected Section.
- **Preconditions:** Section is valid; deterministic sentence capability is advertised.
- **Trigger:** the Agent requests Sentence granularity with no anchor/cursor.
- **Main Success Flow:** enumerate from Section start and return the first bounded source-ordered items; the first eligible prose item is a Sentence with a locator.
- **Alternative Flow:** leading non-prose is returned as an explicit coarser reading item under source-preserving policy before the first Sentence.
- **Failure Flow:** sentence segmentation unavailable or target stale.
- **Degradation Flow:** return Paragraph/coarse items for unsupported/non-prose regions with `effective_kind` and reason; do not fabricate a Sentence.
- **Success Result:** the Agent knows the first precise Sentence and all preceding source regions are represented or explicitly accounted for.
- **Required Information:** Section start, content class, TextUnit kind/range, capability/coverage.
- **Required Capability:** `OrderedTextUnitEnumeration`, `CoverageInspection`.
- **Produced State / Locator / Cursor:** Sentence/coarse locators and `TextUnitCursor` when more remains.
- **Allowed Next Operations:** analyze, context, read/cite, continue.
- **Acceptance Criteria:** no silent loss before the first Sentence.

#### UC-SENTENCE-02 — Read Sentences in source order

- **Actor Goal:** consume prose Sentence by Sentence in author/source order.
- **Preconditions:** sentence stream initialized and version identity unchanged.
- **Trigger:** initial or continued Sentence enumeration.
- **Main Success Flow:** return exact Sentence text and locator in source order with deterministic ordinal/range.
- **Alternative Flow:** request pages of multiple Sentences to reduce round-trips while preserving per-Sentence identity.
- **Failure Flow:** stale cursor/locator, segmentation mismatch, or range validation failure.
- **Degradation Flow:** non-prose/coarse unit appears explicitly rather than being split into fake Sentences.
- **Success Result:** every eligible Sentence is individually addressable and readable.
- **Required Information:** normalized hash, segmentation version, paragraph/sentence ordinals, exact ranges.
- **Required Capability:** `OrderedTextUnitEnumeration`, `LocatorHandoff`.
- **Produced State / Locator / Cursor:** ordered TextLocators and optional cursor.
- **Allowed Next Operations:** analyze each item, read, context, cite, continue.
- **Acceptance Criteria:** exact slices, stable order, no search-based navigation.

#### UC-SENTENCE-03 — Read previous/next Sentence

- **Actor Goal:** move relative to the current Sentence and detect Section boundaries.
- **Preconditions:** valid Sentence locator.
- **Trigger:** the Agent asks for previous/next.
- **Main Success Flow:** enumerate one or more items before/after the anchor, preserving source order in the response.
- **Alternative Flow:** request a symmetric neighbor-context window.
- **Failure Flow:** stale anchor or incompatible segmentation version.
- **Degradation Flow:** adjacent source item may be a coarser non-prose item and is labeled accordingly.
- **Success Result:** current, previous/next position and boundary status are unambiguous.
- **Required Information:** anchor identity, direction, Section boundary, content class.
- **Required Capability:** `OrderedTextUnitEnumeration`.
- **Produced State / Locator / Cursor:** adjacent item locators.
- **Allowed Next Operations:** continue, context, read, cite.
- **Acceptance Criteria:** `next` never means “next search match”; it means next source-ordered reading item.

#### UC-SENTENCE-04 — Sentence to containing Paragraph

- **Actor Goal:** recover the complete local explanatory container of a Sentence.
- **Preconditions:** valid Sentence locator with owning Paragraph relation.
- **Trigger:** the Agent requests container context.
- **Main Success Flow:** resolve and return the exact containing Paragraph plus locator.
- **Alternative Flow:** if canonical Paragraph capability is unavailable, return the owning Section only with explicit degradation.
- **Failure Flow:** stale locator or inconsistent ownership invariant.
- **Degradation Flow:** coarse container is labeled; it is not called a Paragraph.
- **Success Result:** containing context is available without re-search.
- **Required Information:** Sentence→Paragraph ownership, exact ranges, owner Section.
- **Required Capability:** `ContainerContext`, `LocatorHandoff`.
- **Produced State / Locator / Cursor:** Paragraph or explicit coarse-container locator.
- **Allowed Next Operations:** read/cite container, neighbor context, continue Sentences.
- **Acceptance Criteria:** one Sentence belongs to exactly one Paragraph when Paragraph capability is available.

#### UC-SENTENCE-05 — Sentence to surrounding Sentences

- **Actor Goal:** inspect a bounded local window around one Sentence.
- **Preconditions:** valid Sentence locator.
- **Trigger:** the Agent asks for ±N Sentence neighbors.
- **Main Success Flow:** return structured neighbors in source order, identify the anchor, and preserve locators.
- **Alternative Flow:** request Paragraph container instead.
- **Failure Flow:** stale locator or window exceeds server policy.
- **Degradation Flow:** coarse non-prose item may appear with explicit kind/reason; missing outside-boundary neighbors yield a complete shorter window.
- **Success Result:** local discourse context is available with no snippet copying.
- **Required Information:** anchor, unit, before/after, boundary state.
- **Required Capability:** `NeighborContext`.
- **Produced State / Locator / Cursor:** neighbor locators; context responses normally do not create traversal cursors.
- **Allowed Next Operations:** read/cite any returned item, request container, resume enumeration.
- **Acceptance Criteria:** neighbor, container, and structural semantics are not conflated.

#### UC-SENTENCE-06 — Continue until the whole Section is complete

- **Actor Goal:** finish sentence-first analysis of the selected Section while accounting for every source region.
- **Preconditions:** valid initial enumeration or `TextUnitCursor`.
- **Trigger:** prior enumeration reports more items.
- **Main Success Flow:** continue pages until `section_complete=true`; eligible prose is represented by Sentences and non-prose/unsupported regions are represented or explicitly counted.
- **Alternative Flow:** use strict eligible-only mode for a specialized task; it cannot claim full Section consumption.
- **Failure Flow:** cursor stale, stream policy changes, or source coverage has an unreported hole.
- **Degradation Flow:** source-preserving coarse items and coverage diagnostics maintain completeness without false Sentence identity.
- **Success Result:** the Agent knows it reached the Section end and can audit coverage.
- **Required Information:** enumeration policy, item ranges/order, next cursor, eligibility and skipped/fallback coverage.
- **Required Capability:** `OrderedTextUnitEnumeration`, `SequentialContinuation`, `CoverageInspection`.
- **Produced State / Locator / Cursor:** terminal completion or next `TextUnitCursor`.
- **Allowed Next Operations:** finish analysis, move to next Section, cite saved locators.
- **Acceptance Criteria:** no gap/overlap; `next_cursor=null` and complete status agree; non-prose does not create a false failure.

### 4.7 Search

#### UC-SEARCH-01 — Search Chapter/Section titles

- **Actor Goal:** locate structural nodes by title/path.
- **Preconditions:** document/index is available.
- **Trigger:** query names a Chapter/Section/Subsection.
- **Main Success Flow:** return Section candidates, including title-only nodes, with Section-level locators.
- **Alternative Flow:** structure traversal locates the target without lexical search.
- **Failure Flow:** index unavailable and no fallback is advertised.
- **Degradation Flow:** return fewer candidates with explicit index capability status; never fabricate Paragraph identity for title-only nodes.
- **Success Result:** hit can flow directly to Section read/context.
- **Required Information:** candidate kind, Section locator, title/path, score/snippet preview.
- **Required Capability:** `LexicalSearch`, `LocatorHandoff`.
- **Produced State / Locator / Cursor:** Section locator in `SearchHit`.
- **Allowed Next Operations:** read, context, structure selection.
- **Acceptance Criteria:** structural title retrieval remains preserved after TextUnit indexing.

#### UC-SEARCH-02 — Search exact term, API, or system call

- **Actor Goal:** locate precise occurrences of technical syntax.
- **Preconditions:** lexical candidate kinds supported by the capability profile.
- **Trigger:** query contains a term such as `fork()`, `mmap()`, or `O_CREAT`.
- **Main Success Flow:** search deterministic lexical indexes and return the finest trustworthy hit locator selected by requested/auto granularity.
- **Alternative Flow:** Paragraph hit is returned when Sentence precision is unavailable or recall requires it.
- **Failure Flow:** invalid query or unavailable index.
- **Degradation Flow:** Section-level hit remains usable and is labeled as coarse.
- **Success Result:** exact source target can be read/contextualized directly.
- **Required Information:** granularity, candidate kind, locator, score, snippet, index/version diagnostics when relevant.
- **Required Capability:** `LexicalSearch`, `LocatorHandoff`.
- **Produced State / Locator / Cursor:** Section/Paragraph/Sentence locator.
- **Allowed Next Operations:** read or context without re-search.
- **Acceptance Criteria:** tokenizer changes do not renumber TextUnits.

#### UC-SEARCH-03 — Find one defining Sentence

- **Actor Goal:** locate a concise definition as evidence.
- **Preconditions:** Sentence candidates are available.
- **Trigger:** query asks for a definition or exact proposition.
- **Main Success Flow:** return Sentence hit with exact locator and containing ownership.
- **Alternative Flow:** return Paragraph hit when definition spans Sentences.
- **Failure Flow:** no truthful match.
- **Degradation Flow:** coarse hit is explicit; snippet alone is never citation identity.
- **Success Result:** the definition can be re-read, contextualized, and cited.
- **Required Information:** candidate kind, exact locator, owner Paragraph/Section.
- **Required Capability:** `LexicalSearch`, `LocatorHandoff`, `StableCitation`.
- **Produced State / Locator / Cursor:** Sentence or coarse locator.
- **Allowed Next Operations:** read, containing Paragraph, surrounding Sentences, cite.
- **Acceptance Criteria:** no quoted-snippet re-search is required.

#### UC-SEARCH-04 — Find an explanatory Paragraph

- **Actor Goal:** locate a multi-sentence explanation with local coherence.
- **Preconditions:** Paragraph candidates are available.
- **Trigger:** query terms span an explanation.
- **Main Success Flow:** return Paragraph hit with exact locator and owner Section.
- **Alternative Flow:** auto mode may rank Sentence and Paragraph candidates while exposing actual kind.
- **Failure Flow:** no match or index unavailable.
- **Degradation Flow:** Section hit is explicit.
- **Success Result:** the Paragraph can be read/contextualized directly.
- **Required Information:** Paragraph range, owner Section, score/snippet.
- **Required Capability:** `LexicalSearch`, `LocatorHandoff`.
- **Produced State / Locator / Cursor:** Paragraph locator.
- **Allowed Next Operations:** read, neighbor Paragraphs, containing Section, cite.
- **Acceptance Criteria:** search remains “where?”, not an unbounded body-read API.

#### UC-SEARCH-05 — SearchHit directly to Read

- **Actor Goal:** read the exact canonical source represented by a hit.
- **Preconditions:** hit carries a source locator, not only text preview.
- **Trigger:** Agent selects a hit.
- **Main Success Flow:** pass locator unchanged to read; validate identity; return exact source.
- **Alternative Flow:** hit already contains the full bounded TextUnit text, so read is used for revalidation or larger exact range.
- **Failure Flow:** locator stale or unresolved.
- **Degradation Flow:** no automatic fallback to snippet matching; Agent may explicitly read owning Section.
- **Success Result:** canonical passage is retrieved in one handoff.
- **Required Information:** complete locator identity and hit candidate kind.
- **Required Capability:** `LocatorHandoff`, `PreciseRead`.
- **Produced State / Locator / Cursor:** resolved locator; optional read cursor only for an oversized target.
- **Allowed Next Operations:** context, cite, analyze.
- **Acceptance Criteria:** `SearchHit → read`, never `snippet → search again`.

#### UC-SEARCH-06 — SearchHit directly to Context

- **Actor Goal:** expand context around the exact hit.
- **Preconditions:** hit locator is valid.
- **Trigger:** Agent selects neighbor/container/structural context.
- **Main Success Flow:** pass locator unchanged with an explicit context relation.
- **Alternative Flow:** Section hit supports structural/Section-neighbor context only.
- **Failure Flow:** stale locator or unsupported relation.
- **Degradation Flow:** return an explicitly coarser supported relation.
- **Success Result:** context is canonical and directly associated with the hit.
- **Required Information:** locator, relation type, capability profile.
- **Required Capability:** `LocatorHandoff`, `NeighborContext`, `ContainerContext`, or `StructuralContext`.
- **Produced State / Locator / Cursor:** structured context items with locators.
- **Allowed Next Operations:** read/cite any context item.
- **Acceptance Criteria:** snippet text is never used as the join key.

### 4.8 Context

#### UC-CONTEXT-01 — Sentence ± N Sentences

- **Actor Goal:** inspect local Sentence neighbors.
- **Preconditions:** valid Sentence locator.
- **Trigger:** explicit `neighbor` relation with `unit=sentence`.
- **Main Success Flow:** return bounded ordered neighbors and anchor identity.
- **Alternative Flow:** shorter window at Section boundaries.
- **Failure Flow:** stale locator or policy-exceeding window.
- **Degradation Flow:** labeled coarse non-prose items may appear.
- **Success Result:** local context is available with exact locators.
- **Required Information:** anchor, before/after, boundaries, item kinds.
- **Required Capability:** `NeighborContext`.
- **Produced State / Locator / Cursor:** returned item locators.
- **Allowed Next Operations:** read, container, cite.
- **Acceptance Criteria:** deterministic source order and anchor marking.

#### UC-CONTEXT-02 — Sentence to containing Paragraph

- **Actor Goal:** obtain the Sentence's immediate textual container.
- **Preconditions:** ownership relation exists.
- **Trigger:** explicit `container=paragraph` relation.
- **Main Success Flow:** return exactly one containing Paragraph.
- **Alternative Flow:** explicit owning Section fallback.
- **Failure Flow:** ownership invariant violated or locator stale.
- **Degradation Flow:** coarse container is labeled, not renamed.
- **Success Result:** local explanation is recovered.
- **Required Information:** ownership and exact range.
- **Required Capability:** `ContainerContext`.
- **Produced State / Locator / Cursor:** container locator.
- **Allowed Next Operations:** read/cite/neighbors.
- **Acceptance Criteria:** no before/after parameter changes container semantics.

#### UC-CONTEXT-03 — Paragraph ± N Paragraphs

- **Actor Goal:** inspect adjacent explanatory blocks.
- **Preconditions:** valid Paragraph locator.
- **Trigger:** explicit `neighbor` relation with `unit=paragraph`.
- **Main Success Flow:** return ordered Paragraph neighbors.
- **Alternative Flow:** bounded result at Section boundary.
- **Failure Flow:** stale locator or unsupported Paragraph capability.
- **Degradation Flow:** Section-neighbor context only by explicit fallback.
- **Success Result:** surrounding Paragraph context is available.
- **Required Information:** anchor, order, boundary state.
- **Required Capability:** `NeighborContext`.
- **Produced State / Locator / Cursor:** Paragraph locators.
- **Allowed Next Operations:** read/cite/continue enumeration.
- **Acceptance Criteria:** no hidden structural sibling traversal.

#### UC-CONTEXT-04 — TextUnit to owning Section

- **Actor Goal:** recover structural context for a Paragraph/Sentence.
- **Preconditions:** valid TextLocator with `owner_section_id`.
- **Trigger:** explicit `structural=owner_section` relation.
- **Main Success Flow:** resolve the owning Section node and its structure/provenance.
- **Alternative Flow:** return ancestor chain when explicitly requested.
- **Failure Flow:** stale locator or missing owner.
- **Degradation Flow:** unresolved native target remains visible while normalized owner can still resolve.
- **Success Result:** evidence is placed in book structure.
- **Required Information:** owner Section, path, provenance, document identity.
- **Required Capability:** `StructuralContext`, `NativeTraceability`.
- **Produced State / Locator / Cursor:** Section locator.
- **Allowed Next Operations:** read Section, inspect structure, cite.
- **Acceptance Criteria:** owner is obtained from identity/ownership, not title search.

#### UC-CONTEXT-05 — Existing Section-neighbor context

- **Actor Goal:** preserve current before/after Section workflow.
- **Preconditions:** valid Section target.
- **Trigger:** legacy request or explicit `neighbor/unit=section` relation.
- **Main Success Flow:** return shallow neighboring Sections in deterministic flattened source order.
- **Alternative Flow:** new structured response coexists with legacy concatenated content.
- **Failure Flow:** Section not found or window exceeds policy.
- **Degradation Flow:** unsupported neighboring content is reported.
- **Success Result:** existing clients keep their semantics.
- **Required Information:** owner Section, before/after, source order.
- **Required Capability:** `NeighborContext`.
- **Produced State / Locator / Cursor:** Section locators; no traversal cursor by default.
- **Allowed Next Operations:** read selected Section or structured item.
- **Acceptance Criteria:** legacy parameters map only to Section-neighbor mode.

### 4.9 Precise citation

#### UC-CITE-01 — Save a Sentence TextLocator

- **Actor Goal:** preserve exact evidence for later use.
- **Preconditions:** Sentence was returned with a complete locator.
- **Trigger:** Agent records evidence.
- **Main Success Flow:** store the structured locator, not display text or rendered offsets.
- **Alternative Flow:** save Paragraph/CharacterRange locator.
- **Failure Flow:** response lacks version/owner/range identity.
- **Degradation Flow:** coarse Section locator can be saved but cannot claim Sentence precision.
- **Success Result:** evidence identity is portable across Tools for the same version.
- **Required Information:** document/raw/normalized identity, owner, range, segmentation/native provenance.
- **Required Capability:** `StableCitation`, `NativeTraceability`.
- **Produced State / Locator / Cursor:** `TextLocator`.
- **Allowed Next Operations:** later read/context/citation rendering.
- **Acceptance Criteria:** cursor or snippet is never stored as citation identity.

#### UC-CITE-02 — Re-read the same Sentence later

- **Actor Goal:** reproduce saved evidence exactly.
- **Preconditions:** saved locator and referenced version are available.
- **Trigger:** later read using the locator.
- **Main Success Flow:** validate and return the exact canonical slice.
- **Alternative Flow:** explicitly read retained historical version.
- **Failure Flow:** stale/unavailable version fails closed.
- **Degradation Flow:** no fuzzy remap; Agent may explicitly re-search the new version.
- **Success Result:** exact same evidence is reproduced or explicit stale failure occurs.
- **Required Information:** full locator identity and repository version availability.
- **Required Capability:** `PreciseRead`, `FreshnessValidation`.
- **Produced State / Locator / Cursor:** resolved locator.
- **Allowed Next Operations:** cite/context/analyze.
- **Acceptance Criteria:** changed source cannot silently return a similar Sentence.

#### UC-CITE-03 — Cite a CharacterRange

- **Actor Goal:** identify an exact sub-Sentence excerpt.
- **Preconditions:** valid owner TextLocator/Section and normalized coordinate space.
- **Trigger:** Agent narrows evidence to `[start,end)`.
- **Main Success Flow:** validate zero-based half-open Unicode-scalar range relative to owner `Section.content`.
- **Alternative Flow:** cite whole Sentence/Paragraph.
- **Failure Flow:** range out of bounds, mismatched owner, or legacy offset ambiguity.
- **Degradation Flow:** legacy parser `char_start/end` remain provenance only and are not silently reinterpreted.
- **Success Result:** exact excerpt is stable for the normalized version.
- **Required Information:** owner Section, normalized range, normalized hash.
- **Required Capability:** `StableCitation`, `PreciseRead`.
- **Produced State / Locator / Cursor:** CharacterRange `TextLocator`.
- **Allowed Next Operations:** read/cite/container context.
- **Acceptance Criteria:** rendered-response offsets are rejected as source locators.

#### UC-CITE-04 — Preserve EPUB native location/provenance

- **Actor Goal:** trace normalized evidence back to publication-native structure.
- **Preconditions:** parser retained native EPUB facts.
- **Trigger:** locator/hit/read/context is returned.
- **Main Success Flow:** include entry path, fragment/anchor, spine/navigation provenance and resolution status where available.
- **Alternative Flow:** native location is absent for a format/region but normalized identity remains valid.
- **Failure Flow:** fallback is mislabeled native or a claimed resolved target is unverifiable.
- **Degradation Flow:** retain normalized locator with explicit native-traceability gap.
- **Success Result:** machine identity and human/native provenance coexist without replacing each other.
- **Required Information:** native location, provenance, resolution state.
- **Required Capability:** `NativeTraceability`, `ReliabilityInspection`.
- **Produced State / Locator / Cursor:** locator enriched with native facts.
- **Allowed Next Operations:** cite/display/inspect reliability.
- **Acceptance Criteria:** native display text is presentation, not canonical identity.

### 4.10 Stale locator, cursor, and hit

#### UC-STALE-01 — Old TextLocator after normalized document changes

- **Actor Goal:** avoid reading a different passage under an old citation.
- **Preconditions:** locator references an older normalized identity.
- **Trigger:** read/context resolves it against changed current state.
- **Main Success Flow:** resolve the exact retained old version when explicitly available and selected.
- **Alternative Flow:** return stable `STALE_LOCATOR` with expected/actual identity metadata.
- **Failure Flow:** silently map to nearest text or same ordinal in the new version.
- **Degradation Flow:** none for identity; coarse/fuzzy fallback requires an explicit new navigation/search workflow.
- **Success Result:** exact old evidence or explicit failure, never false continuity.
- **Required Information:** expected/actual normalized hash and segmentation version.
- **Required Capability:** `FreshnessValidation`.
- **Produced State / Locator / Cursor:** unchanged old locator or explicit stale error.
- **Allowed Next Operations:** open/select historical version or re-navigate new version.
- **Acceptance Criteria:** fail closed by default.

#### UC-STALE-02 — Old ReadCursor after document change

- **Actor Goal:** prevent continuation into a different stream/version.
- **Preconditions:** cursor was issued before relevant identity changed.
- **Trigger:** continuation call.
- **Main Success Flow:** continue only if all cursor bindings still match.
- **Alternative Flow:** start a new read from the source locator.
- **Failure Flow:** return `STALE_CURSOR`; never adjust stream offset heuristically.
- **Degradation Flow:** none.
- **Success Result:** no cross-version stream splice.
- **Required Information:** raw/normalized identity, target, mode/rendering/cursor versions.
- **Required Capability:** `FreshnessValidation`, `SequentialContinuation`.
- **Produced State / Locator / Cursor:** next cursor or stale error.
- **Allowed Next Operations:** restart from target/open new version.
- **Acceptance Criteria:** cursor mismatch always fails closed.

#### UC-STALE-03 — Old SearchHit after index rebuild

- **Actor Goal:** know whether a previously selected hit still identifies canonical source.
- **Preconditions:** hit carries a locator and the index was rebuilt.
- **Trigger:** hit is passed to read/context.
- **Main Success Flow:** if normalized/segmentation identity is unchanged, resolve through the locator independently of index state.
- **Alternative Flow:** if identity changed, return stale locator and re-search explicitly.
- **Failure Flow:** use snippet/score/index row ID as source identity.
- **Degradation Flow:** index temporarily unavailable does not invalidate still-resolvable canonical locators.
- **Success Result:** index rebuild cannot silently redefine evidence.
- **Required Information:** locator identity, index metadata only for diagnostics.
- **Required Capability:** `LocatorHandoff`, `FreshnessValidation`.
- **Produced State / Locator / Cursor:** resolved locator or stale error.
- **Allowed Next Operations:** read/context or explicit new search.
- **Acceptance Criteria:** SearchIndex remains derived state.

### 4.11 Non-prose content

#### UC-NONPROSE-01 — Code block has no Sentence

- **Actor Goal:** preserve code as readable source without pretending it is prose.
- **Preconditions:** canonical content contains code/non-prose region.
- **Trigger:** Sentence-first enumeration reaches it.
- **Main Success Flow:** emit a source-ordered coarser exact reading item with `content_class=non_prose`, effective kind/range, and degradation reason.
- **Alternative Flow:** strict eligible-only mode skips it but explicitly counts the skipped region and cannot claim full Section consumption.
- **Failure Flow:** split punctuation into fake Sentences or omit code silently.
- **Degradation Flow:** Paragraph/Section-level addressability is accepted and labeled.
- **Success Result:** code is readable and coverage remains truthful.
- **Required Information:** content class, exact range, owner, coverage policy.
- **Required Capability:** `OrderedTextUnitEnumeration`, `CoverageInspection`.
- **Produced State / Locator / Cursor:** coarse TextLocator and enumeration progress.
- **Allowed Next Operations:** read/cite/context/continue.
- **Acceptance Criteria:** no Sentence ordinal is fabricated.

#### UC-NONPROSE-02 — Table has no natural Sentence

- **Actor Goal:** retain table content and source position without false sentence segmentation.
- **Preconditions:** table is normalized/readable at a supported coarse level.
- **Trigger:** precise traversal reaches it.
- **Main Success Flow:** emit an exact coarse item or explicit unsupported region with provenance.
- **Alternative Flow:** future structured table capability may add richer reading units only after a separate use-case decision.
- **Failure Flow:** flattening is labeled as native cell/Sentence precision.
- **Degradation Flow:** coarse normalized text remains readable with table classification.
- **Success Result:** table is represented or explicitly accounted for.
- **Required Information:** range/resource, content class, provenance, support status.
- **Required Capability:** `OrderedTextUnitEnumeration`, `ReliabilityInspection`.
- **Produced State / Locator / Cursor:** coarse locator or coverage gap.
- **Allowed Next Operations:** read/continue/inspect reliability.
- **Acceptance Criteria:** no hidden omission and no premature table-cell identity requirement.

#### UC-NONPROSE-03 — Agent can still read non-prose

- **Actor Goal:** obtain canonical text/content for a code/table region.
- **Preconditions:** a coarse locator or supported structural target exists.
- **Trigger:** read the returned non-prose item.
- **Main Success Flow:** canonical read returns exact available normalized content and native provenance.
- **Alternative Flow:** read owning Section when only structural addressability exists.
- **Failure Flow:** region is unsupported and no truthful normalized representation exists.
- **Degradation Flow:** return explicit unsupported/coarse status and owner Section, not fabricated content.
- **Success Result:** useful content is read or an explicit limitation is known.
- **Required Information:** locator, content class, support status.
- **Required Capability:** `PreciseRead`, `NativeTraceability`.
- **Produced State / Locator / Cursor:** same coarse locator; optional read cursor if oversized.
- **Allowed Next Operations:** cite/context/continue.
- **Acceptance Criteria:** sentence capability is not a prerequisite for general readability.

#### UC-NONPROSE-04 — Sentence coverage remains truthful

- **Actor Goal:** distinguish missing eligible prose from intentionally non-sentence content.
- **Preconditions:** coverage report is generated.
- **Trigger:** Agent validates a sentence-first Section stream.
- **Main Success Flow:** report eligible prose denominator, represented Sentence text, non-prose fallback/skipped counts, and unsupported gaps separately.
- **Alternative Flow:** no non-prose exists and eligible coverage is 100%.
- **Failure Flow:** divide Sentence text by all bytes and label legitimate non-prose as a parser failure, or inflate coverage with fake Sentences.
- **Degradation Flow:** unsupported regions reduce the appropriate coverage dimension with reason.
- **Success Result:** Agent can claim completion accurately.
- **Required Information:** well-defined denominators and classified regions.
- **Required Capability:** `CoverageInspection`.
- **Produced State / Locator / Cursor:** coverage evidence.
- **Allowed Next Operations:** accept completion or inspect/retry unsupported regions.
- **Acceptance Criteria:** prose eligibility and all-content coverage are never conflated.

### 4.12 Reliability and coverage

#### UC-RELIABILITY-01 — Determine native vs fallback EPUB structure

- **Actor Goal:** know the evidence source behind navigation.
- **Preconditions:** EPUB structure extraction attempted.
- **Trigger:** open/structure response is inspected.
- **Main Success Flow:** expose node/aggregate provenance and resolution status.
- **Alternative Flow:** native nav is complete.
- **Failure Flow:** heading/spine fallback is called publisher-native.
- **Degradation Flow:** usable fallback survives with explicit status.
- **Success Result:** structural claims are properly qualified.
- **Required Information:** nav/NCX/heading/spine provenance and coverage.
- **Required Capability:** `ReliabilityInspection`, `NativeTraceability`.
- **Produced State / Locator / Cursor:** reliability evidence attached to document/nodes.
- **Allowed Next Operations:** safe navigation/read.
- **Acceptance Criteria:** factual provenance, no vague confidence score.

#### UC-RELIABILITY-02 — Determine precise-reading coverage

- **Actor Goal:** know whether Paragraph/Sentence traversal can support a complete reading claim.
- **Preconditions:** relevant validators/segmentation ran or capability is unavailable.
- **Trigger:** open, structure, or TextUnit enumeration response.
- **Main Success Flow:** report supported units, eligible/represented counts/ranges, non-prose handling, and unsupported gaps.
- **Alternative Flow:** capability unavailable is reported without running enumeration.
- **Failure Flow:** a single `parse_success=true` is treated as precise-reading proof.
- **Degradation Flow:** Section reading remains available while precise coverage is partial.
- **Success Result:** Agent selects the correct workflow and wording.
- **Required Information:** capability grades, validators, coverage dimensions/denominators.
- **Required Capability:** `CoverageInspection`, `CapabilityAdvertisement`.
- **Produced State / Locator / Cursor:** document/Section coverage evidence.
- **Allowed Next Operations:** enumerate, coarse read, inspect gaps.
- **Acceptance Criteria:** coverage is measurable and reproducible.

#### UC-RELIABILITY-03 — TOC target unresolved

- **Actor Goal:** navigate as far as evidence permits without believing an unresolved target is precise.
- **Preconditions:** TOC href/resource/fragment resolution failed partially.
- **Trigger:** structure/open validation.
- **Main Success Flow:** keep node visible with explicit resolution state and, when safe, a coarse containing-resource fallback.
- **Alternative Flow:** target resolves normally.
- **Failure Flow:** drop node or claim fragment resolution.
- **Degradation Flow:** containing document/spine target is returned as coarse.
- **Success Result:** navigation remains useful and truthful.
- **Required Information:** original href, resolved resource, fragment state, fallback provenance.
- **Required Capability:** `ReliabilityInspection`, `StructuralNavigation`.
- **Produced State / Locator / Cursor:** unresolved/coarse structural locator.
- **Allowed Next Operations:** coarse read or choose another node.
- **Acceptance Criteria:** unresolved precision is never fabricated.

#### UC-RELIABILITY-04 — Partially unsupported spine/resource coverage

- **Actor Goal:** know where source-order reading has unsupported gaps.
- **Preconditions:** one or more publication resources cannot be normalized precisely.
- **Trigger:** open/structure/read/enumeration coverage inspection.
- **Main Success Flow:** preserve spine/source order, report each unsupported/coarse resource and continue supported content.
- **Alternative Flow:** fatal essential failure prevents opening.
- **Failure Flow:** compress failed item N and successful item N+1 into an apparently continuous stream.
- **Degradation Flow:** coarse or unsupported placeholder occupies the source-order position.
- **Success Result:** Agent can distinguish complete supported reading from publication-wide completeness.
- **Required Information:** spine index/path/media type/parse status and coverage.
- **Required Capability:** `CoverageInspection`, `NativeTraceability`, `OrderedTextUnitEnumeration`.
- **Produced State / Locator / Cursor:** ordered coverage gap/coarse locator.
- **Allowed Next Operations:** continue supported stream, inspect or report gap.
- **Acceptance Criteria:** every gap is explicit and ordered.

## 5. Capability Model

The following capabilities are derived from the use cases before Tool selection.

| Capability | Independent responsibility / change reason | Evidence from use cases |
|---|---|---|
| `DocumentDiscovery` | enumerate policy-approved candidate sources without opening them | DISCOVER |
| `DocumentOpenAndVersionResolution` | turn one source into one concrete canonical document version | OPEN-01..03 |
| `CapabilityAdvertisement` | state which precise/coarse operations are truthful | OPEN-04, RELIABILITY-02 |
| `StructuralNavigation` | expose and expand author/source structural nodes | STRUCTURE, RELIABILITY-03 |
| `OrderedTextUnitEnumeration` | enumerate Paragraph/Sentence-first reading items in source order under pagination | PARAGRAPH, SENTENCE, NONPROSE |
| `PreciseRead` | return canonical content for an already-known source target | READ, PARAGRAPH-02, CITE |
| `SequentialContinuation` | resume one bounded stream without gap/overlap | STRUCTURE-03, READ-02..04, SENTENCE-06 |
| `LexicalSearch` | answer “where?” using derived retrieval state | SEARCH |
| `NeighborContext` | return same-level units around an anchor | CONTEXT-01,03,05 |
| `ContainerContext` | return immediate textual container | CONTEXT-02, SENTENCE-04 |
| `StructuralContext` | return owner/ancestor/sibling/child structure | CONTEXT-04 |
| `LocatorHandoff` | pass one source identity unchanged between search/enumeration/read/context | SEARCH-05..06 |
| `StableCitation` | preserve exact version-bound evidence identity | CITE |
| `FreshnessValidation` | validate locator/cursor/version bindings and fail closed | OPEN-02..03, STALE |
| `NativeTraceability` | retain page/anchor/spine/resource/provenance facts | CITE-04, RELIABILITY |
| `ReliabilityInspection` | expose provenance/resolution/degradation facts | OPEN-04, STRUCTURE-04, RELIABILITY |
| `CoverageInspection` | measure represented, fallback, skipped, and unsupported regions with defined denominators | READ-04, SENTENCE-06, NONPROSE-04, RELIABILITY |

### 5.1 Independence decisions

- `OrderedTextUnitEnumeration` is not `PreciseRead`: enumeration discovers the first/next child unit and proves stream completion; read consumes an already-known target.
- `NeighborContext`, `ContainerContext`, and `StructuralContext` are semantically independent even if one MCP Tool exposes them through a tagged relation union.
- `SequentialContinuation` is cross-cutting, but each cursor type is scoped to one stream contract.
- `ReliabilityInspection` and `CoverageInspection` do not automatically require a dedicated Tool; they can be returned where the reading decision is made.
- `StableCitation` is provided by locator-bearing results; no separate “save citation” Tool is justified.

## 6. Workflow / State Machine

### 6.1 Workflow A — Open and completely read a Section

```text
discover (optional)
  ↓
open
  ↓
inspect capabilities/reliability
  ↓
structure
  ↓
select Section
  ↓
read(target=Section, mode=section_tree)
  ↓
complete?
 ├─ no → read(cursor=next_read_cursor) ─┐
 │                                      │
 └─ yes ←───────────────────────────────┘
```

Terminal success requires `complete=true`, no next cursor, and no unreported coverage gap.

### 6.2 Workflow B — Sentence-first precise reading

```text
open
  ↓
structure
  ↓
select §1.1
  ↓
get_text_units(target=§1.1, requested_kind=sentence,
               coverage_policy=preserve_source)
  ↓
reading item
 ├─ Sentence → AI analysis/question
 └─ coarse non-prose item → AI reads it without Sentence claim
  ↓
context(relation=neighbor/container/structural) as needed
  ↓
continue with TextUnitCursor
  ↓
section_complete?
 ├─ no → next page
 └─ yes → finished with coverage evidence
```

Conceptual source order:

```text
§1.1
 ↓
¶1 S1
 ↓
¶1 S2
 ↓
...
 ↓
non-prose ¶/coarse item (when present)
 ↓
¶N SM
 ↓
Section complete
```

### 6.3 Workflow C — Search to canonical source

```text
search
  ↓
SearchHit(candidate_kind + TextLocator)
  ↓
read(target=TextLocator)
```

Rejected:

```text
search → snippet → copy text → search again
```

### 6.4 Workflow D — Search to context

```text
search
  ↓
Sentence/Paragraph/Section locator
  ↓
context(relation=neighbor)
  ↓
context(relation=container or structural)
```

### 6.5 Workflow E — Stale locator/cursor

```text
saved locator/cursor
  ↓
document or relevant policy changed
  ↓
read/context/continue
  ↓
identity matches?
 ├─ yes → resolve exact referenced source
 └─ no  → explicit STALE_LOCATOR / STALE_CURSOR
             ↓
          reopen/re-navigate/re-search explicitly
```

There is no fuzzy-repair transition.

### 6.6 State distinctions

```text
Source candidate
  └─ not yet opened; no DocumentId

Document version
  └─ canonical source identity + capability/reliability profile

TextLocator
  └─ canonical address in one normalized version

ReadCursor
  └─ progress through one read/render stream

TextUnitCursor
  └─ progress through one ordered enumeration stream

StructureCursor / DiscoveryCursor
  └─ bounded response progress for those respective enumerations
```

No cursor is valid as a citation.

## 7. Use Case → Capability Matrix

| Use-case family | Required capabilities |
|---|---|
| DISCOVER | DocumentDiscovery, SequentialContinuation |
| OPEN | DocumentOpenAndVersionResolution, FreshnessValidation, CapabilityAdvertisement, ReliabilityInspection, CoverageInspection |
| STRUCTURE | StructuralNavigation, SequentialContinuation, NativeTraceability, ReliabilityInspection, CoverageInspection |
| READ-SECTION | PreciseRead, SequentialContinuation, FreshnessValidation, CoverageInspection |
| PARAGRAPH | OrderedTextUnitEnumeration, PreciseRead, LocatorHandoff, NeighborContext |
| SENTENCE | OrderedTextUnitEnumeration, SequentialContinuation, NeighborContext, ContainerContext, CoverageInspection |
| SEARCH | LexicalSearch, LocatorHandoff, StableCitation |
| CONTEXT | NeighborContext, ContainerContext, StructuralContext, LocatorHandoff |
| CITE | StableCitation, PreciseRead, FreshnessValidation, NativeTraceability |
| STALE | FreshnessValidation, LocatorHandoff, SequentialContinuation |
| NONPROSE | OrderedTextUnitEnumeration, PreciseRead, ReliabilityInspection, CoverageInspection |
| RELIABILITY | CapabilityAdvertisement, ReliabilityInspection, CoverageInspection, NativeTraceability |

## 8. Capability → Current Tool Evaluation

| Capability | Current candidate | Natural expression today? | Gap / risk |
|---|---|---:|---|
| DocumentDiscovery | `list_documents` | partial | bounded by `max_results`, but no completion/continuation metadata |
| DocumentOpenAndVersionResolution | `open_document` | partial | raw hash only; no normalized identity, version outcome, capability/reliability summary |
| StructuralNavigation | `get_document_structure` | partial | hierarchy works; oversized result has non-actionable truncation |
| OrderedTextUnitEnumeration | none | no | no way to get first/next Paragraph/Sentence or prove Section stream completion |
| PreciseRead | `read_document` | partial | Section subtree only; no TextLocator target or actionable continuation |
| SequentialContinuation | none/current flags | no | `truncated=true` exists, but no cursor for read/structure/list |
| LexicalSearch | `search_document` | partial | Section-owning hits only; no explicit candidate kind/fine locator/granularity |
| NeighborContext | `get_context` | partial | Section neighbors only |
| ContainerContext | `get_context` candidate | no | current parameters cannot express container semantics |
| StructuralContext | `get_context` candidate | no | current parameters cannot express owner/ancestor relations |
| LocatorHandoff | legacy `Location` | partial | owning Section handoff works; fine-grained exact handoff does not |
| StableCitation | outputs | partial | legacy locations lack normalized-version identity |
| FreshnessValidation | open/read internals | partial | no stale locator/cursor taxonomy yet |
| NativeTraceability | `Location` | partial | native fields exist; reliability/provenance grading is incomplete |
| Reliability/Coverage | open/structure candidates | no | not exposed as a complete contract |

### 8.1 Current six-Tool sufficiency

The six current Tools are sufficient for the existing coarse workflow:

```text
discover → open → structure → search → Section context/read
```

They are not sufficient for the accepted use cases because no current contract naturally expresses source-ordered TextUnit discovery/enumeration. The missing capability cannot be repaired by merely adding a locator target to `read_document`: an Agent would still not know the first or next Sentence and could not prove enumeration completion.

## 9. Tool Alternatives for Ordered TextUnit Enumeration

### 9.1 Option A — Extend `read_document(target=section, view/granularity=sentence)`

Advantages:

- one familiar Tool name;
- can return text and locators with few round-trips.

Problems:

- conflates “read an already-known target” with “enumerate unknown child units”;
- mixes `ReadCursor` and TextUnit-enumeration cursor semantics;
- creates a parameter cross-product across target type, read mode, granularity, direction, anchor, pagination, non-prose policy, and rendering;
- makes a Section read response ambiguously mean rendered subtree, one Sentence, or a page of TextUnits;
- weakens SRP and increases backward-compatibility risk.

### 9.2 Option B — Add generic `get_text_units`

Advantages:

- directly models UC-PARAGRAPH-01/03 and UC-SENTENCE-01/02/03/06;
- cleanly owns source-order guarantee, anchor/direction, pagination, completion, and non-prose accounting;
- returns exact bounded text plus locator, avoiding a mandatory extra read per Sentence;
- keeps `read_document` focused on canonical read of a known target or continuation of one read stream;
- applies equally to Paragraph and Sentence without format-specific Tools;
- additive to current clients and extensible to future evidence-backed reading-item kinds.

Costs:

- expands the future surface from six to seven Tools;
- requires a distinct `TextUnitCursor` contract;
- must carefully distinguish requested kind from effective/coarse item kind.

### 9.3 Option C — Express enumeration through `get_context`

Advantages:

- no new Tool name.

Problems:

- context requires an anchor and therefore cannot discover the first Sentence;
- a neighbor window does not define a complete ordered Section stream;
- pagination/completion semantics are unnatural;
- container, neighbor, and traversal would be conflated;
- cannot prove no gap/overlap across the whole Section.

### 9.4 Comparison

| Dimension | Option A: read granularity | Option B: `get_text_units` | Option C: context |
|---|---|---|---|
| SRP | weak | strong | weak |
| Natural semantics | ambiguous | direct enumeration | wrong abstraction |
| Agent round-trips | low | low; text included | high/awkward |
| Token control | possible but mode-heavy | explicit page size/budget | window-oriented only |
| Pagination | cursor ambiguity | first-class TextUnitCursor | unnatural |
| Source-order guarantee | possible but hidden in read mode | explicit invariant | local only |
| Locator handoff | possible | first-class per item | anchor-dependent |
| Paragraph/Sentence reuse | parameter-heavy | one generic contract | incomplete |
| Non-prose degradation | entangled with rendering | explicit requested/effective kind + coverage | unclear |
| Backward compatibility | higher schema/semantic risk | additive Tool | parameter overload |
| Future extensibility | cross-product growth | evidence-gated item kinds | poor |

### 9.5 Decision

Recommend Option B: one new generic `get_text_units` Tool after the underlying normalized-range, TextUnit, and deterministic segmentation capabilities exist.

This is not a preference for “more Tools”. It is the minimum Tool addition supported by multiple independent use cases. No `get_sentences`, `get_paragraphs`, or format-specific enumeration Tool is justified.

## 10. Recommended Tool Surface

### 10.1 Current runtime

The runtime at the reviewed commit exposes six Tools:

```text
list_documents
open_document
get_document_structure
search_document
get_context
read_document
```

### 10.2 Accepted future contract surface

```text
list_documents             # discover source candidates
open_document              # establish concrete version/capabilities
get_document_structure     # navigate structural nodes
get_text_units             # enumerate ordered Paragraph/Sentence-first items
search_document            # locate candidates
get_context                # expand explicit neighbor/container/structural relation
read_document              # read an already-known source target or continue its read stream
```

Tool count is a consequence of independent responsibilities, not a design target.

### 10.3 Capabilities without a dedicated Tool

- citation uses structured locators returned by other Tools;
- reliability/coverage is returned by open/structure/enumeration at the decision point;
- stale validation is part of every locator/cursor-consuming contract;
- native traceability is part of locator/result metadata.

A future dedicated inspection Tool requires a separate use-case review; it is not justified here.

## 11. Detailed Tool Contracts

The schemas below are logical contracts. Exact Rust DTOs and wire encodings are deferred to implementation design. Tagged alternatives are preferred over bags of interacting optional parameters.

### 11.1 `list_documents`

**Responsibility:** enumerate policy-approved source candidates without opening/parsing them.

**Request evolution (additive):**

```text
path?
recursive = true
max_results
cursor?                 # mutually exclusive with a new incompatible scope
```

**Response evolution:**

```text
documents[]
complete
next_cursor?
discovery_capability
```

**Pagination/budget:** deterministic ordering; server-enforced bounds; `complete=false` requires `next_cursor`.

**Failure:** invalid scope, blocked path, unreadable directory, stale/mismatched discovery cursor.

**Degradation:** no configured local discovery roots yields an empty complete result; it does not block direct URL opening.

**Next legal calls:** continue listing or `open_document(source)`.

### 11.2 `open_document`

**Responsibility:** resolve one source into one concrete canonical document version and report truthful capabilities.

**Request:** preserve current `source`, `auth_profile?`, `force_refresh`.

**Response evolution (additive):**

```text
document_id
source/title/media_type
content_hash                 # raw-source provenance, unchanged meaning
normalized_document_hash
normalization_version?       # diagnostics, not identity by itself
section_count
open_outcome                 # opened_new | reused | refreshed | changed
supersedes_document_id?
capabilities
reliability_summary
coverage_summary
```

**Failure:** existing stable retrieval/parse/security errors plus fatal canonical-validation failure.

**Degradation:** readable document succeeds with explicit capability grade, provenance, and coverage gaps.

**Next legal calls:** structure/search/read and only the precise operations advertised.

### 11.3 `get_document_structure`

**Responsibility:** navigate StructuralNodes only; never enumerate all Paragraphs/Sentences.

**Request evolution (additive):**

```text
document_id
root_section_id?             # whole document when absent
max_depth?
max_nodes?
cursor?
```

**Response evolution:**

```text
document_id
root/sections[]
node fields:
  section_id/parent_id/title/level/location
  source_order
  has_children
  children_complete
  provenance/resolution_status?
complete
next_cursor?
structural_coverage
```

**Pagination/budget:** expansion is bounded; `complete=false` is actionable through cursor or explicit subtree expansion.

**Failure:** unknown/stale document, invalid root, stale cursor, structural invariant violation.

**Degradation:** fallback/coarse/unresolved nodes remain visible with provenance.

**Next legal calls:** expand/select/read/enumerate/search/context.

### 11.4 `get_text_units` (new, future)

**Responsibility:** enumerate bounded Paragraph or Sentence-first reading items inside one structural target in canonical source order.

**New enumeration request:**

```text
document_id
target: Section TextLocator / section_id
requested_kind: paragraph | sentence
start:
  section_start
  | after(anchor_locator)
  | before(anchor_locator)
  | cursor(TextUnitCursor)
direction: forward | backward       # default forward; response items remain source ordered
max_items
max_chars?
coverage_policy:
  preserve_source                   # default for complete reading
  | eligible_only                   # cannot claim complete all-source consumption
```

A continuation cursor is mutually exclusive with redefining target/kind/direction/policy.

**Response:**

```text
document_id
target_section_locator
requested_kind
items[]:
  text
  locator
  effective_kind: paragraph | sentence
  content_class: prose | non_prose | unknown
  degradation?                     # reason when effective kind is coarser
  native/provenance?
complete / section_complete
next_cursor?
coverage:
  eligible_prose
  represented_sentences/paragraphs
  coarse_non_prose_items
  intentionally_skipped
  unsupported_gaps
```

**Source-order guarantee:** items cover the declared stream in deterministic canonical order. Under `preserve_source`, every source region relevant to the normalized reading stream is represented or explicitly accounted for; no region disappears because it lacks a Sentence.

**Locator handoff:** every item locator flows directly into `read_document` and `get_context`.

**Pagination:** `TextUnitCursor` is bound to document/raw/normalized identity, target Section, segmentation version, requested kind, direction, coverage policy, and cursor schema version.

**Failure:** unsupported segmentation, stale target/cursor, invalid anchor ownership, range/ordering invariant failure.

**Degradation:** Sentence request may emit an explicitly coarser Paragraph reading item for code/table/non-prose; it never invents Sentence identity.

**Backward compatibility:** entirely additive; no existing request changes.

**Next legal calls:** continue enumeration, read item, context item, save locator/cite.

### 11.5 `search_document`

**Responsibility:** locate bounded candidates; answer “where?”, not return an unbounded read stream.

**Request evolution (additive):**

```text
document_id
query
limit
granularity: auto | section | paragraph | sentence   # default auto
cursor?                                               # when search pagination is introduced
```

**Response evolution:**

```text
hits[]:
  candidate_kind: section | paragraph | sentence
  title/source/snippet/score
  section_id/location                      # legacy fields retained
  text_locator                             # complete source handoff
complete/next_cursor?                      # when paginated
search_capability/index diagnostics?
```

Title-only candidates remain Section-level and never receive fake Paragraph/Sentence identity.

**Failure:** invalid query/limit, document/index unavailable, stale cursor.

**Degradation:** return coarser candidate kinds explicitly; snippet remains preview only.

**Next legal calls:** direct read or explicit context using `text_locator`.

### 11.6 `get_context`

**Responsibility:** expand context around an already-known source anchor. Three semantic capabilities share one Tool through a tagged relation, not an optional-parameter cross-product.

**Request evolution:**

```text
target: legacy section_id | TextLocator
relation:
  neighbor {
    unit: section | paragraph | sentence,
    before,
    after
  }
  | container {
    kind: paragraph | section
  }
  | structural {
    kind: owner_section | ancestors | siblings | children
  }
max_chars?
```

Legacy `section_id + before + after` maps exactly to `neighbor(unit=section)`.

**Response evolution:**

```text
legacy content/location fields retained
anchor_locator
relation
items[]:
  content/title as applicable
  locator
  role: before | anchor | after | container | structural
  effective_kind/provenance/degradation?
complete
coverage?
```

**Failure:** stale target, unsupported relation, policy-exceeding window, invalid ownership.

**Degradation:** explicit coarser container/context only; never silently reinterpret a relation.

**Next legal calls:** read/cite returned locators, resume independent enumeration.

### 11.7 `read_document`

**Responsibility:** return canonical content for an already-known source target or continue one deterministic read stream.

**Request evolution:**

```text
legacy:
  document_id + section_id + max_chars?

future tagged target:
  Section locator
  | TextLocator
  | ReadCursor

read_mode:
  section_tree              # current legacy behavior
  | exact_target            # exact Section.content/TextUnit/range
max_chars?
```

A cursor request cannot redefine target/mode.

**Response evolution:**

```text
legacy fields retained
content
complete                           # `!truncated` mapping for legacy clients
truncated                          # retained
resolved_target_locator
returned_locator?                  # exact-target source range only
read_stream_segment?               # stream metadata, not a source locator
next_cursor?
coverage?
```

For `section_tree`, rendered stream positions are scoped to `SectionTreeReadStream` and rendering version; they are never normalized source ranges. For exact `TextLocator` reads, returned text must equal the canonical range.

**Failure:** unknown target, stale locator/cursor, wrong cursor mode, invalid range, canonical validation failure.

**Degradation:** explicit source coverage gaps may remain; cursor identity never degrades or fuzzily rebases.

**Backward compatibility:** current Section request and response fields retain their meaning; new fields/targets are additive.

**Next legal calls:** continue with `ReadCursor`, context/cite locator, or finish.

## 12. Failure / Degradation Semantics

### 12.1 Stable future error classes

Logical error classes required by the use cases include:

```text
STALE_LOCATOR
STALE_CURSOR
CURSOR_TARGET_MISMATCH
UNSUPPORTED_CAPABILITY
UNSUPPORTED_TEXT_UNIT_KIND
INVALID_LOCATOR
INVALID_NORMALIZED_RANGE
STRUCTURE_INVARIANT_FAILED
TEXT_UNIT_INVARIANT_FAILED
COVERAGE_INCOMPLETE
```

Exact mapping to MCP error codes is implementation work. Errors should retain stable `code + retryable`, expected/actual version metadata when safe, and no document body leakage.

### 12.2 Fail closed

Fail closed for:

- locator normalized-document or segmentation mismatch;
- cursor document/target/mode/policy/version mismatch;
- out-of-bounds or non-exact normalized range;
- ownership/order invariant violation;
- claimed native target that cannot satisfy the required precision.

Never automatically map old locators to “the most similar” new Sentence.

### 12.3 Valid fallback/degradation

Fallback is valid when:

- canonical readable content survives;
- effective precision is lower but explicit;
- source order is preserved;
- provenance and coverage identify the loss;
- the fallback does not reuse a stronger type name.

Examples:

```text
EPUB nav missing → heading/spine Section with fallback provenance
Sentence unavailable for code → coarse Paragraph reading item
missing TOC fragment → containing resource target with missing_fragment status
Sentence capability unavailable → Section reading remains available
```

### 12.4 Reparse/reopen

A locator-consuming Tool does not secretly retrieve or reparse a changed source to repair identity. Source refresh is an explicit `open_document` workflow. After a change, the Agent explicitly re-navigates or re-searches the new version.

## 13. Locator / Cursor Handoff

### 13.1 Canonical source identity

Per ADR 0002, a fine-grained locator logically carries:

```text
document_id
content_hash                       # raw provenance
normalized_document_hash
owner_section_id / section_path
paragraph_index?
sentence_index?
normalized_range?
segmentation_version?
native_location/provenance?
```

`normalized_range` remains zero-based, half-open, Unicode-scalar, and relative to exact persisted owner `Section.content`.

### 13.2 Cursor taxonomy

| Cursor | Meaning | Required binding | May be cited? |
|---|---|---|---:|
| `DiscoveryCursor` | progress through source candidates | scope/order/policy/schema | no |
| `StructureCursor` | progress through one structural expansion | document/root/order/schema | no |
| `TextUnitCursor` | progress through one TextUnit enumeration stream | raw+normalized identity, Section, segmentation, kind, direction, coverage policy, schema | no |
| `SearchCursor` | progress through one result set | document/index/query/options/schema | no |
| `ReadCursor` | progress through one read/render stream | raw+normalized identity, target, read/render mode/version, next stream position, schema | no |

A cursor may internally contain an offset, but the offset is scoped to that stream contract and cannot be promoted to `TextLocator`.

### 13.3 Legal handoffs

```text
Structure Section locator ─┬→ read_document
                           ├→ get_text_units
                           └→ get_context

TextUnit locator ──────────┬→ read_document
                           ├→ get_context
                           └→ citation storage

SearchHit.text_locator ────┬→ read_document
                           └→ get_context

ReadCursor ────────────────→ read_document only
TextUnitCursor ────────────→ get_text_units only
```

Tool-specific cursors are not interchangeable.

## 14. Backward Compatibility

### 14.1 Current calls preserved

```text
list_documents(path?, recursive, max_results)
open_document(source, auth_profile?, force_refresh)
get_document_structure(document_id, max_depth?)
search_document(document_id, query, limit)
get_context(document_id, section_id, before, after, max_chars?)
read_document(document_id, section_id, max_chars?)
```

### 14.2 Compatibility rules

1. current fields retain their meaning;
2. current `Location.char_start/end` are not silently redefined as normalized ranges;
3. legacy Section read remains recursive `section_tree` behavior;
4. legacy Section context remains flattened Section-neighbor behavior;
5. new precise fields are additive in the first implementation;
6. `get_text_units` is additive and must not be advertised until its underlying invariants are implemented;
7. clients must use capability advertisement rather than infer support from media type;
8. existing persisted documents require explicit tested migrations;
9. the current six-Tool runtime remains a truthful implementation state until the seventh Tool is implemented.

### 14.3 ADR relationship

- ADR 0002 remains normative for TextUnit identity, normalized ranges, ReadCursor separation, search candidate kinds, and rebuildability.
- ADR 0003 remains normative for EPUB source order, provenance, capability grading, non-prose handling, and coverage.
- ADR 0004 records the use-case-first Tool decision and supersedes only earlier statements that assumed five/current Tools or deferred the need for a generic TextUnit enumeration Tool.

## 15. Acceptance Matrix

| Acceptance area | Required evidence |
|---|---|
| Tool discovery facts | runtime contract test asserts current six until implementation intentionally adds the seventh |
| Discovery | deterministic bounded listing; complete/cursor consistency; policy isolation |
| Open identity | same source/content reuses identity; changed raw/normalized facts are observable; degraded capability is explicit |
| Structure | full/shallow/subtree expansion; cursor gap/overlap; unresolved/fallback provenance |
| Section continuation | every truncated read has next cursor; concatenation equals full declared stream; stale cursor fails |
| Paragraph enumeration | exact Section slices; deterministic ordinals/order; first/next/previous and completion |
| Sentence enumeration | deterministic versioned boundaries; first/next/previous; complete Section traversal |
| Non-prose | code/table not fabricated as Sentences; source-preserving coarse item or explicit gap; truthful denominators |
| Search title | title-only Section candidate preserved without fake Paragraph |
| Search locator handoff | hit locator passes unchanged to read/context; no re-search required |
| Context semantics | tagged neighbor/container/structural relations; legacy Section mapping unchanged |
| Citation | saved Sentence/CharacterRange re-reads exactly for same version |
| Stale state | locator/cursor fail closed; index rebuild with unchanged locator identity does not invalidate source |
| Native traceability | EPUB entry/fragment/spine/provenance retained and resolution claims validated |
| Reliability/coverage | native/fallback/unsupported states and defined coverage dimensions available to Agent |
| Response budgets | all enumeration/read/context/structure responses remain server bounded and actionable |
| Architecture | MCP remains adapter; Document remains source truth; TextUnit/Search indexes remain rebuildable |
| Compatibility | current valid six-Tool calls and persisted legacy Location semantics remain valid |

## 16. Independent Design Review

A separate review was performed after the first complete draft, checking the design against the requested use cases, current code/tests, ADR 0002, and ADR 0003.

### 16.1 Findings corrected

1. **Sentence-only enumeration would silently lose non-prose.** The contract was corrected to distinguish requested/effective kind, classify non-prose, support source-preserving coarse items, and report separate eligibility/coverage dimensions.
2. **Overloading `read_document` looked superficially smaller but mixed two cursor/state machines.** Enumeration was separated from canonical read, producing the evidence-backed `get_text_units` recommendation.
3. **A generic `get_context` parameter bag would hide three meanings.** The design now uses explicit tagged `neighbor`, `container`, and `structural` relations.
4. **Earlier documents assumed five Tools after `list_documents` had been implemented.** The factual current baseline is six; drift is corrected alongside this design.
5. **A dedicated reliability Tool was initially tempting but lacked an independent workflow.** Reliability/coverage stays embedded at open/structure/enumeration decision points until a real use case proves otherwise.

### 16.2 Review gate outcome

- Use Cases precede Capabilities and Tools throughout the document.
- Complete Sentence-first reading, SearchHit handoff, stale state, non-prose, and EPUB degradation/coverage are present.
- The design does not force all responsibilities into the current six Tools.
- The new Tool is supported by Paragraph/Sentence traversal use cases, not convenience alone.
- No conflict was found with ADR 0002/0003; cursor/source identity and EPUB reliability constraints are preserved.
- Current behavior and future recommended contracts are explicitly separated.
- Physical schema, parser algorithm, cursor encoding, and Rust implementation details remain deferred.

## 17. Open Questions

These require implementation pre-research or prototypes but do not reopen the capability/Tool responsibility decision:

1. exact wire shape for tagged target/start/relation unions under the selected MCP/Rust schema tooling;
2. first deterministic Paragraph/Sentence segmentation policy and version;
3. whether a backward enumeration page is returned in source order (recommended) or traversal order, and how clients signal preference;
4. exact representation of coverage ranges versus aggregate counts while staying within response budgets;
5. opaque cursor encoding, integrity protection, expiry, and schema migration;
6. whether every parser eventually persists canonical block metadata or Paragraph v1 derives solely from exact `Section.content`;
7. how long historical document versions are retained for exact old-locator resolution;
8. whether search result pagination is required in the first precise-reading increment;
9. whether future list-item/table-cell/formula reading-item kinds gain independent identity—only after new use-case evidence and ADR review;
10. exact sequencing of capability advertisement fields across open, structure, and enumeration without duplicating large diagnostics.
