# V2-4a: every measurement script gets an opt-in v2 mode

Step 1 of deleting the v1 tools (v2-4b removes them). Every gate keeps measuring exactly what it
measured: each script grows `--tools v1|v2`, default `v1`, and only the v2 mode starts its servers
with `READING_MCP_TOOLS=v2` and calls the v2 tool set (`open`, `outline`, `read`, `cite`,
`search`, `source_view`, `browse`).

**Same number** means the v2 cell measures the identical fact (same stored state, same text, same
item). **Comparable** means it measures the same quantity through a different call shape (per-call
latency of `read` vs `get_text_units`, items per call bounded by chars instead of count). Rows
marked **dropped at v2-4b** have no v2 tool shape that measures the same thing; the design note's
reason is listed and the measurement disappears when the v1 tools do.

## scripts/doc-read-probe.py

| v1 measurement | v2 equivalent | same/comparable |
| --- | --- | --- |
| `open_document` seconds, RSS after open | `open` seconds, RSS after open | comparable (same work, different tool) |
| `get_document_structure` section enumeration (untimed prep) | `outline` at the doc handle, `next` pagination | same: same section set in the same body order |
| `get_text_units` sentence seek to `start_fraction` | `read` batches (`max_chars` 16000) counting items inside the section handle prefix | same anchor position, comparable cost |
| shape `get_text_units`: `calls` forward pages | `read` single-item calls following `next` | comparable (one item per call; item count vs char budget makes `--max-items` meaningless in v2) |
| shape `get_context`: neighbor 2/2 of the anchor locator | `read` `{"from": <item handle>, "back": 2}` (anchor + 2 before + 2 after = 5 items) | same items, comparable latency |
| shape `read_document`: exact locator read | `read` `{"from": <item handle>}` single item | same item, comparable latency |
| shape `search_document`: query from the anchor text | `search` `{"doc": <doc handle>, "query": q, "limit": 10}`; `hits` reported | same hit set, comparable latency |
| shape `get_document_structure`: `max_nodes: 50` repeated | `outline` `{"at": <doc handle>, "limit": 50}` repeated | comparable |
| shape `v2_read` | `read` following `next` (unchanged; in v2 mode it duplicates the `get_text_units` row) | same |
| RSS peak / after calls, warm waits, io bytes | unchanged | same |

`--policy`/`coverage_policy` has no v2 knob: dropped at v2-4b (v2 reads the one stream the server
offers).

## scripts/ingest-parity.py

The state-dump blocks (`documents`, `sections`, `layouts`, `text`) come from SQLite, not tools:
**same number** in both modes. The tool-driven columns:

| v1 measurement | v2 equivalent | same/comparable |
| --- | --- | --- |
| `open_document` payload column | `open` payload | comparable (same document facts, compact view) |
| `v1 *` read sequence (~40 recorded calls: structure pages + named section, unit streams forward/backward/middle/anchored, contexts, section reads, search, source_view, tree reads) | `run_v2_reads`: `open`, `read` from section/`next`/`back`/batch/backward+`b:` cursor, `outline` at doc and section + `next`, `search` title + body term, `cite` section+document, `source_view` for PDFs | comparable (exercises the same stored state; payloads differ by design) |
| `v1 get_context` paragraph/sentence/container | `read` `back`/`max_chars` equivalents | comparable |
| `v1 get_context structural children` | `outline` `{"at": section, "depth": 1}` | same facts, comparable payload |
| `v1 get_context structural ancestors`, section neighbour 2/2 | none — dropped at v2-4b (v2 has no structural-relation tool) | dropped |
| `v1 get_text_units section anchor` error shape | none — dropped at v2-4b (handles never name a stream-incompatible kind) | dropped |
| v2 block (`open`, `read`, `outline`, `search`, `cite`) | unchanged; becomes the whole `reads` column in v2 mode | same |

## scripts/sequential-read-probe.py

| v1 measurement | v2 equivalent | same/comparable |
| --- | --- | --- |
| `tools/list` footprint | `tools/list` of the v2 set | comparable (what it is for: v2 footprint) |
| `open_document` | `open` (`doc` handle stands in for `document_id`) | comparable |
| structure enumeration | `outline` paginated | same section order |
| pass `page`: 256-item pages per section | `read` `max_chars=16000` batches over the whole book | comparable (char-bounded batch ≈ 256-item page) |
| pass `one`: `max_items=1` | `read` single item per call | same call shape, comparable latency |
| pass `twenty`: `max_items=20` | `read` `max_chars` ≈ mean sentence bytes × 20 | comparable |
| representative calls: structure/search/context/section read/units page | `outline`/`search`/`read back=2`/`read` at section/`read` batch | same facts, comparable footprint |

## scripts/mem-profile-now.py

