#!/usr/bin/env python3
"""Replay the sequential and navigation tasks with both tool sets and compare the cost.

The tasks are the ones a reader actually runs:

  T1  read the first section that has body text to its end, one sentence per call.
  T2  start in the middle of the book and read 500 sentences, one per call, crossing sections.
  T3  search a term, then read two sentences before and after each of the first three hits.
  T4  find the Nth section in the outline and read its first page.
  T5  get the full citation of three sentences.

T1/T2 are replayed without a model against the same binary:

  v1  server ``READING_MCP_TOOLS=v1``: ``open_document``, ``get_text_units`` with
      ``next_cursor``, and a ``get_document_structure`` call to find the next section
      whenever a section ends, as a v1 client needs.
  v2  server ``READING_MCP_TOOLS=v2``: ``open`` and ``read``, following ``next``.

T3-T5 use the matching v1 tools (``search_document``, ``get_document_structure``,
``get_text_units``) and the v2 tools (``search``, ``outline``, ``read``, ``cite``).

Per task and tool set it records calls, request and response tokens, the body share of the
response, ``tools/list`` tokens times calls, and server milliseconds.  Tokens are the same
rough heuristic the sequential probe uses: ``ascii_chars / 4 + other_chars / 1.5``.

The per-sentence overhead counts the reading calls only (``read`` for v2, ``get_text_units``
for v1): request tokens plus response tokens minus body tokens, divided by the sentences those
calls returned.  The ``open`` step has its own column.

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

# T3 search then read closely; T4 jump via outline; T5 cite.
T3_HITS = 3
T3_BACK = 2
T3_MAX_CHARS = 2_000
T4_DEPTH = 3
T4_LIMIT = 100
T4_INDEX = 20
T4_MAX_CHARS = 1_000
T5_SENTENCES = 3
T5_MAX_CHARS = 400
# The real books the extended tasks run on, keyed by the workflow's labels.
T3_TERMS = {"zh-classic": "寶玉", "en-novel": "Pierre"}
T4_BOOKS = frozenset({"zh-large"})
T5_BOOKS = frozenset({"zh-classic"})
EXTENDED_TASKS = ("T3", "T4", "T5")
# The tools that return reading items: v1 pages them with `get_text_units`, v2 with `read`.
READ_TOOLS = frozenset({"read", "get_text_units"})


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
        self.read_calls = 0
        self.read_request_tokens = 0.0
        self.read_response_tokens = 0.0
        self.read_body_tokens = 0.0
        self.read_sentences = 0
        self.error: str | None = None

    def add(self, name: str, arguments: dict, result: dict, wall_ms: float) -> None:
        self.calls += 1
        request_tokens = probe.est_tokens(
            probe.compact_json({"name": name, "arguments": arguments})
        )
        measurement = probe.measure_result(result)
        response_tokens = measurement["est_tokens"]
        self.request_tokens += request_tokens
        self.response_tokens += response_tokens
        self.server_ms += wall_ms
        reading = name in READ_TOOLS
        if reading:
            self.read_calls += 1
            self.read_request_tokens += request_tokens
            self.read_response_tokens += response_tokens
        for text in item_texts(probe.tool_payload(result)):
            body_tokens = probe.est_tokens(text)
            self.body_tokens += body_tokens
            self.sentences += 1
            if reading:
                self.read_body_tokens += body_tokens
                self.read_sentences += 1

    def total_tokens(self, tools_list_tokens: float) -> float:
        return (
            self.request_tokens
            + self.response_tokens
            + tools_list_tokens * self.calls
        )

    def open_tokens(self) -> float:
        """The tokens of the `open` step on its own, reported in its own column."""
        return self.request_tokens + self.response_tokens

    def extra_tokens_per_sentence(self) -> float | None:
        """Per-sentence overhead of the reading calls alone: no `open`, no tool definitions."""
        if not self.read_sentences:
            return None
        return (
            self.read_request_tokens
            + self.read_response_tokens
            - self.read_body_tokens
        ) / self.read_sentences

    def body_share_pct(self) -> float | None:
        if not self.response_tokens:
            return None
        return 100.0 * self.body_tokens / self.response_tokens


def item_text(item):
    """The text of one item, in the v1 object shape or the v2 `[handle, text, class?]` shape."""
    if isinstance(item, dict):
        text = item.get("text")
        return text if isinstance(text, str) else None
    if isinstance(item, (list, tuple)) and len(item) >= 2:
        return item[1] if isinstance(item[1], str) else None
    return None


def item_texts(payload):
    """The item texts of one answer, in the batch shape or the flat single-item shape."""
    if not isinstance(payload, dict):
        return []
    items = payload.get("items")
    if isinstance(items, list):
        texts = []
        for item in items:
            text = item_text(item)
            if text is not None:
                texts.append(text)
        return texts
    text = payload.get("text")
    return [text] if isinstance(text, str) else []


def starts_section(payload) -> bool:
    """Whether one answer returns an item at position 0 of its section stream."""
    if not isinstance(payload, dict):
        return False
    return bool(payload.get("section")) or bool(payload.get("sections"))


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
        if not first and starts_section(payload):
            # A new section started, so the first section with body text ended.
            return record
        first = False
        next_handle = payload.get("next")
        if not next_handle:
            return record
        from_handle = next_handle


def v2_task2(server, opened: dict, deadline: float) -> TaskResult:
    record = TaskResult("T2", V2)
    doc = opened.get("doc")
    sections = opened.get("sections") or 0
    if not isinstance(doc, str) or not isinstance(sections, int) or sections < 1:
        record.error = "open returned no doc handle or section count"
        return record
    middle = max(0, min(sections - 1, sections // 2))
    from_handle = f"{doc}.s{middle}"
    for _ in range(SENTENCES_T2 * 4):
        if record.sentences >= SENTENCES_T2:
            break
        payload = call(server, "read", {"from": from_handle}, deadline, record)
        next_handle = payload.get("next")
        if not next_handle:
            break
        from_handle = next_handle
    return record


def v1_first_section(
    server, document_id: str, deadline: float, record: TaskResult
) -> str | None:
    """The first root section of a document, from one bounded structure call."""
    payload = call(
        server,
        "get_document_structure",
        {"document_id": document_id, "max_nodes": 1},
        deadline,
        record,
    )
    nodes = flatten(payload.get("sections"))
    if not nodes:
        return None
    return nodes[0].get("section_id")


def v2_task3(server, doc_handle: str, term: str, deadline: float) -> TaskResult:
    """Search a term, then read two sentences before and after each of the first hits."""
    record = TaskResult("T3", V2)
    found = call(
        server, "search", {"doc": doc_handle, "query": term, "limit": 10}, deadline, record
    )
    hits = found.get("hits") or []
    if not hits:
        record.error = f"search returned no hits for {term!r}"
        return record
    for hit in hits[:T3_HITS]:
        handle = hit[0]
        call(
            server,
            "read",
            {"from": handle, "back": T3_BACK, "max_chars": T3_MAX_CHARS},
            deadline,
            record,
        )
    return record


def v1_task3(server, document_id: str, term: str, deadline: float) -> TaskResult:
    """The v1 equivalent: search, then two anchored sentence pages per hit."""
    record = TaskResult("T3", V1)
    found = call(
        server,
        "search_document",
        {"document_id": document_id, "query": term, "limit": 10},
        deadline,
        record,
    )
    hits = [
        hit
        for hit in (found.get("hits") or [])
        if hit.get("candidate_kind") == "sentence"
    ]
    if not hits:
        record.error = f"v1 search returned no sentence hits for {term!r}"
        return record
    for hit in hits[:T3_HITS]:
        for direction in ("backward", "forward"):
            call(
                server,
                "get_text_units",
                tool_arguments(
                    document_id=document_id,
                    section_id=hit["section_id"],
                    anchor_locator=hit["text_locator"],
                    requested_kind="sentence",
                    direction=direction,
                    coverage_policy="preserve_source",
                    max_items=T3_BACK,
                ),
                deadline,
                record,
            )
    return record


def v2_task4(server, doc_handle: str, deadline: float) -> TaskResult:
    """Find the Nth section in the outline and read its first page."""
    record = TaskResult("T4", V2)
    outline = call(
        server,
        "outline",
        {"at": doc_handle, "depth": T4_DEPTH, "limit": T4_LIMIT},
        deadline,
        record,
    )
    items = outline.get("items") or []
    if not items:
        record.error = "outline returned no sections"
        return record
    handle = items[min(T4_INDEX, len(items) - 1)][0]
    call(
        server,
        "read",
        {"from": handle, "max_chars": T4_MAX_CHARS},
        deadline,
        record,
    )
    return record


def v1_task4(server, document_id: str, deadline: float) -> TaskResult:
    """The v1 equivalent: enumerate the structure, then read that section's first page."""
    record = TaskResult("T4", V1)
    sections = v1_sections(server, document_id, deadline, record)
    if not sections:
        record.error = "structure returned no sections"
        return record
    section_id = sections[min(T4_INDEX, len(sections) - 1)]["section_id"]
    call(
        server,
        "read_document",
        {"document_id": document_id, "section_id": section_id, "max_chars": T4_MAX_CHARS},
        deadline,
        record,
    )
    return record


