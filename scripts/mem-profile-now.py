#!/usr/bin/env python3
"""Measure where `open_document`'s peak memory goes, now, itemised (MEM-2a).

This script is measurement only: it drives the release server over newline-delimited JSON-RPC and
reads the kernel's own counters, the server's telemetry events and heaptrack's heap profile. It
never changes what the server does.

Subcommands:

`generate --out DIR` writes the deterministic synthetic inputs this task needs next to the books
the workflow downloads: `cjk-64mib.txt` (one 64 MiB section of Chinese prose) and two 300-page
text PDFs, `text-long-en-300.pdf` and `text-long-cjk-300.pdf`. PDF generation needs PyMuPDF
(`pip install pymupdf`); it is imported lazily so the other subcommands do not need it.

`profile --binary B --out DIR --mode rss|heap --inputs label=path[,...]` runs each input once in a
fresh server process on a fresh state directory:

- `rss` samples `/proc/<pid>/status` (`VmRSS`, `VmHWM`) every `--sample-interval` seconds from
  process start through the first `open_document` return, then through a `--settle-seconds` wait
  (the prewarm and other background work), then one `get_document_structure`, one
  `get_text_units` and one `search_document`, then a second `open_document` of the same source in
  the same process. The server's telemetry lines (`READING_MCP_TELEMETRY=1`) are timestamped as
  they arrive, so each event's `duration_ms` gives the phase's `[end-duration, end]` interval and
  the RSS samples align with it. Writes `<label>.timeline.json` and `<label>.sqlite.json`.
- `heap` runs the same open under `heaptrack`, prints its global heap peak with
  `heaptrack_print --print-peaks`, and buckets the allocations live at that peak by the nearest
  project frame. `--malloc-arena-max N` sets `MALLOC_ARENA_MAX` for the server. Writes
  `<label>.<variant>.heap.json` and `<label>.<variant>.peaks.txt`.

`report --dir DIR --out FILE.md` aggregates every `*.json` the profile runs wrote into the tables
this task asks for: RSS timeline per phase, itemised heap peak, top allocation sites, retained
after open, and the 64 MiB and 宋史 itemisations against their bounds.

Standard library only. Never uses `/tmp`: scratch stays under `--out`.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import queue
import re
import shutil
import subprocess
import sys
import threading
import time

MIB = 1024 * 1024
MIB_F = float(MIB)


_MODULES: dict[str, object] = {}


def load_module(name: str, filename: str):
    """Import a hyphenated sibling script as a module, once."""
    if name in _MODULES:
        return _MODULES[name]
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), filename)
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    _MODULES[name] = module
    return module


# --------------------------------------------------------------------------------------------------
# Inputs
# --------------------------------------------------------------------------------------------------


def generate_inputs(out: str, song_shi: str | None, cjk_font: str | None, pdf_pages: int) -> dict:
    """Write the synthetic inputs this task needs; return a manifest of every file."""
    memory_profile = load_module("memory_profile", "memory-profile.py")
    stress = load_module("gen_stress_inputs", "gen-stress-inputs.py")
    os.makedirs(out, exist_ok=True)
    manifest: dict[str, dict] = {}

    cjk_text_path = os.path.join(out, "cjk-64mib.txt")
    memory_profile.write_cjk_document(cjk_text_path, 64 * MIB)
    manifest["cjk-64mib"] = {"path": cjk_text_path, "bytes": os.path.getsize(cjk_text_path)}

    # A 300-page English text PDF and a 300-page Chinese text PDF, laid out with PyMuPDF the way
    # scripts/gen-stress-inputs.py lays out its 1,500/1,000-page versions.
    english = " ".join(
        "Pierre and Natasha walked along the river while the winter light fell on the old house."
        for _ in range(20000)
    )
    en_pdf = os.path.join(out, "text-long-en-300.pdf")
    stress.write_text_pdf(en_pdf, stress.paginate(english, 2000, pdf_pages), fontsize=11,
                          font_path=None)
    manifest["text-long-en-300"] = {"path": en_pdf, "bytes": os.path.getsize(en_pdf)}

    cjk = stress.extract_epub_text(song_shi) if song_shi else None
    if not cjk:
        cjk = stress.synthetic_cjk_text(1000 * pdf_pages)
        manifest["text-long-cjk-300.source"] = {"synthetic": True}
    if not cjk_font or not os.path.isfile(cjk_font):
        raise SystemExit(f"CJK font not found: {cjk_font}")
    cjk_pdf = os.path.join(out, "text-long-cjk-300.pdf")
    stress.write_text_pdf(cjk_pdf, stress.paginate(cjk, 1000, pdf_pages), fontsize=11,
                          font_path=cjk_font)
    manifest["text-long-cjk-300"] = {"path": cjk_pdf, "bytes": os.path.getsize(cjk_pdf)}
    return manifest


# --------------------------------------------------------------------------------------------------
# Sampling
# --------------------------------------------------------------------------------------------------


def read_status(pid: int) -> tuple[int | None, int | None]:
    """Return `(VmRSS, VmHWM)` in KiB from `/proc/<pid>/status`, or `(None, None)` if gone."""
    try:
        with open(f"/proc/{pid}/status", encoding="ascii", errors="replace") as handle:
            rss = hwm = None
            for line in handle:
                if line.startswith("VmRSS:"):
                    rss = int(line.split()[1])
                elif line.startswith("VmHWM:"):
                    hwm = int(line.split()[1])
                elif rss is not None and hwm is not None:
                    break
            return rss, hwm
    except (OSError, ValueError, IndexError):
        return None, None


class Sampler(threading.Thread):
    """Poll `/proc/<pid>/status` every `interval` seconds until `stop()`."""

    def __init__(self, pid: int, started: float, interval: float):
        super().__init__(daemon=True)
        self.pid = pid
        self.started = started
        self.interval = interval
        self.samples: list[list[float]] = []
        self._stop_event = threading.Event()

    def run(self) -> None:
        while not self._stop_event.is_set():
            rss, hwm = read_status(self.pid)
            if rss is not None:
                self.samples.append([round(time.monotonic() - self.started, 4), rss, hwm])
            self._stop_event.wait(self.interval)

    def stop(self) -> None:
        self._stop_event.set()


class EventCollector(threading.Thread):
    """Read the server's stderr, timestamping each JSON telemetry line as it arrives."""

    def __init__(self, stream, started: float):
        super().__init__(daemon=True)
        self.stream = stream
        self.started = started
        self.events: list[dict] = []
        self.raw: list[str] = []

    def run(self) -> None:
        try:
            for line in self.stream:
                text = line.decode("utf-8", errors="replace").strip()
                if not text:
                    continue
                try:
                    item = json.loads(text)
                except json.JSONDecodeError:
                    self.raw.append(text)
                    continue
                if isinstance(item, dict):
                    item["t"] = round(time.monotonic() - self.started, 4)
                    self.events.append(item)
        except Exception:  # noqa: BLE001 - draining is best effort
            pass


def rss_at(samples: list[list[float]], when: float) -> tuple[int | None, int | None]:
    """The last sample at or before `when`, or the first sample if `when` precedes them all."""
    if not samples:
        return None, None
    chosen = samples[0]
    for sample in samples:
        if sample[0] <= when:
            chosen = sample
        else:
            break
    return chosen[1], chosen[2]


