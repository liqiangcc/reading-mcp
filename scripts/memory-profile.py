#!/usr/bin/env python3
"""Measure the peak RSS of `open_document` in reading-mcp over newline-delimited JSON-RPC.

Standard library only. Builds deterministic ASCII documents of the requested sizes, runs the
release binary once per (state mode, size) plus a baseline of each mode, and reports each run's
peak RSS (`os.wait4` rusage), wall time, throughput, and how many extra copies of the document's
bytes the peak implies over that mode's baseline.

Besides the ASCII sizes, additional inputs can be measured: deterministic Chinese prose of a
given size (`--cjk-sizes-mb`), a synthetic English Markdown file with many sections
(`--markdown-sections`), and arbitrary existing files (`--documents label=path`). Every row
carries a `label`, the `sections` count reported by `open_document`, and the first 30 characters
of its `title`.

Each run is bounded by `--run-timeout-seconds`: if `open_document` does not answer within that
deadline the server is killed, its rusage is still collected, and the run is recorded as a
timeout. Rows are printed and the `--out` file is rewritten after every run so partial results
survive a job timeout. Once a size times out or fails, the larger sizes for that state are
skipped.

With `--reopen`, every successful input is opened a second time by a new server process on the
same state directory, which measures the restart-and-reopen case. The row then gains `Reopen s`
and `Reopen extra KiB` columns; without the flag the table is unchanged.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import random
import shlex
import subprocess
import sys
import tempfile
import threading
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

# A fixed pool of about 200 common Chinese characters used for synthetic CJK prose. The pool is
# a constant so that the generated documents are byte-for-byte reproducible.
CJK_CHARACTERS = (
    "的一是了我不人在他有这个上们来到时大地为子中你说生国年着就那和要她出也得里后自以会家可下而过"
    "天去能对小多然于心学么之都好看起发当没成只如事把还用第样道想作种开美总从无情己面最女但现前些"
    "所同日手又行意动方期它头经长儿回位分爱老因很给名法间斯知世什两次使身者被高已亲其进此话常与活"
    "正感金玉光明显达理思义信德智勇忠孝节仁礼山林川河湖海森木火土风雷雨雪云雾花鸟鱼虫龙凤虎豹马牛"
    "羊鸡犬禾苗稻麦米谷刀剑弓箭车船房屋门窗户墙壁桌椅床笔墨纸砚书画琴棋诗酒茶"
)
CJK_SENTENCE_ENDINGS = "。？！"


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


def _trim_utf8_to_boundary(data: bytes, limit: int) -> bytes:
    """Return the longest prefix of `data` no longer than `limit` ending on a character boundary."""
    if len(data) <= limit:
        return data
    end = limit
    while end > 0 and (data[end] & 0xC0) == 0x80:
        end -= 1
    return data[:end]


def write_cjk_document(path: str, size_bytes: int) -> None:
    """Write deterministic Chinese prose as a UTF-8 `.txt` of exactly `size_bytes` bytes."""
    rng = random.Random(0xC1C1E)
    chunks: list[str] = []
    total = 0
    while total < size_bytes:
        paragraph = "".join(
            "".join(rng.choice(CJK_CHARACTERS) for _ in range(rng.randint(8, 30)))
            + rng.choice(CJK_SENTENCE_ENDINGS)
            for _ in range(rng.randint(4, 8))
        )
        block = paragraph + "\n\n"
        chunks.append(block)
        total += len(block.encode("utf-8"))
    data = _trim_utf8_to_boundary("".join(chunks).encode("utf-8"), size_bytes)
    with open(path, "wb") as handle:
        handle.write(data)
        gap = size_bytes - len(data)
        if gap > 0:
            handle.write(b"\n" * gap)


def write_markdown_document(path: str, sections: int) -> None:
    """Write one deterministic English Markdown file with `sections` `##` sections."""
    lines = ["# Synthetic Benchmark Document", ""]
    for index in range(1, sections + 1):
        lines.append(f"## Section {index}")
        lines.append("")
        for paragraph_index in range(1, 4):
            lines.append(
                f"Paragraph {index}-{paragraph_index} of section {index} carries deterministic "
                f"filler prose so that the benchmark document is reproducible and unique."
            )
            lines.append("")
    with open(path, "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines))


def server_environment(roots: list[str], state_dir: str, parse_timeout: float | None) -> dict:
    env = dict(os.environ)
    env["READING_MCP_LOCAL_ROOTS"] = os.pathsep.join(roots)
    env["READING_MCP_TELEMETRY"] = "false"
    env["READING_MCP_MAX_DOCUMENT_BYTES"] = "134217728"
    env["READING_MCP_MAX_NORMALIZED_CHARS"] = "200000000"
    env["READING_MCP_PARSE_TIMEOUT_SECS"] = (
        "600" if parse_timeout is None else f"{parse_timeout:g}"
    )
    env["READING_MCP_STATE_DIR"] = state_dir
    return env


def send(process: subprocess.Popen, message: dict) -> None:
    assert process.stdin is not None
    process.stdin.write((json.dumps(message) + "\n").encode("utf-8"))
    process.stdin.flush()


def pump_stdout(stream, responses: queue.Queue) -> None:
    """Read JSON-RPC responses on a background thread so callers never block past a deadline."""
    try:
        for line in stream:
            try:
                response = json.loads(line.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError):
                continue
            responses.put(response)
    except Exception:  # noqa: BLE001 - draining is best effort
        pass
    finally:
        responses.put(None)


def read_response(responses: queue.Queue, awaited_id: int, timeout: float | None) -> dict:
    deadline = None if timeout is None else time.monotonic() + timeout
    while True:
        if deadline is None:
            wait: float | None = None
        else:
            wait = deadline - time.monotonic()
            if wait <= 0:
                raise TimeoutError(f"timed out waiting for response to id {awaited_id}")
        try:
            item = responses.get(timeout=wait) if wait is not None else responses.get()
        except queue.Empty:
            raise TimeoutError(f"timed out waiting for response to id {awaited_id}") from None
        if item is None:
            raise RuntimeError(f"server closed stdout before responding to id {awaited_id}")
        if isinstance(item, dict) and item.get("id") == awaited_id:
            return item


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


def extract_metadata(response: dict) -> tuple[object | None, str | None]:
    """Return `(section_count, title_truncated)` from a tool result's structured content."""
    result = response.get("result")
    if not isinstance(result, dict):
        return None, None
    structured = result.get("structuredContent")
    if not isinstance(structured, dict):
        structured = result.get("structured_content")
    if not isinstance(structured, dict):
        return None, None
    sections = structured.get("section_count")
    title = structured.get("title")
    if isinstance(title, str):
        title = title[:30]
    else:
        title = None
    return sections, title


