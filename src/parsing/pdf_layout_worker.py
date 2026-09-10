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

VERSION = "pdf-layout/v2"
ENGINE = "pymupdf4llm-layout/1.28.2"

OCR_MIN_HIDDEN_CHARS = 128
OCR_MIN_BODY_LINES = 8
OCR_MIN_BODY_LINE_RATIO = 0.55
OCR_MIN_BODY_LINE_CHARS = 28
OCR_MIN_BODY_LINE_WORDS = 5
OCR_MIN_FONT_SIZE = 4.0


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


def _words(text):
    return re.findall(r"[\w]+(?:['’\-][\w]+)*", text, re.UNICODE)


def _line_text_and_metrics(line):
    text = line_text(line)
    spans = [span for span in line.get("spans", []) if span.get("text", "").strip()]
    sizes = [float(span["size"]) for span in spans if span.get("size") is not None]
    confidences = [float(span["confidence"]) for span in spans if span.get("confidence") is not None]
    bbox = line.get("bbox", [0, 0, 0, 0])
    return {
        "line": line,
        "text": text,
        "words": _words(text),
        "size": min(sizes) if sizes else 0.0,
        "confidence": min(confidences) if confidences else None,
        "x0": float(bbox[0]),
        "y0": float(bbox[1]),
        "x1": float(bbox[2]),
        "y1": float(bbox[3]),
    }


def _is_body_line(metrics):
    if metrics["confidence"] is not None and metrics["confidence"] < 0.60:
        return False
    return (
        len(metrics["text"]) >= OCR_MIN_BODY_LINE_CHARS
        and len(metrics["words"]) >= OCR_MIN_BODY_LINE_WORDS
        and metrics["size"] >= OCR_MIN_FONT_SIZE
    )


def _ocr_picture_candidate(page, box):
    evidence = page.get("ocr_text_layer") or box.get("ocr_text_layer") or {}
    hidden_chars = int(evidence.get("hidden_chars", 0))
    visible_chars = int(evidence.get("visible_chars", 0))
    if hidden_chars < OCR_MIN_HIDDEN_CHARS or hidden_chars < visible_chars * 4:
        return None

    page_area = float(page.get("width", 0)) * float(page.get("height", 0))
    box_area = max(0.0, float(box["x1"]) - float(box["x0"])) * max(
        0.0, float(box["y1"]) - float(box["y0"])
    )
    if page_area <= 0 or box_area / page_area < 0.65:
        return None

    lines = [_line_text_and_metrics(line) for line in (box.get("textlines") or [])]
    lines = [line for line in lines if line["text"]]
    body_lines = [line for line in lines if _is_body_line(line)]
    if len(body_lines) < OCR_MIN_BODY_LINES:
        return None
    if len(body_lines) / len(lines) < OCR_MIN_BODY_LINE_RATIO:
        return None

    # A chart/photo label collection usually has short, sparse lines. Require
    # the body candidates to occupy regular text bands before projecting them.
    wide_lines = [line for line in body_lines if line["x1"] - line["x0"] >= 160]
    if len(wide_lines) / len(body_lines) < 0.70:
        return None
    return lines


def _ocr_block(lines, kind, page_no, region):
    text = ""
    parts = []
    for metrics in lines:
        if text:
            text += " "
        start = len(text)
        text += metrics["text"]
        parts.append({"start": start, "end": len(text), "page": page_no})
    if not text:
        return None
    return {"text": text, "kind": kind, "parts": parts, "region": region}


def _ocr_picture_blocks(page, box, region):
    metrics = _ocr_picture_candidate(page, box)
    if metrics is None:
        return [], 0, 0

    body = []
    projected = []
    promoted_chars = 0
    uncertain_regions = 0
    previous = None

    def flush(kind):
        nonlocal body, promoted_chars, uncertain_regions
        if not body:
            return
        block = _ocr_block(body, kind, page["page_number"], region)
        if block:
            projected.append(block)
            if kind == "paragraph":
                promoted_chars += len(block["text"])
            else:
                uncertain_regions += 1
        body = []

    for current in metrics:
        qualifies = _is_body_line(current)
        if previous is not None:
            vertical_gap = current["y0"] - previous["y1"]
            horizontal_overlap = min(current["x1"], previous["x1"]) - max(
                current["x0"], previous["x0"]
            )
            same_band = vertical_gap >= -2 and vertical_gap <= max(
                14.0, (previous["y1"] - previous["y0"]) * 2.5
            )
            same_column = horizontal_overlap > 0 or abs(current["x0"] - previous["x0"]) < 24
            if not same_band or not same_column or len(body) >= 12:
                flush("paragraph" if all(_is_body_line(item) for item in body) else "preformatted")
        if qualifies:
            body.append(current)
        else:
            flush("paragraph" if all(_is_body_line(item) for item in body) else "preformatted")
            body = [current]
        previous = current
    flush("paragraph" if all(_is_body_line(item) for item in body) else "preformatted")
    return projected, promoted_chars, uncertain_regions


def _text_layer_evidence(page):
    traces = page.get_texttrace()
    hidden_chars = sum(len(trace.get("chars", [])) for trace in traces if trace.get("type") == 3)
    visible_chars = sum(len(trace.get("chars", [])) for trace in traces if trace.get("type") != 3)
    return {
        "hidden_chars": hidden_chars,
        "visible_chars": visible_chars,
        "hidden_traces": sum(trace.get("type") == 3 for trace in traces),
    }


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
    ocr_pages = []
    ocr_uncertain_regions = 0
    ocr_promoted_chars = 0
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
            if cls == "picture":
                ocr_blocks, promoted_chars, uncertain_count = _ocr_picture_blocks(page, box, region)
                if ocr_blocks:
                    ocr_pages.append(page_no)
                    ocr_promoted_chars += promoted_chars
                    ocr_uncertain_regions += uncertain_count
                    body_started = True
                    for projected in ocr_blocks:
                        sections[-1]["blocks"].append(projected)
                    region["projection"] = "external-ocr-prose-with-uncertainty"
                    continue
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
    return {"schema_version": VERSION, "engine": ENGINE, "page_count": layout["page_count"], "pages_without_text": sum(not any((b.get("textlines") or []) for b in p["boxes"]) for p in layout["pages"]), "sections": sections, "regions": regions, "preserved_ambiguous_hyphens": uncertain, "ocr_pages": sorted(set(ocr_pages)), "ocr_uncertain_regions": ocr_uncertain_regions, "ocr_promoted_chars": ocr_promoted_chars}


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
            for layout_page, source_page in zip(layout["pages"], doc):
                layout_page["ocr_text_layer"] = _text_layer_evidence(source_page)
        result = project(layout)
        if not any(b["kind"] == "paragraph" for s in result["sections"] for b in s["blocks"]):
            raise ValueError("no high-confidence prose projection; OCR text is absent or retained as visual/uncertain evidence")
        if sum(len(b["text"]) for s in result["sections"] for b in s["blocks"]) > max_chars:
            raise ValueError("PDF exceeds normalized text limit")
    json.dump(result, sys.stdout, ensure_ascii=False, separators=(",", ":"))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"PDF layout failed: {error}", file=sys.stderr)
        sys.exit(1)
