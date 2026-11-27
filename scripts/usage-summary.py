#!/usr/bin/env python3
"""Summarize the reading-mcp usage log: per-tool calls, error mix, latency and size.

usage-summary.py <dir> [--days N]

Reads the usage-YYYYMMDD.jsonl files of the last N days (default: everything in <dir>) and
prints, per tool: calls, error mix, p50/p95/max latency in milliseconds and p50/p95 response
size in bytes, plus the top error kinds per day. The log is privacy-safe by construction; this
script never sees document text, queries, paths or checksums because the server never writes them.
"""

import argparse
import json
import re
import sys
from collections import Counter, defaultdict
from datetime import date, timedelta
from pathlib import Path

FILE_PATTERN = re.compile(r"^usage-(\d{8})\.jsonl$")


def percentile(sorted_values, q):
    if not sorted_values:
        return 0
    index = int(len(sorted_values) * q / 100)
    return sorted_values[min(index, len(sorted_values) - 1)]


def load_lines(directory, since):
    """Yield (day, record) for every line of the files within the window."""
    for path in sorted(Path(directory).glob("usage-*.jsonl")):
        match = FILE_PATTERN.match(path.name)
        if not match:
            continue
        try:
            day = date(
                int(match.group(1)[0:4]),
                int(match.group(1)[4:6]),
                int(match.group(1)[6:8]),
            )
        except ValueError:
            continue
        if since is not None and day < since:
            continue
        for line in path.read_text().splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                yield day, json.loads(line)
            except json.JSONDecodeError:
                continue


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("directory", help="the <state>/usage directory")
    parser.add_argument(
        "--days",
        type=int,
        default=None,
        help="only the files of the last N days, counting back from the newest file",
    )
    args = parser.parse_args()

    if args.days is not None:
        days_in_dir = sorted(
            {
                date(
                    int(match.group(1)[0:4]),
                    int(match.group(1)[4:6]),
                    int(match.group(1)[6:8]),
                )
                for path in Path(args.directory).glob("usage-*.jsonl")
                if (match := FILE_PATTERN.match(path.name))
            }
        )
        since = days_in_dir[-1] - timedelta(days=args.days - 1) if days_in_dir else date.max
    else:
        since = None

    per_tool = defaultdict(lambda: {"ms": [], "bytes": [], "errors": Counter(), "calls": 0})
    errors_per_day = defaultdict(Counter)
    total = 0

    for day, record in load_lines(args.directory, since):
        total += 1
        tool = record.get("tool", "?")
        stats = per_tool[tool]
        stats["calls"] += 1
        stats["ms"].append(record.get("ms", 0))
        stats["bytes"].append(record.get("resp_bytes", 0))
        outcome = record.get("outcome", "?")
        if outcome != "ok":
            retry = " retry" if record.get("retry") else ""
            stats["errors"][f"{outcome}{retry}"] += 1
            errors_per_day[day][f"{outcome}{retry}"] += 1

    if total == 0:
        print(f"no usage lines in {args.directory}")
        return 0

    print(f"usage summary: {total} calls in {args.directory}\n")
    print(f"{'tool':<12} {'calls':>6}  {'errors':<28} {'p50 ms':>7} {'p95 ms':>7} {'max ms':>7} {'p50 B':>7} {'p95 B':>7}")
    for tool in sorted(per_tool, key=lambda name: -per_tool[name]["calls"]):
        stats = per_tool[tool]
        ms = sorted(stats["ms"])
        size = sorted(stats["bytes"])
        errors = ", ".join(
            f"{kind}×{count}" for kind, count in stats["errors"].most_common(3)
        ) or "-"
        print(
            f"{tool:<12} {stats['calls']:>6}  {errors:<28} "
            f"{percentile(ms, 50):>7} {percentile(ms, 95):>7} {ms[-1] if ms else 0:>7} "
            f"{percentile(size, 50):>7} {percentile(size, 95):>7}"
        )

    print("\ntop error kinds per day:")
    any_errors = False
    for day in sorted(errors_per_day):
        for kind, count in errors_per_day[day].most_common(3):
            print(f"  {day.isoformat()}  {kind}: {count}")
            any_errors = True
    if not any_errors:
        print("  (none)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
