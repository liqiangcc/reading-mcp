# PDF backend data: 30 real papers, default vs layout (PDF-1a)

Measurement only. No product code changed; the corpus list is committed
(`scripts/pdf-corpus-ids.txt`), the PDFs are not.

## What was measured

- **Corpus**: 30 arXiv papers with a CC-BY / CC-BY-SA licence (checked on the abs page at
  selection time and again at download time), 8-30 pages, selected from
  `export.arxiv.org/api/query` by `scripts/pdf-corpus.py --select` in seven groups: cs.CL 6,
  cs.CV 5, cs.LG/stat.ML 4, math 4, physics 3, q-bio/econ 3, and 5 cs.CL papers whose PDF
  carries at least 200 CJK characters (measured with PyMuPDF `get_text`).
- **Run**: GitHub Actions run [39634287777](https://github.com/liqiangcc/reading-mcp/actions/runs/39634287777)
  (`pdf-corpus.yml`, build once, 3 shards of 10 papers, aggregate). Release binary from
  `main` (3867ee8), `READING_MCP_TOOLS=v1`, OCR off, layout backend from
  `scripts/pdf-layout/setup-pdf-layout.sh` (pymupdf/pymupdf4llm/pymupdf-layout 1.28.2).
  No download failed and no paper was skipped by the shard deadline.
- **Reference text**: PyMuPDF `page.get_text()` for every page, independent of both backends.
- **Backend text**: `open_document`, `get_document_structure`, then `get_text_units`
  (`requested_kind=sentence`, one item per call) for the first 200 sentences, exactly as
  `scripts/pdf-probe.py` does.
- **Comparison window**: the backend reads a fixed 200 sentences, so a ratio against the whole
  document would only measure the sentence cap. Character ratios and 5-gram containment are
  computed against the *leading reference pages whose character count reaches the backend's own
  output volume* (at most 20 pages). The window is printed per row.
- **Font census** (same 30 files, raw-byte scan): every PDF carries exactly one Type0 /
  Identity-H CID font and every one of those fonts has a `/ToUnicode` CMap. The default
  backend therefore has a decoding table for every embedded CID font in this corpus - which is
  why it reads all 30 papers (below).

## 1. Per paper, both backends

Columns: outcome and open seconds for the backend; sentences per page from
`get_text_units` totals; characters returned in the first 200 sentences; the reference window;
characters returned / reference-window characters; multiset 5-gram containment of the
reference window in the backend text; non-printable share (U+E000-F8FF, U+FFFD, C0/C1
controls) and CJK share of the backend text.

| Paper | Group | Pages | Backend | Outcome | Open s | Peak RSS MiB | Sections | Sentences | Sent/page | Chars | Ref window (pages/chars) | Char ratio vs ref | 5-gram containment | Non-printable | CJK share |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 2610.14101 | cjk | 11 | default | ok | 0.04 | 128.9 | 33 | 229 | 20.82 | 94319 | 11/41355 | 2.281 | 0.957 | 0.000 | 0.007 |
| 2610.14101 | cjk | 11 | layout | OCR_REQUIRED | 3.12 | 385.3 | - | - | - | 0 | 1/3541 | 0.000 | 0.000 | - | - |
| 2610.16201 | cjk | 13 | default | ok | 0.05 | 129.8 | 19 | 66 | 5.08 | 68903 | 13/43845 | 1.572 | 0.990 | 0.000 | 0.008 |
| 2610.16201 | cjk | 13 | layout | ok | 4.58 | 399.9 | 25 | 354 | 27.23 | 21025 | 7/24021 | 0.875 | 0.858 | 0.000 | 0.003 |
| 2610.17703 | cjk | 29 | default | ok | 0.11 | 159.9 | 36 | 1003 | 34.59 | 24681 | 7/25401 | 0.972 | 0.710 | 0.000 | 0.001 |
| 2610.17703 | cjk | 29 | layout | OCR_REQUIRED | 13.15 | 448.5 | - | - | - | 0 | 1/3607 | 0.000 | 0.000 | - | - |
| 2610.18269 | cjk | 18 | default | ok | 0.05 | 128.9 | 16 | 63 | 3.50 | 74845 | 18/60746 | 1.232 | 0.884 | 0.000 | 0.000 |
| 2610.18269 | cjk | 18 | layout | OCR_REQUIRED | 3.92 | 406.3 | - | - | - | 0 | 1/2998 | 0.000 | 0.000 | - | - |
| 2610.20785 | cjk | 26 | default | ok | 0.14 | 129.3 | 35 | 400 | 15.38 | 111598 | 20/62253 | 1.793 | 0.891 | 0.000 | 0.012 |
| 2610.20785 | cjk | 26 | layout | OCR_REQUIRED | 8.25 | 442.4 | - | - | - | 0 | 1/3050 | 0.000 | 0.000 | - | - |
| 2610.28489 | cs.CL | 22 | default | ok | 0.04 | 78.0 | 42 | 38 | 1.73 | 85665 | 20/53706 | 1.595 | 0.973 | 0.000 | 0.000 |
| 2610.28489 | cs.CL | 22 | layout | ok | 3.63 | 382.6 | 43 | 434 | 19.73 | 21638 | 8/23082 | 0.937 | 0.834 | 0.000 | 0.000 |
| 2610.28498 | cs.CL | 20 | default | ok | 0.04 | 84.3 | 19 | 434 | 21.70 | 33247 | 11/35672 | 0.932 | 0.752 | 0.000 | 0.000 |
| 2610.28498 | cs.CL | 20 | layout | ok | 6.05 | 402.3 | 24 | 410 | 20.50 | 26115 | 9/28421 | 0.919 | 0.868 | 0.000 | 0.000 |
| 2610.28500 | cs.CL | 16 | default | ok | 0.10 | 96.8 | 17 | 166 | 10.38 | 69734 | 16/50741 | 1.374 | 0.998 | 0.000 | 0.000 |
| 2610.28500 | cs.CL | 16 | layout | OCR_REQUIRED | 9.51 | 490.0 | - | - | - | 0 | 1/3052 | 0.000 | 0.000 | - | - |
| 2610.28559 | cs.CL | 27 | default | ok | 0.09 | 74.5 | 23 | 240 | 8.89 | 57793 | 20/55313 | 1.045 | 0.885 | 0.001 | 0.000 |
| 2610.28559 | cs.CL | 27 | layout | ok | 5.38 | 462.2 | 28 | 507 | 18.78 | 21332 | 7/21606 | 0.987 | 0.838 | 0.000 | 0.000 |
| 2610.28586 | cs.CL | 17 | default | ok | 0.07 | 81.6 | 21 | 21 | 1.24 | 66512 | 17/53863 | 1.235 | 0.993 | 0.000 | 0.000 |
| 2610.28586 | cs.CL | 17 | layout | ok | 5.81 | 400.2 | 26 | 444 | 26.12 | 20542 | 7/22680 | 0.906 | 0.864 | 0.000 | 0.000 |
| 2610.28607 | cs.CL | 26 | default | ok | 0.06 | 71.9 | 19 | 168 | 6.46 | 58632 | 20/51397 | 1.141 | 0.834 | 0.010 | 0.000 |
| 2610.28607 | cs.CL | 26 | layout | OCR_REQUIRED | 9.36 | 380.6 | - | - | - | 0 | 1/3386 | 0.000 | 0.000 | - | - |
| 2610.28540 | cs.CV | 14 | default | ok | 0.05 | 128.7 | 24 | 600 | 42.86 | 15073 | 6/15098 | 0.998 | 0.793 | 0.000 | 0.000 |
| 2610.28540 | cs.CV | 14 | layout | ok | 3.54 | 399.3 | 28 | 360 | 25.71 | 15950 | 7/18213 | 0.876 | 0.864 | 0.000 | 0.000 |
| 2610.28567 | cs.CV | 23 | default | ok | 0.25 | 210.8 | 31 | 208 | 9.04 | 71929 | 20/46877 | 1.534 | 0.974 | 0.001 | 0.000 |
| 2610.28567 | cs.CV | 23 | layout | OCR_REQUIRED | 16.91 | 625.3 | - | - | - | 0 | 1/2857 | 0.000 | 0.000 | - | - |
| 2610.28598 | cs.CV | 18 | default | ok | 0.13 | 176.7 | 27 | 186 | 10.33 | 93723 | 18/58480 | 1.603 | 0.987 | 0.000 | 0.000 |
| 2610.28598 | cs.CV | 18 | layout | OCR_REQUIRED | 12.44 | 663.1 | - | - | - | 0 | 1/3153 | 0.000 | 0.000 | - | - |
| 2610.28615 | cs.CV | 20 | default | ok | 0.14 | 110.3 | 28 | 43 | 2.15 | 90660 | 20/65774 | 1.378 | 0.942 | 0.001 | 0.000 |
| 2610.28615 | cs.CV | 20 | layout | OCR_REQUIRED | 8.07 | 482.3 | - | - | - | 0 | 1/2941 | 0.000 | 0.000 | - | - |
| 2610.28631 | cs.CV | 19 | default | ok | 0.08 | 96.8 | 21 | 26 | 1.37 | 72655 | 19/55644 | 1.306 | 0.968 | 0.000 | 0.000 |
| 2610.28631 | cs.CV | 19 | layout | OCR_REQUIRED | 6.38 | 378.0 | - | - | - | 0 | 1/2098 | 0.000 | 0.000 | - | - |
| 2610.28602 | cs.LG/stat.ML | 18 | default | ok | 0.03 | 128.9 | 13 | 19 | 1.06 | 59786 | 18/57912 | 1.032 | 0.983 | 0.000 | 0.000 |
| 2610.28602 | cs.LG/stat.ML | 18 | layout | ok | 4.65 | 490.8 | 17 | 315 | 17.50 | 23313 | 8/24265 | 0.961 | 0.889 | 0.000 | 0.000 |
| 2610.28618 | cs.LG/stat.ML | 15 | default | ok | 0.18 | 128.7 | 10 | 58 | 3.87 | 40720 | 15/41190 | 0.989 | 0.984 | 0.001 | 0.000 |
| 2610.28618 | cs.LG/stat.ML | 15 | layout | ok | 4.61 | 399.8 | 28 | 326 | 21.73 | 28666 | 9/29607 | 0.968 | 0.908 | 0.000 | 0.000 |
| 2610.28619 | cs.LG/stat.ML | 25 | default | ok | 0.12 | 159.9 | 21 | 443 | 17.72 | 31154 | 10/31868 | 0.978 | 0.773 | 0.001 | 0.000 |
| 2610.28619 | cs.LG/stat.ML | 25 | layout | OCR_REQUIRED | 9.49 | 422.2 | - | - | - | 0 | 1/3363 | 0.000 | 0.000 | - | - |
| 2610.28623 | cs.LG/stat.ML | 28 | default | ok | 0.11 | 128.9 | 41 | 393 | 14.04 | 47643 | 16/50149 | 0.950 | 0.843 | 0.002 | 0.000 |
| 2610.28623 | cs.LG/stat.ML | 28 | layout | OCR_REQUIRED | 8.29 | 615.7 | - | - | - | 0 | 1/4172 | 0.000 | 0.000 | - | - |
| 2610.28504 | math | 30 | default | ok | 0.15 | 159.9 | 7 | 952 | 31.73 | 13616 | 7/14584 | 0.934 | 0.928 | 0.001 | 0.000 |
| 2610.28504 | math | 30 | layout | ok | 9.08 | 410.7 | 20 | 1024 | 34.13 | 12092 | 6/12375 | 0.977 | 0.910 | 0.001 | 0.000 |
| 2610.28527 | math | 26 | default | ok | 0.07 | 128.9 | 13 | 647 | 24.88 | 17747 | 9/18842 | 0.942 | 0.919 | 0.004 | 0.000 |
| 2610.28527 | math | 26 | layout | ok | 3.81 | 385.1 | 12 | 735 | 28.27 | 13832 | 7/15354 | 0.901 | 0.889 | 0.000 | 0.000 |
| 2610.28541 | math | 21 | default | ok | 0.07 | 128.7 | 11 | 666 | 31.71 | 15602 | 8/16837 | 0.927 | 0.915 | 0.002 | 0.000 |
| 2610.28541 | math | 21 | layout | ok | 5.14 | 400.0 | 16 | 616 | 29.33 | 15230 | 8/16837 | 0.905 | 0.882 | 0.002 | 0.000 |
| 2610.28581 | math | 14 | default | ok | 0.07 | 159.9 | 15 | 395 | 28.21 | 17824 | 11/18250 | 0.977 | 0.598 | 0.010 | 0.000 |
| 2610.28581 | math | 14 | layout | ok | 4.32 | 365.6 | 23 | 282 | 20.14 | 14462 | 9/15426 | 0.938 | 0.832 | 0.004 | 0.000 |
| 2610.28538 | physics | 29 | default | ok | 0.18 | 159.9 | 42 | 1005 | 34.66 | 35799 | 9/37362 | 0.958 | 0.781 | 0.001 | 0.000 |
| 2610.28538 | physics | 29 | layout | OCR_REQUIRED | 13.31 | 423.9 | - | - | - | 0 | 1/4575 | 0.000 | 0.000 | - | - |
| 2610.28614 | physics | 23 | default | ok | 0.14 | 128.9 | 14 | 579 | 25.17 | 19198 | 6/21058 | 0.912 | 0.899 | 0.003 | 0.000 |
| 2610.28614 | physics | 23 | layout | OCR_REQUIRED | 13.66 | 480.7 | - | - | - | 0 | 1/3982 | 0.000 | 0.000 | - | - |
| 2610.28621 | physics | 12 | default | ok | 0.07 | 128.9 | 18 | 449 | 37.42 | 27456 | 7/28549 | 0.962 | 0.835 | 0.000 | 0.000 |
| 2610.28621 | physics | 12 | layout | OCR_REQUIRED | 9.38 | 538.1 | - | - | - | 0 | 1/4749 | 0.000 | 0.000 | - | - |
| 2610.26805 | q-bio/econ | 8 | default | ok | 0.03 | 159.9 | 11 | 12 | 1.50 | 38351 | 8/26428 | 1.451 | 0.999 | 0.000 | 0.000 |
| 2610.26805 | q-bio/econ | 8 | layout | OCR_REQUIRED | 3.95 | 404.0 | - | - | - | 0 | 1/2972 | 0.000 | 0.000 | - | - |
| 2610.27334 | q-bio/econ | 28 | default | ok | 0.08 | 128.9 | 23 | 1490 | 53.21 | 37509 | 13/39350 | 0.953 | 0.945 | 0.001 | 0.000 |
| 2610.27334 | q-bio/econ | 28 | layout | OCR_REQUIRED | 6.08 | 456.6 | - | - | - | 0 | 1/2500 | 0.000 | 0.000 | - | - |
| 2610.28579 | q-bio/econ | 18 | default | ok | 0.06 | 129.1 | 20 | 165 | 9.17 | 64729 | 18/49898 | 1.297 | 0.960 | 0.001 | 0.000 |
| 2610.28579 | q-bio/econ | 18 | layout | OCR_REQUIRED | 7.93 | 404.4 | - | - | - | 0 | 1/3090 | 0.000 | 0.000 | - | - |

Facts:

- Default backend: 30/30 opened (`ok`). Containment median 0.923, min 0.598 (2610.28581,
  math), max 0.999. **No paper falls below 0.5.**
- Layout backend: 12/30 opened, 18/30 answered `OCR_REQUIRED`.
- 10 papers return more characters than their reference window (ratio > 1.3, up to 2.28):
  the default backend emits some text twice. Containment stays high because the duplicate is
  identical text.
- Default sentence segmentation is coarse: median 12.2 sentences/page, minimum 1.06. On
  2610.28586 (17 pages) the default backend reports 21 sections and 21 sentences totalling
  66,512 characters - one "sentence" per section.
- Where the layout backend opened, its containment median is 0.866 (default: 0.923 on the same
  12 papers) and its char ratio median 0.928; it returns 0.88-0.99 of the reference window.

## 2. Layout refusals

**18 of 30 papers (60%) are refused with `OCR_REQUIRED`.** Every refusal has the same reason,
`mixed page contains raster content without source coverage`; the other three reasons in
`require_disabled_page_coverage` (`image-only page requires local OCR inspection`,
`native image coverage cannot be proven`, `invalid original visual coverage`) never occur. Every
failing page has `has_native_body == True`: these are ordinary text pages that also carry a
raster figure, not scans.

Reproduced by running `src/parsing/pdf_layout_worker.py` directly with the arguments
`layout_pdf.rs` passes (`-I -B -c <worker source> 2000 134217728 200000000 '' '' <worker path>`,
OCR config empty, raw PDF on stdin): stdout
`{"schema":"pdf-layout-ocr-required/v1",...,"error":"OCR_REQUIRED"}`, stderr
`PDF layout requires enabled local OCR inspection`. The worker does not report the page, so the
failing page and the per-page coverage numbers come from a wrapper that imports the worker
module and calls its own `require_disabled_page_coverage` page by page (worker unmodified).
No page in this corpus exceeded the worker's 64M cumulative raster-pixel budget (max 52.2M), so
no refusal is a budget artefact.

| Group | Papers | Layout refused | Rate |
| --- | --- | --- | --- |
| cjk | 5 | 4 | 0.80 |
| cs.CL | 6 | 2 | 0.33 |
| cs.CV | 5 | 4 | 0.80 |
| cs.LG/stat.ML | 4 | 2 | 0.50 |
| physics | 3 | 3 | 1.00 |
| q-bio/econ | 3 | 3 | 1.00 |

| Reason | Papers |
| --- | --- |
| mixed page contains raster content without source coverage | 18 |

| Paper | Group | Pages | Failing page(s) | First reason | Raster on the first failing page | Uncovered share | Box explained | Residual share | After drawings | Drawings | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 2610.14101 | cjk | 11 | 2 | mixed page contains raster content without source coverage | 1 raster(s), largest 7% of page inside picture box (transformed) | 0.072 | 0.972 | 0.0020 | 0.0020 | 0 | raster inside a retained picture/figure box |
| 2610.17703 | cjk | 29 | 16 | mixed page contains raster content without source coverage | 1 raster(s), largest 10% of page inside picture box (transformed) | 0.017 | 0.995 | 0.0001 | 0.0001 | 1 | raster inside a retained picture/figure box |
| 2610.18269 | cjk | 18 | 1,12 | mixed page contains raster content without source coverage | 1 raster(s), largest 7% of page inside picture box (transformed) | 0.012 | 0.738 | 0.0031 | 0.0031 | 0 | raster inside a retained picture/figure box |
| 2610.20785 | cjk | 26 | 1,2,19,20 | mixed page contains raster content without source coverage | 1 raster(s), largest 6% of page inside picture box (transformed) | 0.013 | 0.787 | 0.0027 | 0.0027 | 1 | raster inside a retained picture/figure box |
| 2610.28500 | cs.CL | 16 | 2,4,6 | mixed page contains raster content without source coverage | 1 raster(s), largest 9% of page inside picture box (transformed) | 0.009 | 1.000 | 0.0000 | 0.0000 | 0 | raster inside a retained picture/figure box |
| 2610.28607 | cs.CL | 26 | 15,16 | mixed page contains raster content without source coverage | 1 raster(s), largest 4% of page inside picture box (transformed) | 0.049 | 0.897 | 0.0050 | 0.0013 | 160 | not explained by any box |
| 2610.28567 | cs.CV | 23 | 4,8,21,22,23 | mixed page contains raster content without source coverage | 12 raster(s), largest 1% of page inside picture box (transformed) | 0.107 | 0.970 | 0.0032 | 0.0032 | 27 | raster inside a retained picture/figure box |
| 2610.28598 | cs.CV | 18 | 1,6,8,13,15,16,17 | mixed page contains raster content without source coverage | 1 raster(s), largest 12% of page inside picture box (transformed) | 0.066 | 0.709 | 0.0192 | 0.0192 | 0 | not explained by any box |
| 2610.28615 | cs.CV | 20 | 1,3,8 | mixed page contains raster content without source coverage | 1 raster(s), largest 0% of page inside picture box (transformed) | 0.009 | 0.208 | 0.0072 | 0.0036 | 3 | not explained by any box |
| 2610.28631 | cs.CV | 19 | 1,4,8,19 | mixed page contains raster content without source coverage | 64 raster(s), largest 0% of page inside picture box (transformed) | 0.170 | 0.950 | 0.0085 | 0.0049 | 7 | not explained by any box |
| 2610.28619 | cs.LG/stat.ML | 25 | 4,7,25 | mixed page contains raster content without source coverage | 1 raster(s), largest 12% of page inside picture box (transformed) | 0.029 | 0.588 | 0.0120 | 0.0117 | 3 | not explained by any box |
| 2610.28623 | cs.LG/stat.ML | 28 | 2,3,5,26,28 | mixed page contains raster content without source coverage | 15 raster(s), largest 6% of page (transformed) | 0.047 | 1.000 | 0.0000 | 0.0000 | 285 | residue only at box edges (no box contains the raster) |
| 2610.28538 | physics | 29 | 2,7,9 | mixed page contains raster content without source coverage | 20 raster(s), largest 1% of page inside picture box (transformed) | 0.156 | 0.970 | 0.0046 | 0.0046 | 814 | raster inside a retained picture/figure box |
| 2610.28614 | physics | 23 | 2 | mixed page contains raster content without source coverage | 125 raster(s), largest 4% of page (transformed) | 0.081 | 0.883 | 0.0095 | 0.0091 | 97 | not explained by any box |
| 2610.28621 | physics | 12 | 2,3,4,7,10 | mixed page contains raster content without source coverage | 1 raster(s), largest 3% of page inside picture box (transformed) | 0.033 | 0.569 | 0.0143 | 0.0142 | 6 | not explained by any box |
| 2610.26805 | q-bio/econ | 8 | 4,5 | mixed page contains raster content without source coverage | 1 raster(s), largest 26% of page inside picture box (transformed) | 0.033 | 0.986 | 0.0005 | 0.0002 | 1 | raster inside a retained picture/figure box |
| 2610.27334 | q-bio/econ | 28 | 1,2,5 | mixed page contains raster content without source coverage | 1 raster(s), largest 1% of page inside picture box (transformed) | 0.009 | 0.248 | 0.0064 | 0.0038 | 4 | not explained by any box |
| 2610.28579 | q-bio/econ | 18 | 2,5,15 | mixed page contains raster content without source coverage | 14 raster(s), largest 3% of page inside picture box (transformed) | 0.078 | 1.000 | 0.0000 | 0.0000 | 568 | raster inside a retained picture/figure box |

### What the raster on the failing page is

For every refused paper the table above lists the first failing page, the raster on it, the
uncovered share of the page's raster samples (non-white pixels after masking native text
spans), the share of those uncovered samples that the retained `image`/`picture`/`figure`/`table`
boxes explain, the residue as a share of the page, the residue after also masking every vector
drawing, and the drawing count.

