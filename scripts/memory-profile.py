#!/usr/bin/env python3
"""Measure the peak RSS of `open_document` in reading-mcp over newline-delimited JSON-RPC.

Standard library only. Builds deterministic ASCII documents of the requested sizes, runs the
release binary once per (state mode, size) plus a baseline of each mode, and reports each run's
peak RSS (`os.wait4` rusage), wall time, and how many extra copies of the document's bytes the
peak implies over that mode's baseline.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
import time

MIB = 1024 * 1024

INITIALIZE_MESSAGE = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "memory-profile", "version": "0"},
    },
}

INITIALIZED_NOTIFICATION = {
    "jsonrpc": "2.0",
    "method": "notifications/initialized",
}

SENTENCES_PER_PARAGRAPH = 5


def sentence(number: int) -> str:
    return (
        f"Sentence number {number} deliberately carries a unique ordinal so that the generated "
        f"prose is deterministic while avoiding a single repeated line in the benchmark document."
    )


def paragraph(index: int) -> str:
    return " ".join(
        sentence(index * SENTENCES_PER_PARAGRAPH + offset + 1)
        for offset in range(SENTENCES_PER_PARAGRAPH)
    )


def write_document(path: str, size_bytes: int) -> None:
    written = 0
    index = 0
    with open(path, "wb") as handle:
        while written < size_bytes:
            block = (paragraph(index) + "\n\n").encode("ascii")
            remaining = size_bytes - written
            if len(block) > remaining:
                handle.write(block[:remaining])
                written = size_bytes
            else:
                handle.write(block)
                written += len(block)
            index += 1


def server_environment(root: str, state_dir: str) -> dict:
    env = dict(os.environ)
    env["READING_MCP_LOCAL_ROOTS"] = root
    env["READING_MCP_TELEMETRY"] = "false"
    env["READING_MCP_MAX_DOCUMENT_BYTES"] = "134217728"
    env["READING_MCP_MAX_NORMALIZED_CHARS"] = "200000000"
    env["READING_MCP_PARSE_TIMEOUT_SECS"] = "600"
    env["READING_MCP_STATE_DIR"] = state_dir
    return env


def send(process: subprocess.Popen, message: dict) -> None:
    assert process.stdin is not None
    process.stdin.write((json.dumps(message) + "\n").encode("utf-8"))
    process.stdin.flush()


def read_response(process: subprocess.Popen, awaited_id: int) -> dict:
    assert process.stdout is not None
    while True:
        line = process.stdout.readline()
        if not line:
            raise RuntimeError(f"server closed stdout before responding to id {awaited_id}")
        try:
            response = json.loads(line.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            continue
        if isinstance(response, dict) and response.get("id") == awaited_id:
            return response


def extract_error(response: dict) -> str | None:
    error = response.get("error")
    if isinstance(error, dict):
        return "JSON-RPC error: " + str(error.get("message", error))
    if error is not None:
        return "JSON-RPC error: " + str(error)
    result = response.get("result")
    if isinstance(result, dict) and result.get("isError"):
        content = result.get("content")
        if isinstance(content, list):
            texts = [
                item.get("text", "")
                for item in content
                if isinstance(item, dict) and item.get("type") == "text"
            ]
            if texts:
                return "tool error: " + " ".join(texts)
        return "tool error: open_document reported an error"
    return None


def run_one(binary: str, env: dict, source: str | None) -> tuple[int, float | None, str | None]:
    process = subprocess.Popen(
        [binary],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        env=env,
    )
    open_seconds: float | None = None
    error: str | None = None
    try:
        send(process, INITIALIZE_MESSAGE)
        initialized = read_response(process, 1)
        error = extract_error(initialized)
        send(process, INITIALIZED_NOTIFICATION)
        if source is not None and error is None:
            message = {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "open_document", "arguments": {"source": source}},
            }
            start = time.monotonic()
            send(process, message)
            response = read_response(process, 2)
            open_seconds = time.monotonic() - start
            error = extract_error(response)
    except Exception as exc:  # noqa: BLE001 - the failure is recorded, not swallowed
        error = f"{type(exc).__name__}: {exc}"
    finally:
        try:
            if process.stdin is not None:
                process.stdin.close()
        except Exception:  # noqa: BLE001 - server may already be gone
            pass
        try:
            if process.stdout is not None:
                for _ in process.stdout:
                    pass
        except Exception:  # noqa: BLE001 - draining is best effort
            pass
        try:
            _, status, usage = os.wait4(process.pid, 0)
            process.returncode = os.waitstatus_to_exitcode(status)
        except ChildProcessError:
            status = 0
            usage = None
            process.returncode = 0
    peak_rss_kib = usage.ru_maxrss if usage is not None else 0
    return peak_rss_kib, open_seconds, error


def format_number(value: float | None, digits: int = 2) -> str:
    return "-" if value is None else f"{value:.{digits}f}"


def markdown_table(rows: list[dict]) -> str:
    header = (
        "| State | Size MiB | Peak RSS KiB | Open s | Extra KiB | Copies | OK | Error |\n"
        "| --- | ---: | ---: | ---: | ---: | ---: | :--: | --- |"
    )
    lines = [header]
    for row in rows:
        lines.append(
            "| {state} | {size} | {peak} | {open} | {extra} | {copies} | {ok} | {error} |".format(
                state=row["state"],
                size=row["size_mb"],
                peak=row["peak_rss_kib"],
                open=format_number(row["open_seconds"]),
                extra=row.get("extra_over_baseline_kib", "-"),
                copies=format_number(row.get("copies")),
                ok="yes" if row["ok"] else "no",
                error=row["error"] or "-",
            )
        )
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description="Profile open_document peak memory.")
    parser.add_argument("--binary", required=True)
    parser.add_argument("--sizes-mb", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    sizes = [int(value) for value in args.sizes_mb.split(",") if value.strip()]
    binary = os.path.abspath(args.binary)
    if not os.path.exists(binary):
        print(f"binary not found: {binary}", file=sys.stderr)
        return 2

    rows: list[dict] = []
    with tempfile.TemporaryDirectory(prefix="memory-profile-") as root:
        documents: dict[int, str] = {}
        for size in sizes:
            path = os.path.join(root, f"doc-{size}mb.txt")
            write_document(path, size * MIB)
            documents[size] = os.path.abspath(path)

        for state in ("memory", "dir"):
            if state == "memory":
                state_dir = "memory"
            else:
                state_dir = os.path.abspath(tempfile.mkdtemp(prefix="state-dir-", dir=root))
            env = server_environment(root, state_dir)

            baseline_peak, _, baseline_error = run_one(binary, env, None)
            rows.append(
                {
                    "state": state,
                    "size_mb": 0,
                    "peak_rss_kib": baseline_peak,
                    "open_seconds": None,
                    "ok": baseline_error is None,
                    "error": baseline_error,
                }
            )

            for size in sizes:
                peak, open_seconds, error = run_one(binary, env, documents[size])
                extra = peak - baseline_peak
                rows.append(
                    {
                        "state": state,
                        "size_mb": size,
                        "peak_rss_kib": peak,
                        "open_seconds": open_seconds,
                        "ok": error is None,
                        "error": error,
                        "extra_over_baseline_kib": extra,
                        "copies": round(extra / (size * 1024), 2),
                    }
                )

    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump(rows, handle, indent=2)
        handle.write("\n")

    table = markdown_table(rows)
    print(table)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write(table + "\n")

    failed = [row for row in rows if not row["ok"]]
    if failed:
        for row in failed:
            print(
                f"FAILED state={row['state']} size_mb={row['size_mb']}: {row['error']}",
                file=sys.stderr,
            )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