def v2_task5(server, doc_handle: str, deadline: float) -> TaskResult:
    """Read a few sentences, then cite all of them in one call."""
    record = TaskResult("T5", V2)
    read = call(
        server, "read", {"from": doc_handle, "max_chars": T5_MAX_CHARS}, deadline, record
    )
    items = read.get("items") or []
    handles = [item[0] for item in items][:T5_SENTENCES]
    if not handles:
        record.error = "read returned no sentences to cite"
        return record
    call(server, "cite", {"at": handles}, deadline, record)
    return record


def v1_task5(server, document_id: str, deadline: float) -> TaskResult:
    """The v1 equivalent: find a section, then page three sentences with their locators."""
    record = TaskResult("T5", V1)
    section_id = v1_first_section(server, document_id, deadline, record)
    if section_id is None:
        record.error = "structure returned no sections"
        return record
    payload = call(
        server,
        "get_text_units",
        tool_arguments(
            document_id=document_id,
            section_id=section_id,
            requested_kind="sentence",
            direction="forward",
            coverage_policy="preserve_source",
            max_items=T5_SENTENCES,
        ),
        deadline,
        record,
    )
    if payload.get("items"):
        return record
    # The first section was empty: a v1 client falls back to the full structure.
    for section in v1_sections(server, document_id, deadline, record)[:3]:
        payload = call(
            server,
            "get_text_units",
            tool_arguments(
                document_id=document_id,
                section_id=section["section_id"],
                requested_kind="sentence",
                direction="forward",
                coverage_policy="preserve_source",
                max_items=T5_SENTENCES,
            ),
            deadline,
            record,
        )
        if payload.get("items"):
            return record
    record.error = "v1 found no sentences to cite"
    return record