- **9 of 18**: the largest raster lies **inside a retained `picture` box** and the residue after
  masking native text plus that box is at most 0.5% of the page's pixels (1e-6 to 0.0046; the
  median is 0.0020). The rule is stricter than needed here: the raster is a retained figure and
  what remains is anti-aliased boundary pixels that fall just outside the exact retained
  rectangle (the worker masks `[ceil(x0),ceil(y0),floor(x1),floor(y1)]` without padding, so
  boundary pixels stay unknown). 2610.28500 is the extreme case: 0.9999 of the uncovered
  samples are explained by the box and 1e-6 of the page's pixels still refuse the document.
- **1 of 18** (2610.28623): the retained boxes explain every uncovered sample and the residue is
  0.0000 of the page, but no box *contains* the largest raster (a Form/vector composite with 285
  drawings on the page); a handful of pixels at the box edges still refuses it.
- **8 of 18**: the residue is real, 0.5-1.9% of the page, and the retained boxes explain only
  21-95% of the uncovered pixels. Masking every vector drawing removes part of it on 6 of the 8
  (e.g. 2610.28607: 0.0050 -> 0.0013 with 160 drawings; 7 of the 8), i.e. the uncovered pixels include
  page furniture the rule has no box class for (rules, tinted panels, rendered math), plus
  raster content outside the retained rectangles (2610.28631: 64 rasters, largest 0.4% of the
  page, 17% of the page non-white, 95% explained by boxes).
