#!/usr/bin/env python3
"""Replay two sequential-reading tasks with both tool sets and compare the cost.

The two tasks are the ones a reader actually runs:

  T1  read the first section that has body text to its end, one sentence per call.
  T2  start in the middle of the book and read 500 sentences, one per call, crossing sections.

Both are replayed without a model against the same binary:

  v1  server ``READING_MCP_TOOLS=v1``: ``open_document``, ``get_text_units`` with
      ``next_cursor``, and a ``get_document_structure`` call to find the next section
      whenever a section ends, as a v1 client needs.
  v2  server ``READING_MCP_TOOLS=v2``: ``open`` and ``read``, following ``next``.

Per task and tool set it records calls, request and response tokens, the body share of the
response, ``tools/list`` tokens times calls, and server milliseconds.  Tokens are the same
rough heuristic the sequential probe uses: ``ascii_chars / 4 + other_chars / 1.5``.

The v2 task 2 starting point is addressed directly by its documented section handle
``d<key>~<chk>.s<section_key>``: ``open`` reports the section count, so the middle section is
``s<count / 2>`` and ``read`` skips forward to the next section that has items.

Standard library only.  It never builds anything: pass an existing release binary.
"""

from __future__ import annotations

import argparse
import importlib.util
import os
import sys
import time

PROBE_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "sequential-read-probe.py")


def load_probe():
    """Load ``sequential-read-probe.py`` (its name is not importable) and reuse its plumbing."""
    spec = importlib.util.spec_from_file_location("sequential_read_probe", PROBE_FILE)
    if spec is None or spec.loader is None:  # pragma: no cover - packaging failure
        raise RuntimeError(f"cannot load {PROBE_FILE}")
    module = importlib.util.module_from_spec(spec)
    sys.modules["sequential_read_probe"] = module
    spec.loader.exec_module(module)
    return module


probe = load_probe()

SENTENCES_T2 = 500
V1 = "v1"
V2 = "v2"


class TaskResult:
    """The cost of one task under one tool set."""

    def __init__(self, task: str, toolset: str) -> None:
        self.task = task
        self.toolset = toolset
        self.calls = 0
        self.request_tokens = 0.0
        self.response_tokens = 0.0
        self.body_tokens = 0.0
        self.server_ms = 0.0
        self.sentences = 0
        self.error: str | None = None

    def add(self, name: str, arguments: dict, result: dict, wall_ms: float) -> None:
        self.calls += 1
        self.request_tokens += probe.est_tokens(
            probe.compact_json({"name": name, "arguments": arguments})
        )
        measurement = probe.measure_result(result)
        self.response_tokens += measurement["est_tokens"]
        self.server_ms += wall_ms
        payload = probe.tool_payload(result)
        items = (payload or {}).get("items") if isinstance(payload, dict) else None
        for item in items or []:
            if not isinstance(item, dict):
                continue
            text = item.get("text")
            if isinstance(text, str):
                self.body_tokens += probe.est_tokens(text)
                self.sentences += 1

    def total_tokens(self, tools_list_tokens: float) -> float:
        return (
            self.request_tokens
            + self.response_tokens
            + tools_list_tokens * self.calls
        )

    def extra_tokens_per_sentence(self) -> float | None:
        if not self.sentences:
            return None
        return (self.response_tokens - self.body_tokens + self.request_tokens) / self.sentences

    def body_share_pct(self) -> float | None:
        if not self.response_tokens:
            return None
        return 100.0 * self.body_tokens / self.response_tokens


def tool_arguments(**kwargs) -> dict:
    return {key: value for key, value in kwargs.items() if value is not None}


def call(server, name: str, arguments: dict, deadline: float, record: TaskResult) -> dict:
    result, wall_ms = probe.timed_tool(
        server, name, arguments, probe.deadline_timeout(deadline)
    )
    record.add(name, arguments, result, wall_ms)
    payload = probe.tool_payload(result)
    if not isinstance(payload, dict):
        raise probe.ProbeError(f"{name} returned no JSON object")
    return payload


def open_v1(server, path: str, deadline: float, record: TaskResult) -> str:
    payload = call(server, "open_document", {"source": path}, deadline, record)
    document_id = payload.get("document_id")
    if not isinstance(document_id, str):
        raise probe.ProbeError("open_document returned no document_id")
    return document_id


def open_v2(server, path: str, deadline: float, record: TaskResult) -> dict:
    return call(server, "open", {"source": path}, deadline, record)