def server_command(binary: str, wrapper: str | None, label: str) -> list[str]:
    """Build the server command, applying the optional wrapper prefix and its {label}."""
    if not wrapper:
        return [binary]
    parts = [part.replace("{label}", label) for part in shlex.split(wrapper)]
    return parts + [binary]


def run_one(
    command: list[str], env: dict, source: str | None, run_timeout: float
) -> tuple[int, float | None, str | None, bool, object | None, str | None]:
    """Run one open_document call.

    Returns (peak_rss_kib, open_seconds, error, timed_out, section_count, title).
    """
    process = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        env=env,
    )
    responses: queue.Queue = queue.Queue()
    reader = threading.Thread(target=pump_stdout, args=(process.stdout, responses), daemon=True)
    reader.start()

    open_seconds: float | None = None
    error: str | None = None
    timed_out = False
    section_count: object | None = None
    title: str | None = None
    usage = None
    try:
        send(process, INITIALIZE_MESSAGE)
        initialized = read_response(responses, 1, run_timeout)
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
            try:
                response = read_response(responses, 2, run_timeout)
            except TimeoutError:
                timed_out = True
                open_seconds = float(run_timeout)
                error = f"timeout after {run_timeout:g}s"
                process.kill()
            else:
                open_seconds = time.monotonic() - start
                error = extract_error(response)
                section_count, title = extract_metadata(response)
    except TimeoutError:
        timed_out = True
        open_seconds = float(run_timeout)
        error = f"timeout after {run_timeout:g}s"
        process.kill()
    except Exception as exc:  # noqa: BLE001 - the failure is recorded, not swallowed
        error = f"{type(exc).__name__}: {exc}"
    finally:
        try:
            if process.stdin is not None:
                process.stdin.close()
        except Exception:  # noqa: BLE001 - server may already be gone
            pass
        try:
            _, status, usage = os.wait4(process.pid, 0)
            process.returncode = os.waitstatus_to_exitcode(status)
        except ChildProcessError:
            usage = None
            process.returncode = 0
        reader.join(timeout=1.0)
    peak_rss_kib = usage.ru_maxrss if usage is not None else 0
    return peak_rss_kib, open_seconds, error, timed_out, section_count, title