def max_in(samples: list[list[float]], start: float, end: float) -> tuple[int | None, int | None]:
    window = [sample for sample in samples if start - 1e-9 <= sample[0] <= end + 1e-9]
    if not window:
        return None, None
    peak = max(window, key=lambda sample: sample[1])
    return peak[1], peak[2]


PHASE_EVENTS = (
    "retrieve",
    "parse",
    "repository_save",
    "text_unit_sets_write",
    "index",
    "scan_prewarm",
)


def event_intervals(events: list[dict], names=PHASE_EVENTS) -> dict[str, list[dict]]:
    """Group telemetry events into `[t-duration, t]` intervals per event name."""
    grouped: dict[str, list[dict]] = {}
    for item in events:
        event = item.get("event")
        duration = item.get("duration_ms")
        if event not in names or not isinstance(duration, (int, float)):
            continue
        end = float(item["t"])
        start = max(0.0, end - float(duration) / 1000.0)
        grouped.setdefault(event, []).append(
            {"start": start, "end": end, "rss_kib": item.get("rss_kib"),
             "hwm_kib": item.get("hwm_kib"), "success": item.get("success")}
        )
    return grouped


def build_phases(samples: list[list[float]], events: list[dict], marks: dict) -> list[dict]:
    """Turn event intervals plus the call markers into the phase table of one open."""
    phases: list[dict] = []
    grouped = event_intervals(events)
    for name in PHASE_EVENTS:
        for interval in grouped.get(name, []):
            start_rss, _ = rss_at(samples, interval["start"])
            end_rss, end_hwm = rss_at(samples, interval["end"])
            peak_rss, _ = max_in(samples, interval["start"], interval["end"])
            if peak_rss is None:
                peak_rss = end_rss
            phases.append({
                "phase": name,
                "kind": "telemetry",
                "start": interval["start"],
                "end": interval["end"],
                "start_rss_kib": start_rss,
                "end_rss_kib": end_rss,
                "end_hwm_kib": end_hwm,
                "max_rss_kib": peak_rss,
                "telemetry_rss_kib": interval["rss_kib"],
                "telemetry_hwm_kib": interval["hwm_kib"],
            })
    synthetic = [
        ("startup+initialize", 0.0, marks.get("open_sent")),
        ("open-return", marks.get("open_sent"), marks.get("open_done")),
        ("settle-after-open", marks.get("open_done"), marks.get("settle1_done")),
        ("get_document_structure", marks.get("structure_sent"), marks.get("structure_done")),
        ("get_text_units", marks.get("text_units_sent"), marks.get("text_units_done")),
        ("search_document", marks.get("search_sent"), marks.get("search_done")),
        ("settle-after-calls", marks.get("search_done"), marks.get("settle2_done")),
        ("second-open", marks.get("open2_sent"), marks.get("open2_done")),
        ("settle-after-second-open", marks.get("open2_done"), marks.get("settle3_done")),
    ]
    for name, start, end in synthetic:
        if start is None or end is None:
            continue
        start_rss, _ = rss_at(samples, start)
        end_rss, end_hwm = rss_at(samples, end)
        peak_rss, _ = max_in(samples, start, end)
        if peak_rss is None:
            peak_rss = end_rss
        phases.append({
            "phase": name,
            "kind": "synthetic",
            "start": start,
            "end": end,
            "start_rss_kib": start_rss,
            "end_rss_kib": end_rss,
            "end_hwm_kib": end_hwm,
            "max_rss_kib": peak_rss,
        })
    return phases


def global_peak_phase(samples: list[list[float]], phases: list[dict]) -> tuple[int, str]:
    """Return `(peak_rss_kib, phase_name)`; the most specific interval containing the peak wins.

    The chosen phase object is flagged `global_peak`, so duplicate phase names (two opens in one
    process) mark only the one the peak happened in.
    """
    if not samples:
        return 0, "unknown"
    peak = max(samples, key=lambda sample: sample[1])
    peak_time, peak_rss = peak[0], peak[1]
    containing = [
        phase for phase in phases
        if phase["start"] - 1e-9 <= peak_time <= phase["end"] + 1e-9
    ]
    if containing:
        containing.sort(key=lambda phase: phase["end"] - phase["start"])
        containing[0]["global_peak"] = True
        return peak_rss, containing[0]["phase"]
    return peak_rss, "outside-phases"


# --------------------------------------------------------------------------------------------------
# One timed run
# --------------------------------------------------------------------------------------------------


def extract_document_id(response: dict) -> str | None:
    result = response.get("result")
    if not isinstance(result, dict):
        return None
    structured = result.get("structuredContent") or result.get("structured_content")
    if isinstance(structured, dict) and isinstance(structured.get("document_id"), str):
        return structured["document_id"]
    return None


def call(process, responses: queue.Queue, request_id: int, name: str, arguments: dict,
         timeout: float) -> tuple[dict | None, str | None]:
    """Send one tools/call and wait for its response; return `(response, error)`."""
    memory_profile = load_module("memory_profile", "memory-profile.py")
    message = {
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    }
    memory_profile.send(process, message)
    try:
        response = memory_profile.read_response(responses, request_id, timeout)
    except (TimeoutError, RuntimeError) as error:
        return None, str(error)
    return response, memory_profile.extract_error(response)