- Rasters are rotated/transformed on every failing page (`has_transform`), 1 to 125 per page.
  Nothing in the corpus fails because of a full-page scan, a logo-sized raster alone, or an
  image-only page.

## 3. Detector candidates for "the default backend's text is unusable"

Label: **default 5-gram containment < 0.5**. Papers where the reference has text: 30.
**The label is never true on this corpus** (minimum containment 0.598), so every rule has
TP = 0 and FN = 0, and the tables below measure false alarms only.

| Rule | TP | FP | FN | TN | Errors |
| --- | --- | --- | --- | --- | --- |
| a_char_ratio | 0 | 0 | 0 | 30 | 0 |
| b_sentences_per_page | 0 | 6 | 0 | 24 | 6 |
| c_nonprintable | 0 | 1 | 0 | 29 | 1 |
| d_cjk_share | 0 | 5 | 0 | 25 | 5 |
| a_char_ratio+b_sentences_per_page | 0 | 6 | 0 | 24 | 6 |
| a_char_ratio+c_nonprintable | 0 | 1 | 0 | 29 | 1 |
| a_char_ratio+d_cjk_share | 0 | 5 | 0 | 25 | 5 |
| b_sentences_per_page+c_nonprintable | 0 | 7 | 0 | 23 | 7 |
| b_sentences_per_page+d_cjk_share | 0 | 11 | 0 | 19 | 11 |
| c_nonprintable+d_cjk_share | 0 | 6 | 0 | 24 | 6 |