def format_number(value: float | None, digits: int = 2) -> str:
    return "-" if value is None else f"{value:.{digits}f}"


def table_cell(value: object) -> str:
    text = "-" if value is None else str(value)
    return text.replace("|", "\\|").replace("\n", " ")


def markdown_table(rows: list[dict], reopen: bool = False) -> str:
    header = (
        "| State | Label | Size MiB | Sections | Title | Peak RSS KiB | Open s | MiB/s | "
        "Extra KiB | Copies | OK | Error |\n"
        "| --- | --- | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | :--: | --- |"
    )
    if reopen:
        header = (
            "| State | Label | Size MiB | Sections | Title | Peak RSS KiB | Open s | MiB/s | "
            "Extra KiB | Copies | OK | Error | Reopen s | Reopen extra KiB |\n"
            "| --- | --- | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | :--: | --- "
            "| ---: | ---: |"
        )
    lines = [header]
    for row in rows:
        text = (
            "| {state} | {label} | {size} | {sections} | {title} | {peak} | {open} | {rate} | "
            "{extra} | {copies} | {ok} | {error} |".format(
                state=table_cell(row["state"]),
                label=table_cell(row.get("label")),
                size=table_cell(row["size_mb"]),
                sections=table_cell(row.get("sections")),
                title=table_cell(row.get("title")),
                peak=row["peak_rss_kib"],
                open=format_number(row["open_seconds"]),
                rate=format_number(row.get("mib_per_second")),
                extra=row.get("extra_over_baseline_kib", "-"),
                copies=format_number(row.get("copies")),
                ok="yes" if row["ok"] else "no",
                error=table_cell(row["error"]),
            )
        )
        if reopen:
            text += " {seconds} | {extra} |".format(
                seconds=format_number(row.get("reopen_seconds")),
                extra=table_cell(row.get("reopen_extra_kib", "-")),
            )
        lines.append(text)
    return "\n".join(lines)


def dump_rows(path: str, rows: list[dict]) -> None:
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(rows, handle, indent=2)
        handle.write("\n")


def log_row(row: dict) -> None:
    print(
        "row state={state} label={label} size_mb={size} sections={sections} peak_rss_kib={peak} "
        "open_seconds={open} ok={ok} error={error}".format(
            state=row["state"],
            label=row.get("label"),
            size=row["size_mb"],
            sections=row.get("sections"),
            peak=row["peak_rss_kib"],
            open=format_number(row.get("open_seconds")),
            ok="yes" if row["ok"] else "no",
            error=row["error"] or "-",
        ),
        flush=True,
    )


def parse_floats(spec: str) -> list[float]:
    return sorted(float(value) for value in spec.split(",") if value.strip())


