#!/usr/bin/env python3
"""Read TLPI through a local read-only reading-mcp stdio session.

This script is intended for the repository's existing self-hosted runner. It
reuses only READING_MCP_* runtime configuration, starts an independent process,
and never restarts or modifies the production service.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import select
import shlex
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

DOC_ID = "doc:sha256:286e0104a40d05c3cb76f08e2d6a06391ce9d1bc603351aefc2340aca3349b2f"
DOC_DIGEST = DOC_ID.rsplit(":", 1)[-1]
OUTPUT = Path("tlpi-local-probe.json")
UNIT_CANDIDATES = ("reading-mcp-tunnel.service", "reading-mcp.service")
SUPPORTED_EXTENSIONS = {".epub", ".pdf", ".md", ".markdown", ".txt", ".html", ".htm"}


def parse_json_string(value: Any) -> Any:
    current = value
    for _ in range(5):
        if not isinstance(current, str):
            return current
        text = current.strip()
        if not text:
            return current
        try:
            current = json.loads(text)
        except json.JSONDecodeError:
            return current
    return current


def unwrap_tool_result(value: Any) -> Any:
    value = parse_json_string(value)
    if isinstance(value, dict):
        content = value.get("content")
        if isinstance(content, list):
            texts = [
                item.get("text", "")
                for item in content
                if isinstance(item, dict) and item.get("type") == "text"
            ]
            if texts:
                return unwrap_tool_result("\n".join(texts))
        if "result" in value and len(value) <= 5:
            nested = unwrap_tool_result(value["result"])
            if nested is not value["result"]:
                return nested
    return value


def parse_env_assignment(text: str) -> tuple[str, str] | None:
    text = text.strip()
    if not text or text.startswith("#") or "=" not in text:
        return None
    if text.startswith("export "):
        text = text[7:].lstrip()
    name, value = text.split("=", 1)
    name = name.strip()
    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name):
        return None
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
        value = value[1:-1]
    return name, value


def read_process_env(pid: int) -> dict[str, str]:
    if pid <= 0:
        return {}
    try:
        raw = Path(f"/proc/{pid}/environ").read_bytes()
    except OSError:
        return {}
    result: dict[str, str] = {}
    for item in raw.split(b"\0"):
        if b"=" not in item:
            continue
        name, value = item.split(b"=", 1)
        try:
            result[name.decode()] = value.decode()
        except UnicodeDecodeError:
            continue
    return result


def systemctl_value(unit: str, property_name: str) -> str:
    try:
        completed = subprocess.run(
            ["systemctl", "show", unit, f"--property={property_name}", "--value"],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired):
        return ""
    return completed.stdout.strip() if completed.returncode == 0 else ""


def discover_runtime_environment() -> tuple[dict[str, str], dict[str, Any]]:
    runtime_env = dict(os.environ)
    diagnostics: dict[str, Any] = {
        "service_found": False,
        "service_active": False,
        "service_unit": None,
        "process_environment_readable": False,
        "environment_file_readable": False,
    }

    for unit in UNIT_CANDIDATES:
        load_state = systemctl_value(unit, "LoadState")
        if load_state != "loaded":
            continue
        diagnostics["service_found"] = True
        diagnostics["service_unit"] = unit
        diagnostics["service_active"] = systemctl_value(unit, "ActiveState") == "active"

        try:
            pid = int(systemctl_value(unit, "MainPID") or "0")
        except ValueError:
            pid = 0
        process_env = read_process_env(pid)
        if process_env:
            diagnostics["process_environment_readable"] = True
            for name, value in process_env.items():
                if name.startswith("READING_MCP_") or name in {"HOME", "USERPROFILE"}:
                    runtime_env[name] = value

        environment = systemctl_value(unit, "Environment")
        if environment:
            try:
                tokens = shlex.split(environment)
            except ValueError:
                tokens = environment.split()
            for token in tokens:
                assignment = parse_env_assignment(token)
                if assignment and assignment[0].startswith("READING_MCP_"):
                    runtime_env[assignment[0]] = assignment[1]

        environment_files = systemctl_value(unit, "EnvironmentFiles")
        if environment_files:
            # systemd renders entries as '/path (ignore_errors=no)'. Only inspect
            # the path token; never include it in the artifact.
            for match in re.finditer(r"(?:^|\s)(/\S+?)(?=\s+\(|\s*$)", environment_files):
                path = Path(match.group(1))
                try:
                    lines = path.read_text(encoding="utf-8").splitlines()
                except OSError:
                    continue
                diagnostics["environment_file_readable"] = True
                for line in lines:
                    assignment = parse_env_assignment(line)
                    if assignment and assignment[0].startswith("READING_MCP_"):
                        runtime_env[assignment[0]] = assignment[1]
        break

    runtime_env["READING_MCP_TELEMETRY"] = "false"
    roots = [
        Path(item).expanduser()
        for item in os.pathsep.join([runtime_env.get("READING_MCP_LOCAL_ROOTS", "")]).split(os.pathsep)
        if item
    ]
    diagnostics["configured_root_count"] = len(roots)
    diagnostics["readable_root_count"] = sum(1 for root in roots if root.is_dir())
    diagnostics["state_dir_configured"] = bool(runtime_env.get("READING_MCP_STATE_DIR"))
    return runtime_env, diagnostics


def candidate_document_id(source: str, raw_bytes: bytes) -> str:
    content_hash = "sha256:" + hashlib.sha256(raw_bytes).hexdigest()
    digest = hashlib.sha256(source.encode() + b"\0" + content_hash.encode()).hexdigest()
    return "doc:sha256:" + digest


def locate_source(runtime_env: dict[str, str]) -> Path | None:
    raw_roots = runtime_env.get("READING_MCP_LOCAL_ROOTS", "")
    roots = [Path(item).expanduser() for item in raw_roots.split(os.pathsep) if item]
    for root in roots:
        if not root.is_dir():
            continue
        try:
            iterator = root.rglob("*")
        except OSError:
            continue
        for path in iterator:
            try:
                if not path.is_file() or path.suffix.lower() not in SUPPORTED_EXTENSIONS:
                    continue
                raw = path.read_bytes()
            except OSError:
                continue
            variants = {str(path), str(path.absolute())}
            try:
                variants.add(str(path.resolve()))
            except OSError:
                pass
            for source in variants:
                if candidate_document_id(source, raw) == DOC_ID:
                    return path
    return None


class StdioMcpClient:
    def __init__(self, binary: Path, env: dict[str, str]) -> None:
        self.process = subprocess.Popen(
            [str(binary)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            bufsize=1,
            env=env,
        )
        if self.process.stdin is None or self.process.stdout is None:
            raise RuntimeError("failed to open reading-mcp stdio pipes")
        self.stdin = self.process.stdin
        self.stdout = self.process.stdout
        self.next_id = 1
        self.noise: list[str] = []

    def send(self, message: dict[str, Any]) -> None:
        self.stdin.write(json.dumps(message, ensure_ascii=False) + "\n")
        self.stdin.flush()

    def receive_for_id(self, request_id: int, timeout: float = 180.0) -> dict[str, Any]:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                stderr = ""
                if self.process.stderr is not None:
                    try:
                        stderr = self.process.stderr.read()[-4000:]
                    except OSError:
                        pass
                raise RuntimeError(
                    f"reading-mcp exited with {self.process.returncode}; stderr={stderr}"
                )
            remaining = max(0.0, deadline - time.monotonic())
            ready, _, _ = select.select([self.stdout], [], [], min(1.0, remaining))
            if not ready:
                continue
            line = self.stdout.readline()
            if not line:
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                if len(self.noise) < 20:
                    self.noise.append(line[:500])
                continue
            if message.get("id") == request_id:
                return message
        raise TimeoutError(f"timed out waiting for JSON-RPC id {request_id}")

    def request(self, method: str, params: dict[str, Any], timeout: float = 180.0) -> dict[str, Any]:
        request_id = self.next_id
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        return self.receive_for_id(request_id, timeout)

    def notify(self, method: str, params: dict[str, Any]) -> None:
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def close(self) -> None:
        try:
            self.stdin.close()
        except OSError:
            pass
        try:
            self.process.terminate()
            self.process.wait(timeout=5)
        except Exception:  # noqa: BLE001
            self.process.kill()


def tool_payload(response: dict[str, Any]) -> Any:
    if "error" in response:
        return response
    return unwrap_tool_result(response.get("result"))


def flatten_sections(nodes: Any) -> list[dict[str, Any]]:
    result: list[dict[str, Any]] = []

    def walk(value: Any) -> None:
        if not isinstance(value, list):
            return
        for node in value:
            if not isinstance(node, dict):
                continue
            result.append(node)
            walk(node.get("children"))

    walk(nodes)
    return result


def find_sections_payload(value: Any) -> dict[str, Any] | None:
    value = parse_json_string(value)
    if isinstance(value, dict):
        if isinstance(value.get("sections"), list):
            return value
        for child in value.values():
            found = find_sections_payload(child)
            if found is not None:
                return found
    elif isinstance(value, list):
        for child in value:
            found = find_sections_payload(child)
            if found is not None:
                return found
    return None


def collect_sources(value: Any, result: set[str]) -> None:
    value = parse_json_string(value)
    if isinstance(value, dict):
        for key, child in value.items():
            if key == "source" and isinstance(child, str) and child:
                result.add(child)
            collect_sources(child, result)
    elif isinstance(value, list):
        for child in value:
            collect_sources(child, result)


def sanitize(value: Any, source_values: set[str]) -> Any:
    if isinstance(value, dict):
        result: dict[str, Any] = {}
        for key, child in value.items():
            lowered = key.lower()
            if any(word in lowered for word in ("authorization", "api_key", "token", "cookie")):
                result[key] = "<redacted>"
            elif key == "source" and isinstance(child, str):
                result[key] = f"<redacted-local-source>/{Path(child).name}"
            elif key in {"service_unit"} and isinstance(child, str):
                result[key] = child
            else:
                result[key] = sanitize(child, source_values)
        return result
    if isinstance(value, list):
        return [sanitize(child, source_values) for child in value]
    if isinstance(value, str):
        cleaned = value
        for source in source_values:
            cleaned = cleaned.replace(source, f"<redacted-local-source>/{Path(source).name}")
        # Redact any remaining absolute path fragments without changing URLs.
        cleaned = re.sub(
            r"(?<![:A-Za-z0-9])/(?:[^\s\"']+/)+[^\s\"']+",
            "<redacted-local-path>",
            cleaned,
        )
        return cleaned
    return value


def main() -> None:
    output: dict[str, Any] = {
        "probe_version": 1,
        "document_id": DOC_ID,
        "execution": "self-hosted-local-stdio",
    }
    source_values: set[str] = set()
    client: StdioMcpClient | None = None
    try:
        runtime_env, diagnostics = discover_runtime_environment()
        output["runtime"] = diagnostics
        binary = Path(os.environ.get("GITHUB_WORKSPACE", ".")) / "target/release/reading-mcp"
        output["binary_present"] = binary.is_file()
        if not binary.is_file():
            raise RuntimeError("reading-mcp release binary was not built")

        client = StdioMcpClient(binary, runtime_env)
        initialize = client.request(
            "initialize",
            {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "tlpi-source-map-local-probe", "version": "1.0"},
            },
            timeout=60,
        )
        output["initialize"] = initialize
        client.notify("notifications/initialized", {})
        output["tools_list"] = client.request("tools/list", {}, timeout=60)

        def call(name: str, arguments: dict[str, Any], timeout: float = 180.0) -> Any:
            response = client.request(
                "tools/call", {"name": name, "arguments": arguments}, timeout=timeout
            )
            output.setdefault("tool_calls", []).append(
                {"name": name, "arguments": arguments, "response": response}
            )
            return tool_payload(response)

        structure = call(
            "get_document_structure", {"document_id": DOC_ID, "max_depth": 12}, timeout=240
        )
        root = find_sections_payload(structure)
        if root is None:
            located = locate_source(runtime_env)
            output["source_located_by_exact_document_id"] = bool(located)
            if located is not None:
                opened = call(
                    "open_document", {"source": str(located), "force_refresh": False}, timeout=300
                )
                output["fallback_open_document"] = opened
                structure = call(
                    "get_document_structure",
                    {"document_id": DOC_ID, "max_depth": 12},
                    timeout=240,
                )
                root = find_sections_payload(structure)
        output["structure_payload"] = structure
        if root is None:
            raise RuntimeError("get_document_structure did not return sections")

        flat = flatten_sections(root.get("sections"))
        output["returned_node_count"] = len(flat)
        preface = next(
            (
                node
                for node in flat
                if re.search(r"前言|序言|Preface", str(node.get("title", "")), re.I)
            ),
            None,
        )
        output["preface_locator"] = preface
        if preface and preface.get("section_id"):
            output["preface_payload"] = call(
                "read_document",
                {
                    "document_id": DOC_ID,
                    "section_id": preface["section_id"],
                    "max_chars": 24000,
                },
                timeout=240,
            )

        collect_sources(output, source_values)
        raw_source = next(iter(source_values), None)
        if raw_source:
            output["open_document_payload"] = call(
                "open_document", {"source": raw_source, "force_refresh": False}, timeout=300
            )

        searches = []
        for query in (
            "本书的目标 目标读者",
            "本书的组织结构 章节",
            "基础部分 后续章节",
            "版本 版次 ISBN",
        ):
            searches.append(
                {
                    "query": query,
                    "payload": call(
                        "search_document",
                        {"document_id": DOC_ID, "query": query, "limit": 12},
                        timeout=180,
                    ),
                }
            )
        output["structural_searches"] = searches
        output["stdio_noise_line_count"] = len(client.noise)
    except Exception as error:  # noqa: BLE001 - artifact must survive every failure
        output["fatal_error"] = f"{type(error).__name__}: {error}"
    finally:
        if client is not None:
            client.close()
        collect_sources(output, source_values)
        write_json(sanitize(output, source_values))
        print("sanitized local reading-mcp probe complete")


if __name__ == "__main__":
    main()
