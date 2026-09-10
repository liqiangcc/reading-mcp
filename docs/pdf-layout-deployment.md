# PDF layout candidate deployment

Version: **0.4.0-rc.1**. This is an opt-in candidate release for staged validation of
Issue #89 and the external OCR text-layer adapter in #95, not a claim that all PDF
layouts or OCR are solved. The original PDF backend remains the default. No
production service was changed by preparing this release.

## Package and dependencies

The Linux x86-64 package contains the server, release manifest, deployment script,
this guide, review report, and `pdf-layout/` setup and smoke-test tools. The worker
is embedded in the server. Python packages and paper PDFs are not bundled.

Use Python **3.12+** with `venv`/pip and network access to PyPI for initial setup.
The three PDF packages are pinned to **1.28.2**; all transitive dependencies are
also pinned in `pdf-layout/requirements.txt`. The worker rejects other engine
versions. Inference runs locally without a cloud API, API key or GPU. An offline
host needs an administrator-provided wheel mirror/cache of these exact versions.

After downloading the archive and its matching `SHA256SUMS` from the GitHub
release, verify and extract in a new staging directory:

```bash
sha256sum -c SHA256SUMS
tar -xzf reading-mcp-v0.4.0-rc.1-linux-x86_64.tar.gz
cd reading-mcp-v0.4.0-rc.1-linux-x86_64
PDF_LAYOUT_VENV=/opt/reading-mcp/pdf-layout-1.28.2 \
  bash pdf-layout/setup-pdf-layout.sh
```

The setup script refuses to overwrite an existing environment. Choose a new path
if retrying an interrupted install. The service account must be able to traverse
this path and execute its interpreter. No system Python packages are modified.

Run the isolated smoke test using an existing local PDF before changing services:

```bash
READING_MCP_PDF_LAYOUT_PYTHON=/opt/reading-mcp/pdf-layout-1.28.2/bin/python \
  python3 pdf-layout/smoke.py ./reading-mcp /absolute/path/to/paper.pdf
```

The test uses memory state, disables HTTP retrieval and telemetry, enumerates
canonical sentence/coarse units and exact-reads the returned locators. It checks
protocol consistency; the review report separately assesses sentence correctness.
It neither uses production caches nor starts the network service.

## Activation and rollback

Set `READING_MCP_PDF_LAYOUT_PYTHON` to the absolute interpreter path in the service's
environment. For #87's 2560×3300 original page image, also set
`READING_MCP_SOURCE_VIEW_MAX_PIXELS=16000000`; the existing default of 4,000,000
rejects that embedded image. Output width/height limits remain unchanged. The
optional renderer rejects over-budget images explicitly instead of returning an
apparently successful blank preview. Retain the existing local-root, HTTP and authentication policies.
Only configuring the binary without this variable continues to use the old PDF
parser. Default `READING_MCP_PARSE_TIMEOUT_SECS` is 30; set a larger bounded value
for long PDFs if staging measurements require it. One layout worker runs at a time
per server, and timeout/cancellation kills that worker. Output is bounded.

Use the existing `deploy-production.sh` procedure and its required identity,
checksum, service, state and rollback variables after staging validation. The
script is included in the package, but it **does not configure the Python
interpreter**. Record the old service environment together with the old binary
and state backup before switching. Deployment/restart is the operator's step.

Normalization moves to **v10**; segmentation remains **v3**. The v10 change includes
the external OCR projection protocol and can change canonical text/block boundaries.
Existing stored documents must be explicitly reopened, and clients must discard old
locators and cursors; no old locator is silently rebound. The parser cache includes
the optional PDF backend namespace, now `pdf-layout/v2:...:external-ocr-adapter/v1`,
so v1 parsed output cannot be reused. Original PDF bytes/source still define source
identity; normalized hashes bind the resulting text and blocks. EPUB continues
through its native parser, but the global normalization upgrade also requires
reopening stored EPUBs.

Rollback restores the recorded old binary, **its prior service environment**, and
the matching state backup. A v9 binary must not reinterpret v10 stored documents.
Removing the optional Python environment is not necessary for rollback.

## Known limits and licensing

- Layout headings are inferred and presented as flat owners in source order.
  This does not implement the complete semantic hierarchy requested in #88.
- Printed line-end hyphens are removed only when independently supported by an
  unbroken word in the same document; ambiguous hyphens remain visible. This can
  retain spelling such as `at-tractions`. Exact reading is exact to canonical
  text, not proof of glyph-perfect transcription.
- Front matter, notes, code, tables and captions use coarse blocks. Layout
  classification can still be wrong; inspect `pdf_layout_projection` reliability
  evidence. `integrity=valid` means internally valid mapping, not proven sentence
  accuracy. Do not count coarse units as successful sentence coverage.
- Internal OCR generation remains disabled. If an input already contains an
  external invisible OCR text layer, only full-page, regular body-like lines pass
  the conservative projection gate. Chart/photo labels, low-confidence/unknown
  regions and non-prose text remain coarse or visual region evidence. The profile
  records `pdf_external_ocr_projection` and `pdf_ocr_accuracy_unverified`; parser
  success is not an OCR quality or reading-order claim. Inputs without a qualifying
  prose projection still fail explicitly. Mixed documents can have textless pages;
  `pdf_pages_without_text` flags this gap.
- Original figure/table regions and page evidence are retained; `source_view` uses the pinned PyMuPDF renderer when the optional backend is configured. It renders original PDF pages. This release does not transcribe chart
  values or expose lossless cropped figure exports. Cross-page ranges cannot be
  represented by one page target and fail explicitly for that source-view request.
  Keeping the PDF is lossless for the source file; raster rendering and extraction
  are separate, potentially lossy operations. The optional renderer checks all
  PDF streams conservatively; an oversized stream on another page can reject a
  preview. Linux worker address-space and output-file limits bound allocation.

PyMuPDF/MuPDF offer AGPL and commercial licensing, as described in the
[upstream license documentation](https://pymupdf.readthedocs.io/en/latest/about.html#license-and-copyright).
The pinned PyMuPDF4LLM/Layout distributions also declare AGPLv3 or an Artifex
commercial license. This repository's MIT license does not relicense those
optional dependencies. Their full license notices ship with their distributions;
use the applicable upstream license terms. Public source for this integration is
included in the release's tagged GitHub source archive.
