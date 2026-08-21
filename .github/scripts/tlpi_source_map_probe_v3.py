#!/usr/bin/env python3
"""Sanitized, read-only TLPI probe for a protected reading-mcp endpoint."""

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
CONTROL_KEY = os.environ.get("CONTROL_PLANE_API_KEY", "")
URL_ENV_NAMES = (
    "READING_MCP_PUBLIC_URL",
    "READING_MCP_URL",
    "MCP_SERVER_URL",
    "MCP_URL",
    "CPOLAR_URL",
)
TOKEN_ENV_NAMES = (
    "READING_MCP_BEARER_TOKEN",
    "READING_MCP_TOKEN",
    "MCP_BEARER_TOKEN",
    "MCP_AUTH_TOKEN",
)


def decode(raw: bytes, content_type: str) -> Any:
    text = raw.decode("utf-8", "replace")
    if "text/event-stream" in content_type or text.lstrip().startswith(("data:", "event:")):
        events: list[Any] = []
        for line in text.splitlines():
            if line.startswith("data:"):
                value = line[5:].strip()
                if value and value != "[DONE]":
                    try:
                        events.append(json.loads(value))
                    except json.JSONDecodeError:
                        events.append({"raw_excerpt": value[:4000]})
        return events[-1] if events else {"raw_excerpt": text[:4000]}
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return {"raw_excerpt": text[:4000]}


def request(
    url: str,
    *,
    method: str = "GET",
    payload: dict[str, Any] | None = None,
    session_id: str | None = None,
    bearer: str = "",
) -> tuple[int, dict[str, str], bytes]:
    headers = {
        "Accept": "application/json, text/event-stream",
        "User-Agent": "tlpi-source-map-read-only-probe/3.0",
    }
    if bearer:
        headers["Authorization"] = f"Bearer {bearer}"
    if payload is not None:
        headers["Content-Type"] = "application/json"
        headers["MCP-Protocol-Version"] = "2025-03-26"
    if session_id:
        headers["Mcp-Session-Id"] = session_id
    body = None if payload is None else json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=90) as response:
            return response.status, dict(response.headers.items()), response.read()
    except urllib.error.HTTPError as error:
        return error.code, dict(error.headers.items()), error.read()
    except Exception as error:  # noqa: BLE001
        return 0, {}, f"{type(error).__name__}: {error}".encode()


def tool_payload(response: Any) -> Any:
    if not isinstance(response, dict):
        return response
    result = response.get("result")
    if not isinstance(result, dict):
        return response
    content = result.get("content")
    if not isinstance(content, list):
        return result
    text = "\n".join(
        item.get("text", "")
        for item in content
        if isinstance(item, dict) and item.get("type") == "text"
    )
    if not text:
        return result
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return {"text": text}


def normalize_direct_urls(value: str) -> list[str]:
    if not value:
        return []
    parsed = urllib.parse.urlparse(value)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        return []
    urls = [value.rstrip("/")]
    if parsed.path in {"", "/"}:
        urls.append(value.rstrip("/") + "/mcp")
    return list(dict.fromkeys(urls))


def scrub(value: Any, tunnel_id: str, direct_hosts: set[str]) -> Any:
    if isinstance(value, dict):
        result: dict[str, Any] = {}
        for key, item in value.items():
            lowered = key.lower()
            if any(word in lowered for word in ("authorization", "api_key", "token", "cookie")):
                result[key] = "<redacted>"
            elif key == "source" and isinstance(item, str) and item.startswith("/"):
                result[key] = f"<redacted-local-source>/{Path(item).name}"
            else:
                result[key] = scrub(item, tunnel_id, direct_hosts)
        return result
    if isinstance(value, list):
        return [scrub(item, tunnel_id, direct_hosts) for item in value]
    if isinstance(value, str):
        cleaned = value.replace(tunnel_id, "<redacted-tunnel-id>")
        for host in direct_hosts:
            cleaned = cleaned.replace(host, "<redacted-reading-mcp-host>")
        return cleaned
    return value