def parse_documents(spec: str) -> list[tuple[str, str]]:
    entries: list[tuple[str, str]] = []
    for item in spec.split(","):
        item = item.strip()
        if not item:
            continue
        if "=" not in item:
            raise ValueError(f"--documents entry must be label=path: {item}")
        label, path = item.split("=", 1)
        entries.append((label.strip(), os.path.abspath(path.strip())))
    return entries


def make_row(
    state: str,
    label: str,
    size_mb: float,
    peak: int,
    open_seconds: float | None,
    sections: object | None,
    title: str | None,
    error: str | None,
    timed_out: bool,
    baseline_peak: int,
) -> dict:
    ok = error is None and not timed_out
    row = {
        "state": state,
        "label": label,
        "size_mb": size_mb,
        "sections": sections if sections is not None else "-",
        "title": title if title else "-",
        "peak_rss_kib": peak,
        "open_seconds": open_seconds,
        "ok": ok,
        "error": error,
        "fatal": (not ok) and not timed_out,
    }
    if ok:
        extra = peak - baseline_peak
        row["extra_over_baseline_kib"] = extra
        if size_mb:
            row["copies"] = round(extra / (size_mb * 1024), 2)
        if open_seconds and size_mb:
            row["mib_per_second"] = round(size_mb / open_seconds, 2)
    return row


def run_input_group(
    state: str,
    env: dict,
    binary: str,
    wrapper: str | None,
    run_timeout: float,
    inputs: list[dict],
    baseline_peak: int,
    skip_on_failure: bool,
    rows: list[dict],
    out_path: str,
    reopen: bool = False,
) -> None:
    skip = False
    for entry in inputs:
        if skip:
            row = {
                "state": state,
                "label": entry["label"],
                "size_mb": entry["size_mb"],
                "sections": "-",
                "title": "-",
                "peak_rss_kib": 0,
                "open_seconds": None,
                "ok": False,
                "error": "skipped after smaller size failed",
                "fatal": False,
            }
            rows.append(row)
            log_row(row)
            dump_rows(out_path, rows)
            continue

        command = server_command(binary, wrapper, entry["label"])
        peak, open_seconds, error, timed_out, sections, title = run_one(
            command, env, entry["source"], run_timeout
        )
        row = make_row(
            state,
            entry["label"],
            entry["size_mb"],
            peak,
            open_seconds,
            sections,
            title,
            error,
            timed_out,
            baseline_peak,
        )
        if reopen and row["ok"]:
            # A second server process on the same state directory: the restart-and-reopen case.
            reopen_peak, reopen_seconds, reopen_error, _, _, _ = run_one(
                command, env, entry["source"], run_timeout
            )
            row["reopen_seconds"] = reopen_seconds if reopen_error is None else None
            row["reopen_extra_kib"] = (
                reopen_peak - baseline_peak if reopen_error is None else None
            )
        rows.append(row)
        log_row(row)
        dump_rows(out_path, rows)
        if not row["ok"] and skip_on_failure:
            skip = True


