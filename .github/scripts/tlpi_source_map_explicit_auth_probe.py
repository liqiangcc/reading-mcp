#!/usr/bin/env python3
"""Execute the sanitized v4 product probe, retrying every product-context failure
with explicit MCP authorization.

This wrapper changes only the retry decision in-memory; it never prints or
persists credentials.
"""

from pathlib import Path

probe_path = Path(__file__).with_name("tlpi_source_map_probe_v4.py")
source = probe_path.read_text(encoding="utf-8")
old = '''            if status in (401, 403, 404, 429) and auth_mode == "product-context":
                # A second attempt may distinguish product-context auth from tunnel auth.
                continue
            if status == 400 and auth_mode == "product-context":
                # Some deployments require explicit MCP authorization.
                continue
            break
'''
new = '''            if auth_mode == "product-context":
                # Retry every unsuccessful product-context attempt with explicit
                # tunnel authorization, including 424 Failed Dependency.
                continue
            break
'''
if old not in source:
    raise SystemExit("expected v4 retry block not found")
source = source.replace(old, new, 1)
namespace = {"__name__": "__main__", "__file__": str(probe_path)}
exec(compile(source, str(probe_path), "exec"), namespace)
