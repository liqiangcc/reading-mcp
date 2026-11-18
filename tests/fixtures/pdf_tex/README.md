# pdfTeX Type 1 fixture (PDF-6b)

`type1-builtin.tex` is a four-line article compiled with pdfTeX and **no**
ToUnicode CMap, which is the shape of the paper in issue #232 (NBER WP 24458,
pdfTeX-1.40.18): every font is an embedded Type 1 program with a built-in
encoding and no `/ToUnicode`, so a decoder that falls back to StandardEncoding
drops the OT1 ligature slots, reads CMMI's `period` as `:` and loses CMSY's
`minus`.

The text carries the cases that matter:

- `Efficient finance is the first topic here: fluffy affluent firms do well.`
  (the OT1 ligatures `ff`, `ffi`, `fi`, `fl`, `ffl`);
- `$0.5\%$` in math mode (CMMI's `period` at 0x3A);
- `$10^{-12}$` (CMSY's `minus` at 0x00, in a superscript);
- `$\alpha + \beta = \gamma$` and `$\Gamma$` (CMMI and CMR Greek);
- one sentence of plain text.

TeX Live 2025's LaTeX format turns `\pdfgentounicode` on by default, so the
document switches it off explicitly. Nothing else is non-default: the
`pdflatex` invocation has no extra packages and no `\pdfgentounicode=1`.

## How it was built

`.github/workflows/pdf-tex-fixture.yml` (`PDF TeX fixture`) installs
`texlive-latex-base`, `texlive-fonts-recommended` and `poppler-utils`, runs

```
pdflatex -interaction=nonstopmode -halt-on-error \
  -output-directory "$RUNNER_TEMP/fixture" tests/fixtures/pdf_tex/type1-builtin.tex
pdffonts "$RUNNER_TEMP/fixture/type1-builtin.pdf"
```

and uploads the PDF and the `pdffonts` table. The PDF below is that artifact
(TeX Live 2025/Debian, pdfTeX 1.40.28, 41,266 bytes); it is committed so no
test needs TeX. Re-run the workflow to regenerate it after editing the `.tex`.

## `pdffonts` (the committed PDF)

```
name                                 type              encoding         emb sub uni object ID
------------------------------------ ----------------- ---------------- --- --- --- ---------
IEUQJA+CMR10                         Type 1            Builtin          yes yes no       4  0
GCLAZT+CMMI10                        Type 1            Builtin          yes yes no       5  0
TMCDBM+CMSY8                         Type 1            Builtin          yes yes no       6  0
PXOHER+CMR8                          Type 1            Builtin          yes yes no       7  0
```

`Type 1`, `Builtin`, `emb yes`, `uni no` on all four fonts: the population
PDF-6b repairs. The programs' cleartext parts name their encoding with `dup`
entries, e.g. CMR10

```
/Encoding 256 array
0 1 255 {1 index exch /.notdef put} for
dup 11 /ff put
dup 12 /fi put
dup 13 /fl put
dup 14 /ffi put
dup 15 /ffl put
...
```

CMMI10 has `dup 58 /period put`, CMSY8 has `dup 0 /minus put`.

`tests/pdf_tex_type1.rs` opens this PDF and asserts the extracted text
contains `efficient`, `finance`, `first`, `fluffy`, `affluent`, `0.5%`,
`−12` (U+2212) and `α`, and does not contain `0:5`.