def v1_sections(server, document_id: str, deadline: float, record: TaskResult) -> list[dict]:
    """The whole section order, in v1 body order, one structure enumeration."""
    sections: dict[str, dict] = {}
    cursor = None
    while True:
        arguments = {"document_id": document_id, "max_nodes": 500}
        if cursor is not None:
            arguments["cursor"] = cursor
        payload = call(server, "get_document_structure", arguments, deadline, record)
        for node in flatten(payload.get("sections")):
            sections.setdefault(node["section_id"], node)
        cursor = payload.get("next_cursor")
        if not cursor or payload.get("complete") is True:
            break
    return sorted(sections.values(), key=lambda node: node.get("body_order", 0))


def flatten(nodes) -> list[dict]:
    found: list[dict] = []
    if not isinstance(nodes, list):
        return found
    for node in nodes:
        if not isinstance(node, dict):
            continue
        found.append(node)
        found.extend(flatten(node.get("children")))
    return found


def v1_read_units(
    server,
    document_id: str,
    section_id: str,
    deadline: float,
    record: TaskResult,
    budget: dict,
    counter: dict,
) -> bool:
    """Read one section one item per call. Returns True when the section was non-empty."""
    cursor = None
    empty = True
    while True:
        arguments = tool_arguments(
            document_id=document_id,
            section_id=section_id,
            requested_kind="sentence",
            direction="forward",
            coverage_policy="preserve_source",
            max_items=1,
            cursor=cursor,
        )
        payload = call(server, "get_text_units", arguments, deadline, record)
        items = payload.get("items") or []
        if items:
            empty = False
            counter["items"] += len(items)
        if payload.get("complete"):
            return not empty
        cursor = payload.get("next_cursor")
        if not cursor:
            return not empty
        if budget["limit"] is not None and counter["items"] >= budget["limit"]:
            return not empty


def v1_task1(server, document_id: str, sections: list[dict], deadline: float) -> TaskResult:
    record = TaskResult("T1", V1)
    counter = {"items": 0}
    for section in sections:
        before = counter["items"]
        v1_read_units(
            server,
            document_id,
            section["section_id"],
            deadline,
            record,
            {"limit": None},
            counter,
        )
        if counter["items"] > before:
            # The first section with body text was read to its end.
            return record
    return record


def v1_task2(
    server, document_id: str, sections: list[dict], deadline: float
) -> TaskResult:
    record = TaskResult("T2", V1)
    counter = {"items": 0}
    if not sections:
        return record
    index = len(sections) // 2
    while index < len(sections) and counter["items"] < SENTENCES_T2:
        v1_read_units(
            server,
            document_id,
            sections[index]["section_id"],
            deadline,
            record,
            {"limit": SENTENCES_T2},
            counter,
        )
        if counter["items"] >= SENTENCES_T2:
            break
        # A section ended: v1 has to ask for the structure again to find the next one.
        sections = v1_sections(server, document_id, deadline, record)
        if index >= len(sections):
            break
        index += 1
    return record


def v2_task1(server, doc_handle: str, deadline: float) -> TaskResult:
    record = TaskResult("T1", V2)
    from_handle = doc_handle
    first = True
    while True:
        payload = call(server, "read", {"from": from_handle}, deadline, record)
        sections = payload.get("sections") or []
        if not first and sections:
            # A new section started, so the first section with body text ended.
            return record
        first = False
        next_cursor = payload.get("next")
        if not next_cursor:
            return record
        from_handle = next_cursor