def run_timeline(
    binary: str,
    label: str,
    source: str,
    state_dir: str,
    roots: list[str],
    out_dir: str,
    settle: float,
    sample_interval: float,
    timeout: float,
    env_extra: dict | None,
) -> dict:
    """Run one open, its settle wait, three tool calls and a second open, sampling RSS throughout."""
    memory_profile = load_module("memory_profile", "memory-profile.py")
    env = memory_profile.server_environment(roots, state_dir, timeout, telemetry=True)
    env["READING_MCP_MAX_DOCUMENT_BYTES"] = "134217728"
    env["READING_MCP_MAX_NORMALIZED_CHARS"] = "200000000"
    if env_extra:
        env.update(env_extra)
    started = time.monotonic()
    process = subprocess.Popen(
        [binary],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
    )
    sampler = Sampler(process.pid, started, sample_interval)
    sampler.start()
    collector = EventCollector(process.stderr, started)
    collector.start()
    responses: queue.Queue = queue.Queue()
    reader = threading.Thread(
        target=memory_profile.pump_stdout, args=(process.stdout, responses), daemon=True
    )
    reader.start()

    marks: dict[str, float] = {}
    errors: list[str] = []
    document_id: str | None = None
    section_id: str | None = None
    section_count: object = None
    call_timings: dict[str, float | None] = {}

    def mark(name: str) -> None:
        marks[name] = round(time.monotonic() - started, 4)

    try:
        mark("start")
        memory_profile.send(process, memory_profile.INITIALIZE_MESSAGE)
        try:
            initialized = memory_profile.read_response(responses, 1, timeout)
            init_error = memory_profile.extract_error(initialized)
        except (TimeoutError, RuntimeError) as error:
            init_error = str(error)
        mark("initialize_done")
        if init_error:
            errors.append(f"initialize: {init_error}")
        memory_profile.send(process, memory_profile.INITIALIZED_NOTIFICATION)

        if not init_error:
            mark("open_sent")
            response, error = call(process, responses, 2, "open_document", {"source": source},
                                   timeout)
            mark("open_done")
            if error:
                errors.append(f"open_document: {error}")
            else:
                document_id = extract_document_id(response or {})
                structured = (response or {}).get("result", {}).get("structuredContent") or {}
                section_count = structured.get("section_count")

        time.sleep(settle)
        mark("settle1_done")

        if document_id:
            mark("structure_sent")
            start = time.monotonic()
            response, error = call(process, responses, 3, "get_document_structure",
                                   {"document_id": document_id, "max_depth": 3, "max_nodes": 200},
                                   timeout)
            call_timings["get_document_structure"] = time.monotonic() - start
            mark("structure_done")
            if error:
                errors.append(f"get_document_structure: {error}")
            structured = (response or {}).get("result", {}).get("structuredContent") or {}
            sections = structured.get("sections") or []
            if sections and isinstance(sections[0], dict):
                section_id = sections[0].get("section_id")

            mark("text_units_sent")
            start = time.monotonic()
            response, error = call(process, responses, 4, "get_text_units",
                                   {"document_id": document_id, "section_id": section_id,
                                    "requested_kind": "sentence", "max_items": 32},
                                   timeout)
            call_timings["get_text_units"] = time.monotonic() - start
            mark("text_units_done")
            if error:
                errors.append(f"get_text_units: {error}")

            mark("search_sent")
            start = time.monotonic()
            response, error = call(process, responses, 5, "search_document",
                                   {"document_id": document_id, "query": search_query(label),
                                    "limit": 10}, timeout)
            call_timings["search_document"] = time.monotonic() - start
            mark("search_done")
            if error:
                errors.append(f"search_document: {error}")

        time.sleep(settle)
        mark("settle2_done")

        mark("open2_sent")
        response, error = call(process, responses, 6, "open_document", {"source": source}, timeout)
        mark("open2_done")
        if error:
            errors.append(f"second open_document: {error}")
        time.sleep(settle)
        mark("settle3_done")
    finally:
        try:
            if process.stdin is not None:
                process.stdin.close()
        except Exception:  # noqa: BLE001 - server may already be gone
            pass
        try:
            _, status, usage = os.wait4(process.pid, 0)
            process.returncode = os.waitstatus_to_exitcode(status)
            ru_maxrss = usage.ru_maxrss
        except ChildProcessError:
            ru_maxrss = None
        sampler.stop()
        sampler.join(timeout=1.0)
        reader.join(timeout=1.0)
        collector.join(timeout=1.0)

    samples = sampler.samples
    phases = build_phases(samples, collector.events, marks)
    peak_rss, peak_phase = global_peak_phase(samples, phases)
    retained_rss, retained_hwm = rss_at(samples, marks.get("settle1_done", 0.0))
    after_second_rss, after_second_hwm = rss_at(samples, marks.get("settle3_done", 0.0))
    result = {
        "label": label,
        "source": source,
        "source_bytes": os.path.getsize(source) if os.path.isfile(source) else None,
        "ok": not errors,
        "errors": errors,
        "section_count": section_count,
        "document_id": document_id,
        "open_seconds": round(marks.get("open_done", 0.0) - marks.get("open_sent", 0.0), 3)
        if "open_sent" in marks else None,
        "call_seconds": call_timings,
        "sample_interval": sample_interval,
        "peak_rss_kib": peak_rss,
        "peak_phase": peak_phase,
        "ru_maxrss_kib": ru_maxrss,
        "retained_after_open_rss_kib": retained_rss,
        "retained_after_open_hwm_kib": retained_hwm,
        "after_second_open_rss_kib": after_second_rss,
        "after_second_open_hwm_kib": after_second_hwm,
        "marks": marks,
        "phases": phases,
        "events": collector.events,
        "stderr_other": collector.raw[:50],
        "samples": samples,
    }
    os.makedirs(out_dir, exist_ok=True)
    path = os.path.join(out_dir, f"{label}.timeline.json")
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(result, handle)
    return result


def search_query(label: str) -> str:
    if label in ("zh-large", "text-long-cjk-300"):
        return "皇帝"
    return "the"


# --------------------------------------------------------------------------------------------------
# SQLite facts behind one open
# --------------------------------------------------------------------------------------------------


def sqlite_facts(state_dir: str) -> dict:
    """Read the PRAGMAs and the stored sizes the open's peak is made of."""
    import sqlite3

    database = os.path.join(state_dir, "reading-mcp.sqlite")
    facts: dict = {"database": database, "exists": os.path.exists(database)}
    if not facts["exists"]:
        return facts
    connection = sqlite3.connect(database)
    try:
        facts["pragmas"] = {
            name: connection.execute(f"PRAGMA {name}").fetchone()[0]
            for name in ("page_size", "journal_mode", "cache_size", "temp_store", "mmap_size")
        }
        row = connection.execute(
            "SELECT COUNT(*), COALESCE(SUM(length(CAST(document_json AS BLOB))), 0)"
            " FROM documents"
        ).fetchone()
        facts["documents"] = row[0]
        facts["document_json_bytes"] = row[1]
        facts["document_json_max_bytes"] = connection.execute(
            "SELECT COALESCE(MAX(length(CAST(document_json AS BLOB))), 0) FROM documents"
        ).fetchone()[0]
        for table in ("section_text_v2", "section_text_v1"):
            exists = connection.execute(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?", (table,)
            ).fetchone()
            if exists:
                facts["section_text_table"] = table
                facts["section_text_rows"], facts["section_text_bytes"] = connection.execute(
                    f"SELECT COUNT(*), COALESCE(SUM(length(CAST(text AS BLOB))), 0) FROM {table}"
                ).fetchone()
                break
        else:
            facts["section_text_table"] = None
            facts["section_text_rows"] = 0
            facts["section_text_bytes"] = 0
        meta_exists = connection.execute(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'document_meta_v2'"
        ).fetchone()
        meta: list[dict] = []
        if meta_exists:
            for key, encoding, raw_bytes in connection.execute(
                "SELECT key, encoding, length(CAST(value AS BLOB)) FROM document_meta_v2"
            ):
                meta.append({"key": key, "encoding": encoding, "stored_bytes": raw_bytes})
        facts["metadata_v2"] = meta
        facts["sqlite_file_bytes"] = sum(
            os.path.getsize(database + suffix)
            for suffix in ("", "-wal", "-shm")
            if os.path.exists(database + suffix)
        )
    finally:
        connection.close()
    return facts


# --------------------------------------------------------------------------------------------------
# heaptrack parsing
# --------------------------------------------------------------------------------------------------

# heaptrack prints decimal units: 67.11M is 67,110,000 bytes, i.e. 64.0 MiB.
UNITS = {"B": 1.0, "K": 1e3, "M": 1e6, "G": 1e9, "T": 1e12, "P": 1e15}
NODE = re.compile(r"^(\S+) (?:peak memory )?consumed over (\d+) calls from:?\s*$")
SUMMARY_PEAK_HEAP = re.compile(r"^peak heap memory consumption:\s*(\S+)\s*$")
SUMMARY_PEAK_RSS = re.compile(r"^peak RSS \(including heaptrack overhead\):\s*(\S+)\s*$")
SUMMARY_LEAKED = re.compile(r"^total memory leaked:\s*(\S+)\s*$")

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _project_files() -> set[str]:
    """Every `.rs` file of this checkout, as `src/...` relative paths."""
    files: set[str] = set()
    for dirpath, _, names in os.walk(os.path.join(REPO_ROOT, "src")):
        for name in names:
            if name.endswith(".rs"):
                relative = os.path.relpath(os.path.join(dirpath, name), REPO_ROOT)
                files.add(relative.replace(os.sep, "/"))
    return files


