# Text Index & Source Locator Architecture

> Status: Draft for design review
>
> Branch: `design/text-index-locator`
>
> Scope: architecture and contract design only. This document does not authorize implementation changes on the design branch.

## 1. Problem statement

Reading MCP currently has a sound top-level separation:

```text
Document / Section = normalized source facts
SearchIndex         = rebuildable derived state
Search              ≠ Read
Section             ≠ Chunk
```

The current v0.1 contracts, however, are section-centric:

```text
read_document(document_id, section_id)
get_context(document_id, section_id, before, after)
```

while the SQLite search index already derives paragraph-like search units by splitting `Section.content` on blank lines.

This creates four gaps for precise reading:

1. search can point below a Section, but read/context cannot address the same fine-grained unit;
2. `read_document` can return `truncated=true`, but there is no continuation locator/cursor;
3. `paragraph` is currently an index-side convention rather than a normalized, versioned textual-unit contract;
4. `char_start` / `char_end` do not yet define one stable coordinate space across parser-native text, normalized Section content, and rendered MCP responses.

The design goal is therefore not "add sentence search". It is:

> Any source passage that Reading MCP exposes should be deterministically addressable, re-readable, expandable to surrounding context, and traceable back to the normalized document and native source location without turning derived indexes into source truth.

## 2. Non-goals

This design does not add AI interpretation or learning state to Reading MCP.

Out of scope:

- summaries, explanations, tutoring, question generation, notes, claims, evidence matrices;
- semantic concepts as part of the source hierarchy;
- Word/Token as stable source-addressing levels;
- LLM-based sentence segmentation;
- format-specific read tools such as `read_pdf_sentence`;
- implementation work on this design branch;
- replacing the current v0.1 convergence scope.

## 3. Core model: five addressing levels

Reading MCP should expose five logical addressing levels:

```text
L0 Document
L1 StructuralNode
L2 Paragraph
L3 Sentence
L4 CharacterRange
```

### L0 — Document

A concrete parsed document version, already identified by `DocumentId` and `content_hash`.

### L1 — StructuralNode

Author/source-defined logical structure such as chapter, section, subsection, heading, operation, or another parser-supported structural node.

The current `Section` domain type is the v0.1 representation of this level. Chapter/Section/Subsection are not separate technical index layers; they are recursive structural nodes with `level`, parentage, path, and source location.

### L2 — Paragraph

A deterministic textual unit inside one structural node.

Paragraph is not automatically source structure. It is a derived textual addressing unit unless a parser can preserve a native paragraph boundary. A parser-native boundary may inform segmentation, but the normalized contract must remain format-independent.

### L3 — Sentence

A deterministic textual unit inside one paragraph.

Sentence is the finest human-semantic addressing level. It is derived state and must be reproducible from the same normalized text and segmentation policy version.

### L4 — CharacterRange

The finest machine-precise range inside a normalized text unit.

Word and model Token are intentionally excluded:

- token boundaries depend on tokenizer/model;
- word boundaries are ambiguous for CJK and technical syntax;
- character ranges are sufficient for exact excerpts below sentence level.

## 4. Source structure, text units, and search indexes are separate

The hierarchy above must not become one giant stored tree.

```text
Normalized source facts
Document
└── StructuralNode (current Section)

Derived text-unit index
StructuralNode
└── Paragraph
    └── Sentence

Derived retrieval indexes
FTS / keyword / future semantic index
        │
        └── return TextLocator
```

Rules:

1. `Document` / `Section` remain source facts in `DocumentRepository`.
2. Paragraph/Sentence are deterministic, rebuildable TextUnits.
3. FTS/BM25 and future search indexes are rebuildable indexes over source facts/TextUnits.
4. Search results never become the canonical text store.
5. `get_document_structure` continues to expose structural nodes, not every paragraph/sentence.

This extends the existing principles:

```text
Search Unit ≠ Read Unit
Index ≠ Document
Section ≠ Chunk
```

with:

```text
StructuralNode ≠ TextUnit
TextUnit Index ≠ Document
Search Index ≠ TextUnit Index
```

