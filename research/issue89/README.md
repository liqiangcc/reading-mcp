# Issue #89 PDF sentence reliability research

This directory is research-only. It does not alter the production service or
`/root/.reading-mcp`. The fixed baseline is the deployed v0.3.0 package:

* source SHA `1cfbc4ca032a8bab7b59134a9b03c2526a34b7f4`;
* normalization `reading-mcp-normalization/v8`;
* external #87 input is represented only by URL, raw/document hashes, and
  annotation metadata.

Run the reproducible experiment from the repository root:

```bash
research/issue89/run.sh
```

It generates temporary PDFs, runs the low-level `lopdf` candidate and
`pdf-extract 0.12.0` extractor, and calls the deployed binary through MCP
stdio with an isolated state directory. It never points a command at the
production state directory.

The isolated mature sentence-splitter run is recorded and reproducible from
the supplied evidence: syntok official repository HEAD
`371e6ca0d4e307271eaf439f912fad94efdaa8e9`, with Ubuntu `python3-regex`,
ran without a global installation. Its protected punctuation control split
into three sentences. Three repeated projected #87 Abstract runs each
produced seven segments with output SHA
`51a4348f12c89a821db6f097857bb928dd0d23bc9a5961d2c99905cce2bb80f6`.
The raw physical-line input produced 21 pseudo-segments, so syntok is only
used after deterministic line/paragraph projection. PyMuPDF was not
available; that candidate is explicitly unexecuted rather than inferred.

The frozen gates are in `fixtures/scoring-thresholds.json`. Raw probe output
is debugging material; `results/scored-summary.json` is the formal scored
evaluation. The external paper is never copied into the repository: only its
URL, hashes, projection-coordinate annotations, and license/distribution
policy are retained.