Sensitivity check with a looser label (containment < 0.8, 6 positives):

| Rule | TP | FP | FN | TN | Errors |
| --- | --- | --- | --- | --- | --- |
| a_char_ratio | 0 | 0 | 6 | 24 | 6 |
| b_sentences_per_page | 0 | 6 | 6 | 18 | 12 |
| c_nonprintable | 0 | 1 | 6 | 23 | 7 |
| d_cjk_share | 1 | 4 | 5 | 20 | 9 |
| a_char_ratio+b_sentences_per_page | 0 | 6 | 6 | 18 | 12 |
| a_char_ratio+c_nonprintable | 0 | 1 | 6 | 23 | 7 |
| a_char_ratio+d_cjk_share | 1 | 4 | 5 | 20 | 9 |
| b_sentences_per_page+c_nonprintable | 0 | 7 | 6 | 17 | 13 |
| b_sentences_per_page+d_cjk_share | 1 | 10 | 5 | 14 | 15 |
| c_nonprintable+d_cjk_share | 1 | 5 | 5 | 19 | 10 |

Per-paper rule flags (`a` default chars / reference-window chars < 0.5, `b` default sentences
per page < 3 while the reference has > 500 chars per page, `c` non-printable share > 1%,
`d` CJK share < 5% while the reference has >= 200 CJK chars):

