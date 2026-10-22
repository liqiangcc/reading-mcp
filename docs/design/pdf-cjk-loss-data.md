# Which fonts make the default PDF backend lose CJK characters (PDF-1b)

Measurement only. No product code changed. The 25-paper list is committed
(`scripts/pdf-cjk-ids.txt`); the PDFs, the per-paper font inventories and the one-page repro
are CI artifacts.

## What was measured

- **Corpus**: 25 arXiv papers, each with at least 200 CJK characters in the PDF text layer
  (PyMuPDF `get_text()`, 222-7,139 characters), CC-BY / CC-BY-SA only (the licence line on the
  abs page is checked at selection time and again at download time). Selected by
  `scripts/pdf-cjk.py --select` in run
  [39704336102](https://github.com/liqiangcc/reading-mcp/actions/runs/39704336102): 223
  candidates tried, 99 PDFs downloaded. Producers vary: arXiv GenPDF/pikepdf 16, pdfTeX 4,
  xdvipdfmx (LaTeX) 2, dvips 1, Word 1, unknown 1. Categories: cs.CL 19, cs.CV 3, eess.AS 1,
  physics.soc-ph 1, cs.DL 1. **No math or q-bio paper with >= 200 CJK characters qualified**:
  the math slot saw only 14 API results and the q-bio slot 40; a further local check of 54
  candidates (14 math, 40 q-bio, 23 PDFs downloaded) kept none. The missing categories are a
  property of what is on arXiv under a CC licence, not a filter.
- **Run**: GitHub Actions run [39709841135](https://github.com/liqiangcc/reading-mcp/actions/runs/39709841135)
  (`pdf-cjk.yml`, commit `a63cdb1`): release binary built from that commit, 5 shards x 5
  papers, aggregate. `READING_MCP_TOOLS=v1`, OCR off, layout backend from
  `scripts/pdf-layout/setup-pdf-layout.sh` (pymupdf/pymupdf4llm/pymupdf-layout 1.28.2).
  No paper failed to download and none was skipped by the shard deadline.
- **Reference text**: PyMuPDF `page.get_text()` for every page, independent of both backends.
- **Default and layout text**: `open_document`, `get_document_structure`, then `get_text_units`
  (`requested_kind=sentence`, 256 items per call, cursor to the end) for the **whole document**,
  capped at 20,000 units (never reached; the largest document returned 1,270 units). This is the
  difference from PDF-1a, which compared the first 200 sentences: that read counted 1,560 CJK
  characters on 2610.20785, while the whole document returns 4,139 (as many as the reference).
- **Font inventory** (`scripts/pdf-cjk.py`, PyMuPDF): per font xref, `Subtype`, the descendant
  `CIDFontType`, the `/Encoding` (a name such as `Identity-H`, or a dictionary with
  `/Differences` and optional `/BaseEncoding`, or an embedded CMap stream), whether a
  `/ToUnicode` CMap exists, `FontFile`/`FontFile2`/`FontFile3`, the descendant's
  `CIDSystemInfo` Registry/Ordering and `CIDToGIDMap`, the subset prefix, the producer family,
  and **where the font is used**: the page content stream, or only inside a Form XObject reached
  through `/Name Do` (nested up to depth 4). The inventory of every paper is in the artifact
  (`inventory/<id>.json`).

## 1. Per paper

`CJK recovered` is `chars_found / chars_total` over the CJK characters attributed to a font
(section 2 of the method below); `Loss` is the complement. `Unattributed` counts reference CJK
characters no font on that page could be matched to. `Default CJK` is the raw count in the
backend text, which can exceed the reference because the PDF section model returns overlapping
sections (see the limits).

| Paper | Group | Pages | Producer | Ref CJK | Default CJK | Default units | CJK recovered | Loss | Unattributed CJK | Layout | Layout CJK |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 2404.12618 | cs.AI | 13 | LaTeX | 253 | 159 | 297 | 159/231 | 31.2% | 22 | PARSE_FAILED | 0 |
| 2507.22219 | cs.AI | 15 | arXiv GenPDF | 343 | 343 | 86 | 343/343 | 0.0% | 0 | OCR_REQUIRED | 0 |
| 2604.14180 | cs.AI | 27 | Word | 4136 | 4136 | 1270 | 4136/4136 | 0.0% | 0 | OCR_REQUIRED | 0 |
| 2410.10278 | cs.CL | 6 | pdfTeX | 620 | 620 | 215 | 620/620 | 0.0% | 0 | ok | 604 |
| 2410.13854 | cs.CL | 32 | pdfTeX | 5840 | 0 | 302 | 0/5840 | 100.0% | 0 | OCR_REQUIRED | 0 |
| 2411.04822 | cs.CL | 20 | arXiv GenPDF | 892 | 853 | 215 | 853/892 | 4.4% | 0 | OCR_REQUIRED | 0 |
| 2501.08197 | cs.CL | 19 | pdfTeX | 7139 | 7139 | 269 | 7139/7139 | 0.0% | 0 | OCR_REQUIRED | 0 |
| 2502.07058 | cs.CL | 14 | pdfTeX | 468 | 467 | 165 | 467/468 | 0.2% | 0 | OCR_REQUIRED | 0 |
| 2505.17816 | cs.CL | 7 | dvips | 355 | 0 | 7 | 0/355 | 100.0% | 0 | OCR_REQUIRED | 0 |
| 2511.15059 | cs.CL | 17 | arXiv GenPDF | 979 | 0 | 19 | 0/979 | 100.0% | 0 | OCR_REQUIRED | 0 |
| 2601.00588 | cs.CL | 18 | arXiv GenPDF | 936 | 936 | 95 | 936/936 | 0.0% | 0 | OCR_REQUIRED | 0 |
| 2603.14782 | cs.CL | 23 | arXiv GenPDF | 927 | 1160 | 166 | 927/927 | 0.0% | 0 | ok | 927 |
| 2608.04397 | cs.CL | 18 | arXiv GenPDF | 430 | 378 | 330 | 374/430 | 13.0% | 0 | OCR_REQUIRED | 0 |
| 2608.23448 | cs.CL | 18 | unknown | 604 | 604 | 381 | 604/604 | 0.0% | 0 | OCR_REQUIRED | 0 |
| 2610.14101 | cs.CL | 11 | arXiv GenPDF | 761 | 761 | 89 | 761/761 | 0.0% | 0 | OCR_REQUIRED | 0 |
| 2610.16201 | cs.CL | 13 | arXiv GenPDF | 345 | 382 | 61 | 345/345 | 0.0% | 0 | ok | 302 |
| 2610.17703 | cs.CL | 29 | arXiv GenPDF | 330 | 337 | 691 | 330/330 | 0.0% | 0 | OCR_REQUIRED | 0 |
| 2610.18269 | cs.CL | 18 | arXiv GenPDF | 282 | 0 | 58 | 0/282 | 100.0% | 0 | OCR_REQUIRED | 0 |
| 2610.20785 | cs.CL | 26 | arXiv GenPDF | 4139 | 4502 | 384 | 4139/4139 | 0.0% | 0 | OCR_REQUIRED | 0 |
| 2510.22276 | cs.CV | 14 | arXiv GenPDF | 1399 | 0 | 44 | 0/1399 | 100.0% | 0 | OCR_REQUIRED | 0 |
| 2604.00909 | cs.CV | 16 | arXiv GenPDF | 518 | 0 | 92 | 0/518 | 100.0% | 0 | OCR_REQUIRED | 0 |
| 2603.10420 | eess | 17 | arXiv GenPDF | 450 | 450 | 20 | 450/450 | 0.0% | 0 | OCR_REQUIRED | 0 |
| 2610.09321 | eess | 7 | arXiv GenPDF | 232 | 0 | 69 | 0/232 | 100.0% | 0 | OCR_REQUIRED | 0 |
| 2502.19428 | physics | 33 | LaTeX | 1316 | 1415 | 292 | 1316/1316 | 0.0% | 0 | OCR_REQUIRED | 0 |
| 2604.03776 | physics | 19 | arXiv GenPDF | 222 | 429 | 411 | 222/222 | 0.0% | 0 | OCR_REQUIRED | 0 |

- **9 of 25 papers (36%) lose more than 5% of their reference CJK characters**: 2404.12618,
  2410.13854, 2505.17816, 2511.15059, 2608.04397, 2610.18269, 2510.22276, 2604.00909,
  2610.09321. Seven of them lose everything (100.0%).
- The 16 papers that lose nothing carry their CJK text in page-stream fonts with a usable
  `/ToUnicode` CMap (`Type1` with no `/Encoding`, `Type0/Identity-H`, or `Type0` not embedded).

## 2. Font groups (main result)

Grouped by `Subtype/descendant`, the `/Encoding` shape, `/ToUnicode`, embedding,
`CIDToGIDMap`, **where the text lives** (page content stream vs Form XObject), and producer
family. `Ref CJK` and `Recovered` are CJK characters, not fonts. `Run chars`/`Run loss` is the
position-sensitive measure (whitespace-free runs of >= 4 CJK characters inside one span must
appear as a substring of the whitespace-free backend text); it is only meaningful where a font's
spans hold long runs, which the per-character CJK subsets do not.

| Font group | Papers | Fonts | Ref CJK | Recovered | Loss | Run chars | Run loss |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `TrueType|none|tounicode|embedded|cidtogid=-|xobject|pdfTeX` | 1 | 30 | 5556 | 0 | 100.0% | 5369 | 100.00% |
| `Type1|Differences(none)|tounicode|embedded|cidtogid=-|page|arXiv GenPDF` | 14 | 254 | 1962 | 0 | 100.0% | 674 | 100.00% |
| `TrueType|none|tounicode|embedded|cidtogid=-|xobject|arXiv GenPDF` | 3 | 18 | 763 | 10 | 98.7% | 681 | 100.00% |
| `Type0/CIDFontType2|Identity-H|tounicode|embedded|cidtogid=-|xobject|arXiv GenPDF` | 2 | 15 | 441 | 0 | 100.0% | 362 | 100.00% |
| `Type1|Differences(WinAnsiEncoding)|tounicode|embedded|cidtogid=-|xobject|dvips` | 1 | 62 | 355 | 0 | 100.0% | 0 | - |
| `Type0/CIDFontType2|Identity-H|tounicode|embedded|cidtogid=Identity|xobject|pdfTeX` | 2 | 11 | 284 | 0 | 100.0% | 240 | 100.00% |
| `Type0/CIDFontType0|Identity-H|tounicode|embedded|cidtogid=-|xobject|arXiv GenPDF` | 7 | 47 | 206 | 0 | 100.0% | 123 | 100.00% |
| `Type0/CIDFontType0|Identity-H|no-tounicode|embedded|cidtogid=-|page|arXiv GenPDF` | 1 | 1 | 76 | 0 | 100.0% | 22 | 100.00% |
| `Type0/CIDFontType2|Identity-H|tounicode|embedded|cidtogid=Identity|xobject|LaTeX` | 1 | 10 | 72 | 7 | 90.3% | 0 | - |
| `Type1|none|tounicode|embedded|cidtogid=-|page|arXiv GenPDF` | 16 | 820 | 9335 | 9284 | 0.5% | 207 | 0.00% |
| `Type0|Identity-H|tounicode|not-embedded|cidtogid=-|xobject|arXiv GenPDF` | 2 | 4 | 56 | 41 | 26.8% | 38 | 47.40% |
| `Type0/CIDFontType0|Identity-H|tounicode|embedded|cidtogid=-|page|LaTeX` | 2 | 17 | 1475 | 1468 | 0.5% | 594 | 0.00% |
| `Type0|Identity-H|tounicode|not-embedded|cidtogid=-|xobject|pdfTeX` | 2 | 7 | 1 | 0 | 100.0% | 0 | - |
| `Type0/CIDFontType0|Identity-H|tounicode|embedded|cidtogid=Identity|xobject|arXiv GenPDF` | 1 | 1 | 1 | 0 | 100.0% | 0 | - |
| `Type1|none|tounicode|embedded|cidtogid=-|page|pdfTeX` | 4 | 274 | 8226 | 8226 | 0.0% | 21 | 0.00% |
| `Type0|Identity-H|tounicode|not-embedded|cidtogid=-|page|Word` | 1 | 12 | 4136 | 4136 | 0.0% | 2796 | 0.00% |
| `Type0/CIDFontType2|Identity-H|tounicode|embedded|cidtogid=Identity|page|unknown` | 1 | 1 | 604 | 604 | 0.0% | 538 | 0.00% |
| `Type0/CIDFontType0|Identity-H|tounicode|embedded|cidtogid=-|page|arXiv GenPDF` | 2 | 13 | 345 | 345 | 0.0% | 202 | 0.00% |

Only groups with at least one CJK character are listed (44 groups were observed, most of them
Latin text with no CJK). What the table says:

- **Where the text lives decides more than the font does.** Every `xobject` row loses its CJK
  text, whatever the subtype or encoding: 7,735 CJK characters are drawn from Form XObjects and
  58 come back (99.3%). That is 7,677 of the 9,773 lost characters (78.5%).
- **The largest page-stream loss is a `Type1` font whose `/Encoding` is a dictionary with
  `/Differences` and no `/BaseEncoding`** (`Type1|Differences(none)|...|page|arXiv GenPDF`):
  14 papers carry such a font, 4 of them set CJK text with it (2510.22276, 2604.00909,
  2511.15059, 2610.09321), and all 1,962 characters are lost. The `/Differences` names are of
  the form `/uni9762`, i.e. Unicode-based glyph names (`uniXXXX`), not the Adobe glyph names
  lopdf knows - checked on 2510.22276 (`/uni9762`, `/uni9580`, ...) and 2610.09321
  (`/uni3001`, `/uni3044`, ...); the other two use the same `IMFCTT1`/`IGFCTT1` font family.
- **`Identity-H` without `/ToUnicode`** loses all 76 characters it sets (2610.18269 page 4).
  Every other page-stream `Identity-H` font has a `/ToUnicode` CMap and is read.
- Page-stream `Type1` with no `/Encoding` but a `/ToUnicode` CMap, and page-stream
  `Type0/Identity-H` with `/ToUnicode`, are read essentially completely (0.0-0.5% loss; the
  residual is characters shared with a font that lost its copy, see the limits).
- Not-embedded `Type0` fonts with `/ToUnicode` in the page stream are read (Word: 4,136 of
  4,136). In an XObject they lose most of their text (41 of 56 recovered only because the same
  characters occur elsewhere).

Loss by location:

| Text location | Ref CJK | Recovered | Loss |
| --- | --- | --- | --- |
| page content stream | 26159 | 24063 | 8.0% |
| Form XObject | 7735 | 58 | 99.3% |

Across the corpus: 33,894 CJK characters attributed to a font (33,916 in the reference; 22 on
2404.12618 could not be attributed), 24,121 recovered (71.2%), 9,773 lost (28.8%).

Whole pages disappear, not only their CJK characters. Page 5-gram containment against the
default text is 0.08-0.18 on pages 27-32 of 2410.13854 (figure pages whose text is inside Form
XObjects, English captions included) and 0.05 on page 14 of 2510.22276 (a CJK table page set
with the `uniXXXX` differences font).

## 3. Layout backend

| Outcome | Papers |
| --- | --- |
| ok | 3 (2410.10278, 2603.14782, 2610.16201) |
| `OCR_REQUIRED` | 21 |
| `PARSE_FAILED` | 1 (2404.12618) |

On the 3 papers it opened, the layout backend returned 604/620, 927/927 and 302/345 CJK
characters - all three papers have no default-backend CJK loss to begin with. **The layout
backend recovers none of the lost groups**: it refuses every paper that loses CJK text to an
XObject or to the differences font (2410.13854, 2510.22276, 2604.00909, 2610.09321, 2511.15059,
2505.17816, 2610.18269, 2608.04397), and fails to parse 2404.12618.

## 4. Minimal repro

`repro/2410.13854-page4-repro.pdf` (artifact, not in git): page 4 of 2410.13854, extracted with
PyMuPDF `Document.select([page])` + `save(garbage=4, deflate=True)`, 1 page, 1,187 CJK
characters in its text layer, all inside Form XObjects. The default backend opens it (`ok`) and
returns **0** CJK characters, so the loss reproduces on one page. `repro/repro.json`:

```json
{"group": "TrueType|none|tounicode|embedded|cidtogid=-|xobject|pdfTeX",
 "paper": "2410.13854", "page": 4, "repro": "2410.13854-page4-repro.pdf",
 "reference_cjk": 1187, "default_cjk": 0, "default_outcome": "ok", "still_loses": true}
```

## 5. What the lopdf 0.44 code does, and which group each path explains

`src/parsing/pdf.rs:202-232` is the only text extraction path: per page,
`extract_text_chunks_with_limit` and `extract_text_with_limit(&[*page_number],
MAX_PAGE_DECOMPRESSED_BYTES)`. Both land in lopdf's `extract_text_chunks_from_page`
(`lopdf-0.44.0/src/parser_aux.rs:108-215`), which walks the **page content stream operations**
and matches only `Tf`, `Tj`/`TJ`, `'`, `"`, `T*`, `ET` (`parser_aux.rs:140-207`, the catch-all
`_ => {}` at `:207`). The per-font encoding comes from
`Dictionary::get_font_encoding_with_limit` (`lopdf-0.44.0/src/object.rs:420-465`).

- **Text inside Form XObjects is never visited.** There is no `Do` arm in
  `parser_aux.rs:140-207`, so `q ... /Fm4 Do Q` contributes nothing; the font inventory of
  2410.13854 shows the CJK fonts used only inside `/Fm4`, `/Fm5`. This explains **every
  `xobject` row**: the TrueType group (5,556), the arXiv GenPDF TrueType (753),
  `Type0/CIDFontType2` (441 + 284 + 72 + 1), `Type0/CIDFontType0` (206), the dvips Type1 (355)
  and the not-embedded `Type0` rows (56 + 1). If lopdf did follow `Do`, these fonts would
  decode: their `/ToUnicode` CMaps are present and well formed (the inventory records the
  stream xref for every one of them). No TrueType font with CJK text appears in a page stream
  anywhere in this corpus, so the TrueType rows cannot separate the XObject effect from the
  font type: those rows are XObject-only.
- **`/Encoding` wins over `/ToUnicode`, and an unparseable `/Encoding` falls back to
  StandardEncoding.** `object.rs:440-450`: `if let Ok(object) = self.get(b"Encoding") { return
  self.get_base_encoding(...) }` comes **before** the `/ToUnicode` branch, and
  `object.rs:458-463` turns every error into `Encoding::OneByteEncoding(STANDARD_ENCODING)`.
  `get_base_encoding` (`object.rs:468-528`) resolves the reference and accepts only
  `Object::Dictionary` with `/Type /Encoding`; inside, `/Differences` names are resolved by
  `Glyph::from_name` (`encodings/glyphnames.rs:35-51`), a fixed table of Adobe glyph names.
  `/uni9762` is not in that table, so `differences()` (`object.rs:548-587`) returns
  `InvalidEncodingDifferenceGlyph` for the whole array, the encoding falls back to
  StandardEncoding, and the byte codes come out as Latin letters. This explains the
  `Type1|Differences(none)|...|page` group (1,962 characters, 0 recovered) - the `/ToUnicode`
  CMap next to it is never consulted.
- **`Identity-H` without `/ToUnicode`.** `object.rs:540-543`: the `Identity-H`/`Identity-V` arm
  is `self.get_deref(b"ToUnicode", doc)?.as_stream()?`; with no `/ToUnicode` this errors, and
  `object.rs:458-463` falls back to StandardEncoding, which decodes each byte of the 2-byte CID
  as a Latin character. This explains the `Identity-H|no-tounicode|...|page` group (76
  characters, 0 recovered).
- **Predefined CJK CMaps are not implemented.** `object.rs:544` maps any other `/Encoding` name
  to `Encoding::SimpleEncoding(name)`, and `encodings/mod.rs:74-122` decodes only
  `UniGB-UCS2-H`, `UniGB-UTF16-H` and `WinAnsiEncoding`; every other name hits
  `Self::SimpleEncoding(_) => Err(Error::CharacterEncoding)` (`encodings/mod.rs:122`). That
  error propagates out of `collect_text` (`parser_aux.rs:363-380`) and makes the whole page's
  `extract_text_with_limit` fail, which `src/parsing/pdf.rs:214-230` records as a page error
  with empty text. **No paper in this corpus uses a predefined CJK CMap name**, so this path is
  read but not exercised by these numbers.
- **Simple TrueType fonts with a custom `/Encoding`/`/Differences`** go through the same
  `differences()` + `Glyph::from_name` path as Type1 (the TrueType `cmap` table is explicitly
  not implemented, `object.rs:434-439`), so an unknown glyph name in a TrueType `/Differences`
  array loses the font the same way. The corpus has no TrueType `/Differences` CJK font in the
  page stream (the TrueType losses here are all XObject-borne), so this is code-read, not
  measured.
- **Groups the code does not explain:** the residual 0.5% page-stream loss on
  `Type1|none|tounicode|page` (51 of 9,335) and `Type0/CIDFontType0|Identity-H|page` (7 of
  1,475), and the 22 unattributed characters on 2404.12618. The 51 are characters whose copies
  in the reference outnumber the copies in the returned text - the same character also occurs
  in an XObject or in a font that lost its copy - and the 22 are runs whose span font name
  matches no font on that page. Neither is a separate decoding path as far as the code shows.

## 6. Method limits

- **Per-font survival is character containment, not position.** For each font, the multiset of
  CJK characters it owns in the reference is checked against the CJK characters the default
  backend returned; each returned character is claimed at most once, and the claim budget per
  character is `min(reference count, returned count)`. A font whose characters survive but in
  the wrong order, or on the wrong page, therefore still counts as recovered. The
  position-sensitive run measure is reported next to it, but CJK TeX subsets emit one glyph per
  span (e.g. `gbsnu4e` = one character), so those fonts have no runs of >= 4 at all: 674 run
  characters against 1,962 owned in the differences group, 207 against 9,335 in the Type1
  `ToUnicode` group.
- **Raw counts are not a loss measure.** The PDF section model can return the same text from
  two sections, so the default text sometimes contains more CJK characters than the reference
  (2610.20785: 4,502 vs 4,139; 2604.03776: 429 vs 222). Sections whose whole text is identical
  to an earlier section are skipped, but partial overlap remains. Because the claim budget is
  capped per character by the reference, duplication cannot hide a loss, but it can mask a
  small one.
- **Attribution.** A span names its BaseFont, not its resource name; when two font xrefs on one
  page share a BaseFont the run goes to the lowest xref (counted as `ambiguous_runs` in the
  shard JSONL, 0 in this corpus). 22 reference CJK characters on 2404.12618 were attributed to
  no font.
- **XObject detection** follows `/Name Do` from the page content stream and nested Form
  XObjects up to depth 4, reading the resource dictionaries of the page and of each XObject.
  Text drawn by Type3 glyph procedures or by annotations is not classified as either location;
  a font used by both the page stream and an XObject on the *same* page is labelled by the
  page-stream use.
- **The layout backend numbers are outcome plus CJK count only**; its refusals are recorded by
  the server (`OCR_REQUIRED`) and not analysed here.