## 5. TextUnit

Introduce a conceptual `TextUnit` abstraction for derived fine-grained addressing.

Proposed shape:

```text
TextUnit
├── unit_id
├── kind: paragraph | sentence
├── owner_section_id
├── text
├── locator
└── segmentation_version
```

Important semantics:

- `TextUnit` is derived and rebuildable;
- it does not replace `Section.content` as source truth;
- `text` must be an exact slice of the normalized owner text for the selected coordinate space;
- a sentence must belong to exactly one paragraph and one owner structural node;
- a paragraph must belong to exactly one owner structural node.

Future kinds such as list item, code block, table cell, caption, or footnote may be added only when a real reading/navigation use case requires them. They must not be pre-modeled now.

## 6. Unified TextLocator

Fine-grained tools should converge on one locator model instead of inventing separate sentence/paragraph identifiers per tool.

Proposed logical shape:

```text
TextLocator
├── document_id
├── content_hash
├── owner_section_id
├── section_path
├── unit
│   ├── paragraph_index?
│   └── sentence_index?
├── normalized_range?
│   ├── start
│   └── end
├── segmentation_version?
└── native_location?
```

### 6.1 Index numbering

Human-facing paragraph and sentence ordinals are 1-based:

```text
paragraph_index = 1, 2, 3, ...
sentence_index  = 1, 2, 3, ... within its paragraph
```

This preserves the current search result convention where `paragraph` starts at 1.

Internal arrays may remain 0-based, but DTO semantics must be explicit.

### 6.2 Version scope

A locator containing Paragraph/Sentence ordinals is stable only within:

```text
content_hash + segmentation_version
```

The same `P4/S3` in a different content hash or segmentation version must not be assumed to identify the same source passage.

### 6.3 `unit_id`

For transport efficiency, implementations may expose an opaque `unit_id` derived from the full locator identity, for example:

```text
hash(
  document_id,
  content_hash,
  owner_section_id,
  paragraph_index,
  sentence_index,
  normalized_range,
  segmentation_version
)
```

`unit_id` is a handle, not a substitute for traceability. Responses should continue to return the resolved locator fields needed for debugging/citation.

## 7. Character coordinate model

The current `Location.char_start` / `char_end` cannot safely become the only range contract without clarification.

Observed current behavior includes:

- Markdown computes offsets against the original Markdown UTF-8 text using Rust `.chars().count()` over source prefixes;
- Section bodies may then be `.trim()`ed before storage/rendering;
- `read_document` renders a Section tree by injecting Markdown headings and child content;
- other formats primarily expose page/paragraph/native locations and may have no meaningful document-global character offset.

Therefore three coordinate spaces must be kept distinct:

```text
A. source/native coordinates
   PDF page, EPUB spine/entry, HTML anchor, DOCX paragraph, source text offsets, ...

B. normalized owner-text coordinates
   exact positions inside normalized Section.content / TextUnit text

C. rendered response coordinates
   positions inside a particular MCP response payload
```

Only B is appropriate for general fine-grained TextLocator ranges.

### 7.1 Proposed normalized range semantics

`normalized_range` is:

- relative to the normalized owner text named by the locator;
- zero-based;
- half-open `[start, end)`;
- measured in Unicode scalar values (Rust `char` count), not UTF-8 bytes, UTF-16 code units, grapheme clusters, words, or model tokens.

Example:

```text
owner_section_id = section://chapter-1/processes
paragraph_index  = 4
sentence_index   = 3
normalized_range = [182, 241)
```

The range must identify the exact normalized text returned for that TextUnit.

### 7.2 Existing `char_start` / `char_end`

During migration, the existing fields remain backward compatible and retain their current parser-defined meaning. They must not silently change semantics.

A future implementation should either:

1. introduce explicit `normalized_range` fields; or
2. version/rename the old character fields before giving them new semantics.

Silent reinterpretation is forbidden.

## 8. Segmentation policy

Paragraph and sentence indexes must be deterministic and versioned.

```text
same content_hash
+ same normalized text
+ same segmentation_version
= same Paragraph/Sentence boundaries
```

