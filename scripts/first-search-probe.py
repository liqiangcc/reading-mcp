#!/usr/bin/env python3
"""Measure the first search after `open` (SEARCH-PREWARM).

The probe reuses the MCP stdio transport of `doc-read-probe.py` and answers three questions, one
per `--shape`, each on fresh servers with fresh state directories:

- `first-search`: open the document, wait `--wait-seconds` (1.5 s by default), then time the first
  `search_document` (and, when the server exposes the v2 tools, the first v2 `search` after a v2
  `open`). This is the shape a model produces: the first search comes seconds after `open` returns.
- `immediate-search`: time the first `search_document` right after `open` returns, with no wait. The
  worst case: warming must never make it slower than not warming.
- `foreground-reads`: time `--calls` `get_text_units` calls right after `open`, while the warm of the
  scan text cache runs. The mean must not degrade against the same run with warming off.

Every repetition also times the `open` that produces the result, a second `open` of the same source
(the open-result hit path), samples the server's peak RSS, and collects the `scan_prewarm` telemetry
events the server wrote, so the background warm's own duration is reported next to the latencies.

Standard library only. This script never builds anything: pass an existing release binary.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import statistics
import sys
import tempfile
import time

PROBE_MODULE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "doc-read-probe.py")
SHAPES = ("first-search", "immediate-search", "foreground-reads")


def load_probe_module():
    """The transport, RSS sampler and tool helpers of `doc-read-probe.py`."""
    spec = importlib.util.spec_from_file_location("doc_read_probe", PROBE_MODULE)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {PROBE_MODULE}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def summarize(values: list[float], scale: float = 1.0) -> dict:
    """Median, p95 and max of `values`, scaled (ms for seconds, KiB stays KiB)."""
    if not values:
        return {}
    ordered = sorted(values)
    return {
        "calls": len(ordered),
        "median": round(scale * statistics.median(ordered), 3),
        "p95": round(scale * ordered[min(len(ordered) - 1, int(0.95 * len(ordered)))], 3),
        "max": round(scale * ordered[-1], 3),
        "mean": round(scale * statistics.fmean(ordered), 3),
    }


def run_once(probe, args: argparse.Namespace, binary: str, document: str) -> dict:
    scratch_root = os.path.join(os.getcwd(), ".tmp")
    os.makedirs(scratch_root, exist_ok=True)
    state_dir = tempfile.mkdtemp(prefix="first-search-state-", dir=scratch_root)
    env = dict(os.environ)
    env["READING_MCP_STATE_DIR"] = state_dir
    env["READING_MCP_LOCAL_ROOTS"] = os.path.dirname(document)
    env["READING_MCP_MAX_DOCUMENT_BYTES"] = "134217728"
    env["READING_MCP_MAX_NORMALIZED_CHARS"] = "200000000"
    env["READING_MCP_PARSE_TIMEOUT_SECS"] = "900"
    env["READING_MCP_TOOLS"] = "both"
    env["READING_MCP_TELEMETRY"] = "true"
    for item in args.env:
        key, separator, value = item.partition("=")
        if not separator or not key:
            raise probe.ProbeError(f"--env expects KEY=VALUE, got {item!r}")
        env[key] = value

    log_path = os.path.join(scratch_root, f"first-search-{args.label}-{args.side}.log")
    server = probe.Server([binary], env, log_path)
    record: dict = {
        "shape": args.shape,
        "query": args.query,
        "open_seconds": None,
        "reopen_seconds": None,
        "search_ms": None,
        "v2_search_ms": None,
        "hits": None,
        "v2_hits": None,
        "read_ms": [],
        "rss_after_open_kib": None,
        "rss_after_search_kib": None,
        "peak_rss_kib": None,
        "warm": [],
        "skipped": [],
    }
    sampler = probe.RssSampler(server.process.pid)
    sampler.__enter__()
    try:
        server.initialize()
        tool_names = server.tool_names()

        started = time.monotonic()
        payload = probe.tool_payload(server.tool("open_document", {"source": document}))
        record["open_seconds"] = round(time.monotonic() - started, 4)
        record["rss_after_open_kib"] = probe.proc_rss_kib(server.process.pid)
        if not isinstance(payload, dict) or not isinstance(payload.get("document_id"), str):
            raise probe.ProbeError("open_document did not return a document_id")
        document_id = payload["document_id"]

        if args.shape == "foreground-reads":
            sections = probe.read_structure(server, document_id)
            if not sections:
                raise probe.ProbeError("document has no sections")
            section_id = sections[min(len(sections) - 1, len(sections) // 2)]["section_id"]
            arguments = {
                "document_id": document_id,
                "section_id": section_id,
                "requested_kind": "sentence",
                "direction": "forward",
                "coverage_policy": "preserve_source",
                "max_items": 1,
            }
            for _ in range(args.calls):
                elapsed, _, _ = probe.timed_tool(server, "get_text_units", arguments, False)
                record["read_ms"].append(round(1000.0 * elapsed, 3))
        else:
            if args.shape == "first-search":
                time.sleep(args.wait_seconds)
            elapsed, _, result = probe.timed_tool(
                server,
                "search_document",
                {"document_id": document_id, "query": args.query, "limit": 10},
                False,
            )
            record["search_ms"] = round(1000.0 * elapsed, 3)
            hits = probe.tool_payload(result)
            record["hits"] = len(hits.get("hits", [])) if isinstance(hits, dict) else None
            record["rss_after_search_kib"] = probe.proc_rss_kib(server.process.pid)

            # The v2 search is timed only in the waited shape: opening the v2 handle first costs a
            # call, which would give the warm time in the immediate shape.
            if args.shape == "first-search" and "open" in tool_names and "search" in tool_names:
                opened_v2 = probe.tool_payload(server.tool("open", {"source": document}))
                handle = opened_v2.get("doc") if isinstance(opened_v2, dict) else None
                if isinstance(handle, str):
                    elapsed, _, result = probe.timed_tool(
                        server,
                        "search",
                        {"doc": handle, "query": args.query, "limit": 10},
                        False,
                    )
                    record["v2_search_ms"] = round(1000.0 * elapsed, 3)
                    hits = probe.tool_payload(result)
                    record["v2_hits"] = (
                        len(hits.get("hits", [])) if isinstance(hits, dict) else None
                    )
                else:
                    record["skipped"].append("v2 search: v2 open returned no handle")
            elif args.shape == "immediate-search":
                record["skipped"].append("v2 search: not timed right after open by design")

        # The second open of the same source takes the open-result path.
        started = time.monotonic()
        server.tool("open_document", {"source": document})
        record["reopen_seconds"] = round(time.monotonic() - started, 4)

        record["warm"] = [
            {
                "sections": event.get("sections"),
                "duration_ms": event.get("duration_ms"),
                "success": event.get("success"),
            }
            for event in server.log_since(0)
            if event.get("event") == "scan_prewarm"
        ]
    finally:
        record["peak_rss_kib"] = sampler.peak
        sampler.__exit__()
        server.close()
    return record


def probe(args: argparse.Namespace) -> int:
    probe_module = load_probe_module()
    document = os.path.abspath(args.document)
    if not os.path.isfile(document):
        raise probe_module.ProbeError(f"file not found: {document}")
    binary = os.path.abspath(args.binary)
    if not os.path.isfile(binary):
        raise probe_module.ProbeError(f"binary not found: {binary}")
    if args.repeats <= 0:
        raise probe_module.ProbeError("--repeats must be positive")
    if args.shape not in SHAPES:
        raise probe_module.ProbeError(f"--shape must be one of {', '.join(SHAPES)}")
    if args.shape == "foreground-reads" and args.calls <= 0:
        raise probe_module.ProbeError("--calls must be positive")

    runs = []
    for repeat in range(args.repeats):
        record = run_once(probe_module, args, binary, document)
        record["repeat"] = repeat
        runs.append(record)
        print(
            "{label} side={side} shape={shape} repeat={repeat}: open_s={open} reopen_s={reopen} "
            "search_ms={search} v2_search_ms={v2} peak_rss_kib={peak} warm={warm}".format(
                label=args.label,
                side=args.side,
                shape=args.shape,
                repeat=repeat,
                open=record["open_seconds"],
                reopen=record["reopen_seconds"],
                search=record["search_ms"],
                v2=record["v2_search_ms"],
                peak=record["peak_rss_kib"],
                warm=[(item["duration_ms"], item["sections"]) for item in record["warm"]],
            ),
            flush=True,
        )

    def collect(key: str) -> list[float]:
        return [run[key] for run in runs if isinstance(run.get(key), (int, float))]

    read_ms = [value for run in runs for value in run["read_ms"]]
    warm_ms = [
        float(item["duration_ms"])
        for run in runs
        for item in run["warm"]
        if isinstance(item.get("duration_ms"), (int, float))
    ]
    warm_sections = [
        item["sections"] for run in runs for item in run["warm"] if item.get("sections") is not None
    ]
    result = {
        "label": args.label,
        "side": args.side,
        "shape": args.shape,
        "document": document,
        "binary": binary,
        "query": args.query,
        "repeats": args.repeats,
        "calls": args.calls,
        "wait_seconds": args.wait_seconds if args.shape == "first-search" else 0.0,
        "env": args.env,
        "search_ms": summarize(collect("search_ms")),
        "v2_search_ms": summarize(collect("v2_search_ms")),
        "open_seconds": summarize(collect("open_seconds")),
        "reopen_seconds": summarize(collect("reopen_seconds")),
        "peak_rss_kib": summarize(collect("peak_rss_kib")),
        "rss_after_open_kib": summarize(collect("rss_after_open_kib")),
        "rss_after_search_kib": summarize(collect("rss_after_search_kib")),
        "read_ms": summarize(read_ms),
        "warm_ms": summarize(warm_ms),
        "warm_sections": sorted({int(value) for value in warm_sections}),
        "hits": sorted({run["hits"] for run in runs if run["hits"] is not None}),
        "v2_hits": sorted({run["v2_hits"] for run in runs if run["v2_hits"] is not None}),
        "skipped": sorted({item for run in runs for item in run["skipped"]}),
        "runs": runs,
    }
    out_path = os.path.abspath(args.out)
    os.makedirs(os.path.dirname(out_path) or ".", exist_ok=True)
    with open(out_path, "w", encoding="utf-8") as handle:
        json.dump(result, handle, indent=2)
        handle.write("\n")

    print(
        "{label} side={side} shape={shape}: search_ms median={search} v2_search_ms median={v2} "
        "open_s median={open} reopen_s median={reopen} peak_rss_kib={peak} "
        "read_ms median={read} warm_ms median={warm} warm_sections={sections}".format(
            label=args.label,
            side=args.side,
            shape=args.shape,
            search=result["search_ms"].get("median"),
            v2=result["v2_search_ms"].get("median"),
            open=result["open_seconds"].get("median"),
            reopen=result["reopen_seconds"].get("median"),
            peak=result["peak_rss_kib"].get("max"),
            read=result["read_ms"].get("median"),
            warm=result["warm_ms"].get("median"),
            sections=result["warm_sections"],
        ),
        flush=True,
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Measure the first search after open, warming the scan text cache on or off."
    )
    parser.add_argument("--binary", required=True, help="reading-mcp release binary")
    parser.add_argument("--document", required=True, help="document to open")
    parser.add_argument("--label", required=True, help="input label for the report")
    parser.add_argument(
        "--side", default="prewarm-on", help="side label for the report (prewarm-on or prewarm-off)"
    )
    parser.add_argument("--query", required=True, help="a term from the middle of the document")
    parser.add_argument(
        "--shape",
        default="first-search",
        choices=SHAPES,
        help="which latency to measure",
    )
    parser.add_argument("--repeats", type=int, default=10, help="fresh servers per input")
    parser.add_argument(
        "--wait-seconds",
        type=float,
        default=1.5,
        help="seconds between the open returning and the first search (first-search shape)",
    )
    parser.add_argument(
        "--calls", type=int, default=200, help="get_text_units calls (foreground-reads shape)"
    )
    parser.add_argument("--env", action="append", default=[], metavar="KEY=VALUE")
    parser.add_argument("--out", required=True, help="JSON result path")
    args = parser.parse_args()
    probe_module = load_probe_module()
    try:
        return probe(args)
    except probe_module.ProbeError as exc:
        print(f"first-search-probe: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
