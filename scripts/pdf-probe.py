#!/usr/bin/env python3
"""BENCH-D2 PDF open/read baseline probe for reading-mcp.

Speaks newline-delimited JSON-RPC MCP over stdio to an existing release binary, exactly like
`scripts/stress-probe.py`. Two modes:

- `--mode run --input label=path --backend default|layout`: for one input and one backend, open
  the document (wall time, peak RSS, state-dir bytes, error code, killed), read its structure,
  reopen it in the same state dir, read it sentence by sentence with `get_text_units` (the whole
  document for generated inputs, the first 200 sentences for real papers), score the text against
  the generator's ground truth, and optionally repeat the open under systemd memory caps. Writes
  one JSONL file and prints a markdown table.
- `--mode aggregate --jsonl-dir dir`: merges every JSONL file and prints the full result tables.

The layout backend is selected the same way the server selects it: `READING_MCP_PDF_LAYOUT_PYTHON`
points at a pinned layout interpreter. The default backend leaves the variable unset. OCR stays
off (it is opt-in): an image-only PDF is expected to report `ocr_required`.

Standard library only, plus PyMuPDF for the page count. This script never builds anything.
"""

from __future__ import annotations

import argparse
import difflib
import json
import math
import os
import queue
import re
import shutil
import subprocess
import sys
import threading
import time
import unicodedata

MIB = 1024 * 1024
INITIALIZE_PARAMS = {
    "protocolVersion": "2025-06-18",
    "capabilities": {},
    "clientInfo": {"name": "pdf-probe", "version": "0"},
}
TEXT_UNIT_BASE = {
    "requested_kind": "sentence",
    "direction": "forward",
    "coverage_policy": "preserve_source",
}
CAPS = ["1024M", "768M", "512M"]
KILLED_CODES = (-9, 137)
READ_CAP_SENTENCES = 20000
REAL_PAPER_SENTENCES = 200
SIMILARITY_WINDOW = 2000
FUZZY_MIN_RATIO = 0.90
TOOL_TIMEOUT_S = 3600.0

# Characters the embedded fonts map to a different but visually identical code point on the way
# back out (MuPDF builds ToUnicode from the glyph, not from the source string). Folded on both
# sides before scoring, so a glyph round-trip artefact is not scored as a text-quality defect.
FOLD = {
    0x2010: "-", 0x2011: "-", 0x2012: "-", 0x2013: "-", 0x2014: "-", 0x2212: "-",
    0x00AD: "", 0x2018: "'", 0x2019: "'", 0x201C: '"', 0x201D: '"',
}

BACKENDS = ("default", "layout")


class ProbeError(RuntimeError):
    def __init__(self, message: str, code: str | None = None) -> None:
        super().__init__(message)
        self.code = code


def send(process: subprocess.Popen, message: dict) -> None:
    assert process.stdin is not None
    process.stdin.write((json.dumps(message) + "\n").encode("utf-8"))
    process.stdin.flush()


def pump_stdout(stream, responses: queue.Queue) -> None:
    try:
        for line in stream:
            try:
                responses.put(json.loads(line.decode("utf-8")))
            except (UnicodeDecodeError, json.JSONDecodeError):
                continue
    except Exception:  # noqa: BLE001 - draining is best effort
        pass
    finally:
        responses.put(None)


def read_response(responses: queue.Queue, awaited_id: int, timeout: float) -> dict:
    deadline = time.monotonic() + timeout
    while True:
        wait = deadline - time.monotonic()
        if wait <= 0:
            raise ProbeError(f"timed out after {timeout:.0f}s waiting for id {awaited_id}")
        try:
            item = responses.get(timeout=wait)
        except queue.Empty:
            raise ProbeError(f"timed out after {timeout:.0f}s waiting for id {awaited_id}") from None
        if item is None:
            raise ProbeError("server closed stdout before responding")
        if isinstance(item, dict) and item.get("id") == awaited_id:
            return item


def error_parts(response: dict) -> tuple[str | None, str] | None:
    """Return (code, message) for a failed response, else None."""
    if response.get("error") is not None:
        error = response["error"]
        data = error.get("data") if isinstance(error, dict) else None
        code = data.get("code") if isinstance(data, dict) else None
        message = error.get("message", str(error)) if isinstance(error, dict) else str(error)
        return code or "json_rpc_error", str(message)
    result = response.get("result")
    if isinstance(result, dict) and result.get("isError"):
        texts = [
            item.get("text", "")
            for item in result.get("content", [])
            if isinstance(item, dict) and item.get("type") == "text"
        ]
        return "tool_error", " ".join(texts) or "tool reported an error"
    return None


def payload(result: dict):
    structured = result.get("structuredContent")
    if isinstance(structured, (dict, list)):
        return structured
    for block in result.get("content", []) or []:
        if isinstance(block, dict) and block.get("type") == "text":
            try:
                return json.loads(block.get("text", ""))
            except json.JSONDecodeError:
                return None
    return None


