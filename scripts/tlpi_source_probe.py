#!/usr/bin/env python3
"""Read-only probe of the deployed Reading MCP through Secure MCP Tunnel.

The generated JSON is sanitized before being uploaded as a short-lived Actions artifact.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

DOCUMENT_ID = "doc:sha256:286e0104a40d05c3cb76f08e2d6a06391ce9d1bc603351aefc2340aca3349b2f"
OUTPUT = Path("tlpi-probe.json")
KEY = os.environ.get("CONTROL_PLANE_API_KEY", "")


def request(url: str, payload: dict[str, Any] | None = None, session_id: str | None = None):
    headers = {
        "Authorization": f"Bearer {KEY}",
        "Accept": "application/json, text/event-stream",
        "User-Agent": "tlpi-source-map-read-only-probe/1.0",
    }
    method = "GET"
    body = None
    if payload is not None:
        method = "POST"
        body = json.dumps(payload).encode("utf-8")
        headers["Content-Type"] = "application/json"
        headers["MCP-Protocol-Version"] = "2025-03-26"
    if session_id:
        headers["Mcp-Session-Id"] = session_id
    req = urllib.request.Request(url, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=90) as response:
            return response.status, dict(response.headers.items()), response.read()
    except urllib.error.HTTPError as exc:
        return exc.code, dict(exc.headers.items()), exc.read()
    except Exception as exc:  # diagnostic output only
        return 0, {}, str(exc).encode("utf-8", "replace")


def decode(raw: bytes, content_type: str) -> Any:
    text = raw.decode("utf-8", "replace")
    if "text/event-stream" in content_type or text.lstrip().startswith(("event:", "data:")):
        events: list[Any] = []
        for line in text.splitlines():
            if not line.startswith("data:"):
                continue
            data = line[5:].strip()
            if not data or data == "[DONE]":
                continue
            try:
                events.append(json.loads(data))
            except json.JSONDecodeError:
                events.append({"unparsed_data": data[:4000]})
        return events[-1] if events else {"raw_excerpt": text[:8000]}
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return {"raw_excerpt": text[:8000]}


def tool_payload(response: Any) -> Any:
    if not isinstance(response, dict):
        return response
    rpc_result = response.get("result")
    if not isinstance(rpc_result, dict):
        return response
    content = rpc_result.get("content")
    if not isinstance(content, list):
        return rpc_result
    texts = [
        item.get("text", "")
        for item in content
        if isinstance(item, dict) and item.get("type") == "text"
    ]
    if not texts:
        return rpc_result
    joined = "\n".join(texts)
    try:
        return json.loads(joined)
    except json.JSONDecodeError:
        return {"text": joined}


def scrub(value: Any, tunnel_id: str) -> Any:
    if isinstance(value, dict):
        cleaned: dict[str, Any] = {}
        for key, item in value.items():
            if key.lower() in {"authorization", "api_key", "token", "cookie"}:
                cleaned[key] = "<redacted>"
            elif key == "source" and isinstance(item, str) and item.startswith("/"):
                cleaned[key] = f"<redacted-local-source>/{Path(item).name}"
            else:
                cleaned[key] = scrub(item, tunnel_id)
        return cleaned
    if isinstance(value, list):
        return [scrub(item, tunnel_id) for item in value]
    if isinstance(value, str):
        return value.replace(tunnel_id, "<redacted-tunnel-id>")
    return value


def main() -> int:
    result: dict[str, Any] = {
        "probe_version": 1,
        "document_id": DOCUMENT_ID,
        "control_plane_key_present": bool(KEY),
        "attempts": [],
    }

    deploy = Path(".github/workflows/deploy-tunnel.yml").read_text(encoding="utf-8")
    match = re.search(r"tunnel_[0-9a-f]{32}", deploy)
    if not match:
        result["fatal_error"] = "tunnel id not found in deployment workflow"
        OUTPUT.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
        return 0

    tunnel_id = match.group(0)
    result["tunnel_id_sha256_prefix"] = hashlib.sha256(tunnel_id.encode()).hexdigest()[:12]

    status, headers, raw = request(f"https://api.openai.com/v1/tunnels/{tunnel_id}")
    result["metadata_probe"] = {
        "status": status,
        "content_type": headers.get("Content-Type", ""),
        "body_excerpt": raw.decode("utf-8", "replace")[:4000],
    }

    rpc_id = 0
    endpoint: str | None = None
    session_id: str | None = None
    candidates = (
        f"https://api.openai.com/v1/mcp/{tunnel_id}",
        f"https://mcp.openai.com/v1/mcp/{tunnel_id}",
    )
    for candidate in candidates:
        rpc_id += 1
        initialize = {
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "tlpi-source-map-probe", "version": "1.0"},
            },
        }
        status, headers, raw = request(candidate, initialize)
        decoded = decode(raw, headers.get("Content-Type", ""))
        result["attempts"].append(
            {
                "host": candidate.split("/")[2],
                "initialize_status": status,
                "initialize_response": decoded,
            }
        )
        if status in (200, 202) and isinstance(decoded, dict) and (
            "result" in decoded or status == 202
        ):
            endpoint = candidate
            session_id = headers.get("Mcp-Session-Id") or headers.get("mcp-session-id")
            break

    if endpoint is None:
        OUTPUT.write_text(
            json.dumps(scrub(result, tunnel_id), ensure_ascii=False, indent=2),
            encoding="utf-8",
        )
        return 0

    result["selected_host"] = endpoint.split("/")[2]
    result["session_established"] = bool(session_id)
    request(
        endpoint,
        {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}},
        session_id,
    )

    def rpc(method: str, params: dict[str, Any]) -> dict[str, Any]:
        nonlocal rpc_id
        rpc_id += 1
        status, response_headers, response_raw = request(
            endpoint,
            {"jsonrpc": "2.0", "id": rpc_id, "method": method, "params": params},
            session_id,
        )
        decoded_response = decode(response_raw, response_headers.get("Content-Type", ""))
        return {
            "http_status": status,
            "rpc": decoded_response,
            "payload": tool_payload(decoded_response),
        }

    result["tools_list"] = rpc("tools/list", {})
    structure = rpc(
        "tools/call",
        {
            "name": "get_document_structure",
            "arguments": {"document_id": DOCUMENT_ID, "max_depth": 12},
        },
    )
    result["structure"] = structure

    structure_payload = structure.get("payload")
    roots = structure_payload.get("sections", []) if isinstance(structure_payload, dict) else []
    flat: list[dict[str, Any]] = []

    def walk(nodes: Any) -> None:
        for node in nodes if isinstance(nodes, list) else []:
            if isinstance(node, dict):
                flat.append(node)
                walk(node.get("children", []))

    walk(roots)
    preface = next(
        (
            node
            for node in flat
            if re.search(r"前言|序言|Preface", str(node.get("title", "")), re.IGNORECASE)
        ),
        None,
    )

    if preface and preface.get("section_id"):
        preface_read = rpc(
            "tools/call",
            {
                "name": "read_document",
                "arguments": {
                    "document_id": DOCUMENT_ID,
                    "section_id": preface["section_id"],
                    "max_chars": 32000,
                },
            },
        )
        preface_payload = preface_read.get("payload")
        if isinstance(preface_payload, dict):
            source = preface_payload.get("source")
            if source:
                source_name = Path(source).name
                result["source_basename"] = source_name
                result["open_document"] = rpc(
                    "tools/call",
                    {
                        "name": "open_document",
                        "arguments": {"source": source, "force_refresh": False},
                    },
                )
        result["preface"] = preface_read
    else:
        result["preface_error"] = "preface node not found in returned structure"

    result["structural_searches"] = []
    for query in (
        "本书的目标 读者",
        "本书的组织结构 章节",
        "基础部分 后续章节",
        "版本 版次 ISBN",
    ):
        result["structural_searches"].append(
            {
                "query": query,
                "result": rpc(
                    "tools/call",
                    {
                        "name": "search_document",
                        "arguments": {
                            "document_id": DOCUMENT_ID,
                            "query": query,
                            "limit": 10,
                        },
                    },
                ),
            }
        )

    OUTPUT.write_text(
        json.dumps(scrub(result, tunnel_id), ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