PROJECT_FILES = _project_files()
_FUNCTION_CACHE: dict[str, list[tuple[int, str]]] = {}


def project_relative(location: str) -> str | None:
    """The `src/...` path of a project frame, or None when the frame is a library."""
    path = location.rpartition(":")[0]
    if not path:
        return None
    # Dependencies carry their own `src/...` paths (rusqlite's `src/lib.rs` is one), and rustc and
    # the standard library do too; only this checkout's files are project frames.
    if any(marker in path for marker in
           (".cargo/registry", "/rustc/", "/library/", "target/release/build")):
        return None
    if path.startswith("src/") and path in PROJECT_FILES:
        return path
    marker = "/src/"
    position = path.rfind(marker)
    if position < 0:
        return None
    relative = "src/" + path[position + len(marker):]
    return relative if relative in PROJECT_FILES else None


def is_project(location: str) -> bool:
    return project_relative(location) is not None


def project_path(location: str) -> str:
    return project_relative(location) or location.rpartition(":")[0]


def enclosing_function(relative: str, line: int) -> str:
    """The last `fn` declared at or before `line` in one project file.

    heaptrack's inline collapsing leaves symbols like `poll<>::{closure#0}` on the frames that
    matter, so the function is recovered from the file and line instead.
    """
    if relative not in _FUNCTION_CACHE:
        declarations: list[tuple[int, str]] = []
        path = os.path.join(REPO_ROOT, relative)
        try:
            with open(path, encoding="utf-8", errors="replace") as handle:
                for number, text in enumerate(handle, 1):
                    match = re.match(r"\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)", text)
                    if match:
                        declarations.append((number, match.group(1)))
        except OSError:
            pass
        _FUNCTION_CACHE[relative] = declarations
    found = "?"
    for number, name in _FUNCTION_CACHE[relative]:
        if number <= line:
            found = name
        else:
            break
    return found


def frame_line(location: str) -> int:
    tail = location.rpartition(":")[2]
    return int(tail) if tail.isdigit() else 0


BUCKET_ORDER = (
    "input_bytes",
    "zip_parse",
    "parser_document",
    "document",
    "normalized_text",
    "normalized_block_map",
    "segmentation",
    "document_json",
    "sqlite_bind_copy",
    "sqlite_record_copy",
    "sqlite_page_cache",
    "sqlite_save_shared",
    "metadata_clone",
    "metadata_compress",
    "text_unit_sets",
    "sqlite_write_document_sections",
    "sqlite_other",
    "search",
    "open_result",
    "other",
)

WRITE_FUNCTIONS = (
    "write_document_sections",
    "insert_section_text",
    "store_section_text",
    "store_body_order",
)

TEXT_UNIT_FUNCTIONS = (
    "replace_document_sets",
    "replace_document_segmentation",
    "build_section_layout",
    "build_segmentation_section_layout",
    "build_section_descriptors",
    "write_text_unit",
    "insert_text_unit",
    "walk_section_stream",
    "store_section_stream",
    "section_stream_summary",
)


def bucket_of(frame: Frame, stack_text: str) -> str:
    """Bucket one project frame by its file, its enclosing function and the stack around it."""
    relative = project_relative(frame.location)
    if relative is None:
        return "other"
    low = stack_text.lower()
    line = frame_line(frame.location)
    if relative.endswith("sqlite.rs"):
        function = enclosing_function(relative, line)
        if "pcache1" in low:
            return "sqlite_page_cache"
        if "sqlite3_bind_text" in low:
            return "sqlite_bind_copy"
        if "sqlite3vdbe" in low or "sqlite3_step" in low:
            return "sqlite_record_copy"
        if any(needle in low for needle in ("flate2", "miniz", "zlib")):
            return "metadata_compress"
        if "serde_json" in low or "format_escaped_str" in low:
            return "document_json"
        if function == "split_metadata":
            return "metadata_clone"
        if function == "save_shared":
            return "sqlite_save_shared"
        if function in TEXT_UNIT_FUNCTIONS:
            return "text_unit_sets"
        if function in WRITE_FUNCTIONS:
            return "sqlite_write_document_sections"
        if any(needle in function for needle in (
                "prewarm", "scan_", "open_result", "list_document", "load_document",
                "get_with_hash", "decode_metadata", "read_section_text")):
            return "search"
        return "sqlite_other"
    if relative.endswith("archive.rs"):
        return "zip_parse"
    if relative.endswith(("segmentation.rs", "section_layout.rs")):
        return "segmentation"
    if relative.endswith("normalized_block.rs"):
        return "normalized_block_map"
    if relative.endswith("normalized_text.rs"):
        return "normalized_text"
    if relative.startswith("src/parsing/"):
        return "parser_document"
    if relative.startswith("src/domain/"):
        return "document"
    if relative.endswith(("sqlite_search_index.rs", "lexical.rs")):
        return "search"
    if relative.startswith("src/retrieval/"):
        return "input_bytes"
    if relative.endswith(("text_unit_stream.rs", "text_unit_store.rs", "text_unit_cursor.rs")):
        return "text_unit_sets"
    if relative.endswith("domain/text_unit.rs"):
        return "text_unit_sets"
    if relative.endswith("application/ports.rs"):
        # A `RetrievedResource` clone copies the input bytes; anything else here is the open result.
        return "input_bytes" if "to_vec" in low or "clone" in low else "open_result"
    if relative.startswith("src/application/"):
        return "open_result"
    return "other"


def parse_size(text: str) -> float:
    for suffix, factor in UNITS.items():
        if text.endswith(suffix):
            return float(text[: -len(suffix)]) * factor
    return float(text)


class Frame:
    def __init__(self, symbol: str):
        self.symbol = symbol
        self.location = ""

    def __str__(self) -> str:
        return f"{self.symbol} @ {self.location}" if self.location else self.symbol


class Node:
    def __init__(self, cost: int, depth: int):
        self.cost = cost
        self.depth = depth
        self.frames: list[Frame] = []
        self.children: list[Node] = []