def peak_rss_kib(pid: int) -> int | None:
    """VmHWM: the peak resident set of the process so far."""
    try:
        with open(f"/proc/{pid}/status", encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("VmHWM:"):
                    return int(line.split()[1])
    except (OSError, ValueError):
        return None
    return None


class Server:
    def __init__(self, command: list[str], env: dict, log_path: str, timeout: float) -> None:
        self.timeout = timeout
        self.log = open(log_path, "wb")
        self.process = subprocess.Popen(
            command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log, env=env
        )
        self.pid = self.process.pid
        self.responses: queue.Queue = queue.Queue()
        threading.Thread(
            target=pump_stdout, args=(self.process.stdout, self.responses), daemon=True
        ).start()
        self.next_id = 0
        self.rc: int | None = None
        self.usage = None

    def _request(self, method: str, params: dict, timeout: float) -> dict:
        self.next_id += 1
        request_id = self.next_id
        send(self.process, {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        return read_response(self.responses, request_id, timeout)

    def initialize(self) -> None:
        response = self._request("initialize", INITIALIZE_PARAMS, self.timeout)
        failure = error_parts(response)
        if failure:
            raise ProbeError(failure[1], failure[0])
        send(self.process, {"jsonrpc": "2.0", "method": "notifications/initialized"})

    def tool(self, name: str, arguments: dict, timeout: float | None = None) -> dict:
        response = self._request(
            "tools/call", {"name": name, "arguments": arguments}, timeout or self.timeout
        )
        failure = error_parts(response)
        if failure:
            raise ProbeError(f"{name}: {failure[1]}", failure[0])
        result = response.get("result")
        if not isinstance(result, dict):
            raise ProbeError(f"{name}: result is not an object")
        return result

    def close(self, grace: float = 120.0) -> None:
        try:
            if self.process.stdin is not None:
                self.process.stdin.close()
        except OSError:
            pass
        deadline = time.monotonic() + grace
        while True:
            pid, status, usage = os.wait4(self.pid, os.WNOHANG)
            if pid == self.pid:
                self.rc = os.waitstatus_to_exitcode(status)
                self.usage = usage
                break
            if time.monotonic() > deadline:
                self.kill()
                _, status, usage = os.wait4(self.pid, 0)
                self.rc = os.waitstatus_to_exitcode(status)
                self.usage = usage
                break
            time.sleep(0.05)
        self.log.close()

    def kill(self) -> None:
        try:
            self.process.kill()
        except OSError:
            pass

    def peak_kib(self) -> int:
        return int(self.usage.ru_maxrss) if self.usage is not None else 0


def server_env(state_dir: str, roots: str, backend: str, layout_python: str) -> dict:
    env = dict(os.environ)
    env.pop("READING_MCP_PDF_LAYOUT_PYTHON", None)
    env["READING_MCP_STATE_DIR"] = state_dir
    env["READING_MCP_LOCAL_ROOTS"] = roots
    env["READING_MCP_TELEMETRY"] = "false"
    env["READING_MCP_MAX_DOCUMENT_BYTES"] = "134217728"
    env["READING_MCP_MAX_NORMALIZED_CHARS"] = "200000000"
    env["READING_MCP_PARSE_TIMEOUT_SECS"] = "900"
    # OCR is opt-in and stays off: an image-only PDF must report ocr_required.
    env["READING_MCP_OCR_ENABLED"] = "false"
    if backend == "layout":
        env["READING_MCP_PDF_LAYOUT_PYTHON"] = layout_python
    return env


def cap_command(binary: str, cap: str | None, env: dict) -> list[str]:
    if cap is None:
        return [binary]
    setenv = []
    for key, value in env.items():
        if key.startswith("READING_MCP_"):
            setenv += ["--setenv", f"{key}={value}"]
    return [
        "sudo", "systemd-run", "--scope", "--quiet",
        "-p", f"MemoryMax={cap}", "-p", "MemorySwapMax=0",
        f"--uid={os.getuid()}", f"--gid={os.getgid()}",
        *setenv, "--", binary,
    ]


def cap_available() -> bool:
    if shutil.which("systemd-run") is None or shutil.which("sudo") is None:
        return False
    result = subprocess.run(
        ["sudo", "systemd-run", "--scope", "--quiet", "-p", "MemoryMax=256M", "--", "/bin/true"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=120,
    )
    return result.returncode == 0


def dir_bytes(path: str) -> int:
    total = 0
    for root, _, names in os.walk(path):
        for name in names:
            try:
                total += os.path.getsize(os.path.join(root, name))
            except OSError:
                pass
    return total


def disk_usage(state_dir: str) -> dict:
    total = dir_bytes(state_dir)
    sqlite = 0
    for name in os.listdir(state_dir):
        if name.startswith("reading-mcp.sqlite"):
            sqlite += os.path.getsize(os.path.join(state_dir, name))
    return {"total_bytes": total, "sqlite_bytes": sqlite}


def flatten_structure(nodes, out: dict) -> None:
    if not isinstance(nodes, list):
        return
    for node in nodes:
        if not isinstance(node, dict):
            continue
        section_id = node.get("section_id")
        if isinstance(section_id, str):
            out.setdefault(section_id, node)
        flatten_structure(node.get("children"), out)


def read_sections(server: Server, document_id: str) -> list[dict]:
    found: dict[str, dict] = {}
    cursor = None
    for _ in range(10000):
        arguments: dict = {"document_id": document_id, "max_nodes": 500}
        if cursor is not None:
            arguments["cursor"] = cursor
        data = payload(server.tool("get_document_structure", arguments)) or {}
        flatten_structure(data.get("sections"), found)
        cursor = data.get("next_cursor")
        if not cursor or data.get("complete") is True:
            break
    return sorted(found.values(), key=lambda node: node.get("body_order", 0))


def text_units(document_id: str, section_id: str, max_items: int, cursor=None) -> dict:
    arguments = {"document_id": document_id, "section_id": section_id, **TEXT_UNIT_BASE}
    arguments["max_items"] = max_items
    if cursor is not None:
        arguments["cursor"] = cursor
    return arguments


def percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, math.ceil(fraction * len(ordered)) - 1))
    return ordered[index]


def normalize_text(text: str) -> str:
    """Collapse whitespace after NFKC and the glyph fold map, so both sides compare equally."""
    folded = unicodedata.normalize("NFKC", text).translate(FOLD)
    return re.sub(r"\s+", " ", folded).strip()


def squash(text: str) -> str:
    return re.sub(r"\s+", "", normalize_text(text))


def page_count(path: str) -> int | None:
    try:
        import pymupdf

        with pymupdf.open(path) as document:
            return document.page_count
    except Exception:  # noqa: BLE001 - a broken file is a result, not a crash
        return None


def open_document(server: Server, source: str) -> tuple[dict, float, ProbeError | None]:
    start = time.perf_counter()
    try:
        result = server.tool("open_document", {"source": source})
    except ProbeError as exc:
        return {}, time.perf_counter() - start, exc
    return payload(result) or {}, time.perf_counter() - start, None


def sentence_totals(server: Server, document_id: str, sections: list[dict],
                    deadline: float) -> tuple[list[int], str | None]:
    totals: list[int] = []
    for section in sections:
        if time.monotonic() > deadline:
            return totals, "job deadline during the per-section sentence count"
        try:
            data = payload(
                server.tool("get_text_units", text_units(document_id, section["section_id"], 1))
            ) or {}
        except ProbeError as exc:
            return totals, str(exc)
        totals.append(int((data.get("stream") or {}).get("total_items", 0)))
    return totals, None


def read_sentences(server: Server, document_id: str, sections: list[dict], limit: int,
                   deadline: float) -> dict:
    """Read sentence by sentence (one item per call) and time every call."""
    texts: list[str] = []
    latencies: list[float] = []
    rss_samples: list[tuple[int, int]] = []
    errors: list[str] = []
    calls = 0
    capped = False
    partial = False
    started = time.monotonic()
    for section in sections:
        if len(texts) >= limit:
            capped = True
            break
        if time.monotonic() > deadline:
            partial = True
            break
        cursor = None
        while True:
            if len(texts) >= limit:
                capped = True
                break
            if time.monotonic() > deadline:
                partial = True
                break
            began = time.perf_counter()
            try:
                result = server.tool(
                    "get_text_units", text_units(document_id, section["section_id"], 1, cursor)
                )
            except ProbeError as exc:
                errors.append(str(exc))
                break
            latencies.append((time.perf_counter() - began) * 1000.0)
            data = payload(result) or {}
            items = data.get("items") or []
            calls += 1
            for item in items:
                texts.append(item.get("text") or "")
            if calls % 500 == 0:
                rss = peak_rss_kib(server.pid)
                if rss is not None:
                    rss_samples.append((calls, rss))
            cursor = data.get("next_cursor")
            if not cursor:
                break
    return {
        "texts": texts,
        "calls": calls,
        "latencies_ms": latencies,
        "rss_samples": rss_samples,
        "errors": errors,
        "capped": capped,
        "partial": partial,
        "read_s": time.monotonic() - started,
    }


def growth_mib_per_100k(samples: list[tuple[int, int]]) -> float | None:
    if len(samples) < 3:
        return None
    n = len(samples)
    mean_x = sum(c for c, _ in samples) / n
    mean_y = sum(r for _, r in samples) / n
    denominator = sum((c - mean_x) ** 2 for c, _ in samples)
    if denominator == 0:
        return None
    slope = sum((c - mean_x) * (r - mean_y) for c, r in samples) / denominator
    return slope * 100000 / 1024


def similarity(gt: str, got: str) -> dict:
    """Character-level similarity: mean difflib ratio over aligned 2,000-character windows."""
    ratios: list[float] = []
    weights: list[int] = []
    for offset in range(0, max(len(gt), len(got)), SIMILARITY_WINDOW):
        left = gt[offset:offset + SIMILARITY_WINDOW]
        right = got[offset:offset + SIMILARITY_WINDOW]
        if not left and not right:
            break
        ratios.append(difflib.SequenceMatcher(None, left, right).ratio())
        weights.append(max(len(left), len(right)))
    score = sum(r * w for r, w in zip(ratios, weights)) / sum(weights) if weights else 0.0
    return {"similarity": round(score, 4), "windows": len(ratios)}


def match_paragraphs(doc: str, paragraphs: list[str]) -> dict:
    """How many ground-truth paragraphs appear contiguously and in reading order."""
    position = 0
    exact = 0
    fuzzy = 0
    first_miss: str | None = None
    for paragraph in paragraphs:
        index = doc.find(paragraph, position)
        if index >= 0:
            exact += 1
            position = index + len(paragraph)
            continue
        anchor = paragraph[:24]
        index = doc.find(anchor, position) if anchor else -1
        matched = False
        while index >= 0:
            window = doc[index:index + len(paragraph)]
            matcher = difflib.SequenceMatcher(None, paragraph, window)
            if matcher.quick_ratio() >= 0.95 and matcher.ratio() >= FUZZY_MIN_RATIO:
                fuzzy += 1
                position = index + len(paragraph)
                matched = True
                break
            index = doc.find(anchor, index + 1)
        if not matched and first_miss is None:
            first_miss = paragraph[:80]
    total = len(paragraphs)
    return {
        "paragraphs": total,
        "paragraphs_exact": exact,
        "paragraphs_fuzzy": fuzzy,
        "order_score": round((exact + fuzzy) / total, 4) if total else None,
        "order_score_exact": round(exact / total, 4) if total else None,
        "first_miss": first_miss,
    }


def count_leaks(doc: str, meta: dict) -> dict:
    kinds = meta.get("noise_kinds") or {}
    counts = {}
    for kind in ("header", "footer", "footnote"):
        strings = kinds.get(kind) or []
        counts[kind] = sum(doc.count(squash(text)) for text in strings if text)
    counts["total"] = sum(counts.values())
    return counts


def diff_excerpt(gt: str, got: str, path: str, label: str) -> str | None:
    """Write the first divergent window of ground truth and extraction for review."""
    limit = min(len(gt), len(got))
    index = 0
    while index < limit and gt[index] == got[index]:
        index += 1
    start = max(0, index - 200)
    excerpt = [
        f"input: {label}",
        f"gt_chars: {len(gt)} extracted_chars: {len(got)} first_diff_at: {index}",
        "",
        "--- ground truth ---",
        gt[start:start + 600],
        "",
        "--- extracted ---",
        got[start:start + 600],
        "",
    ]
    with open(path, "w", encoding="utf-8") as handle:
        handle.write("\n".join(excerpt))
    return os.path.basename(path)


ABSTRACT_RE = re.compile(r"abstract", re.IGNORECASE)
# A numbered section heading, e.g. "1 Introduction", "2. Related Work", "III. RESULTS". Author
# affiliation numbers ("Woosuk Kwon 1 ,") are why the section name is required.
HEADING_RE = re.compile(
    r"(?:\b\d{1,2}\.?\s+|\b[IVX]{1,4}\.\s+)(?:introduction|related work|background|"
    r"preliminaries|method|methods|approach|experiments?|evaluation|results|discussion|"
    r"conclusion|motivation|system design)\b",
    re.IGNORECASE,
)


def real_paper_metrics(texts: list[str], errors: list[str]) -> dict:
    joined = " ".join(texts)
    head = joined[:1500]
    abstract_at = None
    match = ABSTRACT_RE.search(head)
    if match:
        abstract_at = match.start()
    heading_at = None
    heading_text = None
    match = HEADING_RE.search(head)
    if match:
        heading_at = match.start(0)
        heading_text = match.group(0).strip()
    return {
        "sentences_read": len(texts),
        "short_lt5": sum(1 for text in texts if len(text.strip()) < 5),
        "long_gt800": sum(1 for text in texts if len(text.strip()) > 800),
        "read_errors": len(errors),
        "first_error": errors[0][:200] if errors else None,
        "abstract_found": abstract_at is not None,
        "heading_found": heading_at is not None,
        "abstract_before_heading": (
            abstract_at is not None and heading_at is not None and abstract_at < heading_at
        ),
        "heading_text": heading_text,
        "head_excerpt": head[:400],
    }


def run_caps(binary: str, label: str, source: str, state_root: str, backend: str,
             layout_python: str, deadline: float) -> list[dict]:
    records = []
    if not cap_available():
        return [{"kind_record": "caps", "input": label, "backend": backend, "cap": cap,
                 "outcome": "unavailable", "note": "systemd-run scope not permitted"} for cap in CAPS]
    for cap in CAPS:
        if time.monotonic() > deadline:
            records.append({"kind_record": "caps", "input": label, "backend": backend, "cap": cap,
                            "outcome": "skipped", "note": "job deadline"})
            continue
        tag = f"cap{cap}-{label}-{backend}"
        state_dir = os.path.join(state_root, tag)
        shutil.rmtree(state_dir, ignore_errors=True)
        os.makedirs(state_dir)
        env = server_env(state_dir, os.path.dirname(os.path.abspath(source)), backend, layout_python)
        server = Server(cap_command(binary, cap, env), env, os.path.join(state_dir, "server.log"),
                        TOOL_TIMEOUT_S)
        record: dict = {"kind_record": "caps", "input": label, "backend": backend, "cap": cap,
                        "outcome": "error", "open_s": None, "peak_kib": 0, "error_code": None,
                        "error": None, "killed": False, "rc": None}
        try:
            server.initialize()
            after_open = None
            _, open_s, error = open_document(server, source)
            record["open_s"] = open_s
            after_open = peak_rss_kib(server.pid)
            record["peak_rss_after_open_kib"] = after_open
            if error is not None:
                record["error"] = str(error)
                record["error_code"] = error.code
            else:
                record["outcome"] = "ok"
        except ProbeError as exc:
            record["error"] = str(exc)
            record["error_code"] = exc.code
        finally:
            server.close()
        record["rc"] = server.rc
        record["peak_kib"] = server.peak_kib()
        if server.rc in KILLED_CODES:
            record["killed"] = True
            record["outcome"] = "oom"
        records.append(record)
    return records


def run_one(args) -> int:
    label, source = args.input.split("=", 1)
    backend = args.backend
    meta: dict = {}
    if args.meta and os.path.isfile(args.meta):
        with open(args.meta, encoding="utf-8") as handle:
            meta = json.load(handle)
    os.makedirs(args.state_root, exist_ok=True)
    os.makedirs(args.artifact_dir, exist_ok=True)
    deadline = time.monotonic() + args.deadline_minutes * 60
    records: list[dict] = []
    if not os.path.isfile(source):
        record = {"input": label, "backend": backend, "source": source, "source_bytes": 0,
                  "kind": meta.get("kind", "real"), "kind_record": "open", "open_s": None,
                  "error_code": "download_failed", "error": "input file is missing",
                  "outcome": "missing", "killed": False, "rc": None, "disk": None,
                  "peak_rss_after_open_kib": None, "peak_rss_total_kib": 0}
        with open(args.out, "a", encoding="utf-8") as handle:
            handle.write(json.dumps(record, ensure_ascii=False) + "\n")
        print(f"| {label} | {backend} | - | - | - | - | - | - | - | - | - | download_failed |")
        return 0
    base = {"input": label, "backend": backend, "source": source,
            "source_bytes": os.path.getsize(source), "kind": meta.get("kind", "real"),
            "lang": meta.get("lang", "?")}

    state_dir = os.path.join(args.state_root, f"open-{label}-{backend}")
    shutil.rmtree(state_dir, ignore_errors=True)
    os.makedirs(state_dir)
    env = server_env(state_dir, os.path.dirname(os.path.abspath(source)), backend,
                     args.layout_python)
    server = Server([args.binary], env, os.path.join(state_dir, "server.log"), TOOL_TIMEOUT_S)
    open_record = dict(base, kind_record="open", open_s=None, error_code=None, error=None,
                       killed=False, rc=None, peak_rss_after_open_kib=None,
                       peak_rss_total_kib=0, disk=None, outcome="error")
    structure_record = dict(base, kind_record="structure", sections=None, sentences=None,
                            pages=page_count(source), sentences_per_page=None, error=None)
    read_record = dict(base, kind_record="read", sentences_read=0, calls=0, capped=False,
                       partial=False, total_s=0.0, p50_ms=None, p95_ms=None, p99_ms=None,
                       max_ms=None, rss_growth_mib_per_100k=None, errors=0, first_error=None)
    quality_record = dict(base, kind_record="quality", similarity=None, order_score=None,
                          paragraphs=None, leaks=None, diff_excerpt=None, gt_chars=None,
                          extracted_chars=None)
    real_record = dict(base, kind_record="realpaper")
    document_id = None
    try:
        server.initialize()
        data, open_s, error = open_document(server, source)
        open_record["open_s"] = round(open_s, 3)
        open_record["peak_rss_after_open_kib"] = peak_rss_kib(server.pid)
        if error is not None:
            open_record["error"] = str(error)[:500]
            open_record["error_code"] = error.code
        else:
            open_record["outcome"] = "ok"
            document_id = data.get("document_id")
            open_record["sections_open"] = data.get("section_count")
            open_record["title"] = (data.get("title") or "")[:80]
            open_record["media_type"] = data.get("media_type")

        if document_id:
            sections = read_sections(server, document_id)
            totals, totals_error = sentence_totals(server, document_id, sections, deadline)
            sentences = sum(totals) if totals and not totals_error else None
            structure_record.update({
                "sections": len(sections), "sentences": sentences,
                "sentences_per_page": (
                    round(sentences / structure_record["pages"], 2)
                    if sentences is not None and structure_record["pages"] else None
                ),
                "error": totals_error,
            })
            if totals_error:
                read_record["first_error"] = totals_error

            # Memory caps first: a slow read must not eat the job deadline before they run.
            if args.caps:
                records += run_caps(args.binary, label, source, args.state_root, backend,
                                    args.layout_python, deadline)

            limit = REAL_PAPER_SENTENCES if meta.get("kind", "real") == "real" else READ_CAP_SENTENCES
            if time.monotonic() < deadline:
                read = read_sentences(server, document_id, sections, limit, deadline)
                latencies = read["latencies_ms"]
                read_record.update({
                    "sentences_read": len(read["texts"]), "calls": read["calls"],
                    "capped": read["capped"], "partial": read["partial"],
                    "total_s": round(read["read_s"], 3),
                    "p50_ms": percentile(latencies, 0.5), "p95_ms": percentile(latencies, 0.95),
                    "p99_ms": percentile(latencies, 0.99),
                    "max_ms": max(latencies) if latencies else None,
                    "rss_growth_mib_per_100k": growth_mib_per_100k(read["rss_samples"]),
                    "errors": len(read["errors"]),
                    "first_error": read["errors"][0][:200] if read["errors"] else None,
                    "sentence_cap": limit,
                })
                joined = normalize_text(" ".join(read["texts"]))
                if meta.get("gt") and meta.get("kind") in ("paper-2col", "long"):
                    gt_path = os.path.join(os.path.dirname(source), meta["gt"])
                    with open(gt_path, encoding="utf-8") as handle:
                        gt_raw = handle.read()
                    gt = normalize_text(gt_raw)
                    squashed_doc = squash(joined)
                    gt_truncated = bool(read["capped"] or read["partial"])
                    paragraphs = [squash(part) for part in re.split(r"\n\s*\n", gt_raw)
                                  if squash(part)]
                    if gt_truncated:
                        # Compare like with like: only the part of the ground truth that the
                        # sentence cap let us read, and only whole paragraphs.
                        budget = len(squashed_doc) + 400
                        kept: list[str] = []
                        used = 0
                        for paragraph in paragraphs:
                            if used + len(paragraph) > budget:
                                break
                            kept.append(paragraph)
                            used += len(paragraph)
                        paragraphs = kept
                        gt = gt[:len(joined) + 400]
                    matched = match_paragraphs(squashed_doc, paragraphs)
                    leaks = count_leaks(squashed_doc, meta)
                    excerpt_path = os.path.join(args.artifact_dir,
                                                f"diff-{label}-{backend}.txt")
                    excerpt = diff_excerpt(gt, joined, excerpt_path, f"{label} ({backend})")
                    quality_record.update({
                        **similarity(gt, joined),
                        "order_score": matched["order_score"],
                        "order_score_exact": matched["order_score_exact"],
                        "paragraphs": matched["paragraphs"],
                        "paragraphs_exact": matched["paragraphs_exact"],
                        "paragraphs_fuzzy": matched["paragraphs_fuzzy"],
                        "first_miss": matched["first_miss"],
                        "leaks": leaks,
                        "gt_chars": len(gt), "extracted_chars": len(joined),
                        "gt_truncated": gt_truncated,
                        "diff_excerpt": excerpt,
                    })
                else:
                    real_record.update(real_paper_metrics(read["texts"], read["errors"]))
            else:
                read_record["first_error"] = "job deadline before the read started"
    except ProbeError as exc:
        if open_record["open_s"] is None:
            open_record["error"] = str(exc)[:500]
            open_record["error_code"] = exc.code
        else:
            read_record["first_error"] = str(exc)[:200]
    finally:
        server.close()
    open_record["rc"] = server.rc
    open_record["peak_rss_total_kib"] = server.peak_kib()
    if server.rc in KILLED_CODES:
        open_record["killed"] = True
        open_record["outcome"] = "oom"
    open_record["disk"] = disk_usage(state_dir)
    records += [open_record, structure_record, read_record]
    if quality_record.get("similarity") is not None or quality_record.get("order_score") is not None:
        records.append(quality_record)
    if real_record.get("sentences_read"):
        records.append(real_record)

    # Reopen in the same state dir with a fresh process: shows whether the parsed cache works.
    reopen_record = dict(base, kind_record="reopen", reopen_s=None, error_code=None, error=None,
                         outcome="error", peak_rss_kib=0, rc=None)
    server2 = Server([args.binary], env, os.path.join(state_dir, "server-reopen.log"),
                     TOOL_TIMEOUT_S)
    try:
        server2.initialize()
        _, reopen_s, error = open_document(server2, source)
        reopen_record["reopen_s"] = round(reopen_s, 3)
        reopen_record["peak_rss_kib"] = peak_rss_kib(server2.pid)
        if error is not None:
            reopen_record["error"] = str(error)[:500]
            reopen_record["error_code"] = error.code
        else:
            reopen_record["outcome"] = "ok"
    except ProbeError as exc:
        reopen_record["error"] = str(exc)[:500]
        reopen_record["error_code"] = exc.code
    finally:
        server2.close()
    reopen_record["rc"] = server2.rc
    reopen_record["disk_after_reopen"] = disk_usage(state_dir)
    records.append(reopen_record)

    with open(args.out, "a", encoding="utf-8") as handle:
        for record in records:
            handle.write(json.dumps(record, ensure_ascii=False) + "\n")
    print(run_table(records))
    return 0


def fmt(value, digits: int = 2) -> str:
    if value is None:
        return "-"
    if isinstance(value, bool):
        return "yes" if value else "no"
    if isinstance(value, float):
        return f"{value:.{digits}f}"
    return str(value)


def run_table(records: list[dict]) -> str:
    lines = ["| Input | Backend | Open s | Peak RSS MiB | Disk MiB | Sections | Sentences | "
             "Pages | Sent/page | Order score | Leaks | Error |",
             "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    by_kind: dict[str, dict] = {}
    for record in records:
        by_kind.setdefault(record["kind_record"], record)
    opened = by_kind.get("open", {})
    structure = by_kind.get("structure", {})
    quality = by_kind.get("quality", {})
    disk = (opened.get("disk") or {}).get("total_bytes")
    error = opened.get("error_code") or opened.get("error") or "-"
    lines.append(
        f"| {opened.get('input')} | {opened.get('backend')} | {fmt(opened.get('open_s'))} | "
        f"{fmt((opened.get('peak_rss_after_open_kib') or 0) / 1024, 1)} | "
        f"{fmt((disk or 0) / MIB, 1)} | {fmt(structure.get('sections'))} | "
        f"{fmt(structure.get('sentences'))} | {fmt(structure.get('pages'))} | "
        f"{fmt(structure.get('sentences_per_page'))} | {fmt(quality.get('order_score'), 3)} | "
        f"{fmt((quality.get('leaks') or {}).get('total'))} | {str(error)[:80]} |"
    )
    reopen = by_kind.get("reopen", {})
    read = by_kind.get("read", {})
    lines += ["", "| Input | Backend | Reopen s | Read sentences | Read s | p50 ms | p95 ms | "
              "p99 ms | RSS growth MiB/100k | Read errors |",
              "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    lines.append(
        f"| {opened.get('input')} | {opened.get('backend')} | {fmt(reopen.get('reopen_s'))} | "
        f"{fmt(read.get('sentences_read'))} | {fmt(read.get('total_s'))} | "
        f"{fmt(read.get('p50_ms'), 1)} | {fmt(read.get('p95_ms'), 1)} | "
        f"{fmt(read.get('p99_ms'), 1)} | {fmt(read.get('rss_growth_mib_per_100k'))} | "
        f"{fmt(read.get('errors'))} |"
    )
    caps = [record for record in records if record["kind_record"] == "caps"]
    if caps:
        lines += ["", "| Input | Backend | Cap | Result | Open s | Peak RSS MiB | Error |",
                  "| --- | --- | --- | --- | --- | --- | --- |"]
        for record in caps:
            lines.append(
                f"| {record['input']} | {record['backend']} | {record['cap']} | "
                f"{record['outcome']} | {fmt(record.get('open_s'))} | "
                f"{fmt((record.get('peak_kib') or 0) / 1024, 1)} | "
                f"{str(record.get('error_code') or record.get('error') or record.get('note') or '-')[:60]} |"
            )
    return "\n".join(lines)


def load_records(jsonl_dir: str) -> list[dict]:
    records: list[dict] = []
    for name in sorted(os.listdir(jsonl_dir)):
        if not name.endswith(".jsonl"):
            continue
        with open(os.path.join(jsonl_dir, name), encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                try:
                    records.append(json.loads(line))
                except json.JSONDecodeError:
                    continue
    return records


def aggregate_mode(args) -> int:
    records = load_records(args.jsonl_dir)
    index: dict[tuple[str, str], dict[str, dict]] = {}
    for record in records:
        key = (record.get("input", "?"), record.get("backend", "?"))
        index.setdefault(key, {})[record.get("kind_record", "?")] = record

    inputs: list[str] = []
    for (label, _) in index:
        if label not in inputs:
            inputs.append(label)
    backends = [b for b in BACKENDS if any((label, b) in index for label in inputs)]

    main_rows = ["| Input | Backend | Open s | Peak RSS MiB | Disk MiB | Sections | Sentences | "
                 "Pages | Sent/page | Order score | Order (exact) | Leaks | Error |",
                 "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for label in inputs:
        for backend in backends:
            entry = index.get((label, backend), {})
            opened = entry.get("open", {})
            structure = entry.get("structure", {})
            quality = entry.get("quality", {})
            if not opened:
                main_rows.append(f"| {label} | {backend} | - | - | - | - | - | - | - | - | - | - | not run |")
                continue
            disk = (opened.get("disk") or {}).get("total_bytes")
            error = opened.get("error_code") or ("killed" if opened.get("killed") else None) or "-"
            main_rows.append(
                f"| {label} | {backend} | {fmt(opened.get('open_s'))} | "
                f"{fmt((opened.get('peak_rss_after_open_kib') or 0) / 1024, 1)} | "
                f"{fmt((disk or 0) / MIB, 1)} | {fmt(structure.get('sections'))} | "
                f"{fmt(structure.get('sentences'))} | {fmt(structure.get('pages'))} | "
                f"{fmt(structure.get('sentences_per_page'))} | {fmt(quality.get('order_score'), 3)} | "
                f"{fmt(quality.get('order_score_exact'), 3)} | "
                f"{fmt((quality.get('leaks') or {}).get('total'))} | {str(error)[:80]} |"
            )

    reopen_rows = ["| Input | Backend | First open s | Reopen s | Reopen outcome |",
                   "| --- | --- | --- | --- | --- |"]
    for label in inputs:
        for backend in backends:
            entry = index.get((label, backend), {})
            opened = entry.get("open", {})
            reopen = entry.get("reopen", {})
            reopen_rows.append(
                f"| {label} | {backend} | {fmt(opened.get('open_s'))} | "
                f"{fmt(reopen.get('reopen_s'))} | {reopen.get('outcome', 'not run')} |"
            )

    read_rows = ["| Input | Backend | Sentences read | Calls | Capped | Total s | p50 ms | p95 ms | "
                 "p99 ms | Max ms | RSS growth MiB/100k | Errors |",
                 "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for label in inputs:
        for backend in backends:
            read = index.get((label, backend), {}).get("read", {})
            if not read:
                continue
            read_rows.append(
                f"| {label} | {backend} | {fmt(read.get('sentences_read'))} | "
                f"{fmt(read.get('calls'))} | {fmt(read.get('capped'))} | "
                f"{fmt(read.get('total_s'))} | {fmt(read.get('p50_ms'), 1)} | "
                f"{fmt(read.get('p95_ms'), 1)} | {fmt(read.get('p99_ms'), 1)} | "
                f"{fmt(read.get('max_ms'), 1)} | {fmt(read.get('rss_growth_mib_per_100k'))} | "
                f"{fmt(read.get('errors'))} |"
            )

    quality_rows = ["| Input | Backend | GT chars | Extracted chars | Similarity | Paragraphs | "
                    "Order score | Leaks: header | footer | footnote | Diff excerpt |",
                    "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for label in inputs:
        for backend in backends:
            quality = index.get((label, backend), {}).get("quality", {})
            if not quality:
                continue
            leaks = quality.get("leaks") or {}
            quality_rows.append(
                f"| {label} | {backend} | {fmt(quality.get('gt_chars'))} | "
                f"{fmt(quality.get('extracted_chars'))} | {fmt(quality.get('similarity'), 3)} | "
                f"{fmt(quality.get('paragraphs'))} | {fmt(quality.get('order_score'), 3)} | "
                f"{fmt(leaks.get('header'))} | {fmt(leaks.get('footer'))} | "
                f"{fmt(leaks.get('footnote'))} | {fmt(quality.get('diff_excerpt'))} |"
            )

    real_rows = ["| Input | Backend | Sentences read | Short <5 chars | Long >800 chars | Read errors | "
                 "Abstract found | Heading found | Abstract before heading |",
                 "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for label in inputs:
        for backend in backends:
            real = index.get((label, backend), {}).get("realpaper", {})
            if not real:
                continue
            real_rows.append(
                f"| {label} | {backend} | {fmt(real.get('sentences_read'))} | "
                f"{fmt(real.get('short_lt5'))} | {fmt(real.get('long_gt800'))} | "
                f"{fmt(real.get('read_errors'))} | {fmt(real.get('abstract_found'))} | "
                f"{fmt(real.get('heading_found'))} | {fmt(real.get('abstract_before_heading'))} |"
            )

    cap_rows = ["| Input | Backend | Cap | Result | Open s | Peak RSS MiB | Error |",
                "| --- | --- | --- | --- | --- | --- | --- |"]
    for record in records:
        if record.get("kind_record") != "caps":
            continue
        cap_rows.append(
            f"| {record['input']} | {record['backend']} | {record['cap']} | {record['outcome']} | "
            f"{fmt(record.get('open_s'))} | {fmt((record.get('peak_kib') or 0) / 1024, 1)} | "
            f"{str(record.get('error_code') or record.get('error') or record.get('note') or '-')[:60]} |"
        )

    summary = [
        "# BENCH-D2 PDF open/read baseline", "",
        "## Open, structure and text quality", "", *main_rows, "",
        "## Reopen (parsed cache)", "", *reopen_rows, "",
        "## Sentence-by-sentence reads", "", *read_rows, "",
        "## Text quality against ground truth", "", *quality_rows, "",
        "## Real papers (first 200 sentences)", "", *real_rows, "",
        "## Memory caps", "", *cap_rows, "",
    ]
    text = "\n".join(summary)
    if args.markdown:
        with open(args.markdown, "w", encoding="utf-8") as handle:
            handle.write(text + "\n")
    if args.out:
        with open(args.out, "w", encoding="utf-8") as handle:
            json.dump(records, handle, ensure_ascii=False, indent=1)
    print(text)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="BENCH-D2 PDF baseline probe.")
    parser.add_argument("--mode", required=True, choices=["run", "aggregate"])
    parser.add_argument("--binary", default="", help="release reading-mcp binary (run mode)")
    parser.add_argument("--input", default="", help="label=path (run mode)")
    parser.add_argument("--meta", default="", help="generator meta JSON for this input")
    parser.add_argument("--backend", default="default", choices=list(BACKENDS))
    parser.add_argument("--layout-python", default="",
                        help="interpreter of the pinned layout environment (layout backend)")
    parser.add_argument("--state-root", default="pdf-state")
    parser.add_argument("--artifact-dir", default="pdf-artifacts")
    parser.add_argument("--out", required=True)
    parser.add_argument("--markdown", default="")
    parser.add_argument("--jsonl-dir", default="")
    parser.add_argument("--caps", action="store_true")
    parser.add_argument("--deadline-minutes", type=float, default=150.0)
    args = parser.parse_args()
    if args.mode == "run":
        if not args.binary or not args.input:
            parser.error("run mode needs --binary and --input label=path")
        if args.backend == "layout" and not args.layout_python:
            parser.error("layout backend needs --layout-python")
        return run_one(args)
    if not args.jsonl_dir:
        parser.error("aggregate mode needs --jsonl-dir")
    return aggregate_mode(args)


if __name__ == "__main__":
    sys.exit(main())