| Paper | Ref CJK | Default CJK | Ref chars/page | Default containment | Default char ratio | a | b | c | d |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 2610.14101 | 761 | 800 | 3760 | 0.957 | 2.281 | no | no | no | yes |
| 2610.16201 | 345 | 673 | 3373 | 0.990 | 1.572 | no | no | no | yes |
| 2610.17703 | 330 | 17 | 3200 | 0.710 | 0.972 | no | no | no | yes |
| 2610.18269 | 282 | 0 | 3375 | 0.884 | 1.232 | no | no | no | yes |
| 2610.20785 | 4139 | 1560 | 2855 | 0.891 | 1.793 | no | no | no | yes |
| 2610.28489 | 0 | 0 | 2625 | 0.973 | 1.595 | no | yes | no | no |
| 2610.28498 | 0 | 0 | 3083 | 0.752 | 0.932 | no | no | no | no |
| 2610.28500 | 0 | 0 | 3171 | 0.998 | 1.374 | no | no | no | no |
| 2610.28559 | 0 | 0 | 2615 | 0.885 | 1.045 | no | no | no | no |
| 2610.28586 | 0 | 0 | 3168 | 0.993 | 1.235 | no | yes | no | no |
| 2610.28607 | 0 | 0 | 2442 | 0.834 | 1.141 | no | no | yes | no |
| 2610.28540 | 0 | 0 | 2647 | 0.793 | 0.998 | no | no | no | no |
| 2610.28567 | 0 | 0 | 2097 | 0.974 | 1.534 | no | no | no | no |
| 2610.28598 | 0 | 0 | 3249 | 0.987 | 1.603 | no | no | no | no |
| 2610.28615 | 0 | 0 | 3289 | 0.942 | 1.378 | no | yes | no | no |
| 2610.28631 | 0 | 0 | 2929 | 0.968 | 1.306 | no | yes | no | no |
| 2610.28602 | 0 | 0 | 3217 | 0.983 | 1.032 | no | yes | no | no |
| 2610.28618 | 0 | 0 | 2746 | 0.984 | 0.989 | no | no | no | no |
| 2610.28619 | 0 | 0 | 2775 | 0.773 | 0.978 | no | no | no | no |
| 2610.28623 | 0 | 0 | 3070 | 0.843 | 0.950 | no | no | no | no |
| 2610.28504 | 0 | 0 | 1989 | 0.928 | 0.934 | no | no | no | no |
| 2610.28527 | 0 | 0 | 1977 | 0.919 | 0.942 | no | no | no | no |
| 2610.28541 | 0 | 0 | 1976 | 0.915 | 0.927 | no | no | no | no |
| 2610.28581 | 0 | 0 | 1761 | 0.598 | 0.977 | no | no | no | no |
| 2610.28538 | 0 | 0 | 4009 | 0.781 | 0.958 | no | no | no | no |
| 2610.28614 | 0 | 0 | 2741 | 0.899 | 0.912 | no | no | no | no |
| 2610.28621 | 0 | 0 | 3875 | 0.835 | 0.962 | no | no | no | no |
| 2610.26805 | 0 | 0 | 3304 | 0.999 | 1.451 | no | yes | no | no |
| 2610.27334 | 0 | 0 | 2806 | 0.945 | 0.953 | no | no | no | no |
| 2610.28579 | 0 | 0 | 2772 | 0.960 | 1.297 | no | no | no | no |