def tasks_for_book(label: str) -> list[str]:
    """The extended tasks one real book runs."""
    tasks = []
    if label in T3_TERMS:
        tasks.append("T3")
    if label in T4_BOOKS:
        tasks.append("T4")
    if label in T5_BOOKS:
        tasks.append("T5")
    return tasks


def run_task_book(
    binary: str,
    toolset: str,
    book: str,
    path: str,
    tasks: list[str],
    state_root: str,
    seconds: float,
) -> dict:
    """Open one book once and run its extended tasks, charging the open to each."""
    state = probe.fresh_directory(os.path.join(state_root, f"{book}-{toolset}-tasks"))
    result = {"book": book, "toolset": toolset, "tasks": [], "error": None, "open": None}
    server = None
    try:
        books = os.path.dirname(os.path.abspath(path))
        environment = probe.server_environment(books, state)
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
        record = TaskResult("open", toolset)
        if toolset == V1:
            document_id = open_v1(server, os.path.abspath(path), deadline, record)
        else:
            opened = open_v2(server, os.path.abspath(path), deadline, record)
            document_id = opened.get("doc")
        completed = []
        for task in tasks:
            if toolset == V1:
                if task == "T3":
                    completed.append(
                        v1_task3(server, document_id, T3_TERMS[book], deadline)
                    )
                elif task == "T4":
                    completed.append(v1_task4(server, document_id, deadline))
                elif task == "T5":
                    completed.append(v1_task5(server, document_id, deadline))
            else:
                if task == "T3":
                    completed.append(v2_task3(server, document_id, T3_TERMS[book], deadline))
                elif task == "T4":
                    completed.append(v2_task4(server, document_id, deadline))
                elif task == "T5":
                    completed.append(v2_task5(server, document_id, deadline))
        for task in completed:
            task.calls += record.calls
            task.request_tokens += record.request_tokens
            task.response_tokens += record.response_tokens
            task.server_ms += record.server_ms
        result["open"] = record
        result["tasks"] = completed
    except probe.ProbeError as exc:
        result["error"] = str(exc)
    except Exception as exc:  # noqa: BLE001 - one book must not hide the others
        result["error"] = f"{type(exc).__name__}: {exc}"
    finally:
        if server is not None:
            server.close()
    return result