def parse_peaks(path: str) -> tuple[list[Node], dict]:
    """Parse `heaptrack_print --print-peaks` output into its consumer tree plus the summary."""
    roots: list[Node] = []
    last_at_depth: dict[int, Node] = {}
    node: Node | None = None
    summary: dict = {}
    lines = open(path, encoding="utf-8", errors="replace").read().splitlines()
    index = 0
    while index < len(lines):
        line = lines[index]
        match = NODE.match(line)
        if match:
            cost = int(parse_size(match.group(1)))
            depth = 0
            probe = index + 1
            while probe < len(lines):
                candidate = lines[probe]
                if not candidate.strip():
                    probe += 1
                    continue
                depth = (len(candidate) - len(candidate.lstrip())) // 4
                break
            node = Node(cost, depth)
            last_at_depth = {d: n for d, n in last_at_depth.items() if d < depth}
            last_at_depth[depth] = node
            if depth == 0:
                roots.append(node)
            else:
                parent = last_at_depth.get(depth - 1)
                (parent.children if parent is not None else roots).append(node)
            index += 1
            continue
        for regex, key in (
            (SUMMARY_PEAK_HEAP, "peak_heap_bytes"),
            (SUMMARY_PEAK_RSS, "peak_rss_bytes"),
            (SUMMARY_LEAKED, "leaked_bytes"),
        ):
            found = regex.match(line.strip())
            if found:
                summary[key] = int(parse_size(found.group(1)))
        if node is not None and line.strip():
            indent = len(line) - len(line.lstrip())
            stripped = line.strip()
            if indent // 4 == node.depth:
                if stripped.startswith("at "):
                    if node.frames:
                        node.frames[-1].location = stripped[3:]
                elif not stripped.startswith(("in ", "0x")):
                    node.frames.append(Frame(stripped))
        index += 1
    return roots, summary


def module_hint(frames: list[Frame]) -> str:
    """Name the library a stack with no project frame belongs to."""
    text = " ".join(str(frame) for frame in frames).lower()
    for needle, name in (
        ("serde_json", "serde_json"),
        ("serde_core", "serde_json"),
        ("sqlite3", "sqlite"),
        ("libsqlite3", "sqlite"),
        ("rusqlite", "sqlite"),
        ("regex", "regex"),
        ("lopdf", "lopdf"),
        ("zip", "zip"),
        ("flate2", "flate2"),
        ("miniz", "flate2"),
        ("roxmltree", "roxmltree"),
        ("html5ever", "html5ever"),
        ("tokio", "tokio"),
        ("alloc::", "allocator"),
        ("malloc", "allocator"),
        ("std::", "std"),
    ):
        if needle in text:
            return name
    return "unknown"


def node_bucket(node: Node, stack_text: str) -> str | None:
    """The bucket of the allocation a node's own frames name, or None when they name none.

    A `serde_json` realloc of the output buffer is attributed by the *caller* that asked for the
    JSON (the `normalized_block_map` serialization or the `document_json` serialization), not by the
    innermost derived `Serialize` impl the stack happens to end in.
    """
    project_frames = [frame for frame in node.frames if is_project(frame.location)]
    if not project_frames:
        return None
    low = stack_text.lower()
    if "pcache1" in low:
        return "sqlite_page_cache"
    if "sqlite3_bind_text" in low:
        return "sqlite_bind_copy"
    if "sqlite3vdbe" in low or "sqlite3_step" in low:
        return "sqlite_record_copy"
    if any(needle in low for needle in ("flate2", "miniz", "zlib")):
        return "metadata_compress"
    if "serde_json" in low or "format_escaped_str" in low:
        paths = [project_relative(frame.location) or "" for frame in project_frames]
        if any(path.endswith("normalized_block.rs") for path in paths):
            return "normalized_block_map"
        if any(path.endswith("sqlite.rs") for path in paths):
            return "document_json"
    paths = [project_relative(frame.location) or "" for frame in project_frames]
    if any(path.startswith("src/parsing/") for path in paths):
        return "parser_document"
    return bucket_of(project_frames[0], stack_text)


def classify(node: Node, prefix: list[Frame]) -> dict[str, int]:
    """Attribute a consumer-tree node's cost to the buckets its own frames name."""
    frames = prefix + node.frames
    text = "\n".join(str(frame) for frame in frames)
    is_sqlite = any(needle in text for needle in ("sqlite3", "libsqlite3-sys", "rusqlite"))
    bucket = node_bucket(node, text)
    if bucket is not None:
        return {bucket: node.cost}
    if not node.children:
        if "library/std/src/fs.rs" in text and "blocking/pool.rs" in text:
            return {"input_bytes": node.cost}
        if is_sqlite:
            return {"sqlite_other": node.cost}
        return {f"lib:{module_hint(node.frames)}": node.cost}
    out: dict[str, int] = {}
    covered = 0
    for child in node.children:
        for name, cost in classify(child, frames).items():
            out[name] = out.get(name, 0) + cost
        covered += child.cost
    if node.cost > covered:
        out["other"] = out.get("other", 0) + (node.cost - covered)
    return out


def attribute_sites(node: Node, inherited: list[Frame], sites: dict) -> int:
    """Attribute each node's cost to the nearest project frame (the allocation site)."""
    frames = inherited + node.frames
    text = "\n".join(str(frame) for frame in frames)
    bucket = node_bucket(node, text)
    if bucket is not None:
        project_frames = [frame for frame in node.frames if is_project(frame.location)]
        nearest = project_frames[0]
        relative = project_path(nearest.location)
        line = frame_line(nearest.location)
        function = enclosing_function(relative, line) if relative in PROJECT_FILES else "?"
        if bucket == "document_json":
            label = f"serde_json::to_string(document_json) @ {relative}:{line}"
        elif bucket == "normalized_block_map":
            label = f"serde_json::to_string(normalized_block_map) @ {relative}:{line}"
        elif bucket == "sqlite_bind_copy":
            label = f"sqlite3_bind_text64 @ {relative}:{line}"
        elif bucket == "sqlite_record_copy":
            label = f"sqlite3VdbeExec record @ {relative}:{line}"
        elif bucket == "sqlite_page_cache":
            label = f"pcache1 @ {relative}:{line}"
        elif bucket == "metadata_compress":
            label = f"zlib heavy metadata @ {relative}:{line}"
        else:
            label = f"{function} @ {relative}:{line}"
        key = f"{label} [{bucket}]"
        entry = sites.setdefault(key, {"site": label, "bytes": 0, "bucket": bucket})
        entry["bytes"] += node.cost
        return node.cost
    covered = 0
    for child in node.children:
        covered += attribute_sites(child, frames, sites)
    leftover = node.cost - covered
    if leftover > 0:
        symbol = node.frames[0].symbol.split("<>")[0][:60] if node.frames else "?"
        if "library/std/src/fs.rs" in text and "blocking/pool.rs" in text:
            label, bucket = "std::fs::read (input bytes)", "input_bytes"
        elif any(needle in text for needle in ("sqlite3", "libsqlite3-sys")):
            label, bucket = f"[sqlite] {symbol}", "sqlite_other"
        else:
            label, bucket = f"[{module_hint(node.frames)}] {symbol}", "library"
        key = f"{label} [{bucket}]"
        entry = sites.setdefault(key, {"site": label, "bytes": 0, "bucket": bucket})
        entry["bytes"] += leftover
    return node.cost


def heap_report(peaks_path: str) -> dict:
    roots, summary = parse_peaks(peaks_path)
    buckets: dict[str, int] = {}
    for root in roots:
        for name, cost in classify(root, []).items():
            buckets[name] = buckets.get(name, 0) + cost
    sites: dict = {}
    for root in roots:
        attribute_sites(root, [], sites)
    listed = sum(root.cost for root in roots)
    heap_peak = summary.get("peak_heap_bytes") or listed
    site_rows = sorted(sites.values(), key=lambda entry: entry["bytes"], reverse=True)
    return {
        "peaks_path": peaks_path,
        "listed_bytes": listed,
        "heap_peak_bytes": heap_peak,
        "peak_rss_under_heaptrack_bytes": summary.get("peak_rss_bytes"),
        "leaked_bytes": summary.get("leaked_bytes"),
        "buckets": buckets,
        "sites": site_rows,
    }


