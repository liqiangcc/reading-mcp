#!/usr/bin/env python3
"""Invoke the local Reading MCP over stdio and emit a sanitized TLPI source map.

This probe is read-only. It reuses the persistent Reading MCP state and calls only
open_document, get_document_structure, search_document, and read_document.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
from pathlib import Path
from typing import Any

DOCUMENT_ID = "doc:sha256:286e0104a40d05c3cb76f08e2d6a06391ce9d1bc603351aefc2340aca3349b2f"
OUTPUT = Path("tlpi-stdio-probe.json")


class McpClient:
    def __init__(self, command: list[str]) -> None:
        env = os.environ.copy()
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            encoding="utf-8",
            bufsize=1,
            env=env,
        )
        self.next_id = 0

    def send(self, message: dict[str, Any]) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps(message, ensure_ascii=False, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

    def call(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        self.next_id += 1
        request_id = self.next_id
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        assert self.process.stdout is not None
        while True:
            line = self.process.stdout.readline()
            if not line:
                raise RuntimeError(f"Reading MCP exited before responding to {method}")
            message = json.loads(line)
            if message.get("id") == request_id:
                return message

    def notify(self, method: str, params: dict[str, Any]) -> None:
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def close(self) -> None:
        if self.process.stdin:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            self.process.wait(timeout=10)


def tool_payload(response: Any) -> Any:
    if not isinstance(response, dict):
        return response
    rpc_result = response.get("result")
    if not isinstance(rpc_result, dict):
        return response
    structured = rpc_result.get("structuredContent") or rpc_result.get("structured_content")
    if structured is not None:
        return structured
    content = rpc_result.get("content")
    if not isinstance(content, list):
        return rpc_result
    texts = [
        item.get("text", "")
        for item in content
        if isinstance(item, dict) and item.get("type") == "text"
    ]
    joined = "\n".join(texts)
    try:
        return json.loads(joined)
    except json.JSONDecodeError:
        return {"text": joined}


def sanitize_source(value: Any) -> Any:
    if isinstance(value, dict):
        cleaned: dict[str, Any] = {}
        for key, item in value.items():
            if key == "source" and isinstance(item, str):
                cleaned[key] = f"<local-source>/{Path(item.removeprefix('file://')).name}"
            else:
                cleaned[key] = sanitize_source(item)
        return cleaned
    if isinstance(value, list):
        return [sanitize_source(item) for item in value]
    return value


def flatten(nodes: Any) -> list[dict[str, Any]]:
    output: list[dict[str, Any]] = []

    def walk(items: Any) -> None:
        for item in items if isinstance(items, list) else []:
            if isinstance(item, dict):
                output.append(item)
                walk(item.get("children", []))

    walk(nodes)
    return output


def select_structural_preface_paragraphs(content: str) -> list[str]:
    paragraphs = [re.sub(r"\s+", " ", part).strip() for part in re.split(r"\n\s*\n", content)]
    patterns = (
        r"本书.*(?:目标|目的|旨在|面向|读者|组织|结构|章节|部分)",
        r"(?:目标|目的|读者|组织|结构|章节|部分).*(?:本书|全书)",
        r"第\s*[一二三四五六七八九十0-9]+\s*(?:章|部分|篇)",
        r"(?:基础|后续|前面|前几章|余下|其余).*(?:章|部分|内容|知识)",
        r"(?:programmer|reader|audience|organization|structure|chapter|part)",
    )
    selected: list[str] = []
    for paragraph in paragraphs:
        if not paragraph:
            continue
        if any(re.search(pattern, paragraph, re.IGNORECASE) for pattern in patterns):
            selected.append(paragraph[:2000])
        if len(selected) >= 20:
            break
    return selected


def main() -> int:
    binary = os.environ.get("READING_MCP_BINARY", "target/release/reading-mcp")
    client = McpClient([binary])
    result: dict[str, Any] = {
        "probe_version": 3,
        "document_id": DOCUMENT_ID,
        "state_dir": "persistent",
    }

    try:
        initialized = client.call(
            "initialize",
            {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "tlpi-local-source-map-probe", "version": "3.0"},
            },
        )
        result["initialize"] = initialized
        client.notify("notifications/initialized", {})

        tools = client.call("tools/list", {})
        result["tools_list"] = tools

        structure_response = client.call(
            "tools/call",
            {
                "name": "get_document_structure",
                "arguments": {"document_id": DOCUMENT_ID, "max_depth": 16},
            },
        )
        structure = tool_payload(structure_response)
        result["structure"] = structure

        roots = structure.get("sections", []) if isinstance(structure, dict) else []
        nodes = flatten(roots)
        result["structure_stats"] = {
            "returned_nodes": len(nodes),
            "max_level": max((int(node.get("level", 0)) for node in nodes), default=0),
            "truncated": structure.get("truncated") if isinstance(structure, dict) else None,
        }

        preface = next(
            (
                node
                for node in nodes
                if re.search(r"前言|序言|Preface", str(node.get("title", "")), re.IGNORECASE)
            ),
            None,
        )
        if preface and preface.get("section_id"):
            preface_response = client.call(
                "tools/call",
                {
                    "name": "read_document",
                    "arguments": {
                        "document_id": DOCUMENT_ID,
                        "section_id": preface["section_id"],
                        "max_chars": 64000,
                    },
                },
            )
            preface_payload = tool_payload(preface_response)
            if isinstance(preface_payload, dict):
                content = str(preface_payload.get("content", ""))
                result["preface"] = {
                    "section_id": preface_payload.get("section_id"),
                    "location": preface_payload.get("location"),
                    "source": preface_payload.get("source"),
                    "truncated": preface_payload.get("truncated"),
                    "content_chars": len(content),
                    "structural_paragraphs": select_structural_preface_paragraphs(content),
                }
                source = preface_payload.get("source")
                if source:
                    opened_response = client.call(
                        "tools/call",
                        {
                            "name": "open_document",
                            "arguments": {"source": source, "force_refresh": False},
                        },
                    )
                    result["open_document"] = tool_payload(opened_response)
        else:
            result["preface_error"] = "preface node not found"

        metadata_patterns = re.compile(
            r"版权|出版|书名|扉页|Copyright|Edition|版本|版次|ISBN|内容简介|作者简介",
            re.IGNORECASE,
        )
        metadata_nodes = [node for node in nodes if metadata_patterns.search(str(node.get("title", "")))][:8]
        result["metadata_nodes"] = metadata_nodes
        result["metadata_reads"] = []
        for node in metadata_nodes:
            section_id = node.get("section_id")
            if not section_id:
                continue
            response = client.call(
                "tools/call",
                {
                    "name": "read_document",
                    "arguments": {
                        "document_id": DOCUMENT_ID,
                        "section_id": section_id,
                        "max_chars": 12000,
                    },
                },
            )
            payload = tool_payload(response)
            if isinstance(payload, dict):
                text = str(payload.get("content", ""))
                paragraphs = [
                    re.sub(r"\s+", " ", part).strip()[:1200]
                    for part in re.split(r"\n\s*\n", text)
                    if re.search(r"Michael Kerrisk|The Linux Programming Interface|版本|版次|ISBN|出版|Copyright|2010|2011", part, re.IGNORECASE)
                ][:12]
                result["metadata_reads"].append(
                    {
                        "section_id": section_id,
                        "title": node.get("title"),
                        "location": payload.get("location"),
                        "truncated": payload.get("truncated"),
                        "matched_paragraphs": paragraphs,
                    }
                )

        result["structural_searches"] = []
        for query in (
            "本书的目标 读者",
            "本书的组织结构 章节",
            "基础部分 后续章节",
            "Michael Kerrisk The Linux Programming Interface ISBN 版本",
        ):
            search_response = client.call(
                "tools/call",
                {
                    "name": "search_document",
                    "arguments": {
                        "document_id": DOCUMENT_ID,
                        "query": query,
                        "limit": 12,
                    },
                },
            )
            result["structural_searches"].append(
                {"query": query, "payload": tool_payload(search_response)}
            )
    except Exception as exc:
        result["error"] = f"{type(exc).__name__}: {exc}"
    finally:
        client.close()

    OUTPUT.write_text(
        json.dumps(sanitize_source(result), ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
