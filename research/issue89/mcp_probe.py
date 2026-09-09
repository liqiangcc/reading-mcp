#!/usr/bin/env python3
import hashlib
import json
import os
import subprocess
import sys


def structured(result):
    if result.get("structuredContent") is not None:
        return result["structuredContent"]
    for block in result.get("content", []):
        if block.get("type") == "text":
            return json.loads(block["text"])
    raise RuntimeError("missing structured content")


class Client:
    def __init__(self):
        env = os.environ.copy()
        env.setdefault("READING_MCP_STATE_DIR", "memory")
        env.setdefault("READING_MCP_TELEMETRY", "false")
        self.proc = subprocess.Popen(
            [env.get("READING_MCP_BINARY", "/root/reading-mcp/target/release/reading-mcp")],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            env=env,
        )
        self.next_id = 1

    def request(self, method, params):
        request_id = self.next_id
        self.next_id += 1
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}) + "\n")
        self.proc.stdin.flush()
        for line in self.proc.stdout:
            response = json.loads(line)
            if response.get("id") == request_id:
                if "error" in response:
                    raise RuntimeError(response["error"])
                return response["result"]
        raise RuntimeError("server closed before response")

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=5)


def main():
    source = sys.argv[1]
    client = Client()
    try:
        initialized = client.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "issue89-research", "version": "1"}})
        client.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}) + "\n")
        client.proc.stdin.flush()
        tools = client.request("tools/list", {})
        opened = structured(client.request("tools/call", {"name": "open_document", "arguments": {"source": source, "force_refresh": False}}))
        structure = structured(client.request("tools/call", {"name": "get_document_structure", "arguments": {"document_id": opened["document_id"], "max_nodes": 200}}))
        section_id = structure["sections"][0]["section_id"]
        units = structured(client.request("tools/call", {"name": "get_text_units", "arguments": {"document_id": opened["document_id"], "section_id": section_id, "requested_kind": "sentence", "coverage_policy": "preserve_source", "max_items": 200, "max_chars": 65536}}))
        first = units.get("items", [None])[0]
        exact = None
        if first is not None:
            exact = structured(client.request("tools/call", {"name": "read_document", "arguments": {"document_id": opened["document_id"], "target_locator": first["locator"], "max_chars": 65536}}))
        item_text = first.get("text", "") if first else ""
        exact_text = exact.get("content", "") if exact else ""
        output = {
            "transport": "stdio",
            "server": initialized.get("serverInfo", {}).get("name"),
            "tool_count": len(tools.get("tools", [])),
            "normalization_version": opened.get("normalization_version"),
            "normalized_document_hash": opened.get("normalized_document_hash"),
            "section_count": opened.get("section_count"),
            "first_section_id_hash": hashlib.sha256(section_id.encode()).hexdigest(),
            "sentence_item_count": len(units.get("items", [])),
            "first_item_effective_kind": first.get("effective_kind") if first else None,
            "first_item_degradation": first.get("degradation") if first else None,
            "first_item_chars": len(item_text),
            "first_item_sha256": hashlib.sha256(item_text.encode()).hexdigest() if first else None,
            "exact_read_chars": len(exact_text),
            "exact_read_equals_item": exact_text == item_text if first else None,
            "coverage": units.get("coverage"),
        }
        print(json.dumps(output, sort_keys=True, separators=(",", ":")))
    finally:
        client.close()


if __name__ == "__main__":
    main()
