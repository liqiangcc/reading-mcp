# obr: evidence for `.tmp/obr-report.md`

Scratch artefacts for the investigation `obr` (why `open` of a large book delays `read` on other
documents). Nothing here is meant to ship.

## What is here

| file | what it is |
|---|---|
| `obr_probe.py` | the harness: one stdio server process, newline-delimited JSON-RPC, a pipelined `read` every 50 ms on an already-open book while one heavy call runs; prints the read latency distribution inside the heavy call's window |
| `obr_table.py` | runs a case N times and prints the median row (this is what the report's tables come from) |
| `trace-open.log` | `READING_MCP_SQLITE_TRACE=1` trace of `open` of `zh-large.epub` (宋史, 7.2 MB, 543 sections) while reads run |
| `trace-pdf.log` | the same for `open` of a 1500-page text PDF whose text is highly repetitive (writes 0.31 MB) |
| `trace-pdf-ent.log` | the same for a 1500-page PDF with high-entropy text (writes 2.05 MB) |
| `trace-search.log` | the same for `search` of `之` on the open `zh-large.epub` |
| `dump-0-open-epub.txt` | `gdb -batch -p PID -ex "thread apply all bt 12"` taken during that `open` |
| `table-*.json` | the raw medians the report quotes |

Trace lines look like:

```
SQLITE_TRACE t_ms=5523 kind=run_sqlite caller=replace_document_segmentation id=1712 wait_ms=0 hold_ms=1150
SQLITE_TRACE t_ms=5523 kind=run_sqlite caller=document_id_for_key id=1713 wait_ms=1129 hold_ms=0
SQLITE_TRACE t_ms=6081 kind=phase caller=open_document::ingest phase=open.save_shared ms=558
```

`t_ms` is milliseconds since the process's first traced event; `wait_ms` is time spent waiting for
the store's `Mutex<Connection>`, `hold_ms` is time holding it.

## How to reproduce

```
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
cargo build --release --locked --bin reading-mcp
cp target/release/reading-mcp .tmp/bin/main

# inputs (public-domain books from the stress-profile set, plus generated PDFs)
mkdir -p .tmp/inputs
cp ~/.cache/reading-mcp-fetch/45ff3f9bfd0a5954-pg24264-images.epub .tmp/inputs/zh-classic.epub
cp ~/.cache/reading-mcp-fetch/da4a6e6063e26276-pg24183-images.epub .tmp/inputs/zh-large.epub

python3 obr/obr_table.py --binary .tmp/bin/main --label main --reps 3 \
  --cases open-epub,open-pdf,search

READING_MCP_SQLITE_TRACE=1 python3 obr/obr_probe.py --binary .tmp/bin/main \
  --state .tmp/state/trace-open --case open-epub \
  --reader-source "$PWD/.tmp/inputs/zh-classic.epub" \
  --heavy-source "$PWD/.tmp/inputs/zh-large.epub" \
  --prewalk 60 --server-stderr .tmp/trace-open.log
```

Every server started by the harness gets its own `READING_MCP_STATE_DIR` under `.tmp/state/` and
talks stdio only, so no port is taken.