def v2_task2(server, opened: dict, deadline: float) -> TaskResult:
    record = TaskResult("T2", V2)
    doc = opened.get("doc")
    sections = opened.get("sections") or 0
    if not isinstance(doc, str) or not isinstance(sections, int) or sections < 1:
        record.error = "open returned no doc handle or section count"
        return record
    middle = max(0, min(sections - 1, sections // 2))
    from_handle = f"{doc}.s{middle}"
    while record.sentences < SENTENCES_T2:
        payload = call(server, "read", {"from": from_handle}, deadline, record)
        next_cursor = payload.get("next")
        if not next_cursor:
            break
        from_handle = next_cursor
    return record


def measure_tools_list(binary: str, state_root: str, toolset: str, seconds: float) -> float:
    """The `tools/list` token footprint of one tool set."""
    state = probe.fresh_directory(os.path.join(state_root, f"tools-list-{toolset}"))
    server = None
    try:
        server = probe.Server(
            binary,
            probe.server_environment(state_root, state),
            os.path.join(state, "server.log"),
            request_timeout=max(30.0, seconds),
            cwd=state_root,
        )
        server.initialize(timeout=max(30.0, seconds))
        result = server.tools_list(timeout=max(30.0, seconds))
        return probe.est_tokens(probe.compact_json(result))
    except Exception:  # noqa: BLE001 - a missing footprint must not hide the tasks
        return 0.0
    finally:
        if server is not None:
            server.close()


def run_toolset(
    binary: str,
    toolset: str,
    book: str,
    path: str,
    state_root: str,
    seconds: float,
) -> dict:
    state = probe.fresh_directory(
        os.path.join(state_root, f"{book}-{toolset}")
    )
    result = {"book": book, "toolset": toolset, "tasks": [], "error": None}
    server = None
    try:
        environment = probe.server_environment(state, state)
        environment["READING_MCP_TOOLS"] = toolset
        server = probe.Server(
            binary,
            environment,
            os.path.join(state, "server.log"),
            request_timeout=max(30.0, seconds),
            cwd=os.path.dirname(os.path.abspath(path)),
        )
        server.initialize(timeout=max(30.0, seconds))
        deadline = time.monotonic() + seconds
        if toolset == V1:
            record = TaskResult("open", V1)
            document_id = open_v1(server, os.path.abspath(path), deadline, record)
            sections = v1_sections(server, document_id, deadline, record)
            t1 = v1_task1(server, document_id, sections, deadline)
            t2 = v1_task2(server, document_id, sections, deadline)
            # Charge the open cost to both tasks: every run has to open the book once.
            for task in (t1, t2):
                task.calls += record.calls
                task.request_tokens += record.request_tokens
                task.response_tokens += record.response_tokens
                task.server_ms += record.server_ms
        else:
            record = TaskResult("open", V2)
            opened = open_v2(server, os.path.abspath(path), deadline, record)
            t1 = v2_task1(server, opened["doc"], deadline)
            t2 = v2_task2(server, opened, deadline)
            for task in (t1, t2):
                task.calls += record.calls
                task.request_tokens += record.request_tokens
                task.response_tokens += record.response_tokens
                task.server_ms += record.server_ms
        result["tasks"] = [t1, t2]
    except probe.ProbeError as exc:
        result["error"] = str(exc)
    except Exception as exc:  # noqa: BLE001 - one book must not hide the others
        result["error"] = f"{type(exc).__name__}: {exc}"
    finally:
        if server is not None:
            server.close()
    return result


def fmt(value, digits: int = 1) -> str:
    if value is None:
        return "-"
    if isinstance(value, float):
        return f"{value:.{digits}f}"
    return str(value)


def table(rows: list[dict], tools_tokens: dict[str, float]) -> str:
    lines = [
        "| Book | Task | Tools | Calls | Sentences | Request tokens | Response tokens | "
        "Body share % | tools/list tokens x calls | Server ms | Total tokens | Extra tokens/sentence |",
        "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for row in rows:
        for task in row["tasks"]:
            tools_tokens_value = tools_tokens.get(task.toolset, 0.0)
            lines.append(
                "| {book} | {task} | {tools} | {calls} | {sentences} | {request} | {response} | "
                "{body} | {defs} | {ms} | {total} | {extra} |".format(
                    book=row["book"],
                    task=task.task,
                    tools=task.toolset,
                    calls=task.calls,
                    sentences=task.sentences,
                    request=fmt(task.request_tokens),
                    response=fmt(task.response_tokens),
                    body=fmt(task.body_share_pct()),
                    defs=fmt(tools_tokens_value * task.calls),
                    ms=fmt(task.server_ms, 0),
                    total=fmt(task.total_tokens(tools_tokens_value)),
                    extra=fmt(task.extra_tokens_per_sentence()),
                )
            )
    return "\n".join(lines)


def ratio(v2_tokens: float, v1_tokens: float) -> float | None:
    if not v1_tokens:
        return None
    return 100.0 * v2_tokens / v1_tokens


def pass_lines(rows: list[dict], tools_tokens: dict[str, float]) -> tuple[list[str], dict]:
    lines: list[str] = []
    summary: dict = {"ratios": {}, "checks": []}
    for row in rows:
        by_task: dict[str, dict[str, TaskResult]] = {}
        for task in row["tasks"]:
            by_task.setdefault(task.task, {})[task.toolset] = task
        for name in ("T1", "T2"):
            pair = by_task.get(name) or {}
            left = pair.get(V1)
            right = pair.get(V2)
            if left is None or right is None:
                continue
            v1_total = left.total_tokens(tools_tokens.get(V1, 0.0))
            v2_total = right.total_tokens(tools_tokens.get(V2, 0.0))
            percent = ratio(v2_total, v1_total)
            ok = percent is not None and percent <= 20.0
            summary["ratios"][f"{row['book']}-{name}"] = percent
            lines.append(
                "{} {book} {task} total tokens: v2 {v2} vs v1 {v1} = {pct}% (limit 20%)".format(
                    "PASS" if ok else "FAIL",
                    book=row["book"],
                    task=name,
                    v2=fmt(v2_total, 0),
                    v1=fmt(v1_total, 0),
                    pct=fmt(percent),
                )
            )
            summary["checks"].append(ok)
            ok = right.calls <= left.calls
            lines.append(
                "{} {book} {task} calls: v2 {v2} vs v1 {v1}".format(
                    "PASS" if ok else "FAIL",
                    book=row["book"],
                    task=name,
                    v2=right.calls,
                    v1=left.calls,
                )
            )
            summary["checks"].append(ok)
            extra = right.extra_tokens_per_sentence()
            ok = extra is not None and extra <= 30.0
            lines.append(
                "{} {book} {task} v2 extra tokens per sentence: {extra} (limit 30)".format(
                    "PASS" if ok else "FAIL",
                    book=row["book"],
                    task=name,
                    extra=fmt(extra),
                )
            )
            summary["checks"].append(ok)
    summary["all_pass"] = all(summary["checks"]) if summary["checks"] else False
    return lines, summary


def parse_documents(spec: str) -> list[tuple[str, str]]:
    return probe.parse_documents(spec)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Replay the two sequential-reading tasks with both tool sets."
    )
    parser.add_argument("--binary", required=True, help="reading-mcp binary to run")
    parser.add_argument(
        "--documents",
        default="",
        help="comma-separated label=path books (zh-large, en-novel)",
    )
    parser.add_argument("--out", required=True, help="raw JSON output path")
    parser.add_argument(
        "--max-seconds-per-task",
        type=float,
        default=900.0,
        help="time budget for each task (default 900)",
    )
    parser.add_argument(
        "--state-dir",
        default=None,
        help="base state directory; a fresh run subdirectory is created inside it",
    )
    args = parser.parse_args()

    try:
        documents = parse_documents(args.documents)
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    binary = os.path.abspath(args.binary)
    if not os.path.isfile(binary):
        print(f"error: binary not found: {binary}", file=sys.stderr)
        return 2

    stamp = time.strftime("%Y%m%d-%H%M%S", time.gmtime())
    base = args.state_dir or os.path.join(os.getcwd(), ".tmp", "v2-eval")
    state_root = os.path.join(base, f"run-{os.getpid()}-{stamp}")
    os.makedirs(state_root, exist_ok=True)

    tools_tokens = {
        toolset: measure_tools_list(
            binary, state_root, toolset, args.max_seconds_per_task
        )
        for toolset in (V1, V2)
    }

    rows: list[dict] = []
    for label, path in documents:
        if not os.path.isfile(path):
            print(f"skipping {label}: {path} is missing", file=sys.stderr)
            continue
        for toolset in (V1, V2):
            print(f"replaying {label} with {toolset}", file=sys.stderr)
            rows.append(
                run_toolset(
                    binary,
                    toolset,
                    label,
                    path,
                    state_root,
                    args.max_seconds_per_task,
                )
            )

    lines, summary = pass_lines(rows, tools_tokens)
    print("## V2 evaluation (T1/T2)")
    print()
    print(
        "Token estimates are a rough heuristic: ascii_chars / 4 + non_ascii_chars / 1.5. "
        "Request tokens include the tool name and arguments of every call; total tokens add "
        "`tools/list` tokens multiplied by the number of calls."
    )
    print()
    print(f"tools/list tokens: v1={tools_tokens.get(V1, 0.0):.0f} v2={tools_tokens.get(V2, 0.0):.0f}")
    print()
    print(table(rows, tools_tokens))
    print()
    for line in lines:
        print(line)
    print()
    for row in rows:
        if row.get("error"):
            print(f"error: {row['book']} {row['toolset']}: {row['error']}")

    probe.dump_output(
        args.out,
        {
            "tools_list_tokens": tools_tokens,
            "rows": [
                {
                    "book": row["book"],
                    "toolset": row["toolset"],
                    "error": row["error"],
                    "tasks": [
                        {
                            "task": task.task,
                            "calls": task.calls,
                            "sentences": task.sentences,
                            "request_tokens": task.request_tokens,
                            "response_tokens": task.response_tokens,
                            "body_tokens": task.body_tokens,
                            "server_ms": task.server_ms,
                        }
                        for task in row["tasks"]
                    ],
                }
                for row in rows
            ],
            "summary": summary,
        },
    )
    return 0 if summary.get("all_pass") else 1


if __name__ == "__main__":
    sys.exit(main())