def main() -> None:
    direct_url_name = next((name for name in URL_ENV_NAMES if os.environ.get(name)), None)
    direct_token_name = next((name for name in TOKEN_ENV_NAMES if os.environ.get(name)), None)
    direct_url = os.environ.get(direct_url_name, "") if direct_url_name else ""
    direct_token = os.environ.get(direct_token_name, "") if direct_token_name else ""

    output: dict[str, Any] = {
        "probe_version": 3,
        "document_id": DOC_ID,
        "control_plane_key_present": bool(CONTROL_KEY),
        "protected_configuration_presence": {
            "url": {name: bool(os.environ.get(name)) for name in URL_ENV_NAMES},
            "token": {name: bool(os.environ.get(name)) for name in TOKEN_ENV_NAMES},
        },
        "attempts": [],
    }

    deploy = Path(".github/workflows/deploy-tunnel.yml").read_text(encoding="utf-8")
    match = re.search(r"tunnel_[0-9a-f]{32}", deploy)
    if not match:
        output["fatal_error"] = "tunnel id not found"
        OUTPUT.write_text(json.dumps(output, ensure_ascii=False, indent=2), encoding="utf-8")
        return
    tunnel_id = match.group(0)
    output["tunnel_id_sha256_prefix"] = hashlib.sha256(tunnel_id.encode()).hexdigest()[:12]

    status, headers, raw = request(
        f"https://api.openai.com/v1/tunnels/{tunnel_id}", bearer=CONTROL_KEY
    )
    metadata = decode(raw, headers.get("Content-Type", ""))
    output["metadata_status"] = status

    candidates: list[tuple[str, str, str]] = []
    for url in normalize_direct_urls(direct_url):
        candidates.append((url, direct_token, "protected-direct"))
    candidates.extend(
        [
            (f"https://api.openai.com/v1/mcp/{tunnel_id}", CONTROL_KEY, "control-plane-derived"),
            (f"https://mcp.openai.com/v1/mcp/{tunnel_id}", CONTROL_KEY, "control-plane-derived"),
        ]
    )

    rpc_id = 0
    chosen: tuple[str, str] | None = None
    session_id: str | None = None
    direct_hosts = {
        urllib.parse.urlparse(url).netloc for url in normalize_direct_urls(direct_url)
    }
    for endpoint, bearer, kind in candidates:
        rpc_id += 1
        init = {
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "tlpi-source-map-probe", "version": "3.0"},
            },
        }
        init_status, init_headers, init_raw = request(
            endpoint, method="POST", payload=init, bearer=bearer
        )
        decoded = decode(init_raw, init_headers.get("Content-Type", ""))
        output["attempts"].append(
            {
                "kind": kind,
                "host_hash": hashlib.sha256(
                    urllib.parse.urlparse(endpoint).netloc.encode()
                ).hexdigest()[:12],
                "initialize_status": init_status,
                "initialize_response": decoded,
            }
        )
        if init_status in (200, 202) and isinstance(decoded, dict) and (
            "result" in decoded or init_status == 202
        ):
            chosen = (endpoint, bearer)
            session_id = init_headers.get("Mcp-Session-Id") or init_headers.get("mcp-session-id")
            break

    if chosen is None:
        OUTPUT.write_text(
            json.dumps(scrub(output, tunnel_id, direct_hosts), ensure_ascii=False, indent=2),
            encoding="utf-8",
        )
        return

    endpoint, bearer = chosen
    output["session_established"] = bool(session_id)
    request(
        endpoint,
        method="POST",
        payload={"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}},
        session_id=session_id,
        bearer=bearer,
    )

    def rpc(method: str, params: dict[str, Any]) -> dict[str, Any]:
        nonlocal rpc_id
        rpc_id += 1
        rpc_status, rpc_headers, rpc_raw = request(
            endpoint,
            method="POST",
            payload={"jsonrpc": "2.0", "id": rpc_id, "method": method, "params": params},
            session_id=session_id,
            bearer=bearer,
        )
        decoded = decode(rpc_raw, rpc_headers.get("Content-Type", ""))
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
            if isinstance(node, dict):
                flat.append(node)
                walk(node.get("children"))

    walk(sections)
    output["returned_node_count"] = len(flat)
    preface = next(
        (node for node in flat if re.search(r"前言|序言|Preface", str(node.get("title", "")), re.I)),
        None,
    )
    read_node = preface or (flat[0] if flat else None)
    if read_node and read_node.get("section_id"):
        read_result = rpc(
            "tools/call",
            {
                "name": "read_document",
                "arguments": {
                    "document_id": DOC_ID,
                    "section_id": read_node["section_id"],
                    "max_chars": 24000 if preface else 2000,
                },
            },
        )
        payload = read_result.get("payload")
        source = payload.get("source") if isinstance(payload, dict) else None
        if isinstance(source, str):
            output["source_basename"] = Path(source).name
            output["open_document"] = rpc(
                "tools/call",
                {"name": "open_document", "arguments": {"source": source, "force_refresh": False}},
            )
        if preface:
            output["preface"] = read_result

    for query in (
        "本书的目标 读者",
        "本书的组织结构 章节",
        "基础部分 后续章节",
        "版本 版次 ISBN",
    ):
        output.setdefault("structural_searches", []).append(
            {
                "query": query,
                "result": rpc(
                    "tools/call",
                    {
                        "name": "search_document",
                        "arguments": {"document_id": DOC_ID, "query": query, "limit": 8},
                    },
                ),
            }
        )

    OUTPUT.write_text(
        json.dumps(scrub(output, tunnel_id, direct_hosts), ensure_ascii=False, indent=2),
        encoding="utf-8",
    )
    print("sanitized reading-mcp probe complete")


if __name__ == "__main__":
    main()
