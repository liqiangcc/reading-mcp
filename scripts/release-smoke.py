#!/usr/bin/env python3
"""Smoke-test a packaged reading-mcp binary over MCP stdio.

The script can execute the binary directly or inside a Docker container. Container mode runs
the server process inside the image while this script remains the stdio client on the host,
which keeps the test harness independent of the minimal distribution image.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import queue
import re
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

INITIALIZE_PARAMS = {
    "protocolVersion": "2025-06-18",
    "capabilities": {},
    "clientInfo": {"name": "release-smoke", "version": "0"},
}
EXPECTED_TOOLS = {"browse", "cite", "open", "outline", "read", "search", "source_view"}


class SmokeError(RuntimeError):
    pass


def send(process: subprocess.Popen, message: dict) -> None:
    assert process.stdin is not None
    process.stdin.write((json.dumps(message) + "\n").encode("utf-8"))
    process.stdin.flush()


def pump_stdout(stream, responses: queue.Queue) -> None:
    try:
        for line in stream:
            try:
                responses.put(json.loads(line.decode("utf-8")))
            except (UnicodeDecodeError, json.JSONDecodeError):
                continue
    finally:
        responses.put(None)


def read_response(responses: queue.Queue, awaited_id: int, timeout: float) -> dict:
    deadline = time.monotonic() + timeout
    while True:
        wait = deadline - time.monotonic()
        if wait <= 0:
            raise SmokeError(f"timed out waiting for response id {awaited_id}")
        try:
            item = responses.get(timeout=wait)
        except queue.Empty as exc:
            raise SmokeError(f"timed out waiting for response id {awaited_id}") from exc
        if item is None:
            raise SmokeError("server closed stdout before responding")
        if isinstance(item, dict) and item.get("id") == awaited_id:
            return item


def response_error(response: dict) -> str | None:
    if response.get("error") is not None:
        return json.dumps(response["error"], sort_keys=True)
    result = response.get("result")
    if isinstance(result, dict) and result.get("isError"):
        texts = [
            block.get("text", "")
            for block in result.get("content", [])
            if isinstance(block, dict) and block.get("type") == "text"
        ]
        return " ".join(texts) or "tool error"
    return None


def payload(result: dict) -> dict:
    for block in result.get("content", []) or []:
        if isinstance(block, dict) and block.get("type") == "text":
            try:
                value = json.loads(block.get("text", ""))
            except json.JSONDecodeError:
                continue
            if isinstance(value, dict):
                return value
    raise SmokeError(f"tool result has no compact JSON object: {result}")


class McpClient:
    def __init__(self, command: list[str], env: dict[str, str], stderr_path: Path) -> None:
        self.stderr = stderr_path.open("wb")
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.stderr,
            env=env,
        )
        self.responses: queue.Queue = queue.Queue()
        threading.Thread(target=pump_stdout, args=(self.process.stdout, self.responses), daemon=True).start()
        self.next_id = 0

    def request(self, method: str, params: dict, timeout: float = 60.0) -> dict:
        self.next_id += 1
        send(
            self.process,
            {"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params},
        )
        response = read_response(self.responses, self.next_id, timeout)
        failure = response_error(response)
        if failure:
            raise SmokeError(f"{method} failed: {failure}")
        result = response.get("result")
        if not isinstance(result, dict):
            raise SmokeError(f"{method} returned no object result")
        return result

    def initialize(self) -> None:
        result = self.request("initialize", INITIALIZE_PARAMS)
        if result.get("serverInfo", {}).get("name") != "reading-mcp":
            raise SmokeError(f"unexpected initialize serverInfo: {result}")
        send(self.process, {"jsonrpc": "2.0", "method": "notifications/initialized"})

    def tool(self, name: str, arguments: dict, timeout: float = 90.0) -> dict:
        return self.request("tools/call", {"name": name, "arguments": arguments}, timeout)

    def close(self) -> None:
        try:
            if self.process.stdin is not None:
                self.process.stdin.close()
        except OSError:
            pass
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=10)
        self.stderr.close()
        if self.process.returncode not in (0, None):
            raise SmokeError(f"server exited with {self.process.returncode}")


def compact(result: dict) -> dict:
    return payload(result)


def first_text_read(client: McpClient, handle: str) -> tuple[str, str]:
    result = client.tool("read", {"from": handle})
    data = compact(result)
    text = data.get("text")
    if not isinstance(text, str) or not text.strip():
        raise SmokeError(f"read returned no text: {data}")
    item_handle = data.get("at")
    if not isinstance(item_handle, str) or not item_handle:
        raise SmokeError(f"read returned no item handle: {data}")
    return item_handle, text


def exercise_document(client: McpClient, source: str) -> tuple[str, str]:
    opened = compact(client.tool("open", {"source": source}, timeout=180.0))
    doc = opened.get("doc")
    if not isinstance(doc, str) or not doc:
        raise SmokeError(f"open returned no document handle: {opened}")
    item_handle, text = first_text_read(client, doc)
    words = re.findall(r"[A-Za-z][A-Za-z-]{3,}", text)
    if not words:
        raise SmokeError(f"cannot choose a search query from read text: {text[:160]!r}")
    query = words[min(2, len(words) - 1)]
    searched = compact(client.tool("search", {"doc": doc, "query": query, "limit": 5}))
    if not isinstance(searched.get("hits"), list) or not searched["hits"]:
        raise SmokeError(f"search returned no hits for {query!r}: {searched}")
    cited = compact(client.tool("cite", {"at": [item_handle]}))
    items = cited.get("items")
    if not isinstance(items, list) or not items:
        raise SmokeError(f"cite returned no items: {cited}")
    identity = items[0].get("doc", {}) if isinstance(items[0], dict) else {}
    if not str(identity.get("content_hash", "")).startswith("sha256:"):
        raise SmokeError(f"cite returned no content hash: {cited}")
    return doc, item_handle


def assert_source_view_png(client: McpClient, handle: str) -> None:
    result = client.tool("source_view", {"at": handle}, timeout=180.0)
    metadata = compact(result)
    if not isinstance(metadata.get("page"), int) or not isinstance(metadata.get("pages"), int):
        raise SmokeError(f"source_view metadata is incomplete: {metadata}")
    image_blocks = [
        block
        for block in result.get("content", []) or []
        if isinstance(block, dict) and block.get("type") == "image" and block.get("mimeType") == "image/png"
    ]
    if not image_blocks:
        raise SmokeError(f"source_view returned no PNG image block: {result}")
    encoded = image_blocks[0].get("data")
    if not isinstance(encoded, str):
        raise SmokeError("source_view PNG image block has no base64 data")
    try:
        png = base64.b64decode(encoded, validate=True)
    except ValueError as exc:
        raise SmokeError("source_view PNG image block is invalid base64") from exc
    if not png.startswith(b"\x89PNG\r\n\x1a\n") or len(png) < 100:
        raise SmokeError(f"source_view image is not a plausible PNG ({len(png)} bytes)")


def prepare_inputs(work_dir: Path, pdf_source: Path) -> tuple[Path, Path]:
    inputs = work_dir / "input"
    state = work_dir / "state"
    shutil.rmtree(inputs, ignore_errors=True)
    shutil.rmtree(state, ignore_errors=True)
    inputs.mkdir(parents=True)
    state.mkdir(parents=True)
    (inputs / "fixture.txt").write_text(
        "Portable release smoke fixture.\n\n"
        "The quick brown fox carries a portable binary across older Linux distributions.\n"
        "SQLite indexing, compressed text, search, citation, and reading all use this sentence.\n",
        encoding="utf-8",
    )
    shutil.copy2(pdf_source, inputs / "fixture.pdf")
    return inputs, state


def server_environment(input_root: str, state_root: str) -> dict[str, str]:
    env = dict(os.environ)
    env.update(
        {
            "READING_MCP_LOCAL_ROOTS": input_root,
            "READING_MCP_STATE_DIR": state_root,
            "READING_MCP_TELEMETRY": "false",
            "READING_MCP_OCR_ENABLED": "false",
        }
    )
    env.pop("READING_MCP_TOOLS", None)
    env.pop("READING_MCP_PDF_LAYOUT_PYTHON", None)
    return env


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True, help="packaged reading-mcp path on the host")
    parser.add_argument("--pdf", type=Path, required=True, help="text-bearing PDF fixture path on the host")
    parser.add_argument("--work-dir", type=Path, required=True, help="scratch directory on the host")
    parser.add_argument("--image", help="Docker image to run inside; omit for a direct native smoke test")
    args = parser.parse_args()

    binary = args.binary.resolve()
    pdf = args.pdf.resolve()
    work_dir = args.work_dir.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise SmokeError(f"binary is not executable: {binary}")
    if not pdf.is_file():
        raise SmokeError(f"PDF fixture does not exist: {pdf}")

    inputs, state = prepare_inputs(work_dir, pdf)
    stderr_path = work_dir / "server-stderr.log"
    if args.image:
        if shutil.which("docker") is None:
            raise SmokeError("docker is required for container smoke mode")
        env = server_environment("/smoke/input", "/smoke/state")
        command = [
            "docker",
            "run",
            "--rm",
            "-i",
            "--network",
            "none",
            "-v",
            f"{binary.parent}:/pkg:ro",
            "-v",
            f"{inputs}:/smoke/input:ro",
            "-v",
            f"{state}:/smoke/state:rw",
        ]
        for key in (
            "READING_MCP_LOCAL_ROOTS",
            "READING_MCP_STATE_DIR",
            "READING_MCP_TELEMETRY",
            "READING_MCP_OCR_ENABLED",
        ):
            command.extend(["-e", f"{key}={env[key]}"])
        command.extend([args.image, "/pkg/reading-mcp"])
        text_source = "/smoke/input/fixture.txt"
        pdf_source = "/smoke/input/fixture.pdf"
    else:
        command = [str(binary)]
        env = server_environment(str(inputs), str(state))
        text_source = str(inputs / "fixture.txt")
        pdf_source = str(inputs / "fixture.pdf")

    client = McpClient(command, env, stderr_path)
    try:
        client.initialize()
        tools = client.request("tools/list", {})
        names = {tool.get("name") for tool in tools.get("tools", []) if isinstance(tool, dict)}
        if names != EXPECTED_TOOLS:
            raise SmokeError(f"tools/list mismatch: got {sorted(names)}")
        exercise_document(client, text_source)
        _pdf_doc, pdf_item = exercise_document(client, pdf_source)
        assert_source_view_png(client, pdf_item)
    finally:
        try:
            client.close()
        except SmokeError:
            raise
        except Exception:
            pass
    print(f"smoke ok: {args.image or 'native'}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except SmokeError as exc:
        print(f"smoke failed: {exc}", file=sys.stderr)
        raise SystemExit(1)
