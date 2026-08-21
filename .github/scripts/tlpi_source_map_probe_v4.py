#!/usr/bin/env python3
"""Invoke the deployed reading-mcp through the OpenAI product-side tunnel path.

The script writes only a sanitized diagnostic artifact. It never prints API keys,
tunnel identifiers, or local source paths.
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

DOC_ID = "doc:sha256:286e0104a40d05c3cb76f08e2d6a06391ce9d1bc603351aefc2340aca3349b2f"
OUTPUT = Path("tlpi-probe.json")
CONTROL_KEY = os.environ.get("CONTROL_PLANE_API_KEY", "")
RESPONSES_KEY = os.environ.get("OPENAI_API_KEY", "") or CONTROL_KEY
MODEL = os.environ.get("TLPI_PROBE_MODEL", "gpt-5.6")


def write_json(value: Any) -> None:
    OUTPUT.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")


def http_post_json(url: str, payload: dict[str, Any], bearer: str) -> tuple[int, dict[str, str], Any]:
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(
        url,
        data=body,
        method="POST",
        headers={
            "Authorization": f"Bearer {bearer}",
            "Content-Type": "application/json",
            "Accept": "application/json",
            "User-Agent": "tlpi-source-map-read-only-probe/4.0",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=240) as response:
            raw = response.read().decode("utf-8", "replace")
            try:
                decoded: Any = json.loads(raw)
            except json.JSONDecodeError:
                decoded = {"raw_excerpt": raw[:8000]}
            return response.status, dict(response.headers.items()), decoded
    except urllib.error.HTTPError as error:
        raw = error.read().decode("utf-8", "replace")
        try:
            decoded = json.loads(raw)
        except json.JSONDecodeError:
            decoded = {"raw_excerpt": raw[:8000]}
        return error.code, dict(error.headers.items()), decoded
    except Exception as error:  # noqa: BLE001 - preserve a diagnostic artifact
        return 0, {}, {"exception": f"{type(error).__name__}: {error}"}


def parse_json_string(value: Any) -> Any:
    current = value
    for _ in range(4):
        if not isinstance(current, str):
            break
        stripped = current.strip()
        if not stripped:
            break
        try:
            current = json.loads(stripped)
        except json.JSONDecodeError:
            break
    return current


def unwrap_tool_result(value: Any) -> Any:
    value = parse_json_string(value)
    if isinstance(value, dict):
        if isinstance(value.get("content"), list):
            texts = [
                item.get("text", "")
                for item in value["content"]
                if isinstance(item, dict) and item.get("type") == "text"
            ]
            if texts:
                return unwrap_tool_result("\n".join(texts))
        if "result" in value and len(value) <= 4:
            nested = unwrap_tool_result(value["result"])
            if nested is not value["result"]:
                return nested
    return value


def mcp_call_items(response: Any) -> list[dict[str, Any]]:
    if not isinstance(response, dict):
        return []
    output = response.get("output")
    if not isinstance(output, list):
        return []
    return [item for item in output if isinstance(item, dict) and item.get("type") == "mcp_call"]


def extract_tool_payload(response: Any, expected_name: str) -> Any:
    calls = mcp_call_items(response)
    selected = next((item for item in calls if item.get("name") == expected_name), None)
    if selected is None and calls:
        selected = calls[-1]
    if selected is None:
        return None
    for key in ("output", "result"):
        if key in selected:
            return unwrap_tool_result(selected[key])
    return selected


def recursive_find_dict(value: Any, predicate: Any) -> dict[str, Any] | None:
    value = parse_json_string(value)
    if isinstance(value, dict):
        if predicate(value):
            return value
        for child in value.values():
            found = recursive_find_dict(child, predicate)
            if found is not None:
                return found
    elif isinstance(value, list):
        for child in value:
            found = recursive_find_dict(child, predicate)
            if found is not None:
                return found
    return None


def collect_source_values(value: Any, result: set[str]) -> None:
    value = parse_json_string(value)
    if isinstance(value, dict):
        for key, child in value.items():
            if key == "source" and isinstance(child, str) and child:
                result.add(child)
            collect_source_values(child, result)
    elif isinstance(value, list):
        for child in value:
            collect_source_values(child, result)


def walk_sections(nodes: Any, result: list[dict[str, Any]]) -> None:
    if not isinstance(nodes, list):
        return
    for node in nodes:
        if not isinstance(node, dict):
            continue
        result.append(node)
        walk_sections(node.get("children"), result)


def sanitize(value: Any, secrets: set[str], source_values: set[str]) -> Any:
    if isinstance(value, dict):
        cleaned: dict[str, Any] = {}
        for key, child in value.items():
            lowered = key.lower()
            if any(word in lowered for word in ("authorization", "api_key", "token", "cookie")):
                cleaned[key] = "<redacted>"
            elif key == "source" and isinstance(child, str):
                cleaned[key] = f"<redacted-local-source>/{Path(child).name}"
            else:
                cleaned[key] = sanitize(child, secrets, source_values)
        return cleaned
    if isinstance(value, list):
        return [sanitize(child, secrets, source_values) for child in value]
    if isinstance(value, str):
        cleaned = value
        for secret in secrets:
            if secret:
                cleaned = cleaned.replace(secret, "<redacted>")
        for source in source_values:
            if source:
                cleaned = cleaned.replace(source, f"<redacted-local-source>/{Path(source).name}")
        return cleaned
    return value


def main() -> None:
    output: dict[str, Any] = {
        "probe_version": 4,
        "document_id": DOC_ID,
        "model": MODEL,
        "responses_key_source": "OPENAI_API_KEY" if os.environ.get("OPENAI_API_KEY") else "CONTROL_PLANE_API_KEY",
        "responses_key_present": bool(RESPONSES_KEY),
        "calls": [],
    }
    deploy = Path(".github/workflows/deploy-tunnel.yml").read_text(encoding="utf-8")
    match = re.search(r"tunnel_[0-9a-f]{32}", deploy)
    if not match:
        output["fatal_error"] = "tunnel id not found"
        write_json(output)
        return
    tunnel_id = match.group(0)
    tunnel_url = f"https://api.openai.com/v1/mcp/{tunnel_id}"
    output["tunnel_id_sha256_prefix"] = hashlib.sha256(tunnel_id.encode()).hexdigest()[:12]

    def call_tool(name: str, arguments: dict[str, Any]) -> tuple[Any, Any]:
        base_tool = {
            "type": "mcp",
            "server_label": "reading_mcp",
            "server_description": "Read-only document navigation and reading service.",
            "server_url": tunnel_url,
            "require_approval": "never",
        }
        attempts: list[dict[str, Any]] = []
        for auth_mode in ("product-context", "runtime-key-forwarded"):
            tool = dict(base_tool)
            if auth_mode == "runtime-key-forwarded":
                tool["authorization"] = CONTROL_KEY
            payload = {
                "model": MODEL,
                "store": False,
                "max_output_tokens": 256,
                "tools": [tool],
                "tool_choice": {"type": "mcp", "server_label": "reading_mcp", "name": name},
                "input": (
                    "Call the specified read-only MCP tool exactly once with exactly these JSON "
                    f"arguments: {json.dumps(arguments, ensure_ascii=False)}. "
                    "Do not call another tool. After the call, answer only DONE."
                ),
            }
            status, headers, response = http_post_json(
                "https://api.openai.com/v1/responses", payload, RESPONSES_KEY
            )
            attempt = {
                "tool": name,
                "auth_mode": auth_mode,
                "http_status": status,
                "request_id": headers.get("x-request-id"),
                "response": response,
            }
            attempts.append(attempt)
            tool_payload = extract_tool_payload(response, name)
            if status == 200 and tool_payload is not None:
                output["calls"].extend(attempts)
                return response, tool_payload
            if status in (401, 403, 404, 429) and auth_mode == "product-context":
                # A second attempt may distinguish product-context auth from tunnel auth.
                continue
            if status == 400 and auth_mode == "product-context":
                # Some deployments require explicit MCP authorization.
                continue
            break
        output["calls"].extend(attempts)
        return attempts[-1]["response"] if attempts else None, None

    structure_response, structure_payload = call_tool(
        "get_document_structure", {"document_id": DOC_ID, "max_depth": 12}
    )
    output["structure_payload"] = structure_payload
    structure_root = recursive_find_dict(
        structure_payload,
        lambda item: isinstance(item.get("sections"), list),
    )
    if structure_root is None:
        output["fatal_error"] = "get_document_structure did not return a sections payload"
        source_values: set[str] = set()
        collect_source_values(output, source_values)
        write_json(sanitize(output, {CONTROL_KEY, RESPONSES_KEY, tunnel_id}, source_values))
        return

    flat: list[dict[str, Any]] = []
    walk_sections(structure_root.get("sections"), flat)
    output["returned_node_count"] = len(flat)
    preface = next(
        (
            node
            for node in flat
            if re.search(r"前言|序言|Preface", str(node.get("title", "")), re.I)
        ),
        None,
    )
    if preface is None:
        output["preface_error"] = "preface node not found in structure"
    elif preface.get("section_id"):
        _, preface_payload = call_tool(
            "read_document",
            {
                "document_id": DOC_ID,
                "section_id": preface["section_id"],
                "max_chars": 24000,
            },
        )
        output["preface_payload"] = preface_payload

    source_values: set[str] = set()
    collect_source_values(output, source_values)
    raw_source = next(iter(source_values), None)
    if raw_source:
        _, open_payload = call_tool(
            "open_document", {"source": raw_source, "force_refresh": False}
        )
        output["open_document_payload"] = open_payload

    _, search_payload = call_tool(
        "search_document",
        {
            "document_id": DOC_ID,
            "query": "前言 本书目标 目标读者 组织结构 基础部分 后续章节 版本 版次 ISBN",
            "limit": 20,
        },
    )
    output["structural_search_payload"] = search_payload

    collect_source_values(output, source_values)
    write_json(sanitize(output, {CONTROL_KEY, RESPONSES_KEY, tunnel_id}, source_values))
    print("sanitized product-side reading-mcp probe complete")


if __name__ == "__main__":
    main()