### 8.1 Paragraph segmentation

The current SQLite implementation uses `Section.content.split("\n\n")`. That is acceptable as an implementation detail for the current search index, but it is not sufficient as the long-term locator contract.

The segmentation layer should prefer, in order:

```text
parser-preserved native paragraph boundaries, when reliable
        ↓
normalized blank-line/block boundaries
        ↓
deterministic fallback rules
```

The selected normalized paragraph boundaries must be reproducible independently of the search engine.

### 8.2 Sentence segmentation

Sentence segmentation must be:

- deterministic;
- non-LLM;
- versioned;
- Unicode-aware;
- conservative around abbreviations, decimals, APIs, shell commands, and technical identifiers;
- able to support at least English and CJK punctuation used by supported documents.

A first policy may recognize terminal punctuation such as:

```text
. ? ! 。 ？！
```

while protecting common non-terminal forms such as abbreviations and numeric/technical punctuation.

### 8.3 Non-prose blocks

Code blocks, tables, signatures, and other non-prose structures must not be force-split into sentences merely to satisfy the hierarchy.

If a normalized paragraph is non-prose, it may remain paragraph-addressable without sentence children.

### 8.4 Versioning

Segmentation policy must carry an explicit stable version, for example:

```text
text-segmentation/v1
```

Changing boundary rules in a way that can renumber Paragraph/Sentence locators requires a new version.

## 9. Tool contract evolution

The preferred direction is to strengthen the existing five MCP tools rather than multiplying format/granularity-specific tools.

### 9.1 `open_document`

No required breaking change.

A future response may advertise supported location capabilities:

```text
paragraph_locator
sentence_locator
normalized_range
native_page
native_anchor
```

Capability advertisement is optional and should be added only if clients need it.

### 9.2 `get_document_structure`

Continue returning structural nodes only.

Do not enumerate every Paragraph/Sentence in the TOC tree.

Optional future aggregate metadata may include:

```text
paragraph_count
sentence_count
```

only if it can be produced within response/resource budgets.

### 9.3 `search_document`

Search continues to answer "where?", not return large reading payloads.

Future hit shape should resolve to a fine-grained locator:

```text
SearchHit
├── section_id
├── title
├── snippet
├── score
├── location            # backward-compatible current location
└── text_locator?       # paragraph/sentence/range when available
```

Search granularity may later support:

```text
auto | paragraph | sentence
```

but FTS/BM25 remains the primary search design unless evidence shows otherwise.

### 9.4 `read_document`

This is the highest-priority contract evolution.

Current behavior:

```text
(document_id, section_id, max_chars)
→ rendered Section tree
→ truncated=true/false
```

Future behavior must support continuation without breaking the existing Section read request.

Conceptually:

```text
read_document
├── legacy target: section_id
└── precise target: locator | unit_id | continuation cursor
```

Responses should provide an explicit returned range and, when truncated, a continuation handle:

```text
content
truncated
returned_range / returned_locator
next_cursor? / next_locator?
```

A continuation token must be bound to the same document version and logical target so it cannot silently continue into changed content.

### 9.5 `get_context`

Current Section-neighbor semantics remain valid.

Future context should be able to expand around the same locator used by search/read:

```text
unit = section | paragraph | sentence
before = N
 after = N
```

Example:

```text
get_context(target=P4/S3, unit=sentence, before=2, after=2)
```

Context expansion must preserve source order and return locators for the expanded boundary.

## 10. Continuation and response budgets

Precise reading must work with the existing MCP response-budget policy rather than bypass it.

Requirements:

1. `max_chars` remains bounded server-side;
2. `truncated=true` must be actionable;
3. continuation must be deterministic for one document version;
4. clients must not be forced to restart from the beginning of a Section;
5. continuation must not rely on rendered-response offsets as a stable source locator.

Recommended conceptual identity for an opaque cursor:

```text
cursor binds:
- document_id/content_hash
- target locator
- rendering/read mode
- next normalized position
- cursor schema version
```

The wire encoding may be opaque; the semantic binding must be testable.

