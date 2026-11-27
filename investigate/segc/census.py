#!/usr/bin/env python3
"""segc census: dump every sentence unit of a set of sources through reading-mcp's
`open` + `read` (one unit per call) and classify segmentation defects.

Usage:
    census.py --binary PATH --state-dir DIR --sources FILE [--out DIR] [--jobs N]

`sources` is a file with one input per line: `path<TAB>group`, or just `path`
(the group defaults to `other`). Blank lines and `#` comments are ignored.
Groups seen in the report: `cjk-epub`, `en-epub`, `pdf-paper`, `fixture`, `other`.

Writes into --out (default `<state-dir>/../segc-out`):
    units.jsonl     one record per unit: {source, group, index, handle, kind, text}
    census.md       the classification tables (same content stdout summarizes)
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import threading
from collections import Counter, defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed

INIT = {
    "protocolVersion": "2025-06-18",
    "capabilities": {},
    "clientInfo": {"name": "segc-census", "version": "0"},
}

ABBREV_TOKENS = {
    "e.g", "i.e", "etc", "vs", "mr", "mrs", "ms", "dr", "prof", "sr", "jr",
    "fig", "eq", "no", "inc", "ltd", "u.s", "u.k", "al",
}
REF_TRIGGERS = re.compile(
    r"^[\[\(]?\d{1,3}[\]\)]?$|^\[\d+(-\d+)?\]$|^(arxiv|doi|issn|isbn|pp?\.|vol\.)",
    re.IGNORECASE,
)


def is_cjk_char(ch: str) -> bool:
    cp = ord(ch)
    return (
        0x4E00 <= cp <= 0x9FFF
        or 0x3400 <= cp <= 0x4DBF
        or 0xF900 <= cp <= 0xFAFF
        or 0x3000 <= cp <= 0x303F
        or 0xFF00 <= cp <= 0xFF65
    )


def cjk_ratio(text: str) -> float:
    letters = [c for c in text if c.isalpha() or is_cjk_char(c)]
    if not letters:
        return 0.0
    return sum(1 for c in letters if is_cjk_char(c)) / len(letters)


class Server:
    def __init__(self, binary: str, state_dir: str):
        env = dict(os.environ)
        env["READING_MCP_STATE_DIR"] = state_dir
        env["READING_MCP_LOCAL_ROOTS"] = "/"
        env.setdefault("READING_MCP_TELEMETRY", "0")
        self.proc = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env=env,
            bufsize=0,
        )
        self.next_id = 0
        self.cond = threading.Condition()
        self.responses: dict[int, dict] = {}
        self.thread = threading.Thread(target=self._pump, daemon=True)
        self.thread.start()
        self._call_raw("initialize", INIT)
        self._notify("notifications/initialized", {})

    def _pump(self):
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if "id" in message:
                with self.cond:
                    self.responses[message["id"]] = message
                    self.cond.notify_all()

    def _write(self, message: dict):
        assert self.proc.stdin is not None
        try:
            self.proc.stdin.write((json.dumps(message) + "\n").encode())
            self.proc.stdin.flush()
        except (BrokenPipeError, ValueError):
            pass

    def _notify(self, method: str, params: dict):
        self._write({"jsonrpc": "2.0", "method": method, "params": params})

    def _call_raw(self, method: str, params: dict) -> dict:
        with self.cond:
            self.next_id += 1
            request_id = self.next_id
        self._write({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        with self.cond:
            while request_id not in self.responses:
                self.cond.wait(timeout=5)
                if self.proc.poll() is not None and request_id not in self.responses:
                    raise RuntimeError(f"server exited (rc={self.proc.returncode})")
            return self.responses.pop(request_id)

    def tool_text(self, tool: str, arguments: dict) -> str:
        message = self._call_raw("tools/call", {"name": tool, "arguments": arguments})
        result = message.get("result", {})
        content = result.get("content") or []
        return content[0].get("text", "") if content else ""

    def close(self):
        try:
            if self.proc.stdin:
                self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def dump_source(binary: str, state_dir: str, source: str, group: str) -> tuple[list[dict], str]:
    """Walk one source sentence by sentence; returns (units, error)."""
    shard = os.path.join(state_dir, f"shard-{abs(hash((source, group))) % 9973}")
    os.makedirs(shard, exist_ok=True)
    units: list[dict] = []
    try:
        server = Server(binary, shard)
    except Exception as error:
        return units, f"server start failed: {error}"
    try:
        text = server.tool_text("open", {"source": f"file://{os.path.abspath(source)}"})
        try:
            opened = json.loads(text)
        except json.JSONDecodeError:
            return units, f"open returned non-json: {text[:160]}"
        handle = opened.get("doc") or opened.get("handle")
        if not handle:
            return units, f"open failed: {text[:160]}"
        index = 0
        current = handle
        seen = set()
        while current and current not in seen:
            seen.add(current)
            try:
                text = server.tool_text("read", {"from": current, "max_chars": 16000})
                data = json.loads(text)
            except (json.JSONDecodeError, RuntimeError) as error:
                return units, f"read failed at {index}: {str(error)[:120]}"
            items = []
            if "items" in data:  # batch shape: items are [handle, text, kind?]
                for item in data["items"]:
                    if isinstance(item, list):
                        items.append(
                            {
                                "handle": item[0] if len(item) > 0 else "",
                                "text": item[1] if len(item) > 1 else "",
                                "kind": item[2] if len(item) > 2 else "sentence",
                            }
                        )
                    elif isinstance(item, dict):
                        items.append(item)
            elif data.get("text") is not None:  # flat single-item shape
                items.append(
                    {
                        "handle": data.get("at") or current,
                        "text": data.get("text", ""),
                        "kind": data.get("kind") or "sentence",
                    }
                )
            if not items:
                break
            for item in items:
                units.append(
                    {
                        "source": source,
                        "group": group,
                        "index": index,
                        "handle": item.get("handle", ""),
                        "kind": item.get("kind") or "sentence",
                        "text": item.get("text", ""),
                    }
                )
                index += 1
            current = data.get("next")
    finally:
        server.close()
    return units, ""


# ---------------------------------------------------------------------------
# classification

NUMBERING_RE = re.compile(
    r"^\(?[\divxDIVX]+[\.\)]{1,2}$"          # 3.1. / iv. / (a) style pieces
    r"|^\([a-z]\)$"                            # (a)
    r"|^[A-Z]?[\.\)]$"
    r"|^(fig|table|eq|sec|app|no)\.?\s*\d*\.?$",
    re.IGNORECASE,
)
INITIALS_RE = re.compile(r"^([A-Z]\.){1,4}[A-Z]?\.?$|^[A-Z]\.$")
ABBREV_END_RE = re.compile(
    r"(?:^|\s)((?:[a-z]\.){2,}|e\.g\.|i\.e\.|etc\.|vs\.|et al\.|[A-Z][a-z]{1,5}\.)$"
)


def latin_words(text: str) -> list[str]:
    return re.findall(r"[A-Za-z]+(?:'[a-z]+)?", text)


def short_class(text: str, cjk: bool) -> str | None:
    stripped = text.strip()
    if cjk:
        if len(stripped) >= 4:
            return None
    else:
        if len(latin_words(stripped)) >= 3 and len(stripped) >= 8:
            return None
        if len(stripped) >= 12 and not re.search(r"[\.。！？!?][\)\"\']?$", stripped):
            return None
    if NUMBERING_RE.match(stripped):
        return "short:numbering"
    if INITIALS_RE.match(stripped):
        return "short:initials"
    low = stripped.lower().rstrip(".")
    if low in {a.rstrip(".") for a in ABBREV_TOKENS} or stripped.lower() in {
        "e.g.", "i.e.", "et al.", "vs.", "etc.",
    }:
        return "short:abbreviation"
    if ABBREV_END_RE.search(stripped) and len(stripped) < 30:
        return "short:abbreviation"
    if REF_TRIGGERS.match(stripped):
        return "short:reference"
    return "short:other"


def long_class(text: str, cjk: bool) -> str | None:
    stripped = text.strip()
    if cjk:
        if len(stripped) <= 300:
            return None
    else:
        if len(latin_words(stripped)) <= 60 and len(stripped) <= 700:
            return None
    has_quote_break = False
    for match in re.finditer(r"[。！？!?]", stripped):
        pos = match.end()
        rest = stripped[pos:]
        if rest.startswith(('"', "”", "」", "』", "”", "'")) or rest.lstrip()[:1] in {
            '"', "“", "「", "『", "(",
        }:
            has_quote_break = True
    if cjk and re.search(r"[。！？]", stripped[:-2]):
        if has_quote_break:
            return "long:missed-split-in-quotes"
        return "long:missed-split-cjk-terminal"
    if not cjk:
        if re.search(r"[!?]+['\")\]]*\s+[A-Z]", stripped) or re.search(
            r"\.['\")\]]*\s+[A-Z][a-z]", stripped
        ):
            if re.search(r"[.!?]['\"\)」』”]\s", stripped):
                return "long:missed-split-in-quotes"
            return "long:missed-split-latin-terminal"
        if re.search(r"(\s[-–•*]\s|\(\w\)|\d+\.\s).{0,200}(\s[-–•*]\s|\(\w\)|\d+\.\s)", stripped):
            return "long:list-runon"
    if has_quote_break:
        return "long:missed-split-in-quotes"
    return "long:other"


def split_inside_defects(units: list[dict]) -> list[tuple[int, str]]:
    """Pairwise checks: a boundary landed inside a decimal, URL/email, or quotes/brackets."""
    out = []
    for i in range(len(units) - 1):
        a, b = units[i]["text"].rstrip(), units[i + 1]["text"].lstrip()
        if not a or not b:
            continue
        # decimal numbers split: ends with digit or digit+period-ish, starts with digit tail
        if re.search(r"\d$", a) and re.match(r"^\d", b) and re.search(r"[\d\.]$", a[-3:]):
            out.append((i, "split-inside:decimal-number"))
        elif re.search(r"\d\.$", a) and re.match(r"^\d", b):
            out.append((i, "split-inside:decimal-number"))
        # url/email: unit ends with a bare scheme or domain-ish fragment
        if re.search(r"(https?://|www\.|ftp://)\S*$", a, re.IGNORECASE) and not re.search(
            r"\s$", a
        ) and re.match(r"^\S", b) and re.match(r"^[\w\-\+/=\.\?%#&]", b):
            out.append((i, "split-inside:url-or-email"))
        elif re.search(r"[\w\.\-]@$", a) or re.search(r"@[\w\-]*$", a) and re.match(r"^[\w\-]", b):
            out.append((i, "split-inside:url-or-email"))
        # quotes / brackets: balanced in neither unit but balanced when joined
        joined = a + " " + b
        for op, cl, tag in [
            ("“", "”", "quotes"), ('"', '"', "quotes"), ("「", "」", "quotes"),
            ("(", ")", "brackets"), ("[", "]", "brackets"), ("（", "）", "brackets"),
        ]:
            if op == cl:
                if a.count(op) % 2 == 1 and b.count(op) % 2 == 1:
                    out.append((i, f"split-inside:{tag}"))
                    break
            elif a.count(op) > a.count(cl) and b.count(cl) > b.count(op) and joined.count(
                op
            ) >= joined.count(cl) - 0:
                # only flag when the join actually pairs them
                if a.count(op) - a.count(cl) == b.count(cl) - b.count(op) or a.count(op) > a.count(cl):
                    out.append((i, f"split-inside:{tag}"))
                    break
    return out


def classify_source(units: list[dict]) -> list[dict]:
    rows = []
    for unit in units:
        text = unit["text"]
        cjk = cjk_ratio(text) > 0.5
        rows.append(
            {
                **unit,
                "cjk": cjk,
                "short": short_class(text, cjk),
                "long": long_class(text, cjk) if unit["kind"] == "sentence" else None,
            }
        )
    for index, tag in split_inside_defects(units):
        rows[index]["split_inside"] = tag
    for row in rows:
        row.setdefault("split_inside", None)
    return rows


def example(row: dict) -> dict:
    text = re.sub(r"\s+", " ", row["text"].strip())
    if len(text) > 97:
        text = text[:94] + "…"
    return {"handle": row["handle"], "text": text[:97]}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--state-dir", required=True)
    parser.add_argument("--sources", required=True)
    parser.add_argument("--out", default="")
    parser.add_argument("--jobs", type=int, default=8)
    parser.add_argument("--limit-per-source", type=int, default=0)
    args = parser.parse_args()

    out_dir = args.out or os.path.join(args.state_dir, "..", "segc-out")
    out_dir = os.path.abspath(out_dir)
    os.makedirs(out_dir, exist_ok=True)
    os.makedirs(args.state_dir, exist_ok=True)

    entries = []
    for line in open(args.sources, encoding="utf-8"):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        path = parts[0].strip()
        group = parts[1].strip() if len(parts) > 1 else "other"
        entries.append((path, group))

    all_rows: list[dict] = []
    errors: list[tuple[str, str]] = []
    done = 0

    def work(entry):
        path, group = entry
        units, error = dump_source(args.binary, args.state_dir, path, group)
        if args.limit_per_source:
            units = units[: args.limit_per_source]
        return path, group, units, error

    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futures = {pool.submit(work, e): e for e in entries}
        for future in as_completed(futures):
            path, group, units, error = future.result()
            done += 1
            if error:
                errors.append((path, error))
            rows = classify_source(units)
            all_rows.extend(rows)
            print(f"[{done}/{len(entries)}] {os.path.basename(path)}: {len(units)} units "
                  + (f"ERROR {error[:80]}" if error else ""), flush=True)

    units_path = os.path.join(out_dir, "units.jsonl")
    with open(units_path, "w", encoding="utf-8") as fh:
        for row in all_rows:
            fh.write(json.dumps(row, ensure_ascii=False) + "\n")

    # ---- aggregate
    by_group_totals = Counter(row["group"] for row in all_rows)
    classes: dict[str, list[dict]] = defaultdict(list)
    per_input: dict[str, Counter] = defaultdict(Counter)
    for row in all_rows:
        for tag in (row["short"], row["long"], row["split_inside"]):
            if tag:
                classes[tag].append(row)
                per_input[row["source"]][tag] += 1

    def rate(tag: str, group: str) -> float:
        total = by_group_totals.get(group, 0)
        if not total:
            return 0.0
        count = sum(1 for r in classes[tag] if r["group"] == group)
        return count / total * 10000

    lines = []
    lines.append("# segc census tables\n")
    lines.append(f"sources: {len(entries)}  units: {len(all_rows)}  "
                 f"open/read errors: {len(errors)}\n")
    lines.append("## class summary (count / units per 10k by group)\n")
    lines.append("| class | total | cjk-epub | en-epub | pdf-paper | fixture |")
    lines.append("|---|---|---|---|---|---|")
    for tag in sorted(classes, key=lambda t: -len(classes[t])):
        row = classes[tag]
        lines.append(
            f"| {tag} | {len(row)} | {rate(tag,'cjk-epub'):.1f} | {rate(tag,'en-epub'):.1f} "
            f"| {rate(tag,'pdf-paper'):.1f} | {rate(tag,'fixture'):.1f} |"
        )
    lines.append("")
    for tag in sorted(classes, key=lambda t: -len(classes[t])):
        rows = classes[tag]
        lines.append(f"## {tag} — {len(rows)} units\n")
        lines.append("per-input top counts: "
                     + ", ".join(
                         f"{os.path.basename(src)}×{cnt}"
                         for src, cnt in sorted(
                             ((s, per_input[s][tag]) for s in per_input if per_input[s][tag]),
                             key=lambda kv: -kv[1],
                         )[:8]
                     )
                     + "\n")
        lines.append("examples:")
        seen_sources = set()
        picked = []
        for row in rows:
            key = (row["source"], row["text"][:30])
            if key in seen_sources:
                continue
            seen_sources.add(key)
            picked.append(row)
            if len(picked) >= 5:
                break
        for row in picked:
            ex = example(row)
            lines.append(f"- `{ex['handle']}` {ex['text']}")
        lines.append("")
    if errors:
        lines.append("## input errors\n")
        for path, error in errors:
            lines.append(f"- `{path}`: {error[:200]}")
        lines.append("")

    report = "\n".join(lines)
    with open(os.path.join(out_dir, "census.md"), "w", encoding="utf-8") as fh:
        fh.write(report)
    print(f"wrote {units_path} ({len(all_rows)} units) and census.md", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
