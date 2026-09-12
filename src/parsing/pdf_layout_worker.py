"""Isolated optional PDF layout worker; stdout is a versioned JSON protocol.

Only source text spans are projected. No Markdown, OCR, generated descriptions,
or remote model calls are used. Ambiguous printed hyphens are preserved.
"""
import contextlib
import importlib.metadata
import json
import re
import sys
import unicodedata

VERSION = "pdf-layout/v1"
ENGINE = "pymupdf4llm-layout/1.28.2"


def line_text(line):
    result = ""
    previous = None
    for span in line["spans"]:
        text = unicodedata.normalize("NFKC", span["text"])
        if previous and result and text and not result[-1].isspace() and not text[0].isspace():
            gap = span["bbox"][0] - previous["bbox"][2]
            # Style changes and superscripts do not introduce word boundaries.
            if gap > min(span["size"], previous["size"]) * 0.12:
                result += " "
        result += text
        previous = span
    return " ".join(result.split())


def join_lines(lines, vocabulary):
    text = ""
    uncertain = 0
    for line in lines:
        if not line:
            continue
        if text.endswith("\u00ad"):
            text = text[:-1] + line
        elif text.endswith("-"):
            left = re.search(r"([\w-]+)-$", text)
            right = re.match(r"([\w-]+)", line)
            if left and right:
                a, b = left[1], right[1]
                combined = a + b
                # Only remove a visible hyphen with independent, unbroken evidence
                # in this PDF. Preserve compounds, capitalization and ambiguity.
                if a.islower() and b.islower() and "-" not in combined and combined in vocabulary and a + "-" + b not in vocabulary:
                    text = text[:-1] + line
                else:
                    text += line
                    uncertain += 1
            else:
                text += " " + line
        else:
            text += (" " if text else "") + line
    return text, uncertain


def project(layout):
    vocabulary = set()
    for page in layout["pages"]:
        for box in page["boxes"]:
            for line in (box.get("textlines") or []):
                vocabulary.update(re.findall(r"\b[\w]+(?:-[\w]+)*\b", line_text(line)))
    sections = [{"title": "Front matter", "blocks": []}]
    auxiliary = []
    regions = []
    uncertain = 0
    abstract_headings = {"abstract", "摘要", "概要"}
    has_abstract = any(" ".join(line_text(l) for l in (b.get("textlines") or [])).strip().casefold() in abstract_headings
                       for p in layout["pages"] for b in p["boxes"] if b["boxclass"] == "section-header")
    body_started = False
    has_heading = any(b["boxclass"] == "section-header" for p in layout["pages"] for b in p["boxes"])
    if not has_heading:
        sections[0]["title"] = "Document"
    for page in layout["pages"]:
        page_no = page["page_number"]
        for index, box in enumerate(page["boxes"]):
            cls = box["boxclass"]
            lines = [line_text(line) for line in (box.get("textlines") or [])]
            text, count = join_lines(lines, vocabulary)
            uncertain += count
            bbox = [box[k] for k in ("x0", "y0", "x1", "y1")]
            region = {"page": page_no, "box": index, "bbox": bbox, "class": cls}
            regions.append(region)
            if cls == "section-header" and text and (body_started or not has_abstract or text.casefold() in abstract_headings):
                body_started = True
                sections.append({"title": text, "blocks": []})
                continue
            if not text:
                continue  # non-text regions remain in the original PDF and region evidence
            spans = [s for line in (box.get("textlines") or []) for s in line["spans"] if s["text"].strip()]
            mono = bool(spans) and all(s["flags"] & 8 for s in spans)
            is_note = cls in ("footnote", "page-header", "page-footer", "caption")
            if spans and spans[0]["flags"] & 1:
                is_note = True
            if len(spans) > 1 and spans[0].get("origin", [0, 0])[1] < spans[1].get("origin", [0, 0])[1] and spans[0]["size"] <= spans[1]["size"] * 0.8:
                is_note = True
            if bbox[1] > page.get("height", float("inf")) * 0.9 and re.search(r"\b(conference|proceedings|copyright|arxiv)\b|©", text, re.IGNORECASE):
                is_note = True
            is_front = has_heading and not body_started
            kind = "paragraph" if cls == "text" and not mono and not is_note and not is_front else "preformatted"
            if cls == "table":
                kind = "table"
            if kind != "paragraph":
                text = "\n".join(lines).strip()
            block = {"text": text, "kind": kind, "parts": [{"start": 0, "end": len(text), "page": page_no}], "region": region}
            if is_note:
                auxiliary.append(block)
                continue
            blocks = sections[-1]["blocks"]
            # Rejoin layout fragments only for an unfinished prose paragraph across
            # a page/column break. Notes never interrupt this main-text stream.
            if blocks and kind == "paragraph" and blocks[-1]["kind"] == "paragraph":
                prev = blocks[-1]
                before = prev["text"]
                prior_region = prev["region"]
                crosses = page_no != prior_region["page"] or bbox[0] > prior_region["bbox"][2]
                if crosses and before and before[-1] not in '.!?。！？:;”"' and text[0].islower():
                    joined, extra = join_lines([before, text], vocabulary)
                    uncertain += extra
                    prefix = len(joined) - len(text)
                    # A removed wrap hyphen also shortens its original-page range.
                    prev["parts"][-1]["end"] = min(prev["parts"][-1]["end"], prefix)
                    prev["parts"].append({"start": prefix, "end": len(joined), "page": page_no})
                    prev["text"] = joined
                    prev["region"] = region
                    continue
            blocks.append(block)
    if auxiliary:
        sections.append({"title": "Page notes and captions", "blocks": auxiliary})
    return {"schema_version": VERSION, "engine": ENGINE, "page_count": layout["page_count"], "pages_without_text": sum(not any((b.get("textlines") or []) for b in p["boxes"]) for p in layout["pages"]), "sections": sections, "regions": regions, "preserved_ambiguous_hyphens": uncertain}


def main():
    max_pages, max_bytes, max_chars = map(int, sys.argv[1:4])
    for package in ("pymupdf", "pymupdf4llm", "pymupdf-layout"):
        if importlib.metadata.version(package) != "1.28.2":
            raise ValueError(f"{package} must be version 1.28.2; run setup-pdf-layout.sh")
    # Libraries may print status messages; reserve stdout for protocol output.
    with contextlib.redirect_stdout(sys.stderr):
        import pymupdf
        import pymupdf4llm
        pymupdf4llm.use_layout(True)
        raw = sys.stdin.buffer.read(max_bytes + 1)
        if len(raw) > max_bytes:
            raise ValueError("PDF exceeds byte limit")
        with pymupdf.open(stream=raw, filetype="pdf") as doc:
            if doc.needs_pass:
                raise ValueError("encrypted PDF requires a password")
            if not 0 < len(doc) <= max_pages:
                raise ValueError("PDF exceeds page limit or has no pages")
            layout = json.loads(pymupdf4llm.to_json(doc, use_ocr=False))
        result = project(layout)
        if not any(b["kind"] == "paragraph" for s in result["sections"] for b in s["blocks"]):
            raise ValueError("no supported prose text; scanned/image-only PDFs need OCR (not enabled)")
        if sum(len(b["text"]) for s in result["sections"] for b in s["blocks"]) > max_chars:
            raise ValueError("PDF exceeds normalized text limit")
    json.dump(result, sys.stdout, ensure_ascii=False, separators=(",", ":"))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"PDF layout failed: {error}", file=sys.stderr)
        sys.exit(1)