What the numbers say:

- **(a) is never true**: the default backend always returns at least 91% of its reference
  window (ratio 0.912-2.281). It cannot separate this corpus.
- **(b) fires on 6 papers** (1.06-2.15 sentences per page) and is wrong on all 6: those papers
  have containment 0.942-0.999, the coarse sentence segmentation of section 1.
- **(c) fires once**, 2610.28607 at 1.02% non-printable characters, containment 0.834. The next
  highest is 2610.28581 at exactly 1.00% (containment 0.598, the lowest in the corpus). A
  threshold of 1% misses the one paper where the default text is most degraded.
- **(d) fires on exactly the 5 CJK papers** and on none of the other 25. It is the only rule
  that tracks a real, measured loss (section 5), but the containment label calls all 5 usable
  because their Latin body text is intact (containment 0.710-0.990).
- No single rule or OR-pair separates the corpus, because there is nothing to separate: no paper
  has unusable default text. The least noisy rule is (c) with 1 false positive on 30 papers; the
  best OR-pair on the looser 0.8 label is (a)+(d) or (d) alone with 1 true positive, 4 false
  positives and 5 false negatives.
- Cost: all four rules need only the first 200 sentences the default backend already produced
  for the open; none needs the reference text or a second backend.

## 4. Open time and peak RSS

