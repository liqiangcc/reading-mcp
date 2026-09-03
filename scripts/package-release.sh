#!/usr/bin/env bash
set -euo pipefail

: "${REPO_DIR:?set REPO_DIR to the clean release-source checkout}"
: "${RELEASE_VERSION:?set RELEASE_VERSION, for example 0.1.0}"
: "${RELEASE_SHA:?set RELEASE_SHA to the exact frozen source SHA}"
: "${RELEASE_TAG:=v$RELEASE_VERSION}"
: "${TARGET_TRIPLE:=x86_64-unknown-linux-gnu}"
: "${PLATFORM:=linux-x86_64}"
: "${OUTPUT_DIR:=$REPO_DIR/dist}"
: "${BUILD_TARGET_DIR:=$REPO_DIR/target/package-$RELEASE_SHA-$TARGET_TRIPLE}"
: "${PACKAGING_COMMIT_SHA:=unknown}"
: "${VERIFY_RELEASE_TAG:=true}"

[[ "$RELEASE_SHA" =~ ^[0-9a-f]{40}$ ]] || {
  echo "RELEASE_SHA must be a 40-character lowercase git SHA" >&2
  exit 1
}
[[ "$RELEASE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || {
  echo "RELEASE_VERSION must be a SemVer-like version" >&2
  exit 1
}
[[ "$VERIFY_RELEASE_TAG" == "true" || "$VERIFY_RELEASE_TAG" == "false" ]] || {
  echo "VERIFY_RELEASE_TAG must be true or false" >&2
  exit 1
}

for cmd in cargo git gzip python3 sha256sum tar; do
  command -v "$cmd" >/dev/null || {
    echo "$cmd is required for packaging" >&2
    exit 1
  }
done

repo_dir=$(cd "$REPO_DIR" && pwd)
cd "$repo_dir"

actual_sha=$(git rev-parse HEAD)
[[ "$actual_sha" == "$RELEASE_SHA" ]] || {
  echo "source checkout SHA mismatch: expected $RELEASE_SHA, got $actual_sha" >&2
  exit 1
}
[[ -z "$(git status --porcelain)" ]] || {
  echo "refusing to package a dirty source checkout" >&2
  exit 1
}

cargo_version=$(python3 - "$repo_dir/Cargo.toml" <<'PY'
import pathlib
import re
import sys

text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
match = re.search(r'^version\s*=\s*"([^"]+)"\s*$', text, re.MULTILINE)
if not match:
    raise SystemExit("could not read package version from Cargo.toml")
print(match.group(1))
PY
)
[[ "$cargo_version" == "$RELEASE_VERSION" ]] || {
  echo "Cargo.toml version mismatch: expected $RELEASE_VERSION, got $cargo_version" >&2
  exit 1
}

if [[ "$VERIFY_RELEASE_TAG" == "true" ]]; then
  tag_sha=$(git rev-parse "$RELEASE_TAG^{commit}" 2>/dev/null) || {
    echo "release tag does not resolve: $RELEASE_TAG" >&2
    exit 1
  }
  [[ "$tag_sha" == "$RELEASE_SHA" ]] || {
    echo "release tag mismatch: $RELEASE_TAG resolves to $tag_sha, expected $RELEASE_SHA" >&2
    exit 1
  }
fi

[[ -f Cargo.lock ]] || {
  echo "Cargo.lock is required" >&2
  exit 1
}

cargo build \
  --release \
  --locked \
  --bin reading-mcp \
  --target "$TARGET_TRIPLE" \
  --target-dir "$BUILD_TARGET_DIR"

built="$BUILD_TARGET_DIR/$TARGET_TRIPLE/release/reading-mcp"
[[ -x "$built" ]] || {
  echo "release binary was not produced: $built" >&2
  exit 1
}

output_dir=$(mkdir -p "$OUTPUT_DIR" && cd "$OUTPUT_DIR" && pwd)
package_name="reading-mcp-v${RELEASE_VERSION}-${PLATFORM}"
stage="$output_dir/$package_name"
archive_name="$package_name.tar.gz"
archive="$output_dir/$archive_name"
checksums="$output_dir/SHA256SUMS"

rm -rf "$stage" "$archive" "$checksums"
install -d -m 0755 "$stage"
install -m 0755 "$built" "$stage/reading-mcp"
install -m 0644 Cargo.toml "$stage/Cargo.toml"
if [[ -f LICENSE ]]; then
  install -m 0644 LICENSE "$stage/LICENSE"
fi

binary_sha256=$(sha256sum "$stage/reading-mcp" | awk '{print $1}')
cargo_lock_sha256=$(sha256sum Cargo.lock | awk '{print $1}')

python3 - \
  "$stage/release-manifest.json" \
  "$RELEASE_VERSION" \
  "$RELEASE_TAG" \
  "$RELEASE_SHA" \
  "$TARGET_TRIPLE" \
  "$PLATFORM" \
  "$binary_sha256" \
  "$cargo_lock_sha256" \
  "$PACKAGING_COMMIT_SHA" <<'PY'
import json
import pathlib
import sys

(
    output,
    version,
    tag,
    git_sha,
    target,
    platform,
    binary_sha256,
    cargo_lock_sha256,
    packaging_commit_sha,
) = sys.argv[1:]

manifest = {
    "schema": "reading-mcp-release-manifest/v1",
    "version": version,
    "tag": tag,
    "git_sha": git_sha,
    "target": target,
    "platform": platform,
    "binary_sha256": binary_sha256,
    "cargo_lock_sha256": cargo_lock_sha256,
    "packaging_commit_sha": packaging_commit_sha,
}
pathlib.Path(output).write_text(
    json.dumps(manifest, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)
PY

# Deterministic container metadata. The compiled binary itself is intentionally
# treated as the single build output that is subsequently deployed unchanged.
tar \
  --sort=name \
  --owner=0 \
  --group=0 \
  --numeric-owner \
  --mtime='UTC 1970-01-01' \
  -C "$output_dir" \
  -cf - "$package_name" | gzip -n > "$archive"

rm -rf "$stage"
(
  cd "$output_dir"
  sha256sum "$archive_name" > SHA256SUMS
  sha256sum -c SHA256SUMS
)

verify_dir=$(mktemp -d)
trap 'rm -rf "$verify_dir"' EXIT

tar -xzf "$archive" -C "$verify_dir"
manifest="$verify_dir/$package_name/release-manifest.json"
binary="$verify_dir/$package_name/reading-mcp"
[[ -f "$manifest" && -x "$binary" ]] || {
  echo "packaged archive is missing manifest or executable" >&2
  exit 1
}

python3 - \
  "$manifest" \
  "$RELEASE_VERSION" \
  "$RELEASE_TAG" \
  "$RELEASE_SHA" \
  "$TARGET_TRIPLE" \
  "$PLATFORM" \
  "$binary" <<'PY'
import hashlib
import json
import pathlib
import sys

manifest_path, version, tag, git_sha, target, platform, binary_path = sys.argv[1:]
data = json.loads(pathlib.Path(manifest_path).read_text(encoding="utf-8"))
expected = {
    "schema": "reading-mcp-release-manifest/v1",
    "version": version,
    "tag": tag,
    "git_sha": git_sha,
    "target": target,
    "platform": platform,
}
for key, value in expected.items():
    if data.get(key) != value:
        raise SystemExit(f"manifest {key} mismatch: expected {value!r}, got {data.get(key)!r}")
actual_binary_sha = hashlib.sha256(pathlib.Path(binary_path).read_bytes()).hexdigest()
if data.get("binary_sha256") != actual_binary_sha:
    raise SystemExit("manifest binary_sha256 does not match packaged binary")
PY

archive_sha256=$(sha256sum "$archive" | awk '{print $1}')
echo "package=$archive"
echo "checksums=$checksums"
echo "archive_sha256=$archive_sha256"
echo "binary_sha256=$binary_sha256"
echo "source_sha=$RELEASE_SHA"
echo "source_tag=$RELEASE_TAG"
