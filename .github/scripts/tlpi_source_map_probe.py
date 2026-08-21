#!/usr/bin/env python3
"""Read TLPI structure from the deployed reading-mcp without exposing secrets."""

from __future__ import annotations

import hashlib
import json
import os
import re
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

DOC_ID = "doc:sha256:286e0104a40d05c3cb76f08e2d6a06391ce9d1bc603351aefc2340aca3349b2f"
OUTPUT = Path("tlpi-probe.json")
API_KEY = os.environ.get("CONTROL_PLANE_API_KEY", "")


def write_output(value: dict[str, Any]) -> None:
    OUTPUT.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")


def decode_body(raw: bytes, content_type: str) -> Any:
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
        return events[-1] if events else {"raw_excerpt": text[:4000]}
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return {"raw_excerpt": text[:4000]}


def tool_payload(response: Any) -> Any:
    if not isinstance(response, dict):
        return response
    result = response.get("result")
    if not isinstance(result, dict):
        return response
    content = result.get("content")
    if not isinstance(content, list):
        return result
    texts = [
        item.get("text", "")
        for item in content
        if isinstance(item, dict) and item.get("type") == "text"
    ]
    if not texts:
        return result
    joined = "\n".join(texts)
    try:
        return json.loads(joined)
    except json.JSONDecodeError:
        return {"text": joined}


def request_json(
    url: str,
    *,
    method: str = "GET",
    payload: dict[str, Any] | None = None,
    session_id: str | None = None,
) -> tuple[int, dict[str, str], bytes]:
    headers = {
        "Authorization": f"Bearer {API_KEY}",
        "Accept": "application/json, text/event-stream",
        "User-Agent": "tlpi-source-map-read-only-probe/1.0",
    }
    if payload is not None:
        headers["Content-Type"] = "application/json"
        headers["MCP-Protocol-Version"] = "2025-03-26"
    if session_id:
        headers["Mcp-Session-Id"] = session_id
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request, timeout=90) as response:
            return response.status, dict(response.headers.items()), response.read()
    except urllib.error.HTTPError as error:
        return error.code, dict(error.headers.items()), error.read()
    except Exception as error:  # noqa: BLE001 - diagnostic artifact must survive failures
        return 0, {}, f"{type(error).__name__}: {error}".encode()


def find_urls(value: Any) -> list[str]:
    urls: list[str] = []
    if isinstance(value, dict):
        for child in value.values():
            urls.extend(find_urls(child))
    elif isinstance(value, list):
        for child in value:
            urls.extend(find_urls(child))
    elif isinstance(value, str) and value.startswith("https://"):
        urls.append(value)
    return urls


def scrub(value: Any, tunnel_id: str) -> Any:
    if isinstance(value, dict):
        cleaned: dict[str, Any] = {}
        for key, item in value.items():
            lowered = key.lower()
            if any(secret_word in lowered for secret_word in ("authorization", "api_key", "token", "cookie")):
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


