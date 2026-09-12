"""Isolated optional PDF layout worker; stdout is a versioned JSON protocol.

Only source text spans are projected. No Markdown, OCR, generated descriptions,
or remote model calls are used. Ambiguous printed hyphens are preserved.
"""
import contextlib
import importlib.metadata
import json
import re
import csv
import os
import subprocess
import tempfile
import sys
import unicodedata
import hashlib

VERSION = "pdf-layout/v1"
ENGINE = "pymupdf4llm-layout/1.28.2"
OCR_CONFIG = {"enabled": False}
EXPECTED_IDENTITY = None

def cjk(value):
    return value and ("\u3400" <= value <= "\u9fff" or "\uf900" <= value <= "\ufaff")


def ocr_page(page, language=None, excluded_regions=()):
    """Run the deployer-selected local Tesseract and retain engine grouping."""
    import pymupdf
    config = OCR_CONFIG
    if not config.get("enabled", False):
        return None
    language = "+".join(config["languages"])
    with tempfile.TemporaryDirectory(prefix="reading-mcp-ocr-") as directory:
        image = os.path.join(directory, "page.png")
        output = os.path.join(directory, "words")
        pixmap = page.get_pixmap(dpi=config["dpi"], colorspace=pymupdf.csRGB, alpha=False)
        scale_x = pixmap.width / page.rect.width
        scale_y = pixmap.height / page.rect.height
        pixmap.save(image)
        command = [config["engine_path"], image, output,
                   "--tessdata-dir", config["tessdata_path"],
                   "-l", language, "--oem", str(config["oem"]), "--psm", str(config["psm"]), "--dpi", str(config["dpi"]), "tsv"]
        try:
            subprocess.run(command, check=True, stdout=subprocess.DEVNULL,
                           stderr=subprocess.PIPE, timeout=15, env={"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "OMP_THREAD_LIMIT": "1"})
        except FileNotFoundError as error:
            raise RuntimeError("local OCR engine is not installed") from error
        except subprocess.TimeoutExpired as error:
            raise RuntimeError("local OCR page exceeded 15 second budget") from error
        rows = []
        with open(output + ".tsv", encoding="utf-8", newline="") as stream:
            for row in csv.DictReader(stream, delimiter="\t"):
                if row.get("level") == "5" and row.get("text", "").strip():
                    rows.append(row)
        if excluded_regions:
            kept = []
            for row in rows:
                cx = (int(row["left"]) + int(row["width"]) / 2) / scale_x
                cy = (int(row["top"]) + int(row["height"]) / 2) / scale_y
                if not any(x0 <= cx <= x1 and y0 <= cy <= y1 for x0, y0, x1, y1 in excluded_regions):
                    kept.append(row)
            rows = kept
        lines = {}
        for row in rows:
            key = (int(row["block_num"]), int(row["par_num"]), int(row["line_num"]))
            lines.setdefault(key, []).append(row)
        textlines = []
        for key, words in lines.items():
            words.sort(key=lambda item: int(item["left"]))
            text = ""
            for item in words:
                token = item["text"]
                if text and not (cjk(text[-1]) and cjk(token[0])):
                    text += " "
                text += token
            left = min(int(item["left"]) for item in words) / scale_x
            top = min(int(item["top"]) for item in words) / scale_y
            right = max(int(item["left"]) + int(item["width"]) for item in words) / scale_x
            bottom = max(int(item["top"]) + int(item["height"]) for item in words) / scale_y
            textlines.append({"text": text, "bbox": [left, top, right, bottom],
                              "spans": [{"text": item["text"], "bbox": [int(item["left"]) / scale_x,
                              int(item["top"]) / scale_y,
                              (int(item["left"]) + int(item["width"])) / scale_x,
                              (int(item["top"]) + int(item["height"])) / scale_y],
                              "flags": 0, "ocr_block": key[0], "ocr_paragraph": key[1],
                              "ocr_line": key[2], "confidence": float(item["conf"])} for item in words]})
        if not textlines:
            return None
        boxes = []
        by_block = {}
        for line in textlines:
            first = line["spans"][0]
            by_block.setdefault((first["ocr_block"], first["ocr_paragraph"]), []).append(line)
        for (block, paragraph), block_lines in by_block.items():
            x0, y0 = min(l["bbox"][0] for l in block_lines), min(l["bbox"][1] for l in block_lines)
            x1, y1 = max(l["bbox"][2] for l in block_lines), max(l["bbox"][3] for l in block_lines)
            boxes.append({"boxclass": "text", "bbox": [x0, y0, x1, y1], "x0": x0, "y0": y0, "x1": x1, "y1": y1,
                         "textlines": block_lines,
                         "ocr_block": block, "ocr_paragraph": paragraph})
        return boxes


def line_text(line):
    result = ""
    previous = None
    for span in line["spans"]:
        text = unicodedata.normalize("NFKC", span["text"])
        if previous and result and text and not result[-1].isspace() and not text[0].isspace():
            gap = span["bbox"][0] - previous["bbox"][2]
            # Style changes and superscripts do not introduce word boundaries.
            if span.get("size") is not None and previous.get("size") is not None:
                separates = gap > min(span["size"], previous["size"]) * 0.12
            else:
                separates = gap > 2.0
            if separates and not (cjk(result[-1]) and cjk(text[0])):
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
    ocr_evidence = []
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
            if box.get("ocr_block") is not None:
                for line in box.get("textlines") or []:
                    for span in line.get("spans") or []:
                        ocr_evidence.append({"page": page_no, "block": span.get("ocr_block"),
                            "paragraph": span.get("ocr_paragraph"), "line": span.get("ocr_line"),
                            "text": span["text"], "bbox": span["bbox"],
                            "confidence": span.get("confidence")})
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
    return {"schema_version": VERSION, "engine": ENGINE, "page_count": layout["page_count"], "pages_without_text": sum(not any((b.get("textlines") or []) for b in p["boxes"]) for p in layout["pages"]), "sections": sections, "regions": regions, "ocr_evidence": ocr_evidence, "preserved_ambiguous_hyphens": uncertain}


def main():
    global OCR_CONFIG, EXPECTED_IDENTITY
    max_pages, max_bytes, max_chars = map(int, sys.argv[1:4])
    if len(sys.argv) > 4 and sys.argv[4]:
        OCR_CONFIG = json.loads(sys.argv[4])
    if len(sys.argv) > 5 and sys.argv[5]:
        EXPECTED_IDENTITY = json.loads(sys.argv[5])
        if EXPECTED_IDENTITY.get("config") != OCR_CONFIG:
            raise ValueError("OCR config does not match expected identity")
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
            if OCR_CONFIG.get("enabled", False):
                language = "+".join(OCR_CONFIG["languages"])
                for page, page_layout in zip(doc, layout["pages"]):
                    has_body_text = any((box.get("textlines") or []) and box.get("boxclass") not in ("page-footer", "page-header")
                                        for box in page_layout["boxes"])
                    has_image_region = any(box.get("boxclass") in ("image", "picture", "figure", "table")
                                           for box in page_layout["boxes"])
                    if not has_body_text or has_image_region:
                        excluded = [tuple(box.get("bbox", [])[i] for i in range(4))
                                    for box in page_layout["boxes"]
                                    if box.get("textlines") and len(box.get("bbox", [])) == 4]
                        box = ocr_page(page, language, excluded)
                        if box is not None:
                            page_layout["boxes"].extend(box)
        result = project(layout)
        if OCR_CONFIG.get("enabled", False):
            engine = OCR_CONFIG["engine_path"]
            tessdata = OCR_CONFIG["tessdata_path"]
            def sha(path):
                digest = hashlib.sha256()
                with open(path, "rb") as stream:
                    for chunk in iter(lambda: stream.read(1024 * 1024), b""): digest.update(chunk)
                return digest.hexdigest()
            language = "+".join(OCR_CONFIG["languages"])
            models = [os.path.join(tessdata, f"{name}.traineddata") for name in language.split("+")]
            libraries = []
            for token in subprocess.check_output(["ldd", engine], text=True).split():
                if token.startswith("/") and os.path.isfile(token): libraries.append(sha(token))
            result["ocr_derivation"] = {"schema": "ocr-derivation/v1", "original_sha256": hashlib.sha256(raw).hexdigest(),
                "engine_sha256": sha(engine), "model_sha256": [sha(path) for path in models],
                "library_sha256": sorted(set(libraries)), "languages": OCR_CONFIG["languages"], "dpi": OCR_CONFIG["dpi"], "oem": OCR_CONFIG["oem"], "psm": OCR_CONFIG["psm"],
                "detector_version": OCR_CONFIG["detector_version"], "protocol_version": OCR_CONFIG["protocol_version"],
                "operator_revision": OCR_CONFIG["operator_revision"], "pages": []}
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
