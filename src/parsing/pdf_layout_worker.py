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
import statistics
import time

OCR_PROCESS_ENV = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "OMP_THREAD_LIMIT": "1"}
import math

VERSION = "pdf-layout/v1"
ENGINE = "pymupdf4llm-layout/1.28.2"
OCR_CONFIG = {"enabled": False}
EXPECTED_IDENTITY = None
PAGE_DEADLINE = None
RASTER_BUDGET = None
PAGE_RASTER = None
INSPECTION_POLICY = "ocr-original-region-inspection/v2"

class OcrRasterBudget:
    """Allocation accounting is separate from the count of required OCR pages."""
    def __init__(self):
        self.pages = set()
        self.pixels = 0

    def reserve_raster(self, pixels):
        if self.pixels + pixels > 64_000_000:
            raise RuntimeError("OCR exceeds 64 million total raster pixel limit")
        self.pixels += pixels

    def require_page(self, page_number):
        if page_number not in self.pages and len(self.pages) >= 8:
            raise RuntimeError("OCR exceeds 8 required page limit")
        self.pages.add(page_number)

def page_time_remaining():
    remaining = 15.0 if PAGE_DEADLINE is None else PAGE_DEADLINE - time.monotonic()
    if remaining <= 0:
        raise RuntimeError("local OCR page exceeded shared 15 second budget")
    return remaining

def raster_pixel_count(page, dpi):
    # Round the transformed rectangle outward, as the renderer does. Check
    # before allocating the pixmap; never downscale an oversized page.
    rect = page.rect
    scale = dpi / 72
    coordinates = [rect.x0, rect.y0, rect.x1, rect.y1]
    if not all(math.isfinite(value) for value in coordinates):
        raise RuntimeError("invalid OCR page rectangle")
    width = math.ceil(rect.x1 * scale) - math.floor(rect.x0 * scale)
    height = math.ceil(rect.y1 * scale) - math.floor(rect.y0 * scale)
    if width <= 0 or height <= 0 or width * height > 16_000_000:
        raise RuntimeError("OCR raster exceeds 16 million pixel page limit")
    return width * height
RETRY_POLICY = {"version": "ocr-regional-retry/v1", "primary_psm": 3,
                "retry_psm": 6, "max_retries_per_page": 1, "overlap_percent": 90,
                "vertical_overlap_percent": 50, "roi_padding_pixels": 2}

def runtime_identity(config, dependencies):
    # Match Rust struct field order and serde_json's compact UTF-8 encoding.
    fields = ("enabled", "engine_path", "tessdata_path", "languages", "operator_revision",
              "dpi", "oem", "psm", "detector_version", "protocol_version")
    ordered_config = {key: config[key] for key in fields}
    dependencies = sorted(dependencies, key=lambda d: d["name"])
    encoded = json.dumps([ordered_config, RETRY_POLICY, INSPECTION_POLICY, dependencies],
                         ensure_ascii=False, separators=(",", ":")).encode()
    return {"config": ordered_config, "retry_policy": RETRY_POLICY,
            "inspection_policy": INSPECTION_POLICY,
            "dependencies": dependencies, "sha256": "sha256:" + hashlib.sha256(encoded).hexdigest()}