def main() -> None:
    output: dict[str, Any] = {
        "probe_version": 2,
        "document_id": DOC_ID,
        "control_plane_key_present": bool(API_KEY),
        "attempts": [],
    }
    deploy_path = Path(".github/workflows/deploy-tunnel.yml")
    deploy = deploy_path.read_text(encoding="utf-8")
    match = re.search(r"tunnel_[0-9a-f]{32}", deploy)
    if not match:
        output["fatal_error"] = "tunnel id not found in existing deployment workflow"
        write_output(output)
        return
    tunnel_id = match.group(0)
    output["tunnel_id_sha256_prefix"] = hashlib.sha256(tunnel_id.encode()).hexdigest()[:12]

    metadata_url = f"https://api.openai.com/v1/tunnels/{tunnel_id}"
    status, headers, raw = request_json(metadata_url)
    metadata = decode_body(raw, headers.get("Content-Type", ""))
    output["metadata_probe"] = {
        "status": status,
        "content_type": headers.get("Content-Type", ""),
        "body": metadata,
    }

    candidates = [
        f"https://api.openai.com/v1/mcp/{tunnel_id}",
        f"https://mcp.openai.com/v1/mcp/{tunnel_id}",
    ]
    for url in find_urls(metadata):
        if "/v1/mcp/" in url or "/public/" in url:
            candidates.append(url)
    candidates = list(dict.fromkeys(candidates))

    rpc_id = 0
    chosen: str | None = None
    session_id: str | None = None
    for endpoint in candidates:
        rpc_id += 1
        init = {
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "tlpi-source-map-probe", "version": "2.0"},
            },
        }
        init_status, init_headers, init_raw = request_json(endpoint, method="POST", payload=init)
        decoded = decode_body(init_raw, init_headers.get("Content-Type", ""))
        output["attempts"].append(
            {
                "host": urllib.parse.urlparse(endpoint).netloc,
                "path_kind": "public" if "/public/" in endpoint else "v1-mcp",
                "initialize_status": init_status,
                "initialize_response": decoded,
            }
        )
        if init_status in (200, 202) and isinstance(decoded, dict) and (
            "result" in decoded or init_status == 202
        ):
            chosen = endpoint
            session_id = init_headers.get("Mcp-Session-Id") or init_headers.get("mcp-session-id")
            break

    if chosen is None:
        write_output(scrub(output, tunnel_id))
        return

    output["selected_host"] = urllib.parse.urlparse(chosen).netloc
    output["session_established"] = bool(session_id)
    request_json(
        chosen,
        method="POST",
        payload={"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}},
        session_id=session_id,
    )

    def rpc(method: str, params: dict[str, Any]) -> dict[str, Any]:
        nonlocal rpc_id
        rpc_id += 1
        payload = {"jsonrpc": "2.0", "id": rpc_id, "method": method, "params": params}
        rpc_status, rpc_headers, rpc_raw = request_json(
            chosen,
            method="POST",
            payload=payload,
            session_id=session_id,
        )
        decoded = decode_body(rpc_raw, rpc_headers.get("Content-Type", ""))
        return {"http_status": rpc_status, "rpc": decoded, "payload": tool_payload(decoded)}

    output["tools_list"] = rpc("tools/list", {})
    structure = rpc(
        "tools/call",
        {
            "name": "get_document_structure",
            "arguments": {"document_id": DOC_ID, "max_depth": 12},
        },
    )
    output["structure"] = structure

    structure_payload = structure.get("payload")
    sections = structure_payload.get("sections", []) if isinstance(structure_payload, dict) else []
    flat: list[dict[str, Any]] = []

    def walk(nodes: Any) -> None:
        if not isinstance(nodes, list):
            return
        for node in nodes:
            if not isinstance(node, dict):
                continue
            flat.append(node)
            walk(node.get("children"))

    walk(sections)
    output["returned_node_count"] = len(flat)

    preface = next(
        (node for node in flat if re.search(r"前言|序言|Preface", str(node.get("title", "")), re.I)),
        None,
    )
    source_node = preface or (flat[0] if flat else None)
    if source_node and source_node.get("section_id"):
        source_read = rpc(
            "tools/call",
            {
                "name": "read_document",
                "arguments": {
                    "document_id": DOC_ID,
                    "section_id": source_node["section_id"],
                    "max_chars": 24000 if preface else 2000,
                },
            },
        )
        source_payload = source_read.get("payload")
        raw_source = source_payload.get("source") if isinstance(source_payload, dict) else None
        if isinstance(raw_source, str):
            source_name = Path(raw_source).name
            output["source_basename"] = source_name
            opened = rpc(
                "tools/call",
                {"name": "open_document", "arguments": {"source": raw_source, "force_refresh": False}},
            )
            output["open_document"] = opened
        if preface:
            output["preface"] = source_read
    else:
        output["preface_error"] = "no readable preface or fallback node returned"

    for query in (
        "本书的目标 读者",
        "本书的组织结构 章节",
        "基础部分 后续章节",
        "版本 版次 ISBN",
    ):
        result = rpc(
            "tools/call",
            {
                "name": "search_document",
                "arguments": {"document_id": DOC_ID, "query": query, "limit": 8},
            },
        )
        output.setdefault("structural_searches", []).append({"query": query, "result": result})

    write_output(scrub(output, tunnel_id))
    print("sanitized reading-mcp probe complete")


if __name__ == "__main__":
    main()
