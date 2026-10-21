#!/usr/bin/env python3
"""Collapse `perf script` output into folded stacks (root;...;leaf <count>).

`stackcollapse-perf.pl` is not installed on the CI runner, so this does the one thing the CPU
profile needs: read `perf script` text on stdin (or `--input`) and print one line per distinct
call stack with its sample count, sorted by count descending. Frame offsets, module paths and
bare addresses are dropped so that the same function aggregates across a run, and the stack is
written root first, leaf last, which is the layout `cpu-profile-report.py` reads.

Standard library only.
"""

from __future__ import annotations

import argparse
import re
import sys
from collections import Counter

ADDRESS = re.compile(r"^[0-9a-f]{4,}$")


def normalize_frame(line: str) -> str | None:
    """Turn one `perf script` callchain line into a bare function name.

    A frame perf cannot symbolize keeps the basename of its DSO, `[unknown]@libc.so.6`, so that
    the report can say which module the unresolved time is in instead of only "[unknown]".
    """
    text = line.strip()
    if not text:
        return None
    # Drop a trailing module/DSO note: "sym+0x12 (/path/to/binary)" or "([kernel.kallsyms])".
    dso = ""
    marker = text.rfind(" (")
    if marker > 0 and text.endswith(")"):
        dso = text[marker + 2 : -1]
        text = text[:marker]
    # perf script prefixes each frame with its address: "4a1b2c sym+0x1f". Drop that token.
    head, _, rest = text.partition(" ")
    if rest and ADDRESS.match(head):
        text = rest.strip()
    # Drop the "+0x1f" offset; keep a symbol that is only an address as [unknown].
    if "+0x" in text:
        text = text.rsplit("+0x", 1)[0]
    text = text.strip()
    if not text or text == "[unknown]" or ADDRESS.match(text):
        name = dso.strip("[]").rsplit("/", 1)[-1]
        return f"[unknown]@{name}" if name else "[unknown]"
    return text


def collapse(lines) -> Counter:
    stacks: Counter = Counter()
    frames: list[str] = []
    for line in lines:
        if not line.strip():
            continue
        if not line[0].isspace():
            # A sample header: "comm pid/tid [cpu] time: event: ...". Flush the previous sample.
            if frames:
                stacks[";".join(reversed(frames))] += 1
                frames = []
            continue
        frame = normalize_frame(line)
        if frame is not None:
            frames.append(frame)
    if frames:
        stacks[";".join(reversed(frames))] += 1
    return stacks


def main() -> int:
    parser = argparse.ArgumentParser(description="Collapse `perf script` output to folded stacks.")
    parser.add_argument("--input", default=None, help="perf script text (default: stdin)")
    parser.add_argument("--min-count", type=int, default=1, help="drop stacks below this count")
    parser.add_argument("--limit", type=int, default=0, help="print at most N stacks (0 = all)")
    args = parser.parse_args()

    handle = open(args.input, encoding="utf-8", errors="replace") if args.input else sys.stdin
    try:
        stacks = collapse(handle)
    finally:
        if args.input:
            handle.close()

    total = sum(stacks.values())
    print(f"# samples={total} stacks={len(stacks)}", file=sys.stderr)
    printed = 0
    for stack, count in stacks.most_common():
        if count < args.min_count:
            continue
        print(f"{stack} {count}")
        printed += 1
        if args.limit and printed >= args.limit:
            break
    return 0


if __name__ == "__main__":
    sys.exit(main())