| Backend | Opens | Median s | p95 s | Worst s | Median peak RSS MiB | Worst peak RSS MiB |
| --- | --- | --- | --- | --- | --- | --- |
| default | 30 | 0.08 | 0.18 | 0.25 | 128.9 | 210.8 |
| layout | 30 | 6.23 | 13.66 | 16.91 | 408.5 | 663.1 |

- Default: median 0.08 s, p95 0.18 s, worst 0.25 s; median peak RSS 129 MiB, worst 211 MiB.
- Layout: median 6.23 s, p95 13.66 s, worst 16.91 s; median peak RSS 409 MiB, worst 663 MiB.
- Ratio of open times (layout / default) per paper: median 82x, min 25x, max 167x
  (2610.28607: 0.06 s vs 9.36 s). Refusals are not cheaper than successes: the 18 refused opens took a median 8.8 s,
  because the coverage check rasterises the pages before it refuses.
- Layout peak RSS is the server process (`ru_maxrss`); the layout worker is a child process and
  is not included, so the layout column understates its real cost.

## 5. CJK papers

Reference CJK characters are counted over the whole document; recall is against the same
leading-page window as the other ratios.

| Paper | Reference CJK chars | Default CJK chars | Default recall | Layout CJK chars | Layout recall |
| --- | --- | --- | --- | --- | --- |
| 2610.14101 | 761 | 800 | 1.051 | 0 | - |
| 2610.16201 | 345 | 673 | 1.951 | 77 | 0.726 |
| 2610.17703 | 330 | 17 | 1.000 | 0 | - |
| 2610.18269 | 282 | 0 | 0.000 | 0 | - |
| 2610.20785 | 4139 | 1560 | 0.665 | 0 | - |