def main() -> int:
    parser = argparse.ArgumentParser(description="Profile open_document peak memory.")
    parser.add_argument("--binary", required=True)
    parser.add_argument("--sizes-mb", default="")
    parser.add_argument("--out", required=True)
    parser.add_argument("--run-timeout-seconds", type=float, default=480.0)
    parser.add_argument("--states", default="memory,dir")
    parser.add_argument("--wrapper", default=None)
    parser.add_argument("--documents", default="")
    parser.add_argument("--cjk-sizes-mb", default="")
    parser.add_argument("--markdown-sections", type=int, default=None)
    parser.add_argument("--parse-timeout-seconds", type=float, default=None)
    parser.add_argument(
        "--reopen",
        action="store_true",
        help="also open each successful input again in a new process on the same state dir",
    )
    args = parser.parse_args()

    sizes = parse_floats(args.sizes_mb)
    cjk_sizes = parse_floats(args.cjk_sizes_mb)
    states = [value.strip() for value in args.states.split(",") if value.strip()]
    try:
        document_entries = parse_documents(args.documents)
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 2
    run_timeout = args.run_timeout_seconds
    binary = os.path.abspath(args.binary)
    if not os.path.exists(binary):
        print(f"binary not found: {binary}", file=sys.stderr)
        return 2

    rows: list[dict] = []
    with tempfile.TemporaryDirectory(prefix="memory-profile-") as root:
        size_documents: dict[float, str] = {}
        for size in sizes:
            path = os.path.join(root, f"doc-{size:g}mb.txt")
            write_document(path, round(size * MIB))
            size_documents[size] = os.path.abspath(path)

        cjk_documents: dict[float, str] = {}
        for size in cjk_sizes:
            path = os.path.join(root, f"cjk-{size:g}mb.txt")
            write_cjk_document(path, round(size * MIB))
            cjk_documents[size] = os.path.abspath(path)

        markdown_path: str | None = None
        if args.markdown_sections is not None:
            path = os.path.join(root, f"markdown-{args.markdown_sections}.md")
            write_markdown_document(path, args.markdown_sections)
            markdown_path = os.path.abspath(path)

        roots = [root]
        for _, path in document_entries:
            parent = os.path.dirname(path)
            if parent not in roots:
                roots.append(parent)

        for state in states:
            if state == "memory":
                state_dir = "memory"
            else:
                state_dir = os.path.abspath(tempfile.mkdtemp(prefix="state-dir-", dir=root))
            env = server_environment(roots, state_dir, args.parse_timeout_seconds)

            baseline_label = f"{state}-baseline"
            baseline_command = server_command(binary, args.wrapper, baseline_label)
            baseline_peak, _, baseline_error, baseline_timed_out, _, _ = run_one(
                baseline_command, env, None, run_timeout
            )
            baseline_row = make_row(
                state,
                baseline_label,
                0,
                baseline_peak,
                None,
                None,
                None,
                baseline_error,
                baseline_timed_out,
                baseline_peak,
            )
            rows.append(baseline_row)
            log_row(baseline_row)
            dump_rows(args.out, rows)

            size_inputs = [
                {"label": f"{state}-{size:g}mib", "size_mb": size, "source": size_documents[size]}
                for size in sizes
            ]
            run_input_group(
                state, env, binary, args.wrapper, run_timeout, size_inputs,
                baseline_peak, True, rows, args.out, args.reopen,
            )

            if cjk_documents:
                cjk_inputs = [
                    {"label": f"cjk-{size:g}mib", "size_mb": size, "source": cjk_documents[size]}
                    for size in cjk_sizes
                ]
                run_input_group(
                    state, env, binary, args.wrapper, run_timeout, cjk_inputs,
                    baseline_peak, True, rows, args.out, args.reopen,
                )

            if markdown_path is not None:
                markdown_inputs = [
                    {
                        "label": f"md-{args.markdown_sections}",
                        "size_mb": round(os.path.getsize(markdown_path) / MIB, 2),
                        "source": markdown_path,
                    }
                ]
                run_input_group(
                    state, env, binary, args.wrapper, run_timeout, markdown_inputs,
                    baseline_peak, False, rows, args.out, args.reopen,
                )

            if document_entries:
                document_inputs = [
                    {
                        "label": label,
                        "size_mb": round(os.path.getsize(path) / MIB, 2),
                        "source": path,
                    }
                    for label, path in document_entries
                ]
                run_input_group(
                    state, env, binary, args.wrapper, run_timeout, document_inputs,
                    baseline_peak, False, rows, args.out, args.reopen,
                )

    table = markdown_table(rows, args.reopen)
    print(table, flush=True)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write(table + "\n")

    fatal = [row for row in rows if row.get("fatal")]
    if fatal:
        for row in fatal:
            print(
                f"FAILED state={row['state']} label={row.get('label')} "
                f"size_mb={row['size_mb']}: {row['error']}",
                file=sys.stderr,
            )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