def measure_tools_list(binary: str, state_root: str, toolset: str, seconds: float) -> tuple:
    """The `tools/list` footprint of one tool set: (estimated tokens, bytes)."""
    state = probe.fresh_directory(os.path.join(state_root, f"tools-list-{toolset}"))
    server = None
    try:
        environment = probe.server_environment(state_root, state)
        environment["READING_MCP_TOOLS"] = toolset
        server = probe.Server(
            binary,
            environment,
            os.path.join(state, "server.log"),
            request_timeout=max(30.0, seconds),
            cwd=state_root,
        )
        server.initialize(timeout=max(30.0, seconds))
        result = server.tools_list(timeout=max(30.0, seconds))
        response_json = probe.compact_json(result)
        return probe.est_tokens(response_json), len(response_json.encode("utf-8"))
    except Exception:  # noqa: BLE001 - a missing footprint must not hide the tasks
        return 0.0, 0
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
        books = os.path.dirname(os.path.abspath(path))
        environment = probe.server_environment(books, state)
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
        result["open"] = record
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
        "Body share % | tools/list tokens x calls | Server ms | Total tokens | "
        "Open tokens | Extra tokens/sentence (read calls) |",
        "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for row in rows:
        open_tokens = row["open"].open_tokens() if row.get("open") else None
        for task in row["tasks"]:
            tools_tokens_value = tools_tokens.get(task.toolset, 0.0)
            lines.append(
                "| {book} | {task} | {tools} | {calls} | {sentences} | {request} | {response} | "
                "{body} | {defs} | {ms} | {total} | {open} | {extra} |".format(
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
                    open=fmt(open_tokens),
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
    # Rows are per (book, tool set); a criterion compares the two tool sets of one book.
    books: dict[str, dict[str, dict[str, TaskResult]]] = {}
    for row in rows:
        by_task = books.setdefault(row["book"], {})
        for task in row["tasks"]:
            by_task.setdefault(task.task, {})[task.toolset] = task
    for book, by_task in books.items():
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
            summary["ratios"][f"{book}-{name}"] = percent
            lines.append(
                "{} {book} {task} total tokens: v2 {v2} vs v1 {v1} = {pct}% (limit 20%)".format(
                    "PASS" if ok else "FAIL",
                    book=book,
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
                    book=book,
                    task=name,
                    v2=right.calls,
                    v1=left.calls,
                )
            )
            summary["checks"].append(ok)
            extra = right.extra_tokens_per_sentence()
            ok = extra is not None and extra <= 30.0
            lines.append(
                "{} {book} {task} v2 extra tokens per sentence on read calls: {extra} (limit 30)".format(
                    "PASS" if ok else "FAIL",
                    book=book,
                    task=name,
                    extra=fmt(extra),
                )
            )
            summary["checks"].append(ok)
    # T3-T5: the extended tasks. Per-task numbers are reported; the gate is the T3-T5 total,
    # exactly as the task states: total tokens not higher than v1 and calls not more than v1.
    extended: dict[str, dict[str, float]] = {}
    for book, by_task in books.items():
        for name in EXTENDED_TASKS:
            pair = by_task.get(name) or {}
            left = pair.get(V1)
            right = pair.get(V2)
            if left is None or right is None:
                continue
            v1_total = left.total_tokens(tools_tokens.get(V1, 0.0))
            v2_total = right.total_tokens(tools_tokens.get(V2, 0.0))
            percent = ratio(v2_total, v1_total)
            lines.append(
                "INFO {book} {task} total tokens: v2 {v2} vs v1 {v1} = {pct}%".format(
                    book=book,
                    task=name,
                    v2=fmt(v2_total, 0),
                    v1=fmt(v1_total, 0),
                    pct=fmt(percent),
                )
            )
            lines.append(
                "INFO {book} {task} calls: v2 {v2} vs v1 {v1}".format(
                    book=book,
                    task=name,
                    v2=right.calls,
                    v1=left.calls,
                )
            )
            accumulator = extended.setdefault(
                name,
                {"v1_tokens": 0.0, "v2_tokens": 0.0, "v1_calls": 0, "v2_calls": 0},
            )
            accumulator["v1_tokens"] += v1_total
            accumulator["v2_tokens"] += v2_total
            accumulator["v1_calls"] += left.calls
            accumulator["v2_calls"] += right.calls
    for name in EXTENDED_TASKS:
        accumulator = extended.get(name)
        if accumulator is None:
            continue
        summary["ratios"][name] = ratio(
            accumulator["v2_tokens"], accumulator["v1_tokens"]
        )
    combined = {
        key: sum(accumulator[key] for accumulator in extended.values())
        for key in ("v1_tokens", "v2_tokens", "v1_calls", "v2_calls")
    }
    combined_pct = ratio(combined["v2_tokens"], combined["v1_tokens"])
    tokens_ok = combined_pct is not None and combined_pct <= 100.0
    calls_ok = combined["v2_calls"] <= combined["v1_calls"]
    lines.append(
        "{} T3-T5 total tokens: v2 {v2} vs v1 {v1} = {pct}% (limit 100%)".format(
            "PASS" if tokens_ok else "FAIL",
            v2=fmt(combined["v2_tokens"], 0),
            v1=fmt(combined["v1_tokens"], 0),
            pct=fmt(combined_pct),
        )
    )
    lines.append(
        "{} T3-T5 calls: v2 {v2} vs v1 {v1}".format(
            "PASS" if calls_ok else "FAIL",
            v2=combined["v2_calls"],
            v1=combined["v1_calls"],
        )
    )
    summary["checks"].extend([tokens_ok, calls_ok])
    summary["t3_t5"] = {
        "v1_tokens": combined["v1_tokens"],
        "v2_tokens": combined["v2_tokens"],
        "v1_calls": combined["v1_calls"],
        "v2_calls": combined["v2_calls"],
        "percent": combined_pct,
    }
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
        help="comma-separated label=path books for T1/T2 (zh-large, en-novel)",
    )
    parser.add_argument(
        "--task-books",
        default="",
        help="comma-separated label=path books for T3-T5 (zh-classic, en-novel, zh-large)",
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
        task_books = parse_documents(args.task_books)
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

    tools_footprint = {
        toolset: measure_tools_list(
            binary, state_root, toolset, args.max_seconds_per_task
        )
        for toolset in (V1, V2)
    }
    tools_tokens = {toolset: tokens for toolset, (tokens, _bytes) in tools_footprint.items()}

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

    for label, path in task_books:
        if not os.path.isfile(path):
            print(f"skipping tasks for {label}: {path} is missing", file=sys.stderr)
            continue
        tasks = tasks_for_book(label)
        if not tasks:
            continue
        for toolset in (V1, V2):
            print(f"replaying {label} {tasks} with {toolset}", file=sys.stderr)
            rows.append(
                run_task_book(
                    binary,
                    toolset,
                    label,
                    path,
                    tasks,
                    state_root,
                    args.max_seconds_per_task,
                )
            )

    lines, summary = pass_lines(rows, tools_tokens)
    print("## V2 evaluation (T1-T5)")
    print()
    print(
        "Token estimates are a rough heuristic: ascii_chars / 4 + non_ascii_chars / 1.5. "
        "Request tokens include the tool name and arguments of every call; total tokens add "
        "`tools/list` tokens multiplied by the number of calls."
    )
    print()
    for toolset in (V1, V2):
        tokens, size = tools_footprint.get(toolset, (0.0, 0))
        print(
            f"tools/list {toolset}: {size} bytes, ~{tokens:.0f} est. tokens"
        )
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
            "tools_list": {
                toolset: {"tokens": tokens, "bytes": size}
                for toolset, (tokens, size) in tools_footprint.items()
            },
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
                            "read_calls": task.read_calls,
                            "read_request_tokens": task.read_request_tokens,
                            "read_response_tokens": task.read_response_tokens,
                            "read_body_tokens": task.read_body_tokens,
                            "read_sentences": task.read_sentences,
                        }
                        for task in row["tasks"]
                    ],
                    "open": (
                        {
                            "calls": row["open"].calls,
                            "request_tokens": row["open"].request_tokens,
                            "response_tokens": row["open"].response_tokens,
                        }
                        if row.get("open")
                        else None
                    ),
                }
                for row in rows
            ],
            "summary": summary,
        },
    )
    return 0 if summary.get("all_pass") else 1


if __name__ == "__main__":
    sys.exit(main())