## 11. Citation and human-readable form

Machine locators remain structured. A human-readable citation may be rendered as:

```text
TLPI §1.1 ¶4 S3
```

or, when native source information is available:

```text
TLPI §1.1 ¶4 S3, p. 5
```

This display string is presentation, not identity.

Identity remains the structured locator scoped to document content/version.

## 12. Persistence and rebuildability

The repository remains the source of normalized document facts.

Derived text-unit state may be stored in SQLite for performance, but must be rebuildable from the persisted Document plus the declared segmentation policy.

Recommended separation:

```text
DocumentRepository
        │
        ├── normalized Section facts
        │
TextUnitIndex (derived)
        │
        ├── paragraph/sentence boundaries + locators
        │
SearchIndex (derived)
        │
        └── FTS/BM25 retrieval records referencing locators
```

Changing the search engine must not change Paragraph/Sentence identity.

Changing segmentation policy may change Paragraph/Sentence identity and therefore requires a segmentation-version change and TextUnit/SearchIndex rebuild.

## 13. Lexical index and tokenizer architecture

Precise TextUnits should also be the stable bridge between reading locators and lexical retrieval. The lexical layer is derived state and must not introduce new source-addressing levels.

```text
Normalized Section                  # canonical source
        │
        ├── Paragraph TextUnit      # derived, persisted/rebuildable
        │
        └── Sentence TextUnit       # derived, persisted/rebuildable
                 │
                 ▼
          TokenizerPolicy
                 │
                 ▼
      Paragraph FTS / Sentence FTS
                 │
                 ▼
              BM25
                 │
                 ▼
            TextLocator
```

The critical boundary is:

```text
Sentence = reading/evidence unit
Token    = retrieval implementation detail
FTS      = rebuildable lexical index
```

Token is therefore not L5 in the source hierarchy and does not receive stable source identity.

### 13.1 Search granularity

The lexical index should support both Paragraph and Sentence TextUnits. They optimize for different retrieval goals:

- Sentence FTS improves precision for exact terminology, API names, definitions, and evidence localization.
- Paragraph FTS preserves local explanatory context and improves recall when several query terms occur across one argument or explanation.

The intended future search contract remains additive:

```text
granularity = auto | paragraph | sentence
```

`auto` is the default client-facing mode. Its exact ranking/fallback policy is implementation-defined, but a conforming implementation may, for example, try high-precision Sentence matches and fall back to Paragraph retrieval when results are insufficient. This is compatible with the existing FTS/BM25-first design and the current relaxed-search work.

Search granularity must never change TextUnit identity. It only changes which existing TextUnits are indexed/ranked.

### 13.2 Tokenizer policy

Tokenization must be deterministic, non-LLM, versioned, and replaceable independently of TextUnit segmentation.

Conceptually:

```text
TokenizerPolicy
├── Latin-like text: Unicode-aware word/token strategy
└── CJK/mixed text: deterministic CJK-capable strategy
```

The architecture does not mandate one tokenizer library today. For CJK, implementations should evaluate deterministic n-gram/trigram or another CJK-capable tokenizer before introducing dictionary-heavy NLP dependencies. The selected strategy must handle mixed technical text such as:

```text
系统调用 fork() mmap() O_CREAT virtual memory
```

without depending on whitespace-only boundaries.

### 13.3 Tokenizer version is independent of segmentation version

TextUnit identity is scoped by:

```text
content_hash + segmentation_version
```

Lexical-index identity additionally depends on:

```text
tokenizer_version
```

For example:

```text
content_hash         = A
segmentation_version = text-segmentation/v1
tokenizer_version    = lexical-tokenizer/v1
```

Changing `tokenizer_version` may change search ranking/matching and requires rebuilding the lexical index, but it must not renumber Paragraph/Sentence TextUnits or invalidate their locators.

Changing `segmentation_version`, by contrast, may change TextUnit boundaries and therefore requires rebuilding both TextUnit and lexical indexes.

### 13.4 Do not persist a separate token table by default

