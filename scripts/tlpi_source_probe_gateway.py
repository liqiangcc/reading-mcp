#!/usr/bin/env python3
"""Retry the TLPI read-only probe against the connector-facing tunnel gateway."""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

from tlpi_source_probe import DOCUMENT_ID, decode, request, scrub, tool_payload

OUTPUT = Path("tlpi-probe.json")
GATEWAY = "https://tunnel-service.gateway.unified-0.internal.api.openai.org"


def main() -> int:
    deploy = Path(".github/workflows/deploy-tunnel.yml").read_text(encoding="utf-8")
    match = re.search(r"tunnel_[0-9a-f]{32}", deploy)
    result: dict[str, Any] = {
        "probe_version": 2,
        "document_id": DOCUMENT_ID,
        "gateway": GATEWAY,
    }
    if not match:
        result["fatal_error"] = "tunnel id not found"
        OUTPUT.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
        return 0

    tunnel_id = match.group(0)
    endpoint = f"{GATEWAY}/v1/mcp/{tunnel_id}"
    rpc_id = 1
    initialize = {
        "jsonrpc": "2.0",
        "id": rpc_id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "tlpi-source-map-probe", "version": "2.0"},
        },
    }
    status, headers, raw = request(endpoint, initialize)
    decoded = decode(raw, headers.get("Content-Type", ""))
    result["initialize"] = {
        "status": status,
        "headers": {
            key: value
            for key, value in headers.items()
            if key.lower() in {"content-type", "mcp-session-id", "x-request-id"}
        },
        "response": decoded,
    }

    if status not in (200, 202) or not isinstance(decoded, dict) or "result" not in decoded:
        OUTPUT.write_text(
            json.dumps(scrub(result, tunnel_id), ensure_ascii=False, indent=2),
            encoding="utf-8",
        )
        return 0

    session_id = headers.get("Mcp-Session-Id") or headers.get("mcp-session-id")
    request(
        endpoint,
        {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}},
        session_id,
    )

    def rpc(method: str, params: dict[str, Any]) -> dict[str, Any]:
        nonlocal rpc_id
        rpc_id += 1
        call_status, call_headers, call_raw = request(
            endpoint,
            {"jsonrpc": "2.0", "id": rpc_id, "method": method, "params": params},
            session_id,
        )
        call_decoded = decode(call_raw, call_headers.get("Content-Type", ""))
        return {
            "status": call_status,
            "rpc": call_decoded,
            "payload": tool_payload(call_decoded),
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

    payload = structure.get("payload")
    roots = payload.get("sections", []) if isinstance(payload, dict) else []
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
        result["preface"] = preface_read
        preface_payload = preface_read.get("payload")
        if isinstance(preface_payload, dict) and preface_payload.get("source"):
            result["open_document"] = rpc(
                "tools/call",
                {
                    "name": "open_document",
                    "arguments": {
                        "source": preface_payload["source"],
                        "force_refresh": False,
                    },
                },
            )
    else:
        result["preface_error"] = "preface node not found"

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