def fingerprint_dependencies(config):
    def sha(path):
        digest = hashlib.sha256()
        with open(path, "rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""): digest.update(chunk)
        return digest.hexdigest()
    paths = [("engine", config["engine_path"])] + [(f"model:{lang}", os.path.join(config["tessdata_path"], lang + ".traineddata")) for lang in config["languages"]]
    output = subprocess.run(["ldd", config["engine_path"]], text=True, capture_output=True, env=OCR_PROCESS_ENV)
    if output.returncode != 0 or "not found" in output.stdout or "not found" in output.stderr: raise RuntimeError("OCR dependency ldd failure")
    libraries = sorted({token for token in output.stdout.split() if token.startswith("/")})
    paths += [("library:" + path, path) for path in libraries]
    return [{"name": name, "sha256": sha(path)} for name, path in sorted(paths)]

def cjk(value):
    return value and ("\u3400" <= value <= "\u9fff" or "\uf900" <= value <= "\ufaff")


def prepare_page_raster(page):
    import pymupdf
    pixels = raster_pixel_count(page, OCR_CONFIG["dpi"])
    if RASTER_BUDGET is not None:
        RASTER_BUDGET.reserve_raster(pixels)
    return page.get_pixmap(dpi=OCR_CONFIG["dpi"], colorspace=pymupdf.csRGB, alpha=False)

def blank_raster_evidence(page, pixmap):
    samples = pixmap.samples
    if (pixmap.n != 3 or len(samples) != pixmap.width * pixmap.height * 3
            or samples.count(b"\xff") != len(samples) or page.get_texttrace()):
        return None
    return {"width": pixmap.width, "height": pixmap.height, "channels": 3,
            "white_samples": len(samples), "glyph_spans": 0,
            "samples_sha256": hashlib.sha256(samples).hexdigest()}

def ocr_page(page, language=None, excluded_regions=()):
    """Run the deployer-selected local Tesseract and retain engine grouping."""
    config = OCR_CONFIG
    if excluded_regions:
        raise ValueError("native exclusions require the observation-preserving wrapper")
    if not config.get("enabled", False):
        return None
    language = "+".join(config["languages"])
    page_time_remaining()
    pixmap = PAGE_RASTER if PAGE_RASTER is not None else prepare_page_raster(page)
    if RASTER_BUDGET is not None:
        RASTER_BUDGET.require_page(page.number)
    with tempfile.TemporaryDirectory(prefix="reading-mcp-ocr-") as directory:
        image = os.path.join(directory, "page.png")
        output = os.path.join(directory, "words")
        scale_x = pixmap.width / page.rect.width
        scale_y = pixmap.height / page.rect.height
        pixmap.save(image)
        command = [config["engine_path"], image, output,
                   "--tessdata-dir", config["tessdata_path"],
                   "-l", language, "--oem", str(config["oem"]), "--psm", str(config["psm"]), "--dpi", str(config["dpi"]), "tsv"]
        try:
            subprocess.run(command, check=True, stdout=subprocess.DEVNULL,
                           stderr=subprocess.PIPE, timeout=page_time_remaining(), env=OCR_PROCESS_ENV)
        except FileNotFoundError as error:
            raise RuntimeError("local OCR engine is not installed") from error
        except subprocess.TimeoutExpired as error:
            raise RuntimeError("local OCR page exceeded 15 second budget") from error
        rows = []
        with open(output + ".tsv", encoding="utf-8", newline="") as stream:
            for row in csv.DictReader(stream, delimiter="\t"):
                if row.get("level") == "5" and row.get("text", "").strip():
                    rows.append(row)
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

def _bbox_area(box):
    b=box["bbox"]; return max(0,b[2]-b[0])*max(0,b[3]-b[1])
def _bbox_overlap(a,b):
    return max(0,min(a[2],b[2])-max(a[0],b[0]))*max(0,min(a[3],b[3])-max(a[1],b[1]))
def _merge_components(pairs):
    parent={i:i for pair in pairs for i in pair}
    def find(x):
        while parent[x]!=x: parent[x]=parent[parent[x]]; x=parent[x]
        return x
    for a,b in pairs:
        a,b=find(a),find(b)
        if a!=b: parent[b]=a
    groups={}
    for i in parent: groups.setdefault(find(i),set()).add(i)
    return list(groups.values())
def _adjacent(a,b,gap):
    for x in [l["bbox"] for l in a.get("textlines",[])]:
        for y in [l["bbox"] for l in b.get("textlines",[])]:
            vertical=max(0,min(x[3],y[3])-max(x[1],y[1])); horizontal=max(0,max(x[0],y[0])-min(x[2],y[2]))
            if vertical >= min(x[3]-x[1],y[3]-y[1])*.5 and horizontal <= gap: return True
    return False

def _attempt_reference(page, attempt, box, line=None, word=None):
    reference = {"page": page, "attempt": attempt, "box": box}
    if line is not None:
        reference["line"] = line
    if word is not None:
        reference["word"] = word
    return reference

def _regional_evidence(page, bounds, primary, retry, selected, components):
    """References address immutable observations, not projected text or gold."""
    attempts = [{"id": "primary", "psm": 3, "boxes": primary}]
    if retry is not None:
        attempts.append({"id": "retry", "psm": 6, "boxes": retry})
    selection = []
    for selected_index, box in enumerate(selected):
        matches = [(attempt, index) for attempt in attempts
                   for index, observed in enumerate(attempt["boxes"]) if observed is box]
        if len(matches) != 1:
            raise ValueError("OCR selected box has ambiguous attempt reference")
        attempt, index = matches[0]
        selection.append({
            "selected_box": selected_index,
            "source": _attempt_reference(page, attempt["id"], index),
            "words": [_attempt_reference(page, attempt["id"], index, line_index, word_index)
                      for line_index, line in enumerate(box.get("textlines", []))
                      for word_index, _ in enumerate(line.get("spans", []))],
        })
    return {"schema": "ocr-regional-observations/v3", "page": page, "page_bounds": bounds,
            "complete": all(component["resolved"] for component in components),
            "attempts": attempts, "selection": selection, "components": components}

def native_text_regions(page_layout):
    regions = []
    for index, box in enumerate(page_layout["boxes"]):
        text = "\n".join(line_text(line) for line in (box.get("textlines") or []))
        if text.strip():
            regions.append({"source_box": index, "source_class": box["boxclass"],
                            "bbox": [box[k] for k in ("x0", "y0", "x1", "y1")], "text": text})
    return regions

def exclude_native_boxes(selected, evidence, regions):
    evidence["native_regions"] = regions
    evidence["excluded_sources"] = []
    retained, selection = [], []
    for box, chosen in zip(selected, evidence["selection"]):
        centers = [((w["bbox"][0] + w["bbox"][2]) / 2, (w["bbox"][1] + w["bbox"][3]) / 2)
                   for line in box["textlines"] for w in line["spans"]]
        covered = [any(r["bbox"][0] <= x <= r["bbox"][2] and r["bbox"][1] <= y <= r["bbox"][3]
                       for r in regions) for x, y in centers]
        if covered and all(covered):
            evidence["excluded_sources"].append(chosen["source"])
        else:
            if any(covered):
                evidence["complete"] = False
                evidence["projection_failure"] = "partial_native_overlap"
            retained.append(box)
            selection.append(dict(chosen, selected_box=len(selection)))
    evidence["selection"] = selection
    return retained, evidence

def _regional_ocr(page, excluded_regions=()):
    global PAGE_DEADLINE, PAGE_RASTER
    previous = PAGE_DEADLINE
    previous_raster = PAGE_RASTER
    deadline = time.monotonic() + 15
    PAGE_DEADLINE = deadline if previous is None else min(previous, deadline)
    try:
        PAGE_RASTER = prepare_page_raster(page)
        blank = blank_raster_evidence(page, PAGE_RASTER)
        if blank is not None:
            page_time_remaining()
            return [], {"schema": "ocr-regional-observations/v3", "page": page.number + 1,
                        "page_bounds": [0, 0, page.rect.width, page.rect.height],
                        "complete": True, "attempts": [], "selection": [], "components": [],
                        "blank_raster": blank}
        selected, evidence = _regional_ocr_attempts(page)
        result = exclude_native_boxes(selected, evidence, list(excluded_regions))
        page_time_remaining()
        return result
    finally:
        PAGE_DEADLINE = previous
        PAGE_RASTER = previous_raster

def _regional_ocr_attempts(page, excluded_regions=()):
    global OCR_CONFIG
    primary=ocr_page(page, excluded_regions=excluded_regions); primary=primary or []
    conflicts=[(i,j) for i,a in enumerate(primary) for j,b in enumerate(primary[i+1:],i+1)
               if min(_bbox_area(a),_bbox_area(b)) and _bbox_overlap(a["bbox"],b["bbox"])/min(_bbox_area(a),_bbox_area(b))>=.9]
    components=_merge_components(conflicts)
    heights=[l["bbox"][3]-l["bbox"][1] for b in primary for l in b.get("textlines",[]) if l["bbox"][3]>l["bbox"][1]]
    gap=statistics.median(heights) if heights else 0
    changed=True
    while changed:
        changed=False
        for component in components:
            for i,box in enumerate(primary):
                if i not in component and any(_adjacent(primary[j],box,gap) for j in component): component.add(i); changed=True
    changed=True
    while changed:
        changed=False
        for i in range(len(components)):
            for j in range(i+1,len(components)):
                if components[i] & components[j]:
                    components[i].update(components.pop(j)); changed=True; break
            if changed: break
    page_number = page.number + 1
    if not components:
        return primary, _regional_evidence(page_number, [0,0,page.rect.width,page.rect.height], primary, None, primary, [])
    original=list(primary); retry_config=dict(OCR_CONFIG); retry_config["psm"]=6
    page_time_remaining()
    saved=OCR_CONFIG; OCR_CONFIG=retry_config
    try: retry=ocr_page(page, excluded_regions=excluded_regions) or []
    finally: OCR_CONFIG=saved
    tolerance=2*72/saved["dpi"]; rect=[0,0,page.rect.width,page.rect.height]; replacements=[]; diagnostics=[]
    for component in components:
        raw=[min(original[i]["bbox"][k] for i in component) for k in (0,1)]+[max(original[i]["bbox"][k] for i in component) for k in (2,3)]
        roi=[max(rect[0],raw[0]-tolerance),max(rect[1],raw[1]-tolerance),min(rect[2],raw[2]+tolerance),min(rect[3],raw[3]+tolerance)]
        candidate_indices=[i for i,b in enumerate(retry) if all(b["bbox"][k]>=roi[k] for k in (0,1)) and all(b["bbox"][k]<=roi[k] for k in (2,3))]
        candidates=[retry[i] for i in candidate_indices]
        centers=[((s["bbox"][0]+s["bbox"][2])/2,(s["bbox"][1]+s["bbox"][3])/2) for i in component for l in original[i].get("textlines",[]) for s in l.get("spans",[])]
        covered=all(any(l["bbox"][0]<=x<=l["bbox"][2] and l["bbox"][1]<=y<=l["bbox"][3] for b in candidates for l in b.get("textlines",[])) for x,y in centers)
        resolved=bool(candidates) and covered
        diagnostics.append({"indices":sorted(component),"roi":raw,"effective_roi":roi,
                            "candidate_count":len(candidates),"resolved":resolved,
                            "failure":None if resolved else "candidate_does_not_cover_component",
                            "primary_refs":[_attempt_reference(page_number,"primary",i) for i in sorted(component)],
                            "candidate_refs":[_attempt_reference(page_number,"retry",i) for i in candidate_indices],
                            "replaced_refs":[_attempt_reference(page_number,"primary",i) for i in sorted(component)] if resolved else []})
        if resolved: replacements.append((min(component),set(component),candidates))
    members=set().union(*(m for _,m,_ in replacements)) if replacements else set(); selected=[]
    by_first={i:c for i,_,c in replacements}
    for i,box in enumerate(original):
        if i in by_first: selected.extend(by_first[i])
        if i not in members: selected.append(box)
    return selected, _regional_evidence(page_number, rect, original, retry, selected, diagnostics)


def line_text(line):
    is_ocr_line = any(span.get("ocr_block") is not None for span in line["spans"])
    result = ""
    previous = None
    for span in line["spans"]:
        # Preserve the engine's actual characters.  Compatibility folding (NFKC)
        # would silently turn full-width/CJK punctuation into different output.
        text = unicodedata.normalize("NFC" if is_ocr_line else "NFKC", span["text"])
        if previous and result and text and not result[-1].isspace() and not text[0].isspace():
            gap = span["bbox"][0] - previous["bbox"][2]
            # Style changes and superscripts do not introduce word boundaries.
            if span.get("size") is not None and previous.get("size") is not None:
                separates = gap > min(span["size"], previous["size"]) * 0.12
            else:
                separates = gap > 2.0
            left, right = result[-1], text[0]
            left_punct = unicodedata.category(left).startswith("P")
            right_punct = unicodedata.category(right).startswith("P")
            no_boundary = cjk(left) and cjk(right)
            if is_ocr_line:
                no_boundary = (no_boundary
                               or (right_punct and (cjk(left) or left_punct))
                               or (left_punct and (cjk(right) or right_punct)))
            if separates and not no_boundary:
                result += " "
        result += text
        previous = span
    return result.strip() if is_ocr_line else " ".join(result.split())


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
    global OCR_CONFIG, EXPECTED_IDENTITY, RASTER_BUDGET
    protocol_stdout = sys.stdout
    max_pages, max_bytes, max_chars = map(int, sys.argv[1:4])
    if len(sys.argv) > 4 and sys.argv[4]:
        OCR_CONFIG = json.loads(sys.argv[4])
    if len(sys.argv) > 5 and sys.argv[5]:
        EXPECTED_IDENTITY = json.loads(sys.argv[5])
        if EXPECTED_IDENTITY.get("config") != OCR_CONFIG:
            raise ValueError("OCR config does not match expected identity")
    actual_dependencies = None
    if OCR_CONFIG.get("enabled"):
        RASTER_BUDGET = OcrRasterBudget()
        if EXPECTED_IDENTITY is None: raise ValueError("OCR expected identity is required")
        actual_dependencies = fingerprint_dependencies(OCR_CONFIG)
        if EXPECTED_IDENTITY.get("dependencies") != actual_dependencies: raise ValueError("OCR dependency identity mismatch")
        actual_identity = runtime_identity(OCR_CONFIG, actual_dependencies)
        if EXPECTED_IDENTITY != actual_identity:
            raise ValueError("OCR policy/runtime identity mismatch")
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
                        excluded = native_text_regions(page_layout)
                        selected, retry_diagnostic = _regional_ocr(page, excluded)
                        if selected:
                            page_layout["boxes"].extend(selected)
                        page_layout["ocr_retry_diagnostic"] = retry_diagnostic
        observations = [p["ocr_retry_diagnostic"] for p in layout["pages"]
                        if "ocr_retry_diagnostic" in p]
        if any(not observation["complete"] for observation in observations):
            json.dump({"schema": "ocr-worker-failure/v1",
                       "original_sha256": hashlib.sha256(raw).hexdigest(),
                       "runtime_identity_sha256": actual_identity["sha256"],
                       "ocr_attempts": observations,
                       "error": "OCR_NO_SUPPORTED_PROJECTION"}, protocol_stdout, ensure_ascii=False)
            raise ValueError("OCR geometric conflict remains unresolved")
        result = project(layout)
        result["ocr_attempts"] = observations
        if OCR_CONFIG.get("enabled", False):
            engine = OCR_CONFIG["engine_path"]
            tessdata = OCR_CONFIG["tessdata_path"]
            def sha(path):
                digest = hashlib.sha256()
                with open(path, "rb") as stream:
                    for chunk in iter(lambda: stream.read(1024 * 1024), b""): digest.update(chunk)
                return digest.hexdigest()
            language = "+".join(OCR_CONFIG["languages"])
            result["ocr_derivation"] = {"schema": "ocr-derivation/v3", "original_sha256": hashlib.sha256(raw).hexdigest(),
                "inspection_policy": INSPECTION_POLICY,
                "retry_policy": RETRY_POLICY, "runtime_identity_sha256": actual_identity["sha256"],
                "engine_sha256": next(d["sha256"] for d in actual_dependencies if d["name"] == "engine"), "model_sha256": [d["sha256"] for d in actual_dependencies if d["name"].startswith("model:")],
                "library_sha256": [d["sha256"] for d in actual_dependencies if d["name"].startswith("library:")], "languages": OCR_CONFIG["languages"], "dpi": OCR_CONFIG["dpi"], "oem": OCR_CONFIG["oem"], "psm": OCR_CONFIG["psm"],
                "detector_version": OCR_CONFIG["detector_version"], "protocol_version": OCR_CONFIG["protocol_version"],
                "operator_revision": OCR_CONFIG["operator_revision"], "pages": []}
        if not any(b["kind"] == "paragraph" for s in result["sections"] for b in s["blocks"]):
            if OCR_CONFIG.get("enabled", False):
                json.dump({"schema": "ocr-worker-failure/v1",
                           "original_sha256": hashlib.sha256(raw).hexdigest(),
                           "runtime_identity_sha256": actual_identity["sha256"],
                           "error": "OCR_NO_SUPPORTED_PROJECTION",
                           "ocr_attempts": observations}, protocol_stdout, ensure_ascii=False)
                raise ValueError("no supported prose text in inspected original pages")
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
