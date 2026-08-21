#!/usr/bin/env python3
"""Retrieve TLPI structural evidence through OpenAI Responses + Secure MCP Tunnel.

Only read-only Reading MCP tools are exposed. The artifact is sanitized and expires
within one day; no book body is committed to the repository.
"""

from __future__ import annotations

import json
import os
import re
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

API_KEY = os.environ.get("CONTROL_PLANE_API_KEY", "")
DOCUMENT_ID = "doc:sha256:286e0104a40d05c3cb76f08e2d6a06391ce9d1bc603351aefc2340aca3349b2f"
OUTPUT = Path("tlpi-responses-probe.json")
PREFERRED_MODELS = (
    "gpt-5.6-luna",
    "gpt-5.6-mini",
    "gpt-5.6",
    "gpt-5.5-mini",
    "gpt-5.4-mini",
    "gpt-5-mini",
    "gpt-4.1-mini",
)
READ_TOOLS = [
    "open_document",
    "get_document_structure",
    "search_document",
    "read_document",
]


def api(method: str, path: str, payload: dict[str, Any] | None = None, timeout: int = 180):
    headers = {
        "Authorization": f"Bearer {API_KEY}",
        "Content-Type": "application/json",
        "User-Agent": "tlpi-source-map-responses-probe/1.0",
    }
    body = None if payload is None else json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(
        f"https://api.openai.com{path}",
        data=body,
        headers=headers,
        method=method,
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            raw = response.read()
            return response.status, dict(response.headers.items()), raw
    except urllib.error.HTTPError as exc:
        return exc.code, dict(exc.headers.items()), exc.read()
    except Exception as exc:
        return 0, {}, str(exc).encode("utf-8", "replace")


def json_body(raw: bytes) -> Any:
    text = raw.decode("utf-8", "replace")
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return {"raw_excerpt": text[:12000]}


def select_model() -> tuple[str, dict[str, Any]]:
    status, _, raw = api("GET", "/v1/models", timeout=60)
    response = json_body(raw)
    ids: set[str] = set()
    if status == 200 and isinstance(response, dict):
        ids = {
            item.get("id")
            for item in response.get("data", [])
            if isinstance(item, dict) and isinstance(item.get("id"), str)
        }
    selected = next((model for model in PREFERRED_MODELS if model in ids), "gpt-5-mini")
    return selected, {
        "status": status,
        "selected": selected,
        "preferred_available": [model for model in PREFERRED_MODELS if model in ids],
        "model_count": len(ids),
    }


def response_call(
    model: str,
    tunnel_id: str,
    tool_name: str,
    arguments: dict[str, Any],
    *,
    max_output_tokens: int = 256,
) -> dict[str, Any]:
    payload = {
        "model": model,
        "store": False,
        "max_output_tokens": max_output_tokens,
        "parallel_tool_calls": False,
        "instructions": (
            "You are a deterministic MCP transport. Call the forced Reading MCP tool "
            "exactly once with the exact arguments supplied by the user. Do not alter "
            "identifiers. After the tool returns, answer only: done"
        ),
        "input": json.dumps({"tool": tool_name, "arguments": arguments}, ensure_ascii=False),
        "tools": [
            {
                "type": "mcp",
                "server_label": "reading_mcp",
                "server_description": "Read-only document structure and section reader",
                "tunnel_id": tunnel_id,
                "allowed_tools": READ_TOOLS,
                "require_approval": "never",
            }
        ],
        "tool_choice": {
            "type": "mcp",
            "server_label": "reading_mcp",
            "name": tool_name,
        },
    }
    status, headers, raw = api("POST", "/v1/responses", payload, timeout=300)
    return {
        "http_status": status,
        "request_id": headers.get("x-request-id", ""),
        "response": json_body(raw),
    }


def mcp_output(call_result: dict[str, Any], expected_name: str) -> Any:
    response = call_result.get("response")
    if not isinstance(response, dict):
        return None
    for item in response.get("output", []):
        if not isinstance(item, dict) or item.get("type") != "mcp_call":
            continue
        if item.get("name") != expected_name:
            continue
        output = item.get("output")
        if not isinstance(output, str):
            return {"mcp_error": item.get("error"), "mcp_status": item.get("status")}
        try:
            return json.loads(output)
        except json.JSONDecodeError:
            return {"text": output}
    return None


def unwrap_tool_payload(value: Any) -> Any:
    """Normalize RMCP tool output into its structured payload when possible."""
    if not isinstance(value, dict):
        return value
    structured = value.get("structuredContent") or value.get("structured_content")
    if structured is not None:
        return structured
    content = value.get("content")
    if isinstance(content, list):
        texts = [
            item.get("text", "")
            for item in content
            if isinstance(item, dict) and item.get("type") == "text"
        ]
        joined = "\n".join(texts)
        if joined:
            try:
                return json.loads(joined)
            except json.JSONDecodeError:
                return {"text": joined}
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


def sanitize(value: Any, tunnel_id: str) -> Any:
    if isinstance(value, dict):
        cleaned: dict[str, Any] = {}
        for key, item in value.items():
            if key.lower() in {"authorization", "api_key", "token", "cookie"}:
                cleaned[key] = "<redacted>"
            elif key == "source" and isinstance(item, str):
                source_path = item.removeprefix("file://")
                cleaned[key] = f"<local-source>/{Path(source_path).name}"
            elif key == "request_id":
                cleaned[key] = "<redacted-request-id>" if item else ""
            else:
                cleaned[key] = sanitize(item, tunnel_id)
        return cleaned
    if isinstance(value, list):
        return [sanitize(item, tunnel_id) for item in value]
    if isinstance(value, str):
        return value.replace(tunnel_id, "<redacted-tunnel-id>")
    return value


def main() -> int:
    result: dict[str, Any] = {
        "probe_version": 4,
        "document_id": DOCUMENT_ID,
        "api_key_present": bool(API_KEY),
    }
    deploy = Path(".github/workflows/deploy-tunnel.yml").read_text(encoding="utf-8")
    tunnel_match = re.search(r"tunnel_[0-9a-f]{32}", deploy)
    if not tunnel_match:
        result["fatal_error"] = "tunnel id not found"
        OUTPUT.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
        return 0
    tunnel_id = tunnel_match.group(0)

    model, model_probe = select_model()
    result["model_probe"] = model_probe

    structure_call = response_call(
        model,
        tunnel_id,
        "get_document_structure",
        {"document_id": DOCUMENT_ID, "max_depth": 16},
        max_output_tokens=128,
    )
    result["structure_call"] = structure_call
    structure = unwrap_tool_payload(mcp_output(structure_call, "get_document_structure"))
    result["structure_payload"] = structure

    roots = structure.get("sections", []) if isinstance(structure, dict) else []
    nodes = flatten(roots)
    result["structure_stats"] = {
        "node_count": len(nodes),
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
        preface_call = response_call(
            model,
            tunnel_id,
            "read_document",
            {
                "document_id": DOCUMENT_ID,
                "section_id": preface["section_id"],
                "max_chars": 64000,
            },
            max_output_tokens=128,
        )
        result["preface_call"] = preface_call
        preface_payload = unwrap_tool_payload(mcp_output(preface_call, "read_document"))
        result["preface_payload"] = preface_payload
        if isinstance(preface_payload, dict) and preface_payload.get("source"):
            open_call = response_call(
                model,
                tunnel_id,
                "open_document",
                {"source": preface_payload["source"], "force_refresh": False},
                max_output_tokens=128,
            )
            result["open_document_call"] = open_call
            result["open_document_payload"] = unwrap_tool_payload(
                mcp_output(open_call, "open_document")
            )
    else:
        result["preface_error"] = "preface node not found in structure"

    result["search_calls"] = []
    for query in (
        "本书的目标 读者 组织结构",
        "基础部分 后续章节 前面知识",
        "Michael Kerrisk The Linux Programming Interface 版本 版次 ISBN 出版",
    ):
        search_call = response_call(
            model,
            tunnel_id,
            "search_document",
            {"document_id": DOCUMENT_ID, "query": query, "limit": 12},
            max_output_tokens=128,
        )
        result["search_calls"].append(
            {
                "query": query,
                "call": search_call,
                "payload": unwrap_tool_payload(mcp_output(search_call, "search_document")),
            }
        )

    OUTPUT.write_text(
        json.dumps(sanitize(result, tunnel_id), ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
