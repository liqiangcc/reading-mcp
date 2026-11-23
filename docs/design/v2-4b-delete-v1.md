# V2-4b: the plan for deleting the v1 tools

The default tool set has been v2 (`open`, `read`, `browse`, `outline`, `search`, `cite`,
`source_view`) since v2-3. The nine v1 tools — `list_documents`, `list_directory`,
`open_document`, `get_document_structure`, `get_text_units`, `search_document`,
`read_document`, `get_source_view`, `get_context` — still exist behind
`READING_MCP_TOOLS=v1|both` (`src/runtime/config.rs:170`, `src/mcp/server.rs:166`) because
tests, measurement scripts and CI gates call them. V2-4a (`v2-4a-measurement-in-v2.md`,
merged PR #246) gave every measurement script an opt-in `--tools v2`; V2-4b PR-4 flipped every
script and workflow default to `v2`, so v1 is now only reachable by asking for it. This document
is the plan for deleting v1 without losing test coverage. It changes no code.

The inventory below is generated; re-run it before each PR:

```
python3 scripts/v1-inventory.py            # markdown
python3 scripts/v1-inventory.py --json     # machine-readable
```

## 1. Inventory

### 1.1 v1 tool mention counts (lines containing the whole-word name)

| v1 tool | files | mentions |
|---|---:|---:|
| `list_documents` | 13 | 36 |
| `list_directory` | 9 | 16 |
| `open_document` | 77 | 171 |
| `get_document_structure` | 39 | 84 |
| `get_text_units` | 61 | 163 |
| `search_document` | 31 | 72 |
| `read_document` | 42 | 91 |
| `get_source_view` | 12 | 13 |
| `get_context` | 29 | 74 |

`open_document`, `get_text_units`, `read_document`, `get_document_structure` dominate; most
test files only *mention* a v1 name in a fixture or a v1/v2 comparison — 73 files under
`tests/` contain a name, but only the 24 in §2 actually drive the v1 tool surface
(`READING_MCP_TOOLS=v1|both` and/or real `tools/call` to v1 names).

### 1.2 v1-only code under `src/` (deleted with the switch)

| file | lines | what it is |
|---|---:|---|
| `src/mcp/contracts.rs` | 706 | v1 request/response DTOs (`OpenDocumentRequest` … `GetContextResponse`) |
| `src/mcp/server.rs` `tool_router_v1` block | ~575 | the nine `#[tool]` handlers + `ToolSet` dispatch in `active_tools()` |
| `src/mcp/ocr_error_tests.rs` | 125 | in-crate test bound to `ToolSet::V1` + `open_document` |
| `src/application/get_document_structure.rs` | 862 | v1 structure use case |
| `src/application/get_context.rs` | 898 | v1 `get_context` use case (incl. legacy + structured forms) |
| `src/application/read_document.rs` | 582 | v1 `read_document` use case |
| `src/application/read_cursor.rs` | 304 | v1 read continuation cursor (`read-cursor/v1`) |
| `src/application/text_unit_cursor.rs` | 228 | v1 `get_text_units` cursor (`text-unit-cursor/v1`) |
| `src/application/structure_cursor.rs` | 234 | v1 `get_document_structure` cursor |
| `src/runtime/config.rs` `ToolSet` + parse | ~35 | enum, `ToolSet::parse`, the `READING_MCP_TOOLS` env parse, its unit tests |
| `src/runtime/mod.rs` wiring | ~30 | `ToolSet` re-export, `build_server` match on `config.tools` |
| `src/mcp/profile_mapping.rs` | 122 | maps `ReadingProfile` to the v1 `ReadingProfileDto` (`contracts.rs`); v2 `open` never surfaces it (§2.1 item 5). `src/application/reading_profile.rs` stays: v2 `open` reads `canonical_text_coverage.owner_chars` from it |
| `src/mcp/ocr_error_tests.rs` + `src/mcp/server.rs` v1 fields | — | server fields `open_document`…`get_context` stay: v2 uses the same use-case objects |

### 1.3 src/ files v1 calls into but v2 keeps — deletion must not touch

| file | lines | why v2 keeps it |
|---|---:|---|
| `src/application/open_document.rs` | 293 | v2 `open` delegates to `OpenDocumentUseCase` (`open_reading.rs:5`) |
| `src/application/get_text_units.rs` | 773 | kind/policy enums (`EffectiveTextUnitKind`, `TextUnitDirection`, coverage policy) are used by `read_reading.rs`, `sqlite.rs`, `sqlite_search_index.rs`, `observability.rs`; only the v1 use case + cursor path inside dies |
| `src/application/search_document.rs` | 291 | v2 `search` wraps this use case (`search_reading.rs:12`) |
| `src/application/text_unit_stream.rs` | 492 | `whole_paragraph_kind`/`coarse_degradation` feed v2 `read`/`cite` |
| `src/application/list_documents.rs` / `list_directories.rs` | 304 / 319 | v2 `browse` is these two use cases behind one tool |
| `src/application/source_view.rs` | 324 | shared renderer for `get_source_view` and `source_view` |
| `src/application/locator_resolution.rs` | 258 | v2 `cite`/`read` resolve the same locators |
| `src/application/section_rows.rs`, `handle_reading.rs`, `short_handle.rs`, `section_view.rs`, `reading_support.rs`, `derived_text_units.rs`, `light_sections.rs`, `discovery_cursor.rs`, `directory_cursor.rs` | ~2300 | shared plumbing used by both routers |

Partial-delete files: `get_text_units.rs` and `search_document.rs` keep their shared
internals and lose only the v1 use-case entry points and DTO plumbing; `server.rs` loses
the v1 router block, the `ToolSet` match arm (keeping the v2 router unconditionally), and
v1-only request/response imports.

## 2. Per test file that drives v1 today

The 24 files that run v1 tools (`READING_MCP_TOOLS=v1|both` or literal v1 `tools/call`).
"Coverage" columns name an existing v2 test when one protects the same behaviour;
"needs" means a v2 port must land **before** the file is deleted.

| # | test file | behaviour it protects | covered by existing v2 test? | plan |
|---|---|---|---|---|
| 1 | `tests/acceptance_matrix.rs` | stdio smoke over txt/md/html/pdf/epub: open → structure → read → search | `v2_open_read::v2_reading_matches_v1_section_by_section_for_every_format` covers open+read per format; `v2_tools` covers search — but not the **per-format matrix** shape | **new v2 test**: matrix over the same fixtures calling `open`/`outline`/`read`/`search` |
| 2 | `tests/context_granularity_stdio.rs` | `get_context` consumes a text locator, tagged relations | none — v2 has no context tool | **new v2 test** of the kept slice: `cite` + `read{"from":locator}` against the same locator; the tagged parent/children/neighbour relations are dropped (see §2.1) |
| 3 | `tests/directory_navigation_stdio.rs` | browse a workspace tree then open a document over stdio | `v2_tools::browse_lists_documents_and_directories_as_compact_triples` covers browsing; the "browse then open" journey is not | **new v2 test**: `browse` → `open` stdio journey |
| 4 | `tests/named_section_boundary_mcp.rs` | named-section query returns boundary metadata without body leakage | `v2_tools::outline_lists_the_tree_and_its_handles_read` covers tree+handles; the *named-section* query is v1-only (`get_document_structure` arg) | **new v2 test** only if v2 grows a named query; otherwise **dropped** — `outline` has no name filter (decide per §2.1) |
| 5 | `tests/normalized_text_contract_mcp.rs` | open response carries `normalized_document_hash(_version)`, `normalization_version`, `normalized_text_coordinate_space`; read stream's coordinate space differs | none — v2 DTOs do not surface `normalized_text_coordinate_space`; hash identity lives inside `cite` items | **new v2 test** of the kept slice (hash/version fields on `open`/`cite` payload); the coordinate-space field assertion is **dropped** (v1-only output field) |
| 6 | `tests/ocr_mcp_stdio.rs` | real OCR over stdio: mixed native order, disabled-OCR rejection, cold/warm open budgets, saved locator re-read after restart | `src/mcp/ocr_error_tests.rs` is v1 too; `tests/ocr_private_mcp.rs` support also v1; v2 has no OCR-over-MCP test | **port**: same four tests on `open`/`read`/`source_view` |
| 7 | `tests/ocr_mcp_http.rs` | real-OCR saved locator survives an HTTP server restart | none | **port** to `open`+`cite`/`read` |
| 8 | `tests/open_reading_profile_stdio.rs` | `open` response carries the reading profile; tool surface unchanged | none — v2 `open` answers with `OpenView` (`doc`, `title`, `format`, `sections`, `chars`, `top`, `more_top`) and no profile; PR-1 (#258) found this gap | **dropped** (decided 2026-11-23, §2.1 item 5); the tool-surface leg is covered by `the_default_tool_set_is_the_seven_v2_tools` |
| 9 | `tests/parsed_cache_pdf_only_stdio.rs` | opening Markdown writes no parsed-cache entry | none over MCP | **port** to `open` (or drop — `parsed_cache` behaviour is covered non-MCP; see file) |
| 10 | `tests/phase6_mcp_stdio.rs` | end-to-end stdio reading flow incl. restart persistence | `v2_open_read`+`v2_tools` cover the calls but not the journey + restart | **port** to v2 tools (the journey is the coverage) |
| 11 | `tests/phase8_http_reading_lifecycle.rs` | same journey over streamable HTTP + restart resume | none — all v2 tests are stdio | **port** to v2 tools; keeps the only HTTP lifecycle test |
| 12 | `tests/precise_read_locator_stdio.rs` | text locator hands off to an exact small-budget read with continuation | `v2_open_read::every_item_handle_starts_at_its_own_item` + `forward_next…` cover handle-anchored reads; not the small `max_chars` continuation | **new v2 test**: `read{"from":handle, "max_chars":n}` continuation |
| 13 | `tests/read_continuation_mcp.rs` | section-tree read continuation until `complete` | `v2_open_read` section walk covers it partially | **port**: `read` + `outline` next-section continuation |
| 14 | `tests/search_locator_stdio.rs` | sentence search hit hands an exact locator to read and context | `v2_tools::search_returns_sentence_handles_that_read_at_the_match` covers search→read; the get_context leg is v1-only | covered **except** context leg → **dropped** per §2.1 |
| 15 | `tests/section_text_content_store.rs` | `document_json` keeps no section text when `section_text_v3` has it; reads back on load | storage test that happens to drive v1 MCP calls | **port** to v2 calls (or drive the store directly) |
| 16 | `tests/source_view_stdio.rs` | `get_source_view` audit metadata + PNG over stdio; isolated-renderer deadline kill | `v2_tools::source_view_renders_the_page_of_a_handle` covers the v2 tool; the renderer-deadline test is transport-agnostic but bound to v1 | **port** both tests to `source_view` |
| 17 | `tests/source_view_http.rs` | bound original PNG over streamable HTTP | none (no HTTP v2 test) | **port** to `source_view` |
| 18 | `tests/stdio_initialization_guard.rs` | pre-`initialize` `tools/call` is rejected without killing stdio | protocol guard; only the tool name is v1 | **keep**, s/`list_documents`/`browse`/ |
| 19 | `tests/structure_continuation_mcp.rs` | structure cursor continues a page forest without repeating ancestors | none — v2 `outline` has `limit`/`next` paging but no forest cursor test | **new v2 test**: `outline` `next` cursor across a multi-page forest |
| 20 | `tests/text_unit_anchor_stdio.rs` | `get_text_units` continues exclusively after a locator anchor | `v2_open_read::at_names_the_item_only_when_from_is_not_its_own_handle` covers anchored reads | **port** the exclusive-continuation assertion to `read{"from":handle}` |
| 21 | `tests/v2_book_equivalence.rs` | real-book v2/v1 sequence equivalence (ignored, needs `READING_MCP_BOOK_DIR`) | it *is* the equivalence proof | **rewire to v1less**: after v1 dies it can still compare `read` sequences against stored expectations, or be dropped — decide in PR-5 |
| 22 | `tests/v2_coarse_paragraph_kind.rs` | v2 `read`/`cite` mark a whole-paragraph answer; env helper tolerates absent `READING_MCP_TOOLS` | v2 already | **keep as-is** (env helper just stops setting the var) |
| 23 | `tests/v2_open_read.rs` | v2 open/read contract **plus** v1 tools/list byte-pin, switch behaviour, v1 comparators | the v1 comparators exist only to prove v2==v1 | **keep v2 tests**; delete `tools_switch_*` v1 legs, `DEFAULT_TOOLS_LIST_BYTES`, `v1_*` helpers |
| 24 | `tests/v2_tools.rs` | v2 browse/outline/search/cite/source_view contracts; `cite`/v1 locator equality; `v1_stays_selectable` | — | **keep**; drop the `get_text_units` rejection check and `v1_*` helpers, keep `cite` locator assertions against stored locators |

Plus `src/mcp/ocr_error_tests.rs` (in-crate): binds `ToolSet::V1` + `open_document` to prove
OCR errors stay actionable and private. **Port** to `ToolSet::V2` + `open` — trivial, same
assertions. And `tests/support/ocr_private_mcp.rs`: helper sets `READING_MCP_TOOLS=v1` —
used by `ocr_private_mcp` tests; **port** the env (or drop the helper when its callers move
to v2, which is the default).

### 2.1 Behaviours v1 protects that v2 cannot express — dropped, not silently

These have no v2 counterpart; each is a deliberate deletion, listed so nothing is lost
silently:

1. `get_context` tagged relations (parent / children / ancestors / neighbours) —
   `context_granularity_stdio.rs`, `search_locator_stdio.rs` context leg. V2-4a already
   dropped these from the measurement scripts; no v2 tool returns structural relations.
2. `get_document_structure` `named_section_query` — `named_section_boundary_mcp.rs`.
   `outline` has no name filter. *Alternative:* add `at`-by-name to `outline` first; that
   is a feature decision, out of scope here — default is drop.
3. v1 response fields with no v2 name: `open.normalized_text_coordinate_space`,
   `read.stream.coordinate_space`, `GetTextUnitsResponse` coverage statistics,
   `get_context` section window shapes.
4. The `READING_MCP_TOOLS` switch itself — `tools_switch_selects_the_registered_tools`,
   `v1_stays_selectable`, `the_default_tool_set_is_the_seven_v2_tools` becomes the only
   remaining list test.
5. The `open` reading profile (`reading_profile`: schema version, capabilities such as
   paragraph enumeration, reliability summary) — `open_reading_profile_stdio.rs`. v2 `open`
   answers with `OpenView` and no profile; the versions a caller needs to pin are on `cite`
   (`doc.seg`, `doc.norm`, `doc.ndh`). **Decided 2026-11-23: drop it; v2 `open` does not grow
   a profile.** PR-1 (#258) had added an ignored v2 test for it; that test is removed.

## 3. Scripts and workflows: the defaults are v2

PR-1 (#258), PR-2 (#262) and PR-3 (#260, PDF batch; #261, memory batch) are merged. PR-4
(#265, this PR) flips every measurement default from v1 to v2: every script that has `--tools v1|v2` now
defaults to `v2`, and every workflow `tools` input defaults to `v2` (push, pull_request and
schedule events use the `${{ inputs.tools || 'v2' }}` fallback). v1 stays selectable with
`--tools v1` / `tools=v1` for rollback until PR-6 deletes it.

The 17 scripts with the flag, all defaulting to `v2`:
`deep-next-probe.py`, `doc-read-probe.py`, `first-search-probe.py`, `ingest-parity.py`,
`long-read-probe.py`, `mem-profile-now.py`, `memory-profile.py`, `pdf-cjk.py`,
`pdf-corpus.py`, `pdf-probe.py`, `pdf-quality.py`, `pdf-sweep.py`,
`sequential-read-probe.py`, `stress-probe.py`, `url-resolve-probe.py`, `v2-eval.py`, and
`scripts/pdf-layout/smoke.py`.

The 18 workflows whose `tools` input now defaults to `v2` (and whose fallback is `v2`):
`book-profile`, `cpu-profile`, `doc-read-profile`, `first-search-probe`, `heap-profile`,
`ingest-parity`, `leak-profile`, `mem-profile-now`, `memory-profile`, `pdf-cjk`,
`pdf-corpus`, `pdf-profile`, `pdf-quality`, `pdf-sweep`, `pdf2c-measure`, `section-scale`,
`stress-profile`, `url-resolve-probe`.

Not flipped, and why:

| script | why the default stays v1 |
|---|---|
| `scripts/pdf-open-profile.py` | has no `--tools` and calls v1 tools only; `pdf2c-measure.yml` runs it beside the now-v2 `memory-profile.py` and its input description says so. It is ported or deleted with the OCR/PDF leftovers in PR-5/PR-6, not here. |

The workflows that add `--env READING_MCP_TOOLS=both` only in v1 mode keep that expression; it
now fires only when a run asks for `tools=v1`.

Before the flip, the `ingest-parity.py` v2 sequence gained the coverage the v1 legs carried
(`outline` first page + `next`, `search` with the v1 body term and section title, `cite` of the
first read item, a `next` item and the first search hit), so the default gate keeps search and
structure coverage in v2 mode. `run_v1_reads` and every v1 code path stay for PR-6.

Every other script mention (`v2-eval.py`'s v1 legs, etc.) is the already-gated `--tools`
machinery; `v2-eval.py`'s v1 legs and the workflows' `tools` inputs are deleted with the
server code in the last PR.

## 4. Outside users of v1

The deployed host runs v2 by default (`ToolSet::default() == V2`). A client can still be
using v1 names only if something explicitly sets `READING_MCP_TOOLS=v1` or `=both` — the
deploy scripts (`deploy/`, `install-production.sh`) and any client-side service wrapper are
the places to grep before the deletion PR lands.

Two options for what removing the switch does to such a config:

- **Refuse to start**: keep parsing `READING_MCP_TOOLS`, accept only `v2`, and error on
  `v1`/`both` — a config that asked for v1 fails loudly at boot instead of silently serving
  a different tool list. Cost: keeps ~15 lines of parse + error for one env var.
- **Ignore / fall back to v2**: delete the env var entirely; a stale `=v1` deployment
  silently serves v2, and the client's first `open_document` call fails with
  tool-not-found. Cleanest code; the failure surfaces at call time, not boot.

**Recommendation: refuse to start.** An operator who pinned `v1` deserves a loud boot
error (`READING_MCP_TOOLS must be one of v2, got "v1"` — reuse the existing message shape),
and the check is 15 lines; silent fallback turns a config mistake into a confusing client
error. The user decides; either way the parse lives or dies in the final PR.

## 5. The PR split

Ordered so every PR is reviewable and the tree is green at each step.

| PR | content | gate | risk |
|---|---|---|---|
| **1. v2 tests for the uncovered behaviours** — merged, #258 | new v2 tests: acceptance matrix, browse→open journey, `read` small-`max_chars` continuation, `outline` forest cursor, `read{"from":handle}` exclusive continuation, `open` reading-profile fields, cite/read context slice, OCR-over-MCP (stdio + HTTP + `ocr_error_tests.rs` ported to `open`), v2 HTTP lifecycle; normalized-hash slice | `CI` | low — pure test additions; may expose real v2 gaps (that is the point) |
| **2. Re-cover or rewire** — merged, #262 | port `phase6_mcp_stdio`, `phase8_http_reading_lifecycle`, `source_view_stdio`, `source_view_http`, `read_continuation_mcp`, `parsed_cache_pdf_only_stdio`, `section_text_content_store`, `ocr_mcp_stdio`, `ocr_mcp_http`, `ocr_private_mcp` helper to v2 names; `stdio_initialization_guard` s/`list_documents`/`browse`/ | `CI` | medium — big diff, mechanical; risk is a subtly weaker assertion |
| **3. Port the deferred scripts** — merged, #260 (PDF batch) and #261 (memory batch) | `pdf-probe.py` (+`pdf-corpus.py` helpers), `pdf-quality.py`, `pdf-sweep.py`, `pdf-cjk.py`, `pdf-layout/smoke.py`, `memory-profile.py` grow the V2-4a `--tools` mode | `CI`; each workflow's dispatch runs once with `--tools v2` before merge | medium — measurement drift; must land **after** PDF-6/LAYOUT-1 PRs (#247, #248) merge |
| **4. Flip every default to v2** — this PR, #265 | `--tools` default `v1`→`v2` in all 17 scripts that have the flag; the `tools` default and `\|\|` fallback in all 18 workflows; the new `outline`/`search`/`cite` coverage in `ingest-parity.py`'s v2 sequence. The v1 legs of `ingest-parity.py` and `v2-eval.py` stay until PR-6. | `CI` + one default-mode dispatch each | low — v1 still exists for rollback |
| **5. Delete the v1 tests** | delete tests 1–21 legs that are now duplicated, plus `v2_book_equivalence`/`v2_open_read`/`v2_tools` v1 legs | `CI` | low — coverage already re-built in PRs 1–2 |
| **6. Delete the v1 server code and the switch** | `tool_router_v1` block, `contracts.rs`, the v1-only use cases/cursors, `ToolSet`/`READING_MCP_TOOLS` (keep the refuse-to-start check per §4), `ocr_error_tests.rs` v1 remnants; workflow `tools` inputs removed | `CI` | the point of no return; also the biggest line-count win (~4.5k lines) |

Nothing may merge to `main` by this plan; each PR waits for its own checks.
