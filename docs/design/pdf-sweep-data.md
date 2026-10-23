# PDF sweep data: 121 real PDFs with the production backend (PDF-5a)

Measurement only. No product code changed; the corpus list is committed
(`scripts/pdf-sweep-ids.txt`), the PDFs are not.

Every number below comes from GitHub Actions run
[39892014353](https://github.com/liqiangcc/reading-mcp/actions/runs/39892014353)
(`pdf-sweep.yml`: build once, `--select`, 8 probe shards, aggregate). The committed list is
byte-identical to the list that run's probe shards used
(`sha256 254624fedbdb7f7675e1e8ada8efa17f5252cdf2c474d7f286bd7f43c7b73342`, 121 entries).

**Reproducibility**: the same corpus re-measured on the committed list (run
[39897330444](https://github.com/liqiangcc/reading-mcp/actions/runs/39897330444), `select`
skipped) reproduced the outcome table exactly (121 `ok`, 0 non-ok), the same two suspects with
the same containment (0.244 and 0.328), and the same peak RSS to the tenth of a MiB (median
28.1, p95 135.2, worst 284.3 MiB). Only the open times moved: median 0.06 s there vs 0.07 s
here, worst 2.74 s there vs 1.68 s here (both the 495-page 2311.05131). The timings below are
single-run; the memory and text numbers are reproducible.

## What was measured

- **Server**: release binary from `main` (1d183fc, #220), `READING_MCP_TOOLS=v1`, OCR off
  (`READING_MCP_OCR_ENABLED=false`), **default** PDF backend (no
  `READING_MCP_PDF_LAYOUT_PYTHON`), `READING_MCP_MAX_DOCUMENT_BYTES=134217728` (the server's
  own default).
- **Memory cap**: every open, structure read, text read and search ran inside
  `sudo systemd-run --scope -p MemoryMax=600M -p MemorySwapMax=0` (the same helper
  `scripts/pdf-probe.py` uses), to mimic the user's 1.9 GB host with about 600 MB free. The
  scope was available on every runner (`systemd-run MemoryMax scope: available`), so no run was
  uncapped.
- **Peak RSS** is the server process's own `VmHWM`, read at the end of the open from
  `/proc/<server>/status`; the server is found by descending the `sudo -> systemd-run -> server`
  chain. `peak_rss_source` is `vmhwm` for all 121 documents (the `wait4` fallback was never
  needed).
- **Corpus**: 121 arXiv PDFs, CC-BY / CC-BY-SA / CC0 only (the licence line is read on the abs
  page at selection time *and* re-checked at download time), 4,842 pages, 541.6 MiB in total,
  none already measured by PDF-1a or PDF-1b. Groups: 96 ordinary (10 per category slot:
  cs.CL, cs.CV, cs.LG, cs.CR, cs.DB, math, physics, q-bio, stat, econ/eess, 6-40 pages),
  8 long (250-900 pages), 17 layout hard cases (6 math-heavy, 6 table-heavy, 5 raster-figure),
  0 pre-2005 scan-risk papers, 0 beamer slide decks (see the selection section).
- **Reference text**: PyMuPDF `page.get_text()` for every page, independent of the backend.
- **Backend text**: `open_document`, `get_document_structure`, 200 sequential
  `get_text_units` calls (`requested_kind=sentence`, one item per call) from the first sentence,
  then the whole document in 256-item batches (character budget 1.5 M; it was never reached -
  `text_truncated` is false for all 121), then one `search_document` call with a token taken
  from the middle of the text (limit 50).

## 1. Selection: what was tried and what was kept

746 candidates were examined (abs page fetched, licence line read) and 232 PDFs were
downloaded to check pages and the group metric.

| Group | Kept | Target | Candidates tried | Rejections |
| --- | --- | --- | --- | --- |
| ordinary | 96 | 100 | 291 | 195 |
| long | 8 | 8 | 150 | 142 |
| hard-math | 6 | 6 | 16 | 10 |
| hard-tables | 6 | 6 | 9 | 3 |
| hard-figures | 5 | 6 | 60 | 55 |
| hard-slides | 0 | 6 | 60 | 60 |
| old-scan | 0 | 10 | 160 | 160 |

| Rejection reason | Candidates |
| --- | --- |
| `licence_not_cc` | 514 |
| `pages` (outside the group's page range) | 70 |
| `slides` (not beamer, not landscape) | 24 |
| `figure_pages` (fewer than 5 pages with a raster figure) | 15 |
| `tables` (fewer than 6 detected tables) | 2 |

Two groups are empty, and that is a finding rather than a gap in the run:

- **old-scan (0 of 160)**: arXiv has scanned PDFs of old papers, but pre-2005 submissions
  almost never carry a CC licence. 160 candidates from `submittedDate:[19940101 TO 20041231]`
  (cs.*, math.*, hep-th, plus a dedicated 2004 window, where CC options first appeared) were
  examined and **every one was rejected on the licence line** (`licence_not_cc`: 160 of 160).
  The CC-only rule and the scan-risk group are therefore mutually exclusive on arXiv.
- **hard-slides (0 of 60)**: 35 candidates were non-CC, 1 was outside the page range, and 24
  passed the licence check but were portrait documents with a normal text density (not beamer
  decks). No CC-BY beamer deck was found within the candidate budget.

Consequence: **this sample contains no image-only PDF**, so `ocr_required` and `unreadable`
cannot occur in it. The image-only evidence remains the 5-PDF host measurement in the task
description (pure scan -> `unreadable`; 4-page image-only -> `ok` with 156 characters).

## 2. Outcome classes

| Group | Documents | ok | ocr_required | unreadable | parse_failed | too_large | timeout | oom_killed | download_failed | not_run |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| ordinary | 96 | 96 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| old-scan | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| long | 8 | 8 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| hard | 17 | 17 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| all | 121 | 121 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |

No download failed, no document was skipped by the shard deadline, and no open errored. Under
the 600 MB cap the server was never killed (`oom_killed` 0) and no open took over 120 s
(`timeout` 0); the worst peak RSS in the whole sample is 284.3 MiB, 47% of the cap.

## 3. Silent failures: the `suspect` flag

Two documents opened `ok` and are `suspect` (2 of 121, 1.7%). Both fail the same rule
(5-gram containment of the first 20 reference pages below 0.6); neither trips the
"reference-rich but default-poor per page" or "image-only pages" rule.

| Id | Group | Pages | Producer | Reason | Fonts (top 3, subtype, embedded, pages used) | Images | Containment | Char ratio |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 2403.02163 | ordinary | 12 | pdfTeX-1.40.25 | containment_below_0.6 | RUHGJO+NimbusSanL-Bold (Type1, embedded, 12p); AOYYLM+NimbusRomNo9L-Regu (Type1, embedded, 12p); TOSMPJ+CMSY10 (Type1, embedded, 3p) | 13 images on 3 pages, 0 pages with a raster over 20% of the page | 0.244 | 0.244 |
| 2311.05131 | long | 495 | GPL Ghostscript 10.01.2 | containment_below_0.6 | LQQMWP+SFRM1000 (Type1, embedded, 494p); ZMOXDK+CMMI10 (Type1, embedded, 488p); RUJWIR+TeX-matha10 (Type1, embedded, 484p) | 130,172 images on 446 pages, 0 pages with a raster over 20% of the page | 0.328 | 0.549 |

What the two have in common, and what the fonts/images say:

- **2403.02163** (12 pages): the reference text is 41,423 characters (3,452 per page), the
  backend returns 10,111 (842 per page, 24.4%). The document opens as **one section with 24
  sentence units**, so the loss happens in parsing, not in reading. Eight fonts are used, all
  embedded Type1 except a **non-embedded Type3 `DejaVuSans` on 3 pages**; no page is image-only
  (`pages_low_text` 0, 13 small images on 3 pages). A single Type3 font on a quarter of the
  pages does not explain a 75% loss over the whole document, so the cause is not visible in the
  font inventory: the extraction drops text that PyMuPDF finds. This is the case the user
  described as "opens successfully but is not the paper".
- **2311.05131** (495 pages, 5.6 MiB): 880,869 reference characters against 483,646 returned
  (54.9%), containment 0.328, and **130,172 raster images on 446 pages** - the PDF (a 495-page
  book converted by Ghostscript, SFRM/matha fonts) renders formulas and fragments as small
  images, so the text layer is inherently partial. 78.8% of its sentence units are longer than
  800 characters: with the math missing, whole paragraphs merge into one unit. Sections are
  found correctly (331 sections, heading-based, 618 sentences).

Containment and character ratio over the whole sample: median 0.982 / 1.179, minimum 0.244 /
0.244; 4 documents below 0.8, 14 below 0.9, 22 below 0.95; 4 documents return less than 90% of
the reference characters, 31 return more than 150% (repeated headings and table cells count
twice).

## 4. Open time and peak RSS versus pages and bytes

| Pages | Documents | Median open s | p95 open s | Worst open s | Median peak RSS MiB | p95 peak RSS MiB | Worst peak RSS MiB |
| --- | --- | --- | --- | --- | --- | --- | --- |
| <5 | 1 | 0.02 | 0.02 | 0.02 | 18.9 | 18.9 | 18.9 |
| 5-20 | 74 | 0.05 | 0.12 | 0.27 | 25.2 | 63.5 | 237.2 |
| 20-100 | 38 | 0.12 | 0.38 | 0.42 | 29.4 | 236.7 | 284.3 |
| 100-400 | 6 | 1.10 | 1.37 | 1.37 | 100.2 | 238.2 | 238.2 |
| 400+ | 2 | 1.68 | 1.68 | 1.68 | 124.1 | 135.1 | 135.1 |
| all | 121 | 0.07 | 1.03 | 1.68 | 28.1 | 135.1 | 284.3 |

| Rank | Slowest open: id | Pages | Open s | Heaviest: id | Pages | Peak RSS MiB |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 2311.05131 | 495 | 1.68 | 2401.14345 | 36 | 284.3 |
| 2 | 2305.18793 | 490 | 1.68 | 2510.07408 | 331 | 238.2 |
| 3 | 2510.07408 | 331 | 1.37 | 2403.18575 | 12 | 237.2 |
| 4 | 2311.18111 | 307 | 1.16 | 2403.19144 | 32 | 236.7 |
| 5 | 2307.09982 | 266 | 1.12 | 2603.17845 | 32 | 179.8 |

Open time follows the page count; peak RSS follows the **file size**, not the page count: the
four heaviest documents are 39-53 MiB files of 12-36 pages (the 53.1 MiB, 32-page 2403.19144
peaks at 236.7 MiB; the 45.4 MiB, 36-page 2401.14345 at 284.3 MiB), while the 495-page
5.6 MiB book peaks at 113.2 MiB. Every one of them stayed under half the 600 MB cap, so the
cap was never hit.

State directory after the open: median 0.5 MiB, worst 3.9 MiB (the parsed cache is small; no
document wrote its PDF copy into the state dir).

## 5. Read cost and search

| Measure | Median | p95 | Worst |
| --- | --- | --- | --- |
| per-document mean of the 200 sequential `get_text_units` calls (ms) | 0.72 | 0.84 | 1.26 |
| per-document p95 of those calls (ms) | 0.78 | 0.91 | 1.40 |
| per-document max of those calls (ms) | 0.85 | 1.67 | 3.06 |
| `search_document` for one middle token (ms) | 4.26 | 22.54 | 35.46 |

- 200 sequential one-item calls were made in 23 documents; the other 98 have fewer than 200
  sentence units in total and were read completely (as few as 6 units for a 6-page
  `page_fallback` paper). No read call and no search errored.
- Search hits: median 6, minimum 0, maximum 50; **8 documents reached the 50-hit limit**, so
  their hit count is a floor, not a total. The slowest search (35.5 ms) is the 307-page
  2311.18111; search cost grows with document size but stays in tens of milliseconds.

## 6. Long documents (250-900 pages)

| Id | Pages | Open s | s/page | Peak RSS MiB | RSS MiB/page | Sections | Structure | Sentences | Sentences/page | Chars | Containment | Search ms | Cap hit |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 2311.05131 | 495 | 1.68 | 0.0034 | 113.2 | 0.229 | 331 | headings | 618 | 1.25 | 483646 | 0.328 | 11.00 | no |
| 2305.18793 | 490 | 1.68 | 0.0034 | 135.1 | 0.276 | 356 | headings | 7799 | 15.92 | 649409 | 0.708 | 34.17 | no |
| 2510.18203 | 364 | 0.99 | 0.0027 | 110.6 | 0.304 | 79 | headings | 8342 | 22.92 | 531949 | 0.963 | 22.88 | no |
| 2510.07408 | 331 | 1.37 | 0.0041 | 238.2 | 0.720 | 117 | headings | 5526 | 16.69 | 558648 | 0.910 | 15.67 | no |
| 2311.18111 | 307 | 1.16 | 0.0038 | 89.8 | 0.293 | 89 | headings | 3704 | 12.07 | 363294 | 0.857 | 35.46 | no |
| 2306.05976 | 291 | 1.03 | 0.0035 | 86.6 | 0.297 | 87 | headings | 4559 | 15.67 | 423048 | 0.787 | 5.56 | no |
| 2407.10296 | 274 | 1.08 | 0.0039 | 131.6 | 0.480 | 86 | headings | 1203 | 4.39 | 452005 | 0.885 | 22.54 | no |
| 2307.09982 | 266 | 1.12 | 0.0042 | 81.2 | 0.305 | 113 | headings | 652 | 2.45 | 685816 | 0.972 | 9.96 | no |

- 2.7-4.2 ms per page to open, 0.23-0.72 MiB of peak RSS per page; the 600 MB cap was never
  approached.
- All 8 long documents are **heading-based** (an inferred numbered-heading structure), none fell
  back to one section per page. Section counts are 79-356 for 266-495 pages.
- Sentences per page vary by a factor of 9 (1.25 to 22.92): 2311.05131 (the Ghostscript book
  with formula images) and 2407.10296 (4.39) produce very long units, while 2510.18203 (a
  primer on Fourier series) segments normally.
- Containment for the long documents: 0.328, 0.708, 0.787, 0.857, 0.885, 0.910, 0.963, 0.972 -
  two of eight below 0.9.

## 7. Structure and text-quality distributions over all 121 documents

| Measure | Median | Tail |
| --- | --- | --- |
| sections per document | 18 | max 356 |
| sentences per page | 13.88 | p05 1.07, p95 56.58 |
| 5-gram containment (first 20 pages) | 0.982 | 4 documents < 0.8, 14 < 0.9 |
| characters returned / reference | 1.179 | 4 documents < 0.9, 1 < 0.5 |
| share of sentence units shorter than 5 chars | 0.040 | worst 0.415 (2410.05294) |
| share of sentence units longer than 800 chars | 0.056 | worst 0.788 (2311.05131) |
| private-use + replacement character share | 0.000 | 2 documents over 1% (2510.18203: 1.39% U+FFFD; 2312.01833: 0.64% private-use + 0.59% U+FFFD) |
| a line repeated on more than 30% of pages | - | 85 documents (see below) |

- Structure: **107 documents are heading-based, 14 fall back to one section per page**
  (`section://page-N`). The page fallback is not harmful by itself (2410.05294, 27 pages, 27
  sections, 118.8 sentences per page), but the two documents with the fewest sentences per page
  (2403.02642 and 2308.01909, 1.0) are both page fallback.
- Duplicate-line proxy: 85 documents contain a line repeated on more than 30% of their pages.
  Of those, **32 repeat it on at least 80% of the pages** (a real running header or footer:
  `Published as a conference paper at the Global AI Cultures Workshop ICLR 2024` 12x on 7
  pages, `Self-supervised Pretraining of Cell Segmentation Models APREPRINT` 31x on 18 pages,
  `S Kotlyk MODERN INFORMATION TECHNOLOGIES IN SCIENTIFIC RESEARCH` 138x on 274 pages), and
  **19 have a repeated line of at least 20 characters**. The remaining 66 are short fragments
  produced by PDF line breaking (`that`, `Proof.`, `,vol.`), which the proxy counts but a
  reader would not see as page furniture. The rule as specified (same line on > 30% of pages)
  therefore over-counts; the 32/19 figures are the header/footer signal.

## 8. What would matter to a reader, ranked by documents hit

| What a reader would notice | Documents |
| --- | --- |
| a line repeated on over 30% of pages (header/footer noise) | 85 (32 of them on >= 80% of pages) |
| more than 5% of sentences longer than 800 chars | 61 |
| more than 10% of sentences shorter than 5 chars | 29 |
| silent failure (opens ok, text is suspect) | 2 |
| 5-gram containment below 0.6 | 2 |
| over 1% private-use or replacement characters | 2 |
| open over 10 s / over 60 s | 0 / 0 |
| peak RSS over 400 MiB | 0 |
| refused, unreadable or killed | 0 |
| search slower than 1000 ms | 0 |
| read p95 slower than 50 ms | 0 |
| state directory over 100 MiB after the open | 0 |

Read plainly: for this sample the cost side is quiet (nothing over 1.7 s, nothing over 300 MiB,
no read or search above 50 ms) and the risk side is **text quality**, not speed or memory. Two
documents out of 121 lose most of their text while reporting `ok`, and the two sentence-length
tails (61 documents with long units, 29 with short fragments) show that sentence segmentation
on PDF text is the weaker part of the reading experience, before any question of PDF size.

## 9. What did not run, and limits of this sample

- **No image-only PDF**: the old-scan group is empty (0 of 160 pre-2005 candidates had a CC
  licence), and the ordinary group required a text layer (>= 200 characters per page) that no
  candidate failed, so `ocr_required`, `unreadable`, `too_large` and the "opens ok with 156
  characters" case are **not represented here** - the only evidence for them is the 5-PDF host
  measurement in the task description.
- **No beamer deck**: 0 of 60 slide candidates qualified (35 non-CC, 24 portrait documents).
- **No CJK document**: the 25 CJK papers already measured by PDF-1b are excluded by id.
- **No file over 53 MiB**: the server's 128 MiB document limit and the user's 87 MiB
  OCR-layer book (8.3 s, 398 MiB peak) are outside this sample; the largest file here is
  53.1 MiB and opens in 0.25 s.
- **Timings are single-run**: this is one run on a GitHub-hosted runner, not the user's host;
  the host's own measurement was 0.14 s for a born-digital paper against 0.07 s median here.
- Every document was measured with the default backend only; the layout backend was not used
  (`READING_MCP_PDF_LAYOUT_PYTHON` unset) and OCR was off.