# --------------------------------------------------------------------------------------------------
# heaptrack run
# --------------------------------------------------------------------------------------------------


def run_open_under_heaptrack(
    binary: str,
    label: str,
    source: str,
    state_dir: str,
    roots: list[str],
    out_dir: str,
    timeout: float,
    env_extra: dict | None,
    variant: str = "default",
) -> dict:
    """Open the document once under heaptrack, then summarize the global heap peak."""
    memory_profile = load_module("memory_profile", "memory-profile.py")
    if shutil.which("heaptrack") is None:
        return {"label": label, "ok": False, "error": "heaptrack not installed"}
    heap_dir = os.path.join(out_dir, "heap")
    os.makedirs(heap_dir, exist_ok=True)
    prefix = os.path.join(heap_dir, f"{label}.{variant}")
    env = memory_profile.server_environment(roots, state_dir, timeout, telemetry=False)
    env["READING_MCP_MAX_DOCUMENT_BYTES"] = "134217728"
    env["READING_MCP_MAX_NORMALIZED_CHARS"] = "200000000"
    if env_extra:
        env.update(env_extra)
    command = ["heaptrack", "-o", prefix, binary]
    process = subprocess.Popen(
        command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        env=env,
    )
    responses: queue.Queue = queue.Queue()
    reader = threading.Thread(
        target=memory_profile.pump_stdout, args=(process.stdout, responses), daemon=True
    )
    reader.start()
    error = None
    try:
        memory_profile.send(process, memory_profile.INITIALIZE_MESSAGE)
        initialized = memory_profile.read_response(responses, 1, timeout)
        error = memory_profile.extract_error(initialized)
        memory_profile.send(process, memory_profile.INITIALIZED_NOTIFICATION)
        if error is None:
            _, error = call(process, responses, 2, "open_document", {"source": source}, timeout)
    except (TimeoutError, RuntimeError) as exc:
        error = str(exc)
    finally:
        try:
            if process.stdin is not None:
                process.stdin.close()
        except Exception:  # noqa: BLE001
            pass
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        reader.join(timeout=1.0)

    profile = f"{prefix}.zst"
    if not os.path.exists(profile):
        return {"label": label, "ok": False, "error": error or "heaptrack produced no profile"}
    peaks_path = f"{profile}.peaks.txt"
    print_path = subprocess.run(
        [
            "heaptrack_print", "--file", profile,
            "--print-peaks=1", "--peak-limit=200", "--sub-peak-limit=40",
            "--print-allocators=0", "--print-temporary=0", "--print-leaks=0",
            "--print-histogram=0",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    with open(peaks_path, "w", encoding="utf-8") as handle:
        handle.write(print_path.stdout)
        if print_path.stderr:
            handle.write("\n# stderr\n")
            handle.write(print_path.stderr)
    report = heap_report(peaks_path)
    if not report.get("heap_peak_bytes"):
        error = error or "heaptrack_print produced no peak consumers"
    report.update({"label": label, "ok": error is None, "error": error, "profile": profile})
    return report


# --------------------------------------------------------------------------------------------------
# Formatting helpers
# --------------------------------------------------------------------------------------------------


def mib(value: int | float | None) -> str:
    """Bytes to MiB, one decimal."""
    return "-" if value is None else f"{value / MIB_F:.1f}"


def rss_mib(value: int | float | None) -> str:
    """KiB (the unit `/proc/<pid>/status` and rusage use) to MiB, one decimal."""
    return "-" if value is None else f"{value / 1024.0:.1f}"


def pct(value: int | float | None, total: int | float | None) -> str:
    if not total:
        return "-"
    return f"{100.0 * value / total:.1f}%"


def rss_cell(rss_kib: int | None, hwm_kib: int | None) -> str:
    if rss_kib is None:
        return "-"
    if hwm_kib is None:
        return f"{rss_kib / 1024:.1f}"
    return f"{rss_kib / 1024:.1f} / {hwm_kib / 1024:.1f}"


# --------------------------------------------------------------------------------------------------
# Report
# --------------------------------------------------------------------------------------------------


def load_jsons(directory: str, suffix: str) -> list[dict]:
    rows: list[dict] = []
    for name in sorted(os.listdir(directory)):
        if name.endswith(suffix):
            with open(os.path.join(directory, name), encoding="utf-8") as handle:
                rows.append(json.load(handle))
    return rows


def timeline_table(runs: list[dict]) -> str:
    lines = [
        "| Input | Phase | Kind | Start RSS MiB | End RSS MiB | Max RSS MiB | Global peak |",
        "| --- | --- | --- | ---: | ---: | ---: | :--: |",
    ]
    for run in runs:
        for phase in run.get("phases", []):
            marker = "**yes**" if phase.get("global_peak") else ""
            lines.append(
                f"| {run['label']} | {phase['phase']} | {phase['kind']} | "
                f"{rss_mib(phase.get('start_rss_kib'))} | {rss_mib(phase.get('end_rss_kib'))} | "
                f"{rss_mib(phase.get('max_rss_kib'))} | {marker} |"
            )
    return "\n".join(lines)


def headline_table(runs: list[dict], heaps: list[dict]) -> str:
    """One row per input: sampled RSS peak, the phase it happens in, and the top heap buckets."""
    by_label: dict[str, dict] = {}
    for heap in heaps:
        if heap.get("variant", "default") == "default":
            by_label[heap["label"]] = heap
    lines = [
        "| Input | Source MiB | Sections | Peak RSS MiB | Peak phase | Heap peak MiB | "
        "Top 3 heap consumers |",
        "| --- | ---: | ---: | ---: | --- | ---: | --- |",
    ]
    for run in runs:
        heap = by_label.get(run["label"], {})
        buckets = heap.get("buckets", {})
        top = sorted(buckets.items(), key=lambda item: item[1], reverse=True)[:3]
        top_text = "; ".join(f"{name} {mib(cost)}" for name, cost in top) or "-"
        lines.append(
            f"| {run['label']} | {mib((run.get('source_bytes') or 0))} | "
            f"{run.get('section_count')} | {rss_mib(run.get('peak_rss_kib'))} | "
            f"{run.get('peak_phase')} | {mib(heap.get('heap_peak_bytes'))} | {top_text} |"
        )
    return "\n".join(lines)


def bucket_table(heaps: list[dict], variant: str) -> str:
    labels = {
        "input_bytes": "input bytes (`retrieval/file.rs`)",
        "zip_parse": "ZIP decompression (`parsing/archive.rs`)",
        "parser_document": "parsed `Document` / parser (`src/parsing/`)",
        "document": "`Document` / domain metadata (`src/domain/`)",
        "normalized_text": "normalized text (`normalized_text.rs`)",
        "normalized_block_map": "`normalized_block_map` (`normalized_block.rs`)",
        "segmentation": "`DocumentSegmentation` and layouts",
        "document_json": "`document_json` String (`StoredDocument` / `StoredSection`)",
        "sqlite_bind_copy": "SQLite bind copy (`sqlite3_bind_text64`)",
        "sqlite_record_copy": "SQLite record copy (`sqlite3VdbeExec`)",
        "sqlite_page_cache": "SQLite page cache (`pcache1`)",
        "sqlite_save_shared": "SQLite inside `save_shared` (other)",
        "metadata_clone": "heavy metadata clone (`split_metadata`)",
        "metadata_compress": "heavy metadata compression (`flate2`)",
        "text_unit_sets": "text unit sets and section descriptors",
        "sqlite_write_document_sections": "SQLite inside `write_document_sections`",
        "sqlite_other": "SQLite other",
        "search": "search index / scan cache",
        "open_result": "open result and application",
        "other": "other / allocator residual",
    }
    selected = [heap for heap in heaps if heap.get("variant") == variant]
    if not selected:
        return f"(no `{variant}` heap runs)"
    names = sorted({name for heap in selected for name in heap.get("buckets", {})})
    names = [name for name in BUCKET_ORDER if name in names] + [
        name for name in names if name not in BUCKET_ORDER
    ]
    header = "| Bucket | " + " | ".join(heap["label"] for heap in selected) + " |"
    align = "| --- | " + " | ".join("---:" for _ in selected) + " |"
    lines = [header, align]
    for name in names:
        cells = []
        for heap in selected:
            cost = heap.get("buckets", {}).get(name)
            total = heap.get("heap_peak_bytes")
            cells.append("-" if not cost else f"{mib(cost)} ({pct(cost, total)})")
        lines.append(f"| {labels.get(name, name)} | " + " | ".join(cells) + " |")
    lines.append(
        "| **heap peak (heaptrack)** | "
        + " | ".join(f"**{mib(heap.get('heap_peak_bytes'))}**" for heap in selected)
        + " |"
    )
    lines.append(
        "| **peak RSS (heaptrack run)** | "
        + " | ".join(mib(heap.get("peak_rss_under_heaptrack_bytes")) for heap in selected)
        + " |"
    )
    return "\n".join(lines)


def sites_table(heap: dict, limit: int = 15) -> str:
    total = heap.get("heap_peak_bytes")
    lines = [
        f"### {heap['label']}: top {limit} allocation sites at the heap peak",
        "",
        "| # | Site | Bucket | MiB | % of heap peak |",
        "| ---: | --- | --- | ---: | ---: |",
    ]
    for index, site in enumerate(heap.get("sites", [])[:limit], 1):
        lines.append(
            f"| {index} | `{site['site']}` | {site['bucket']} | {mib(site['bytes'])} | "
            f"{pct(site['bytes'], total)} |"
        )
    return "\n".join(lines)


def retained_table(runs: list[dict]) -> str:
    lines = [
        "| Input | Peak RSS KiB | Peak (VmHWM) KiB | 3 s after open RSS KiB | Returned KiB | "
        "Still resident KiB | After 2nd open RSS KiB |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for run in runs:
        peak = run.get("peak_rss_kib") or 0
        hwm = run.get("ru_maxrss_kib") or 0
        retained = run.get("retained_after_open_rss_kib")
        returned = None if retained is None else max(0, peak - retained)
        lines.append(
            f"| {run['label']} | {peak} | {hwm} | {retained} | {returned} | {retained} | "
            f"{run.get('after_second_open_rss_kib')} |"
        )
    return "\n".join(lines)


def itemisation_table(heap: dict, facts: dict | None, label: str, bound_mib: float) -> str:
    """The itemised heap peak of one input against the task's bound."""
    total = heap.get("heap_peak_bytes")
    buckets = heap.get("buckets", {})
    rows: list[tuple[str, int | None, str]] = []

    def add(name: str, key: str, note: str) -> None:
        rows.append((name, buckets.get(key), note))

    add("input bytes (raw file)", "input_bytes", "whole parse")
    add("ZIP decompression", "zip_parse", "per section")
    add("parsed `Document` / parser", "parser_document", "whole parse")
    add("`Document` / domain metadata", "document", "whole parse")
    add("normalized text copy", "normalized_text", "whole parse")
    add("`normalized_block_map` String", "normalized_block_map", "whole parse")
    add("`DocumentSegmentation`", "segmentation", "whole parse")
    add("`document_json` String", "document_json", "transient in save_shared")
    add("SQLite bind copy", "sqlite_bind_copy", "transient in save_shared")
    add("SQLite record copy", "sqlite_record_copy", "transient in save_shared")
    add("SQLite page cache", "sqlite_page_cache", "transient")
    add("SQLite other inside `save_shared`", "sqlite_save_shared", "transient")
    add("heavy metadata clone", "metadata_clone", "transient in save_shared")
    add("heavy metadata compression", "metadata_compress", "transient in save_shared")
    add("text unit sets and section descriptors", "text_unit_sets", "transient")
    add("SQLite section text writes", "sqlite_write_document_sections", "transient")
    add("SQLite other", "sqlite_other", "transient")
    add("search index / scan cache", "search", "transient")
    add("open result and application", "open_result", "transient")
    add("other / allocator residual", "other", "transient")
    for name in sorted(buckets):
        if name.startswith("lib:"):
            rows.append((name.removeprefix("lib:"), buckets[name], "transient"))

    lines = [
        f"### {label}: itemised heap peak (heaptrack), bound {bound_mib:.0f} MiB",
        "",
        "| Item | MiB | % of heap peak | Exists |",
        "| --- | ---: | ---: | --- |",
    ]
    for name, cost, note in rows:
        if cost is None:
            continue
        lines.append(f"| {name} | {mib(cost)} | {pct(cost, total)} | {note} |")
    lines.append(f"| **heap peak** | **{mib(total)}** | 100.0% | |")
    if facts:
        lines.append("")
        lines.append("Stored sizes from the same run's fresh database:")
        lines.append("")
        lines.append("| Stored fact | MiB |")
        lines.append("| --- | ---: |")
        lines.append(f"| `documents.document_json` (max row) | "
                     f"{mib(facts.get('document_json_max_bytes'))} |")
        if facts.get("section_text_table"):
            lines.append(f"| `{facts['section_text_table']}` text bytes | "
                         f"{mib(facts.get('section_text_bytes'))} |")
        for entry in facts.get("metadata_v2", []):
            lines.append(f"| `document_meta_v2.{entry['key']}` ({entry['encoding']}) | "
                         f"{mib(entry['stored_bytes'])} |")
    return "\n".join(lines)


def build_report(directory: str, out_path: str) -> str:
    runs = [run for run in load_jsons(directory, ".timeline.json")]
    heaps = load_jsons(directory, ".heap.json")
    facts: dict[str, dict] = {}
    for name in sorted(os.listdir(directory)):
        if name.endswith(".sqlite.json"):
            with open(os.path.join(directory, name), encoding="utf-8") as handle:
                data = json.load(handle)
            label = data["label"]
            if label not in facts or data.get("variant", "default") == "default":
                facts[label] = data

    # Attach the sqlite facts and the heaptrack numbers to the timeline rows by label.
    heap_by_label: dict[str, dict] = {}
    for heap in heaps:
        heap_by_label.setdefault(heap["label"], {}).setdefault(heap.get("variant", "default"), heap)

    sections: list[str] = ["# Open peak memory, measured now (MEM-2a)", ""]
    sections.append("## 0. Headline per input")
    sections.append("")
    sections.append(headline_table(runs, heaps))
    sections.append("")
    sections.append("## 1. RSS timeline of one open (20 ms samples, aligned with telemetry)")
    sections.append("")
    sections.append(timeline_table(runs))
    sections.append("")
    sections.append("## 2. Itemised heap peak (heaptrack)")
    sections.append("")
    for variant in ("default", "arena2"):
        sections.append(f"### Allocator variant `{variant}`")
        sections.append("")
        sections.append(bucket_table(heaps, variant))
        sections.append("")
    for heap in heaps:
        if heap.get("variant", "default") == "default":
            sections.append(sites_table(heap))
            sections.append("")
    sections.append("## 3. Retained after open (VmRSS vs VmHWM)")
    sections.append("")
    sections.append(retained_table(runs))
    sections.append("")
    sections.append("## 4. The 64 MiB single-section case against the 178 MiB bound")
    sections.append("")
    for heap in heaps:
        if heap["label"] == "cjk-64mib" and heap.get("variant", "default") == "default":
            sections.append(itemisation_table(heap, facts.get("cjk-64mib"), "cjk-64mib", 178))
            sections.append("")
    sections.append("## 5. 宋史 against the 50 MB bound")
    sections.append("")
    for heap in heaps:
        if heap["label"] == "zh-large" and heap.get("variant", "default") == "default":
            sections.append(itemisation_table(heap, facts.get("zh-large"), "zh-large", 50))
            sections.append("")
    sections.append("## 6. SQLite facts per input")
    sections.append("")
    sections.append("| Input | page_size | cache_size | temp_store | mmap_size | document_json MiB | "
                    "section text MiB | SQLite file MiB |")
    sections.append("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
    for label, data in facts.items():
        pragmas = data.get("pragmas") or {}
        sections.append(
            f"| {label} | {pragmas.get('page_size', '-')} | {pragmas.get('cache_size', '-')} | "
            f"{pragmas.get('temp_store', '-')} | {pragmas.get('mmap_size', '-')} | "
            f"{mib(data.get('document_json_max_bytes'))} | {mib(data.get('section_text_bytes'))} | "
            f"{mib(data.get('sqlite_file_bytes'))} |"
        )
    table = "\n".join(sections) + "\n"
    with open(out_path, "w", encoding="utf-8") as handle:
        handle.write(table)
    return table


# --------------------------------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------------------------------


def parse_inputs(spec: str) -> list[tuple[str, str]]:
    entries: list[tuple[str, str]] = []
    for item in spec.split(","):
        item = item.strip()
        if not item:
            continue
        if "=" not in item:
            raise SystemExit(f"--inputs entry must be label=path: {item}")
        label, path = item.split("=", 1)
        entries.append((label.strip(), os.path.abspath(path.strip())))
    return entries


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)

    gen = sub.add_parser("generate", help="write the synthetic inputs")
    gen.add_argument("--out", required=True)
    gen.add_argument("--song-shi", default=None)
    gen.add_argument("--cjk-font", default=None)
    gen.add_argument("--pdf-pages", type=int, default=300)

    prof = sub.add_parser("profile", help="profile one set of inputs")
    prof.add_argument("--binary", required=True)
    prof.add_argument("--out", required=True)
    prof.add_argument("--mode", choices=("rss", "heap"), required=True)
    prof.add_argument("--inputs", required=True, help="label=path[,label=path...]")
    prof.add_argument("--state-root", default=None)
    prof.add_argument("--settle-seconds", type=float, default=3.0)
    prof.add_argument("--sample-interval", type=float, default=0.02)
    prof.add_argument("--timeout-seconds", type=float, default=900.0)
    prof.add_argument("--variant", default="default")
    prof.add_argument("--malloc-arena-max", default=None)

    rep = sub.add_parser("report", help="aggregate the profile JSON into markdown")
    rep.add_argument("--dir", required=True)
    rep.add_argument("--out", required=True)

    args = parser.parse_args()

    if args.command == "generate":
        manifest = generate_inputs(args.out, args.song_shi, args.cjk_font, args.pdf_pages)
        print(json.dumps(manifest, indent=2, ensure_ascii=False))
        return 0

    if args.command == "profile":
        binary = os.path.abspath(args.binary)
        if not os.path.exists(binary):
            raise SystemExit(f"binary not found: {binary}")
        os.makedirs(args.out, exist_ok=True)
        entries = parse_inputs(args.inputs)
        roots: list[str] = []
        for _, path in entries:
            parent = os.path.dirname(path)
            if parent not in roots:
                roots.append(parent)
        env_extra: dict[str, str] = {}
        if args.malloc_arena_max:
            env_extra["MALLOC_ARENA_MAX"] = str(args.malloc_arena_max)
        for label, path in entries:
            if not os.path.exists(path):
                print(f"missing input {label}: {path}", file=sys.stderr)
                continue
            state_dir = os.path.join(args.out, f"state-{label}")
            if os.path.isdir(state_dir):
                shutil.rmtree(state_dir)
            os.makedirs(state_dir, exist_ok=True)
            if args.mode == "rss":
                run = run_timeline(
                    binary, label, path, state_dir, roots, args.out,
                    args.settle_seconds, args.sample_interval, args.timeout_seconds,
                    env_extra,
                )
                facts = sqlite_facts(state_dir)
                facts["label"] = label
                with open(os.path.join(args.out, f"{label}.sqlite.json"), "w",
                          encoding="utf-8") as handle:
                    json.dump(facts, handle)
                print(
                    f"rss label={label} ok={run['ok']} peak_rss_kib={run['peak_rss_kib']} "
                    f"peak_phase={run['peak_phase']} "
                    f"retained_kib={run['retained_after_open_rss_kib']} "
                    f"after_second_kib={run['after_second_open_rss_kib']}",
                    flush=True,
                )
                if not run["ok"]:
                    for error in run["errors"]:
                        print(f"  error: {error}", flush=True)
            else:
                heap = run_open_under_heaptrack(
                    binary, label, path, state_dir, roots, args.out,
                    args.timeout_seconds, env_extra, args.variant,
                )
                heap["variant"] = args.variant
                heap["malloc_arena_max"] = args.malloc_arena_max
                with open(os.path.join(args.out, f"{label}.{args.variant}.heap.json"), "w",
                          encoding="utf-8") as handle:
                    json.dump(heap, handle)
                facts = sqlite_facts(state_dir)
                facts["label"] = label
                facts["variant"] = args.variant
                with open(os.path.join(args.out, f"{label}.{args.variant}.sqlite.json"), "w",
                          encoding="utf-8") as handle:
                    json.dump(facts, handle)
                print(
                    f"heap label={label} variant={args.variant} ok={heap.get('ok')} "
                    f"heap_peak_bytes={heap.get('heap_peak_bytes')} "
                    f"peak_rss_bytes={heap.get('peak_rss_under_heaptrack_bytes')} "
                    f"error={heap.get('error') or '-'}",
                    flush=True,
                )
        return 0

    if args.command == "report":
        table = build_report(args.dir, args.out)
        print(table)
        summary = os.environ.get("GITHUB_STEP_SUMMARY")
        if summary:
            with open(summary, "a", encoding="utf-8") as handle:
                handle.write(table + "\n")
        return 0

    return 2


if __name__ == "__main__":
    sys.exit(main())
