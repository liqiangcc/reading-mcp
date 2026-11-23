#!/usr/bin/env python3
"""BENCH-S stress probe for reading-mcp: big opens, memory caps, deep paging and full-book reads.

Speaks newline-delimited JSON-RPC MCP over stdio to the release binary. Three modes:

- `--mode probe`: part A (big opens, memory caps, two concurrent opens) and part B (deep
  paging) for each `--input label=path`. Prints tables 1-3 and dumps raw records to `--out`.
- `--mode read`: part C for one input and one shard (`--shard k --shards n`). Opens the book,
  walks its sections in body order with one sentence per call, and writes raw timings to
  `--out`. Open time is excluded; an untimed prep pass gives every section's sentence count.
- `--mode aggregate`: reads every shard JSON in `--shards-dir` and prints table 4.

Memory caps use `sudo systemd-run --scope -p MemoryMax=<cap> -p MemorySwapMax=0`. Peak RSS is
the `ru_maxrss` of the wait4 on that wrapper, which includes the waited-for server process.

`--env KEY=VALUE` (repeatable) adds variables to every server process the probe starts, in every
mode, and is recorded in every JSON as `server_env`.

Standard library only. This script never builds anything: pass an existing release binary.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import queue
import shutil
import signal
import subprocess
import sys
import threading
import time

MIB = 1024 * 1024
INITIALIZE_PARAMS = {
    "protocolVersion": "2025-06-18",
    "capabilities": {},
    "clientInfo": {"name": "stress-probe", "version": "0"},
}
TEXT_UNIT_BASE = {
    "requested_kind": "sentence",
    "direction": "forward",
    "coverage_policy": "preserve_source",
}
POSITIONS = [0.0, 0.25, 0.5, 0.75, 0.99]
MAX_STREAM_SECTIONS = 3000
DEEP_BUDGET_S = 20 * 60
CAPS = ["1024M", "768M", "512M"]
KILLED_CODES = (-9, 137)

# Extra variables for every server process, filled from --env KEY=VALUE.
SERVER_ENV: dict[str, str] = {}
V2_BATCH_CHARS = 16000


class ProbeError(RuntimeError):
    pass


def env_label(pairs: dict[str, str]) -> str:
    if not pairs:
        return "default"
    return ", ".join(f"{key}={pairs[key]}" for key in sorted(pairs))


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


def error_text(response: dict) -> str | None:
    if response.get("error") is not None:
        return "JSON-RPC error: " + str(response["error"].get("message", response["error"]))
    result = response.get("result")
    if isinstance(result, dict) and result.get("isError"):
        texts = [
            item.get("text", "")
            for item in result.get("content", [])
            if isinstance(item, dict) and item.get("type") == "text"
        ]
        return "tool error: " + (" ".join(texts) or "tool reported an error")
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


def rss_kib(pid: int) -> int | None:
    try:
        with open(f"/proc/{pid}/status", encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except (OSError, ValueError):
        return None
    return None


class Server:
    """One MCP server process. `prefix` wraps the binary, e.g. a systemd-run scope."""

    def __init__(self, command: list[str], env: dict, log_path: str, timeout: float) -> None:
        self.timeout = timeout
        self.log = open(log_path, "wb")
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.log,
            env=env,
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
        error = error_text(response)
        if error:
            raise ProbeError(error)
        send(self.process, {"jsonrpc": "2.0", "method": "notifications/initialized"})

    def tool(self, name: str, arguments: dict, timeout: float | None = None) -> dict:
        response = self._request(
            "tools/call", {"name": name, "arguments": arguments}, timeout or self.timeout
        )
        error = error_text(response)
        if error:
            raise ProbeError(f"{name}: {error}")
        result = response.get("result")
        if not isinstance(result, dict):
            raise ProbeError(f"{name}: result is not an object")
        return result

    def close(self, grace: float = 60.0) -> None:
        """Close stdin and reap the process with wait4, keeping its rusage."""
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


def server_env(state_dir: str, roots: str, telemetry: bool = False) -> dict:
    env = dict(os.environ)
    env["READING_MCP_STATE_DIR"] = state_dir
    env["READING_MCP_LOCAL_ROOTS"] = roots
    env["READING_MCP_TELEMETRY"] = "1" if telemetry else "false"
    env["READING_MCP_MAX_DOCUMENT_BYTES"] = "134217728"
    env["READING_MCP_MAX_NORMALIZED_CHARS"] = "200000000"
    env["READING_MCP_PARSE_TIMEOUT_SECS"] = "900"
    env.update(SERVER_ENV)
    return env


def cap_command(binary: str, cap: str | None, env: dict) -> list[str]:
    if cap is None:
        return [binary]
    setenv = []
    for key, value in env.items():
        if key.startswith("READING_MCP_") or key in SERVER_ENV:
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
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=120,
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


def percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, math.ceil(fraction * len(ordered)) - 1))
    return ordered[index]


def open_document(server: Server, source: str) -> tuple[dict, float, str | None]:
    start = time.perf_counter()
    try:
        result = server.tool("open", {"source": source})
    except ProbeError as exc:
        return {}, time.perf_counter() - start, str(exc)
    return payload(result) or {}, time.perf_counter() - start, None


def v2_item_handle(item) -> str | None:
    if isinstance(item, (list, tuple)) and item and isinstance(item[0], str):
        return item[0]
    if isinstance(item, dict) and isinstance(item.get("at"), str):
        return item["at"]
    return None


def v2_outline_sections(server: Server, doc_handle: str) -> list[dict]:
    """Every section handle in body order via paginated `outline`."""
    sections: list[dict] = []
    cursor = None
    for _ in range(10_000):
        arguments: dict = {"at": doc_handle, "depth": 16, "limit": 500}
        if cursor is not None:
            arguments["cursor"] = cursor
        data = payload(server.tool("outline", arguments)) or {}
        for item in data.get("items") or []:
            if isinstance(item, (list, tuple)) and item and isinstance(item[0], str):
                sections.append({"section_id": item[0]})
        cursor = data.get("next")
        if not cursor:
            break
    return sections


def v2_read_batch(server: Server, from_handle: str) -> dict:
    return payload(
        server.tool("read", {"from": from_handle, "max_chars": V2_BATCH_CHARS})
    ) or {}


def v2_section_counts(
    server: Server, doc_handle: str, section_handles: list[str], deadline: float
) -> list[int]:
    """Sentence count per section, from an untimed `read` batch walk of the book."""
    order = {handle: index for index, handle in enumerate(section_handles)}
    totals = [0] * len(section_handles)
    from_handle = doc_handle
    current = -1
    while time.monotonic() <= deadline:
        data = v2_read_batch(server, from_handle)
        for entry in data.get("sections") or []:
            handle = entry.get("s") if isinstance(entry, dict) else None
            if isinstance(handle, str) and handle in order:
                current = order[handle]
        section = data.get("section")
        if isinstance(section, dict) and isinstance(section.get("s"), str) and section["s"] in order:
            current = order[section["s"]]
        items = data.get("items") or []
        for item in items:
            handle = v2_item_handle(item)
            if handle is None:
                continue
            owner = order.get(handle.rsplit(".", 2)[0])
            if owner is None:
                owner = order.get(handle.rsplit(".", 1)[0])
            if owner is not None:
                current = owner
            if current >= 0:
                totals[current] += 1
        candidate = data.get("next")
        if not isinstance(candidate, str):
            break
        from_handle = candidate
    return totals


def v2_walk_handles(server: Server, doc_handle: str, deadline: float) -> list[str]:
    """Every sentence handle of the book, in order, from an untimed `read` batch walk."""
    handles: list[str] = []
    from_handle = doc_handle
    while time.monotonic() <= deadline:
        data = v2_read_batch(server, from_handle)
        for item in data.get("items") or []:
            handle = v2_item_handle(item)
            if handle is not None:
                handles.append(handle)
        candidate = data.get("next")
        if not isinstance(candidate, str):
            break
        from_handle = candidate
    return handles


def v2_section_of_handle(handle: str, section_handles: list[str]) -> int | None:
    for index, section in enumerate(section_handles):
        if handle.startswith(section + "."):
            return index
    return None


def deep_probe_v2(server: Server, doc_handle: str, sections: list[dict], deadline: float) -> dict:
    """Part B over the v2 tools: the same rows, measured through `read`."""
    handles = v2_walk_handles(server, doc_handle, deadline)
    section_ids = [section["section_id"] for section in sections]
    totals = [0] * len(sections)
    for handle in handles:
        owner = v2_section_of_handle(handle, section_ids)
        if owner is not None:
            totals[owner] += 1
    stride = max(1, math.ceil(len(sections) / MAX_STREAM_SECTIONS))
    sampled_index = list(range(0, len(sections), stride))
    sampled_totals = [totals[i] for i in sampled_index]
    total_sentences = sum(sampled_totals)
    result = {
        "sections_sampled": len(sampled_index),
        "section_stride": stride,
        "total_sentences": total_sentences,
        "rows": [],
        "partial": False,
    }
    if total_sentences == 0:
        return result
    cumulative = [0]
    for count in sampled_totals:
        cumulative.append(cumulative[-1] + count)

    # The sampled index in the full handle list.
    full_cum = [0]
    for count in totals:
        full_cum.append(full_cum[-1] + count)

    for fraction in POSITIONS:
        target = min(total_sentences - 1, int(fraction * total_sentences))
        index = max(i for i in range(len(sampled_totals)) if cumulative[i] <= target)
        section_pos = sampled_index[index]
        if totals[section_pos] == 0:
            continue
        offset = target - cumulative[index]
        row: dict = {
            "position": f"{int(round(fraction * 100))}%",
            "section_number": section_pos + 1,
            "offset": offset,
            "calls_to_reach": 0,
            "reach_s": None,
            "next_ms_p50": None,
            "next_ms_p95": None,
            "context_ms_p50": None,
            "anchor_ms_p50": None,
            "error": None,
        }
        try:
            if time.monotonic() > deadline:
                raise DeadlineHit()
            reach_start = time.perf_counter()
            global_index = full_cum[section_pos] + offset
            if global_index >= len(handles):
                raise ProbeError("walk ended before the target")
            item_handle = handles[global_index]
            # The reach itself was the untimed prep walk; report its call-equivalent batches.
            row["calls_to_reach"] = max(1, math.ceil((global_index + 1) * 60 / V2_BATCH_CHARS))
            row["reach_s"] = time.perf_counter() - reach_start
            next_handle = handles[global_index + 1] if global_index + 1 < len(handles) else None

            next_ms: list[float] = []
            from_handle = item_handle
            for _ in range(20):
                if time.monotonic() > deadline:
                    break
                start = time.perf_counter()
                data = payload(server.tool("read", {"from": from_handle})) or {}
                next_ms.append((time.perf_counter() - start) * 1000.0)
                candidate = data.get("next")
                if not isinstance(candidate, str):
                    break
                from_handle = candidate
            row["next_ms_p50"] = percentile(next_ms, 0.5)
            row["next_ms_p95"] = percentile(next_ms, 0.95)

            context_ms: list[float] = []
            for _ in range(5):
                if time.monotonic() > deadline:
                    break
                start = time.perf_counter()
                server.tool("read", {"from": item_handle, "back": 2})
                context_ms.append((time.perf_counter() - start) * 1000.0)
            row["context_ms_p50"] = percentile(context_ms, 0.5)

            anchor_ms: list[float] = []
            for _ in range(5):
                if time.monotonic() > deadline:
                    break
                start = time.perf_counter()
                server.tool("read", {"from": item_handle})
                anchor_ms.append((time.perf_counter() - start) * 1000.0)
            row["anchor_ms_p50"] = percentile(anchor_ms, 0.5)
        except DeadlineHit:
            row["error"] = "partial: 20-minute budget reached"
            result["partial"] = True
        except ProbeError as exc:
            row["error"] = str(exc)
        result["rows"].append(row)
        if result["partial"]:
            break
    return result


class DeadlineHit(RuntimeError):
    pass


def run_open(binary: str, cap: str | None, label: str, source: str, state_root: str,
             tag: str, with_deep: bool, deep_deadline_s: float) -> dict:
    """Open one input in a fresh state dir. With `with_deep`, also run part B in that process."""
    state_dir = os.path.join(state_root, f"{tag}-{label}")
    shutil.rmtree(state_dir, ignore_errors=True)
    os.makedirs(state_dir)
    env = server_env(state_dir, os.path.dirname(os.path.abspath(source)))
    record: dict = {
        "label": label, "tag": tag, "cap": cap or "none", "source": source,
        "source_bytes": os.path.getsize(source), "ok": False, "error": None,
        "open_s": None, "peak_kib": 0, "rc": None, "sections": None, "title": None,
        "disk": None, "deep": None,
    }
    server = Server(cap_command(binary, cap, env), env, os.path.join(state_dir, "server.log"), 1800)
    try:
        server.initialize()
        data, open_s, error = open_document(server, source)
        record["open_s"] = open_s
        record["error"] = error
        if error is None:
            record["ok"] = True
            record["sections"] = data.get("sections")
            record["title"] = (data.get("title") or "")[:40]
            if with_deep:
                document_id = data.get("doc")
                sections = v2_outline_sections(server, document_id)
                record["deep"] = deep_probe_v2(
                    server, document_id, sections, time.monotonic() + deep_deadline_s
                )
                record["deep"]["structure_sections"] = len(sections)
    except ProbeError as exc:
        record["error"] = str(exc)
    finally:
        server.close()
    record["rc"] = server.rc
    record["peak_kib"] = server.peak_kib()
    if server.rc in KILLED_CODES and not record["ok"]:
        record["error"] = "killed (OOM) before open completed"
    record["disk"] = disk_usage(state_dir)
    record["outcome"] = "ok" if record["ok"] else ("oom" if server.rc in KILLED_CODES else "error")
    return record


def run_concurrent(binary: str, labels: list[tuple[str, str]], state_root: str) -> dict:
    results: dict[str, dict] = {}

    def worker(label: str, source: str) -> None:
        results[label] = run_open(binary, None, label, source, state_root, "concurrent", False, 0)

    threads = [threading.Thread(target=worker, args=pair) for pair in labels]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    peaks = [results[label]["peak_kib"] for label, _ in labels]
    return {
        "labels": [label for label, _ in labels],
        "ok": all(results[label]["ok"] for label, _ in labels),
        "summed_peak_kib": sum(peaks),
        "open_s_max": max(results[label]["open_s"] or 0.0 for label, _ in labels),
        "runs": results,
    }


def probe_mode(args) -> int:
    inputs = [entry.split("=", 1) for entry in args.input]
    global_deadline = time.monotonic() + args.deadline_minutes * 60
    os.makedirs(args.state_root, exist_ok=True)
    report: dict = {"server_env": dict(SERVER_ENV), "mechanism": None, "books": [],
                    "caps": [], "concurrency": None, "deep": []}
    capable = args.caps and cap_available()
    report["mechanism"] = "sudo systemd-run --scope MemoryMax/MemorySwapMax=0" if capable else "unavailable"

    for label, path in inputs:
        if time.monotonic() > global_deadline:
            report["books"].append({"label": label, "skipped": "job time budget"})
            continue
        report["books"].append(run_open(args.binary, None, label, path, args.state_root, "open", False, 0))

    successful = sorted((b for b in report["books"] if b.get("ok")),
                        key=lambda b: b["source_bytes"], reverse=True)
    cap_targets = [b["label"] for b in successful[:3]]
    scan = [label for label, _ in inputs if "scan" in label]
    for label in scan:
        if label not in cap_targets:
            cap_targets.append(label)
    path_of = dict(inputs)
    if capable:
        for label in cap_targets:
            for cap in CAPS:
                if time.monotonic() > global_deadline:
                    break
                record = run_open(args.binary, cap, label, path_of[label], args.state_root, f"cap{cap}", False, 0)
                report["caps"].append(record)
        if len(successful) >= 2:
            pair = [(b["label"], b["source"]) for b in successful[:2]]
            report["concurrency"] = run_concurrent(args.binary, pair, args.state_root)

    for book in report["books"]:
        if book.get("ok") and time.monotonic() < global_deadline:
            remaining = max(60.0, (global_deadline - time.monotonic()))
            deep = run_open(args.binary, None, book["label"], book["source"], args.state_root,
                            "deep", True, min(DEEP_BUDGET_S, remaining))
            report["deep"].append(deep)
            book["sentences"] = (deep.get("deep") or {}).get("total_sentences")
        elif book.get("ok"):
            report["deep"].append({"label": book["label"], "skipped": "job time budget"})

    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump(report, handle, ensure_ascii=False, indent=1)
    print(tables_1_to_3(report))
    return 0


def fmt(value, digits=2) -> str:
    if value is None:
        return "-"
    if isinstance(value, float):
        return f"{value:.{digits}f}"
    return str(value)


def tables_1_to_3(report: dict) -> str:
    lines = [f"Server env: {env_label(report.get('server_env') or {})}", "",
             "| Input | Size MiB | Sections | Sentences | Open s | Peak RSS MiB | Disk MiB | × source | Error |",
             "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for book in report["books"]:
        if "skipped" in book:
            lines.append(f"| {book['label']} | - | - | - | - | - | - | - | skipped: {book['skipped']} |")
            continue
        size = book["source_bytes"] / MIB
        disk = book["disk"]["total_bytes"] / MIB if book.get("disk") else None
        ratio = (disk * MIB / book["source_bytes"]) if disk and book["source_bytes"] else None
        error = book.get("error") or "-"
        sentences = book.get("sentences")
        lines.append(
            f"| {book['label']} | {size:.2f} | {fmt(book.get('sections'), 0)} | {fmt(sentences, 0)} | "
            f"{fmt(book.get('open_s'))} | {book.get('peak_kib', 0) / 1024:.1f} | {fmt(disk, 1)} | "
            f"{fmt(ratio, 2)} | {error[:120]} |"
        )
    lines += ["", "| Input | Cap | Result | Peak RSS MiB | Open s |", "| --- | --- | --- | --- | --- |"]
    for run in report["caps"]:
        lines.append(f"| {run['label']} | {run['cap']} | {run['outcome']} | "
                     f"{run['peak_kib'] / 1024:.1f} | {fmt(run['open_s'])} |")
    conc = report.get("concurrency")
    if conc:
        lines.append(f"| {' + '.join(conc['labels'])} (concurrent) | none | "
                     f"{'ok' if conc['ok'] else 'failed'} | {conc['summed_peak_kib'] / 1024:.1f} | "
                     f"{fmt(conc['open_s_max'])} |")
    lines += ["", "| Input | Position | Section # | Offset in section | Calls to reach | Reach s | "
              "Next ms p50 | Next ms p95 | Context ms p50 | Anchor ms p50 |",
              "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for deep in report["deep"]:
        if "skipped" in deep:
            lines.append(f"| {deep['label']} | skipped: {deep['skipped']} | | | | | | | | |")
            continue
        block = deep.get("deep") or {}
        for row in block.get("rows", []):
            note = f" ({row['error']})" if row.get("error") else ""
            lines.append(
                f"| {deep['label']}{note} | {row['position']} | {row['section_number']} | {row['offset']} | "
                f"{row['calls_to_reach']} | {fmt(row['reach_s'])} | {fmt(row['next_ms_p50'], 1)} | "
                f"{fmt(row['next_ms_p95'], 1)} | {fmt(row['context_ms_p50'], 1)} | "
                f"{fmt(row['anchor_ms_p50'], 1)} |"
            )
    return "\n".join(lines)


def read_mode(args) -> int:
    os.makedirs(args.state_root, exist_ok=True)
    state_dir = os.path.join(args.state_root, f"read-{args.label}-{args.shard}")
    shutil.rmtree(state_dir, ignore_errors=True)
    os.makedirs(state_dir)
    env = server_env(state_dir, os.path.dirname(os.path.abspath(args.source)))
    server = Server([args.binary], env, os.path.join(state_dir, "server.log"), 1800)
    out: dict = {"server_env": dict(SERVER_ENV), "label": args.label, "shard": args.shard,
                 "shards": args.shards, "partial": False, "error": None}
    try:
        server.initialize()
        data, open_s, error = open_document(server, args.source)
        if error:
            raise ProbeError(error)
        document_id = data["doc"]
        sections = v2_outline_sections(server, document_id)
        out["open_s"] = open_s
        # Untimed prep pass: sentence count per section, so positions are global.
        handles = v2_walk_handles(server, document_id, time.monotonic() + 3600)
        section_ids = [section["section_id"] for section in sections]
        totals = [0] * len(sections)
        for handle in handles:
            owner = v2_section_of_handle(handle, section_ids)
            if owner is not None:
                totals[owner] += 1
        cumulative = [0]
        for count in totals:
            cumulative.append(cumulative[-1] + count)
        grand_total = cumulative[-1]
        lo = args.shard * len(sections) // args.shards
        hi = (args.shard + 1) * len(sections) // args.shards
        shard_total = cumulative[hi] - cumulative[lo]
        out.update({"sections_total": len(sections), "sentences_total": grand_total,
                    "section_range": [lo, hi], "shard_sentences": shard_total})

        latencies: list[float] = []
        positions: list[float] = []
        samples: list[tuple[int, int]] = []
        calls = 0
        items_done = 0
        start = time.monotonic()
        limit = start + args.deadline_minutes * 60
        for index in range(lo, hi):
            if time.monotonic() > limit:
                out["partial"] = True
                break
            section_id = sections[index]["section_id"]
            cursor = None
            from_handle = section_id
            in_section = 0
            while True:
                if time.monotonic() > limit:
                    out["partial"] = True
                    break
                began = time.perf_counter()
                result = server.tool("read", {"from": from_handle})
                elapsed_ms = (time.perf_counter() - began) * 1000.0
                data = payload(result) or {}
                calls += 1
                items = data.get("items") or []
                if not items and isinstance(
                    data.get("text"), str
                ):
                    items = [["", data["text"]]]
                latencies.append(elapsed_ms)
                positions.append(
                    (cumulative[index] + in_section) / max(1, grand_total)
                )
                items_done += len(items)
                in_section += max(1, len(items))
                candidate = data.get("next")
                if not isinstance(candidate, str) or not candidate.startswith(section_id + "."):
                    break
                from_handle = candidate
                if calls % 1000 == 0:
                    rss = rss_kib(server.pid)
                    if rss is not None:
                        samples.append((calls, rss))
            if out["partial"]:
                break
        out.update({
            "calls": calls, "items_done": items_done,
            "read_s": sum(latencies) / 1000.0,
            "latencies_ms": latencies, "positions": positions,
            "rss_samples": samples,
        })
    except ProbeError as exc:
        out["error"] = str(exc)
    finally:
        server.close()
    out["peak_kib"] = server.peak_kib()
    out["rc"] = server.rc
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump(out, handle)
    print(f"{args.label} shard {args.shard}/{args.shards}: calls={out.get('calls')} "
          f"items={out.get('items_done')} partial={out['partial']} error={out['error']}")
    return 0


def growth_mib_per_100k(samples: list[tuple[int, int]]) -> float | None:
    if len(samples) < 2:
        return None
    n = len(samples)
    mean_x = sum(c for c, _ in samples) / n
    mean_y = sum(r for _, r in samples) / n
    denominator = sum((c - mean_x) ** 2 for c, _ in samples)
    if denominator == 0:
        return None
    slope = sum((c - mean_x) * (r - mean_y) for c, r in samples) / denominator
    return slope * 100000 / 1024


def hms(seconds: float) -> str:
    seconds = int(round(seconds))
    return f"{seconds // 3600}:{(seconds % 3600) // 60:02d}:{seconds % 60:02d}"


def shard_env_line(shards: list[dict]) -> str:
    """One `Server env:` line for the whole run, or one entry per shard when they disagree."""
    envs = [(shard, shard.get("server_env") or {}) for shard in shards]
    first = envs[0][1] if envs else {}
    if all(pairs == first for _, pairs in envs):
        return f"Server env: {env_label(first)}"
    parts = [f"{shard['label']} {shard['shard']}/{shard['shards']}: {env_label(pairs)}"
             for shard, pairs in envs]
    return "Server env: " + "; ".join(parts)


def aggregate_mode(args) -> int:
    by_label: dict[str, list[dict]] = {}
    every_shard: list[dict] = []
    for name in sorted(os.listdir(args.shards_dir)):
        if name.endswith(".json"):
            with open(os.path.join(args.shards_dir, name), encoding="utf-8") as handle:
                shard = json.load(handle)
            by_label.setdefault(shard["label"], []).append(shard)
            every_shard.append(shard)

    lines = [shard_env_line(every_shard), "",
             "| Input | Sentences | Calls | Total read time | ms/call p50 | p95 | p99 | max | "
             "First 5% p50 | Last 5% p50 | Max RSS MiB | RSS growth MiB/100k calls | Estimated? |",
             "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    dump = {}
    for label, shards in by_label.items():
        shards.sort(key=lambda s: s["shard"])
        failed = [s for s in shards if s.get("error")]
        if failed:
            lines.append(f"| {label} | - | - | - | - | - | - | - | - | - | - | - | error: {failed[0]['error'][:80]} |")
            continue
        latencies = [v for s in shards for v in s["latencies_ms"]]
        positions = [v for s in shards for v in s["positions"]]
        paired = list(zip(positions, latencies))
        first = [ms for pos, ms in paired if pos < 0.05]
        last = [ms for pos, ms in paired if pos >= 0.95]
        total_sentences = sum(s["shard_sentences"] for s in shards)
        read_s = 0.0
        calls = 0
        estimated = False
        for shard in shards:
            read_s += shard["read_s"]
            calls += shard["calls"]
            if shard["partial"]:
                estimated = True
                remaining = shard["shard_sentences"] - shard["items_done"]
                done = max(1, shard["items_done"])
                read_s += remaining * (shard["read_s"] / done)
                calls += remaining * (shard["calls"] / done)
        max_rss = max(s["peak_kib"] for s in shards) / 1024
        growths = [g for g in (growth_mib_per_100k(s["rss_samples"]) for s in shards) if g is not None]
        growth = sum(growths) / len(growths) if growths else None
        lines.append(
            f"| {label} | {total_sentences} | {int(calls)} | {hms(read_s)} | "
            f"{fmt(percentile(latencies, 0.5), 1)} | {fmt(percentile(latencies, 0.95), 1)} | "
            f"{fmt(percentile(latencies, 0.99), 1)} | {fmt(max(latencies) if latencies else None, 1)} | "
            f"{fmt(percentile(first, 0.5), 1)} | {fmt(percentile(last, 0.5), 1)} | "
            f"{max_rss:.1f} | {fmt(growth, 2)} | {'estimated' if estimated else 'no'} |"
        )
        dump[label] = {"sentences": total_sentences, "calls": calls, "read_s": read_s,
                       "estimated": estimated, "max_rss_mib": max_rss}
    print("\n".join(lines))
    dump["server_env"] = dict(SERVER_ENV)
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump(dump, handle, indent=1)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="BENCH-S stress probe.")
    parser.add_argument("--mode", required=True, choices=["probe", "read", "aggregate"])
    parser.add_argument("--binary", default="")
    parser.add_argument("--out", required=True)
    parser.add_argument("--state-root", default="stress-state")
    parser.add_argument("--input", action="append", default=[], help="label=path (probe)")
    parser.add_argument("--caps", action="store_true", help="run the memory-cap rows (probe)")
    parser.add_argument("--deadline-minutes", type=float, default=150.0)
    parser.add_argument("--label", default="")
    parser.add_argument("--source", default="")
    parser.add_argument("--shard", type=int, default=0)
    parser.add_argument("--shards", type=int, default=1)
    parser.add_argument("--shards-dir", default="")
    parser.add_argument("--env", action="append", default=[], metavar="KEY=VALUE",
                        help="extra server env var, repeatable")
    args = parser.parse_args()
    for entry in args.env:
        key, sep, value = entry.partition("=")
        if not sep or not key:
            parser.error(f"--env expects KEY=VALUE, got {entry!r}")
        SERVER_ENV[key] = value
    if args.mode == "probe":
        return probe_mode(args)
    if args.mode == "read":
        return read_mode(args)
    return aggregate_mode(args)


if __name__ == "__main__":
    raise SystemExit(main())