- The default backend returns 0 of 282 CJK characters on 2610.18269 and 1560 of 4139 (38%) on
  2610.20785, while its 5-gram containment stays at 0.884 and 0.891.
- On the other three CJK papers the default backend returns at least as many CJK characters as
  the window contains (recall 1.00-1.95; some duplication).
- The layout backend opened only one CJK paper (2610.16201) and returned 77 CJK characters
  against 106 in its window (recall 0.73); the other four were refused. The layout backend's CJK
  reading is therefore not established by this corpus.

## 6. What this data does not show

- **No paper in this corpus breaks the default backend.** The BENCH-D2 failure (garbage glyph
  codes for Type0/Identity-H fonts) needs a CID font without a usable ToUnicode map; all 30
  papers here carry one, so the corpus cannot validate any "unusable text" detector, and its
  absence is a property of this corpus, not evidence that such PDFs do not exist. The two
  measured degradations are different: duplicated text (10 papers) and lost CJK characters
  (2 of 5 CJK papers).
- The per-page coverage analysis re-runs the predicate with a fresh raster budget per page so
  that every page gets an independent verdict; the worker uses one budget for the whole
  document. The cumulative pixels per page are in the shard JSONL and stayed under the 64M cap
  for all 18 refused papers, so this cannot have changed a verdict here.
- Timing is one GitHub Actions runner (`ubuntu-latest`), one run, one repetition per paper.
