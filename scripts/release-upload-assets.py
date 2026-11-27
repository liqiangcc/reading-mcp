#!/usr/bin/env python3
"""Upload release assets without overwriting existing immutable asset names."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import urllib.parse
import urllib.request

API = "https://api.github.com"
UPLOAD_API = "https://uploads.github.com"


def request(method: str, url: str, token: str, data: bytes | None = None, content_type: str | None = None):
    headers = {
        "Accept": "application/vnd.github+json",
        "Authorization": f"Bearer {token}",
        "X-GitHub-Api-Version": "2022-11-28",
    }
    if content_type:
        headers["Content-Type"] = content_type
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=120) as response:
        body = response.read()
    return json.loads(body) if body else {}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("files", nargs="+", type=pathlib.Path)
    args = parser.parse_args()

    token = os.environ.get("GH_TOKEN")
    if not token:
        raise SystemExit("GH_TOKEN is required")
    files = []
    for path in args.files:
        if not path.is_file():
            raise SystemExit(f"release asset does not exist: {path}")
        files.append(path)
    names = [path.name for path in files]
    if len(set(names)) != len(names):
        raise SystemExit("release asset names must be unique")

    # Keep the owner/repo slash while quoting each path segment.
    repo_path = urllib.parse.quote(args.repository, safe="/")
    tag_path = urllib.parse.quote(args.tag, safe="")
    release = request("GET", f"{API}/repos/{repo_path}/releases/tags/{tag_path}", token)
    release_id = release.get("id")
    if not isinstance(release_id, int):
        raise SystemExit(f"release lookup did not return an id for {args.tag}")

    existing = set()
    page = 1
    while True:
        assets = request(
            "GET",
            f"{API}/repos/{repo_path}/releases/{release_id}/assets?per_page=100&page={page}",
            token,
        )
        if not isinstance(assets, list):
            raise SystemExit("release asset listing did not return a list")
        existing.update(asset.get("name") for asset in assets if isinstance(asset, dict))
        if len(assets) < 100:
            break
        page += 1
    collisions = sorted(set(names) & existing)
    if collisions:
        raise SystemExit(f"refusing to overwrite existing release assets: {', '.join(collisions)}")

    for path in files:
        query = urllib.parse.urlencode({"name": path.name})
        url = f"{UPLOAD_API}/repos/{repo_path}/releases/{release_id}/assets?{query}"
        request("POST", url, token, data=path.read_bytes(), content_type="application/octet-stream")
        print(f"uploaded {path.name}")


if __name__ == "__main__":
    main()