The default persistence model should store Paragraph/Sentence TextUnits plus the FTS index, not one relational row per token.

Avoid this unless a demonstrated feature requires it:

```text
tokens
├── token_id
├── sentence_id
├── token_index
└── token
```

SQLite FTS already materializes an inverted lexical structure internally. Duplicating every token into an application-level table increases schema/storage complexity without improving source traceability.

A future feature may add token-level diagnostics, but such data remains derived retrieval metadata and still must not become a source locator level.

### 13.5 Lexical persistence model

The intended logical separation is:

```text
documents / sections
────────────────────────
canonical normalized source

text_units
────────────────────────
unit_id
document_id
content_hash
owner_section_id
kind: paragraph | sentence
parent_unit_id?
paragraph_index
sentence_index?
normalized_range
text
segmentation_version

text_units_fts
────────────────────────
unit_id
kind
text
# tokenizer/version/config tracked by index metadata
```

The exact SQLite schema is deferred, but these semantics are required:

1. Paragraph/Sentence text may be persisted for efficient direct reads, snippets, validation, and index rebuilds.
2. Persisted TextUnit text is derived and must exactly correspond to the normalized source range.
3. `text_units_fts` references TextUnits; it does not duplicate the full locator contract.
4. Deleting/rebuilding FTS must not alter TextUnits.
5. Deleting/rebuilding TextUnits with the same content and segmentation version must reproduce the same boundaries and deterministic identities.

### 13.6 SearchHit → TextLocator

Lexical search results should resolve back to the same locator contract used by reading and context expansion. A future fine-grained result may conceptually contain:

```text
SearchHit
├── unit_id
├── unit_kind: paragraph | sentence
├── score
├── snippet/text
└── text_locator
    ├── document/content version
    ├── owner section
    ├── paragraph/sentence ordinal
    └── normalized range
```

The expected interaction is:

```text
search_document("system call")
        ↓
Sentence TextUnit / TextLocator
        ↓
get_context(target, unit=sentence, before=2, after=2)
        ↓
read_document(target)
```

No re-search by quoted text should be necessary to move from retrieval to reading.

### 13.7 Search-index rebuild rules

A lexical index is valid only for the declared inputs that produced it. Rebuild is required when any retrieval-affecting input changes, including:

- indexed document content hash;
- TextUnit segmentation version;
- tokenizer version/configuration;
- FTS schema/ranking configuration when the implementation cannot migrate safely.

Rebuilding the lexical index is operational state maintenance, not a source-content mutation.

### 13.8 Future semantic indexes remain separate

Embedding, semantic, concept, claim, or entity indexes may be introduced later, but they follow the same rule:

```text
Derived retrieval index
        ↓
returns TextUnit/TextLocator
        ↓
canonical read/context path
```

They do not redefine source identity and do not bypass the Paragraph/Sentence/CharacterRange locator model.

## 14. Backward compatibility

The first implementation must preserve all current valid calls:

```text
open_document
get_document_structure(document_id, max_depth?)
search_document(document_id, query, limit)
read_document(document_id, section_id, max_chars?)
get_context(document_id, section_id, before, after, max_chars?)
```

Compatibility rules:

1. existing request fields keep their meaning;
2. existing response fields are not silently reinterpreted;
3. new precise-reading fields are additive in the first version;
4. legacy Section reads remain supported even after locator reads exist;
5. old `Location` data remains readable from persisted state;
6. migrations must be explicit and tested against already-opened documents.

## 15. Proposed implementation sequence after design approval

Implementation should not begin on this branch. After review/merge, use short-lived feature branches in this order:

