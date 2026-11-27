#!/usr/bin/env python3
"""obr: run each case N times on one binary and print the median read latency of the heavy window."""

from __future__ import annotations

import argparse
import json
import os
import statistics
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
PROBE = os.path.join(HERE, "obr_probe.py")


def run(binary: str, state: str, case: str, reader: str, heavy: str, query: str, prewalk: int) -> dict:
    command = [
        "flock",
        "-x",
        "/root/.devin-bench.lock",
        sys.executable,
        PROBE,
        "--binary",
        binary,
        "--state",
        state,
        "--case",
        case,
        "--reader-source",
        reader,
        "--heavy-source",
        heavy,
        "--prewalk",
        str(prewalk),
    ]
    if query:
        command += ["--query", query]
    output = subprocess.run(command, capture_output=True, text=True)
    line = output.stdout.strip().splitlines()[-1] if output.stdout.strip() else "{}"
    return json.loads(line)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--label", required=True)
    parser.add_argument("--reps", type=int, default=3)
    parser.add_argument("--cases", default="open-epub,open-pdf,search,source-view")
    parser.add_argument("--prewalk", type=int, default=120)
    parser.add_argument("--out", default="")
    args = parser.parse_args()

    inputs = os.path.join(HERE, "inputs")
    reader = os.path.join(inputs, "zh-classic.epub")
    heavy = {
        "open-epub": os.path.join(inputs, "zh-large.epub"),
        "open-pdf": os.path.join(HERE, "pdf-text-1500p.pdf"),
        "open-pdf-entropy": os.path.join(HERE, "pdf-entropy-1500p.pdf"),
        "search": os.path.join(inputs, "zh-large.epub"),
        "source-view": "",
    }
    query = {"search": "之"}
    # `source_view` renders an original PDF page, so that case reads a PDF.
    readers = {"source-view": os.path.join(HERE, "pdf-text-1500p.pdf")}

    rows = []
    for case in args.cases.split(","):
        runs = []
        for rep in range(args.reps):
            state = os.path.join(HERE, "state", f"table-{args.label}-{case}-{rep}")
            subprocess.run(["rm", "-rf", state], check=True)
            result = run(
                args.binary,
                state,
                case,
                readers.get(case, reader),
                heavy.get(case, ""),
                query.get(case, ""),
                args.prewalk,
            )
            runs.append(result)
            print(f"{args.label} {case} rep{rep} {json.dumps(result, ensure_ascii=False)}", flush=True)
        def median(key):
            values = [r[key] for r in runs if isinstance(r.get(key), (int, float))]
            return round(statistics.median(values), 2) if values else None
        rows.append(
            {
                "label": args.label,
                "case": case,
                "heavy_ms": median("heavy_ms"),
                "p50": median("p50"),
                "p95": median("p95"),
                "max": median("max"),
                "idle_p50": median("idle_p50"),
                "idle_p95": median("idle_p95"),
                "reads": median("reads_during"),
            }
        )
    print()
    print("| case | heavy ms | read p50 | read p95 | read max | idle p50 |")
    print("|---|---|---|---|---|---|")
    for row in rows:
        print(
            f"| {row['case']} | {row['heavy_ms']} | {row['p50']} | {row['p95']} | {row['max']} | {row['idle_p50']} |"
        )
    if args.out:
        with open(args.out, "w", encoding="utf-8") as handle:
            json.dump(rows, handle, indent=1)


if __name__ == "__main__":
    main()
