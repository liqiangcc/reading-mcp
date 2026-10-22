#!/usr/bin/env python3
"""Live check that a paper's landing-page link reads the paper (PDF-3).

The probe speaks newline-delimited JSON-RPC MCP over stdio against a release binary built from
this branch, with ``READING_MCP_TOOLS=v1``.  For every pinned link it calls ``open_document`` and
then reads the first 200 sentences of the opened document (``get_document_structure`` for the root
section, then ``read_document`` on it).  Every non-control link is compared with the same paper
opened through its direct ``.pdf`` link:

* ``document_id`` — the id derives from the final source plus the content hash, so a resolved
  landing-page link and the direct PDF link of the same paper must give the *same* id;
* the first 200 sentences — the same text, whitespace-normalised.

``open`` does not expose which rule fired (the v2 tool budget forbids a new field), so the rule is
reported as the pinned expectation for that link plus the evidence the probe can see: the URL
pattern, whether the landing page's first 256 KiB carry a ``citation_pdf_url`` meta tag (the
generic rule), and whether the id equals the control's (which only happens when the PDF was read).

Sites that are unreachable from the runner (or that answer with a bot challenge) are recorded as
``skipped`` with the reason and never fail the job; only a missing binary or a broken protocol
does.  The script also measures the ``tools/list`` footprint of both tool sets, because PDF-3 must
not grow it.

Standard library only.  This script never builds anything: pass an existing release binary.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import re
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

REQUEST_TIMEOUT = 600.0
META_SCAN_LIMIT = 256 * 1024
SENTENCE_LIMIT = 200
INITIALIZE_MESSAGE = {
    "protocolVersion": "2025-06-18",
    "capabilities": {},
    "clientInfo": {"name": "url-resolve-probe", "version": "1"},
}

# Pinned links.  ``control`` is the same paper opened through its direct PDF link.
ENTRIES = [
    {
        "label": "arxiv_abs",
        "url": "https://arxiv.org/abs/2309.06180",
        "rule": "arxiv_abs",
        "control": "arxiv_pdf",
    },
    {
        "label": "arxiv_abs_v1",
        "url": "https://arxiv.org/abs/2309.06180v1",
        "rule": "arxiv_abs",
        "control": "arxiv_pdf_v1",
    },
    {
        "label": "doi_arxiv",
        "url": "https://doi.org/10.48550/arXiv.2309.06180",
        "rule": "arxiv_abs (after the doi.org redirect chain)",
        "control": "arxiv_pdf",
    },
    {
        "label": "arxiv_pdf",
        "url": "https://arxiv.org/pdf/2309.06180",
        "rule": "control",
        "control": None,
    },
    {
        "label": "arxiv_pdf_v1",
        "url": "https://arxiv.org/pdf/2309.06180v1",
        "rule": "control",
        "control": None,
    },
    {
        # Pinned from the ICLR 2024 virtual site (iclr.cc/static/virtual/data).  OpenReview puts a
        # bot challenge in front of most runners; the entry is then recorded as skipped.
        "label": "openreview_forum",
        "url": "https://openreview.net/forum?id=IL71c1z7et",
        "rule": "openreview_forum",
        "control_url": "https://openreview.net/pdf?id=IL71c1z7et",
    },
    {
        # ACL Anthology: materials published in or after 2016 are CC BY 4.0 (site footer).
        "label": "acl_anthology",
        "url": "https://aclanthology.org/2023.acl-long.1/",
        "rule": "acl_anthology",
        "control_url": "https://aclanthology.org/2023.acl-long.1.pdf",
    },
    {
        # PLOS ONE, CC BY 4.0 ("Creative Commons Attribution License"), ships citation_pdf_url.
        "label": "plos_doi",
        "url": "https://doi.org/10.1371/journal.pone.0000308",
        "rule": "citation_pdf_url",
        "control_url": (
            "https://journals.plos.org/plosone/article/file"
            "?id=10.1371/journal.pone.0000308&type=printable"
        ),
    },
]

USER_AGENT = "reading-mcp-url-resolve-probe/1"


class ProbeError(RuntimeError):
    """A transport, protocol, tool or deadline failure."""


def compact_json(value) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


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


class Server:
    """One stdio MCP server process."""

    def __init__(self, binary: str, environment: dict, log_path: str, timeout: float) -> None:
        self.timeout = timeout
        self.log = open(log_path, "wb")
        self.process = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.log,
            env=environment,
        )
        self.responses: queue.Queue = queue.Queue()
        self.next_id = 0
        self.reader = threading.Thread(
            target=pump_stdout, args=(self.process.stdout, self.responses), daemon=True
        )
        self.reader.start()

    def request(self, method: str, params: dict, timeout: float | None = None) -> dict:
        self.next_id += 1
        awaited = self.next_id
        send(
            self.process,
            {"jsonrpc": "2.0", "id": awaited, "method": method, "params": params},
        )
        deadline = time.monotonic() + (self.timeout if timeout is None else timeout)
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise ProbeError(f"{method} timed out")
            try:
                response = self.responses.get(timeout=remaining)
            except queue.Empty:
                raise ProbeError(f"{method} timed out") from None
            if response is None:
                raise ProbeError(f"{method}: server closed stdout")
            if response.get("id") != awaited:
                continue
            return response

    def notify(self, method: str) -> None:
        send(self.process, {"jsonrpc": "2.0", "method": method})

    def initialize(self, timeout: float | None = None) -> None:
        response = self.request("initialize", INITIALIZE_MESSAGE, timeout)
        error = response.get("error")
        if error is not None:
            raise ProbeError(f"initialize: {error}")
        self.notify("notifications/initialized")

    def tools_list(self, timeout: float | None = None) -> dict:
        response = self.request("tools/list", {}, timeout)
        error = response.get("error")
        if error is not None:
            raise ProbeError(f"tools/list: {error}")
        result = response.get("result")
        if not isinstance(result, dict):
            raise ProbeError("tools/list did not return an object")
        return result

    def tool(self, name: str, arguments: dict, timeout: float | None = None) -> dict:
        response = self.request(
            "tools/call", {"name": name, "arguments": arguments}, timeout
        )
        error = response.get("error")
        if error is not None:
            raise ProbeError(f"{name}: {error}")
        result = response.get("result")
        if not isinstance(result, dict):
            raise ProbeError(f"{name}: result is not an object")
        if result.get("isError"):
            raise ProbeError(f"{name}: {tool_text(result)[:400]}")
        return result

    def close(self) -> None:
        try:
            if self.process.stdin is not None:
                self.process.stdin.close()
        except Exception:  # noqa: BLE001 - closing is best effort
            pass
        try:
            self.process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.process.kill()
        self.log.close()


def tool_text(result: dict) -> str:
    for block in result.get("content") or []:
        if isinstance(block, dict) and block.get("type") == "text":
            return block.get("text") or ""
    return ""


def tool_json(result: dict) -> dict:
    structured = result.get("structuredContent")
    if isinstance(structured, dict):
        return structured
    text = tool_text(result)
    try:
        value = json.loads(text)
    except json.JSONDecodeError:
        raise ProbeError(f"tool result is not JSON: {text[:200]}") from None
    if not isinstance(value, dict):
        raise ProbeError("tool result is not a JSON object")
    return value


SENTENCE_END = re.compile(r"(?<=[.!?])\s+")


def first_sentences(text: str, limit: int = SENTENCE_LIMIT) -> tuple[int, str]:
    """The first `limit` sentences of a read, and how many sentences the read carried."""
    body = " ".join(
        line.strip()
        for line in text.splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    )
    body = " ".join(body.split())
    sentences = [piece for piece in SENTENCE_END.split(body) if piece]
    return len(sentences), " ".join(sentences[:limit])


def open_and_sample(
    server: Server, url: str, timeout: float
) -> dict:
    """Open one URL, then read the first sentences of its root section."""
    started = time.monotonic()
    opened = tool_json(
        server.tool("open_document", {"source": url, "force_refresh": False}, timeout)
    )
    seconds = time.monotonic() - started
    document_id = opened.get("document_id")
    if not document_id:
        raise ProbeError("open_document returned no document_id")

    structure = tool_json(
        server.tool(
            "get_document_structure",
            {"document_id": document_id, "max_depth": 1, "max_nodes": 1},
            timeout,
        )
    )
    sections = structure.get("sections") or []
    section_id = sections[0]["section_id"] if sections else None
    read = tool_json(
        server.tool(
            "read_document",
            {
                "document_id": document_id,
                "section_id": section_id,
                "max_chars": 40000,
            },
            timeout,
        )
    )
    sentences, sample = first_sentences(read.get("content") or "")
    return {
        "document_id": document_id,
        "title": opened.get("title"),
        "media_type": opened.get("media_type"),
        "section_count": opened.get("section_count"),
        "seconds": round(seconds, 2),
        "sentences": sentences,
        "sample": sample,
        "content_hash": opened.get("content_hash"),
    }


def fetch_head(url: str, timeout: float = 60.0) -> tuple[int | None, bytes, str]:
    """The status, the first 256 KiB of the body, and the final URL of one GET."""
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            body = response.read(META_SCAN_LIMIT)
            return response.status, body, response.geturl()
    except urllib.error.HTTPError as error:
        return error.code, b"", url
    except Exception as error:  # noqa: BLE001 - any transport failure means unreachable
        return None, b"", f"{type(error).__name__}: {error}"


def preflight(entry: dict) -> str | None:
    """`None` when the landing page is reachable, else the reason to skip."""
    status, body, final = fetch_head(entry["url"])
    if status is None:
        return f"unreachable: {final}"
    if status != 200:
        return f"HTTP {status} from {final}"
    if b"citation_pdf_url" in body:
        entry["meta_present"] = True
    return None


def measure_tools_list(binary: str, state_dir: str, toolset: str, timeout: float) -> dict:
    environment = server_environment(state_dir, os.path.join(state_dir, f"tools-{toolset}"))
    environment["READING_MCP_TOOLS"] = toolset
    server = None
    try:
        server = Server(
            binary,
            environment,
            os.path.join(state_dir, f"tools-{toolset}.log"),
            timeout,
        )
        server.initialize(timeout)
        result = server.tools_list(timeout)
        payload = compact_json(result)
        return {"tools": len(result.get("tools") or []), "bytes": len(payload.encode("utf-8"))}
    except ProbeError as error:
        return {"tools": 0, "bytes": 0, "error": str(error)}
    finally:
        if server is not None:
            server.close()


def server_environment(state_dir: str, state: str) -> dict:
    environment = dict(os.environ)
    environment.update(
        {
            "READING_MCP_STATE_DIR": state,
            "READING_MCP_TELEMETRY": "false",
            "READING_MCP_ALLOW_HTTP": "false",
            "READING_MCP_LOCAL_ROOTS": state_dir,
            "READING_MCP_TOOLS": "v1",
        }
    )
    return environment


def run_probe(binary: str, state_dir: str, timeout: float) -> dict:
    state = os.path.join(state_dir, "links")
    os.makedirs(state, exist_ok=True)
    server = Server(
        binary,
        server_environment(state_dir, state),
        os.path.join(state_dir, "links.log"),
        timeout,
    )
    samples: dict[str, dict] = {}
    records: list[dict] = []
    try:
        server.initialize(timeout)
        for entry in ENTRIES:
            record = {
                "label": entry["label"],
                "url": entry["url"],
                "rule": entry["rule"],
                "control": entry.get("control") or entry.get("control_url"),
                "outcome": "ok",
                "reason": None,
                "meta_present": False,
                "id_equal": None,
                "text_equal": None,
                "seconds": None,
                "sentences": None,
                "document_id": None,
                "title": None,
                "media_type": None,
            }
            records.append(record)
            reason = preflight(entry)
            if reason is not None:
                record["outcome"] = "skipped"
                record["reason"] = reason
                continue
            record["meta_present"] = bool(entry.get("meta_present"))
            try:
                sample = open_and_sample(server, entry["url"], timeout)
            except ProbeError as error:
                record["outcome"] = "failed"
                record["reason"] = str(error)
                continue
            samples[entry["label"]] = sample
            record.update(
                {
                    "document_id": sample["document_id"],
                    "title": sample["title"],
                    "media_type": sample["media_type"],
                    "seconds": sample["seconds"],
                    "sentences": sample["sentences"],
                }
            )

        # The direct PDF link of every paper, opened the same way.
        for entry in ENTRIES:
            control_url = entry.get("control_url")
            if not control_url or control_url in samples:
                continue
            try:
                samples[control_url] = open_and_sample(server, control_url, timeout)
            except ProbeError as error:
                samples[control_url] = {"error": str(error)}
    finally:
        server.close()

    for record, entry in zip(records, ENTRIES):
        control = entry.get("control") or entry.get("control_url")
        if not control or record["outcome"] != "ok":
            continue
        reference = samples.get(control)
        if not reference:
            record["outcome"] = "skipped"
            record["reason"] = "control PDF link was skipped"
            continue
        if "error" in reference:
            record["outcome"] = "failed"
            record["reason"] = f"control {control}: {reference['error']}"
            continue
        own = samples.get(record["label"]) or {}
        record["id_equal"] = record["document_id"] == reference["document_id"]
        record["text_equal"] = own.get("sample") == reference.get("sample")

    return {"links": records}


def render_markdown(report: dict) -> str:
    lines = [
        "| link | outcome | rule | time (s) | document id | id = control | text = control | sentences |",
        "| --- | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for record in report["links"]:
        document_id = record["document_id"] or ""
        short = document_id.replace("doc:sha256:", "")[:12]
        id_equal = {True: "yes", False: "no", None: "n/a"}[record["id_equal"]]
        text_equal = {True: "yes", False: "no", None: "n/a"}[record["text_equal"]]
        seconds = "" if record["seconds"] is None else f"{record['seconds']:.1f}"
        sentences = "" if record["sentences"] is None else str(record["sentences"])
        outcome = record["outcome"]
        if record["reason"]:
            outcome = f"{outcome}: {record['reason']}"
        lines.append(
            f"| {record['label']} | {outcome} | {record['rule']} | {seconds} | {short} "
            f"| {id_equal} | {text_equal} | {sentences} |"
        )
    tools = report.get("tools_list", {})
    for toolset in ("v1", "v2"):
        entry = tools.get(toolset) or {}
        lines.append(
            f"\n`tools/list` {toolset}: {entry.get('tools', 0)} tools, "
            f"{entry.get('bytes', 0)} bytes"
        )
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, help="release reading-mcp binary")
    parser.add_argument("--state-dir", required=True)
    parser.add_argument("--timeout", type=float, default=REQUEST_TIMEOUT)
    parser.add_argument("--out", help="write the JSON report here")
    parser.add_argument("--md", help="write the Markdown table here")
    args = parser.parse_args(argv)

    binary = args.binary
    if not os.path.isfile(binary) or not os.access(binary, os.X_OK):
        print(f"::error::no executable binary at {binary}")
        return 2

    state_dir = os.path.abspath(args.state_dir)
    os.makedirs(state_dir, exist_ok=True)

    report = run_probe(binary, state_dir, args.timeout)
    report["tools_list"] = {
        toolset: measure_tools_list(binary, state_dir, toolset, args.timeout)
        for toolset in ("v1", "v2")
    }
    markdown = render_markdown(report)

    print(markdown)
    for record in report["links"]:
        if record["outcome"] != "ok":
            print(f"::warning::{record['label']}: {record['outcome']} {record['reason'] or ''}")
    if args.out:
        with open(args.out, "w", encoding="utf-8") as handle:
            json.dump(report, handle, indent=2, sort_keys=True)
    if args.md:
        with open(args.md, "w", encoding="utf-8") as handle:
            handle.write(markdown + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