```text
P0 feat/read-continuation
   - actionable continuation for truncated Section reads
   - explicit returned range/cursor semantics

P0 feat/normalized-text-range
   - define/test normalized owner-text coordinate space
   - no silent reuse of parser-native offsets

P1 feat/text-unit-index
   - deterministic Paragraph units
   - TextLocator + TextUnit abstraction

P1 feat/sentence-locator
   - versioned deterministic sentence segmentation
   - Sentence locators

P1 feat/context-granularity
   - paragraph/sentence context windows

P1 feat/search-locator
   - search hits resolve to TextLocator
   - preserve FTS/BM25-first design

P1 feat/lexical-text-unit-index
   - persist/rebuild Paragraph + Sentence FTS
   - version TokenizerPolicy independently from segmentation
   - support CJK-capable deterministic tokenization
   - keep Token outside source locator identity

P2 evaluate/get-text-units-tool
   - add a new tool only if real client usage shows that paged enumeration
     of Paragraph/Sentence units cannot be expressed cleanly through existing tools
```

This sequence deliberately separates continuation from sentence segmentation so the current truncation problem can be solved without waiting for the full indexing architecture.

## 16. Acceptance criteria for the architecture

A future implementation conforms to this design only if all of the following are true.

### Addressability

- A SearchHit below Section level can be passed to read/context without re-searching the source text.
- A Paragraph/Sentence locator resolves to exactly one normalized passage in the same document version.
- Character ranges can identify an exact sub-sentence excerpt without Word/Token identity.

### Determinism

- Rebuilding TextUnitIndex with the same content and segmentation version yields identical boundaries/locators.
- Sentence segmentation does not call an LLM.
- Search-engine rebuild/replacement does not renumber TextUnits.

### Traceability

- Fine-grained locators retain owner Section and document version.
- Native page/anchor/spine/paragraph information remains available when the parser can provide it.
- Human citation formatting can be derived from structured locator data.

### Lexical retrieval

- Paragraph and Sentence TextUnits can be indexed without becoming source truth.
- Sentence lexical hits can be passed directly to read/context through TextLocator.
- Tokenizer changes rebuild lexical indexes without renumbering TextUnits.
- CJK/mixed technical text does not depend on whitespace-only tokenization.
- No stable Word/Token locator is introduced by the FTS implementation.

### Continuation

- Every truncated read has an actionable continuation.
- Repeated continuation can consume the complete logical target without gaps or overlap.
- A cursor cannot silently continue against a different content hash.

### Compatibility

- Existing Section-based clients continue to work.
- Existing persisted Location records do not become semantically invalid through silent reinterpretation.

### Resource control

- Fine-grained reads/context obey the same response/resource budgets as Section reads.
- `get_document_structure` does not explode into a sentence-sized tree.

## 17. Design invariants

The following are hard constraints unless replaced by a later ADR:

1. **Document is source truth; indexes are derived state.**
2. **StructuralNode is source structure; Paragraph/Sentence are textual addressing units.**
3. **Sentence is not a child Section.**
4. **Word/Token are not stable source locator levels.**
5. **Paragraph/Sentence identity is scoped by content hash and segmentation version.**
6. **Search returns locators; search results do not become reading truth.**
7. **Normalized ranges and native/source offsets are distinct coordinate spaces.**
8. **Rendered MCP response offsets are never stable source locators.**
9. **Segmentation is deterministic, non-LLM, and versioned.**
10. **Truncation without a continuation path is incomplete reading semantics.**
11. **Existing five tools should be extended before adding granularity-specific tools.**
12. **Token is a retrieval implementation detail, never a stable source locator level.**
13. **Tokenizer versioning is independent from segmentation versioning.**
14. **Paragraph/Sentence FTS is rebuildable derived state and must resolve back to TextLocator.**
15. **This design does not move AI reasoning/learning state into Reading MCP.**

## 18. Questions intentionally deferred to implementation design

These choices need prototypes/tests but do not block the architecture:

- exact Unicode sentence-boundary library or custom policy implementation;
- exact Latin/CJK tokenizer implementation and first `TokenizerPolicy` version;
- physical SQLite schema for TextUnitIndex and Paragraph/Sentence FTS;
- opaque cursor encoding/signing/checksum strategy;
- whether `unit_id` is persisted or deterministically recomputed;
- whether paragraph boundaries should be emitted directly by every parser or normalized in one shared segmentation layer;
- exact MCP DTO shape for additive locator targets;
- whether a separate `get_text_units` tool is ever necessary.

They may change implementation details, but they must preserve the invariants above.