RSS/heap measurements are tool-agnostic (**same number**). Only the three mid-timeline calls
change: `get_document_structure` → `outline` (comparable), `get_text_units` 32 sentences → `read`
`max_chars=4000` from the first outline section (comparable), `search_document` → `search` (same
query, comparable latency). Phase names are kept so the report table is diffable across modes.

## scripts/v2-eval.py

Already measures both tool sets; `--tools` selects which legs run. `v1` (default) runs both
exactly as today. `v2` runs only the v2 legs (T1-T5, `tools/list` v2), so the gate still reports
after v1 is deleted; cross-toolset PASS lines are skipped when a leg is absent (**same number**
for the v2 rows it still produces).

## scripts/stress-probe.py

| v1 measurement | v2 equivalent | same/comparable |
| --- | --- | --- |
| open seconds, peak RSS, rc, disk usage per cap | `open` | same (tool-agnostic counters) |
| section list | `outline` paginated | same order |
| per-section `total_items` (deep prep, read prep) | untimed `read` batch walk counting items per section handle prefix | same counts, comparable cost |
| deep `reach` / `next` / `context` / `anchor` rows | `read` batch walk to the target item; `read` from `next`; `read` `back=2`; `read` re-anchored at the item handle | comparable |
| `--mode read` per-section sentence walk | `read` single-item calls in the shard's section range | same items, comparable latency |
| `--mode aggregate` | offline aggregation, unchanged | same |

## scripts/deep-next-probe.py

| v1 measurement | v2 equivalent | same/comparable |
| --- | --- | --- |
| structure + `--largest-section` totals | `outline` + untimed `read` batch counts | same section choice, comparable cost |
| seek | `read` batch walk counting items | same anchor position, comparable cost |
| mode `next` | `read` following `next` within the section | comparable |
| mode `anchored` | `read` `{"from": <anchor handle>}` repeated | comparable |
| mode `context` | `read` `{"from": anchor, "back": 2}` | same items, comparable |
| mode `read` | `read` `{"from": anchor}` | same item, comparable |
| `--kind paragraph` | `read` `unit=paragraph` | same |
| `--policy eligible_only` | none — dropped at v2-4b (ignored in v2 mode) | dropped |

## scripts/first-search-probe.py

| v1 measurement | v2 equivalent | same/comparable |
| --- | --- | --- |
| open / reopen seconds, RSS | `open` | same counters, comparable latency |
| `search_document` after wait / immediately | `search` | same query, comparable |
| `foreground-reads` `get_text_units` | `read` single items from the middle section | comparable |
| `v2_search_ms` extra column | becomes `search_ms` in v2 mode (the only search); the extra column is empty | same value under a shared name |
| `scan_prewarm` events | unchanged | same |

## scripts/long-read-probe.py

| v1 measurement | v2 equivalent | same/comparable |
| --- | --- | --- |
| section walk + per-section `get_text_units` one-item calls, wrapped | `outline` once + `read` `{"from": <item handle>}` following `next`, restarting at the doc handle | same items and call count, comparable latency; RSS samples are the same counters |

## scripts/url-resolve-probe.py

| v1 measurement | v2 equivalent | same/comparable |
| --- | --- | --- |
| `document_id` equality vs control | doc handle equality (`d<key>~<checksum>` identifies document + identity) | same fact |
| `title`, `media_type`, `section_count` | `open`'s `title`/`format`/`sections` | same facts |
| first-200-sentences text equality | `read` `max_chars` batches from the doc handle, joined text | same text |
| `tools/list` v1+v2 footprints | unchanged (spawns one server per tool set regardless of `--tools`) | same |

## .github/workflows/section-scale.yml (inline Python)

Reads `doc-read-probe` JSON. With `--tools v2` the probe writes the same `shapes` keys filled by
the v2 equivalents above, so the flatness gate rows stay structurally identical; their ms/rchar
values are **comparable**, and the `tools` input is passed through to the probe so a v1 run on the
head reproduces today's table.

## Workflow `tools` inputs

`book-profile`, `cpu-profile`, `doc-read-profile`, `first-search-probe`, `ingest-parity`,
`leak-profile`, `mem-profile-now`, `section-scale`, `stress-profile`, `url-resolve-probe` get a
`workflow_dispatch` input `tools` (`v1`|`v2`, default `v1`) passed to every affected script.
Push triggers are unchanged.

## Dropped at v2-4b (summary)

- `coverage_policy`/`--policy` everywhere (v2 exposes a single stream per unit kind).
- v1 `get_context` structural `ancestors` and section-neighbour shapes (ingest-parity).
- The `get_text_units` section-anchor error shape (ingest-parity).
- v1 `read_document` char-budgeted continuation cursors (the v2 `next` handle covers them).
