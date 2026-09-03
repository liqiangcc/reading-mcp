#!/usr/bin/env bash
set -euo pipefail

: "${PACKAGE_FILE:?set PACKAGE_FILE to the verified release archive}"
: "${CHECKSUM_FILE:?set CHECKSUM_FILE to the matching SHA256SUMS file}"
: "${EXPECTED_ARCHIVE_SHA256:?set EXPECTED_ARCHIVE_SHA256 from the completed Package Issue}"
: "${EXPECTED_VERSION:?set EXPECTED_VERSION to the release version}"
: "${EXPECTED_SHA:?set EXPECTED_SHA to the frozen source SHA}"
: "${EXPECTED_TARGET_TRIPLE:?set EXPECTED_TARGET_TRIPLE to the package Rust target triple}"
: "${EXPECTED_PLATFORM:?set EXPECTED_PLATFORM to the package platform, for example linux-x86_64}"
: "${RELEASE_BIN_DIR:?set RELEASE_BIN_DIR to the versioned binary directory}"
: "${SERVICE_NAME:=reading-mcp-tunnel.service}"
: "${STATE_DIR:?set STATE_DIR to persistent Reading MCP state}"
: "${ROLLBACK_SHA:?set ROLLBACK_SHA to the current known-good deployed SHA}"

[[ "$EXPECTED_ARCHIVE_SHA256" =~ ^[0-9a-f]{64}$ ]] || {
  echo "EXPECTED_ARCHIVE_SHA256 must be a 64-character lowercase SHA256" >&2
  exit 1
}
[[ "$EXPECTED_SHA" =~ ^[0-9a-f]{40}$ ]] || {
  echo "EXPECTED_SHA must be a 40-character lowercase git SHA" >&2
  exit 1
}
[[ "$ROLLBACK_SHA" =~ ^[0-9a-f]{40}$ ]] || {
  echo "ROLLBACK_SHA must be a 40-character lowercase git SHA" >&2
  exit 1
}
[[ "$EXPECTED_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || {
  echo "EXPECTED_VERSION must be a SemVer-like version" >&2
  exit 1
}
: "${ROLLBACK_DIR:=$RELEASE_BIN_DIR/rollback}"

if [[ ${EUID} -ne 0 ]]; then
  echo "deploy-production.sh must run as root (use sudo)" >&2
  exit 1
fi

for cmd in python3 sha256sum systemctl tar; do
  command -v "$cmd" >/dev/null || {
    echo "$cmd is required for deployment" >&2
    exit 1
  }
done

package_file=$(readlink -f "$PACKAGE_FILE")
checksum_file=$(readlink -f "$CHECKSUM_FILE")
[[ -f "$package_file" ]] || { echo "package file not found" >&2; exit 1; }
[[ -f "$checksum_file" ]] || { echo "checksum file not found" >&2; exit 1; }

package_name=$(basename "$package_file")
read -r checksum_archive_sha checksum_name < <(awk 'NF >= 2 { print $1, $2; exit }' "$checksum_file")
[[ -n "${checksum_archive_sha:-}" && "$checksum_name" == "$package_name" ]] || {
  echo "SHA256SUMS does not identify the selected package exactly" >&2
  exit 1
}
[[ "$checksum_archive_sha" == "$EXPECTED_ARCHIVE_SHA256" ]] || {
  echo "SHA256SUMS does not match the Package Issue archive identity" >&2
  exit 1
}
actual_archive_sha=$(sha256sum "$package_file" | awk '{print $1}')
[[ "$actual_archive_sha" == "$EXPECTED_ARCHIVE_SHA256" ]] || {
  echo "release archive SHA256 does not match the Package Issue identity" >&2
  exit 1
}

# Deployment runs as root. Reject all archive entry types except regular files
# and directories before tar is allowed to extract anything.
python3 - "$package_file" <<'PY'
import pathlib
import sys
import tarfile

archive = sys.argv[1]
with tarfile.open(archive, mode="r:gz") as tf:
    members = tf.getmembers()
    if not members:
        raise SystemExit("release archive is empty")
    for member in members:
        path = pathlib.PurePosixPath(member.name)
        if path.is_absolute() or ".." in path.parts:
            raise SystemExit(f"unsafe archive path: {member.name}")
        if not (member.isdir() or member.isfile()):
            raise SystemExit(f"unsupported archive entry type: {member.name}")
PY

extract_dir=$(mktemp -d)
trap 'rm -rf "$extract_dir"' EXIT
tar --no-same-owner --no-same-permissions -xzf "$package_file" -C "$extract_dir"

mapfile -t manifests < <(find "$extract_dir" -mindepth 2 -maxdepth 2 -type f -name release-manifest.json -print)
[[ ${#manifests[@]} -eq 1 ]] || {
  echo "release archive must contain exactly one release-manifest.json" >&2
  exit 1
}
manifest=${manifests[0]}
package_root=$(dirname "$manifest")
packaged_binary="$package_root/reading-mcp"
[[ -f "$packaged_binary" && -x "$packaged_binary" && ! -L "$packaged_binary" ]] || {
  echo "release archive does not contain a regular executable reading-mcp" >&2
  exit 1
}

read -r manifest_version manifest_sha manifest_target manifest_platform manifest_binary_sha < <(
  python3 - "$manifest" <<'PY'
import json
import pathlib
import sys

data = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
if data.get("schema") != "reading-mcp-release-manifest/v1":
    raise SystemExit("unsupported release manifest schema")
for key in ("version", "git_sha", "target", "platform", "binary_sha256"):
    value = data.get(key)
    if not isinstance(value, str) or not value:
        raise SystemExit(f"manifest is missing {key}")
print(data["version"], data["git_sha"], data["target"], data["platform"], data["binary_sha256"])
PY
)

[[ "$manifest_version" == "$EXPECTED_VERSION" ]] || {
  echo "package version mismatch: expected $EXPECTED_VERSION, got $manifest_version" >&2
  exit 1
}
[[ "$manifest_sha" == "$EXPECTED_SHA" ]] || {
  echo "package source SHA mismatch: expected $EXPECTED_SHA, got $manifest_sha" >&2
  exit 1
}
[[ "$manifest_target" == "$EXPECTED_TARGET_TRIPLE" ]] || {
  echo "package target mismatch: expected $EXPECTED_TARGET_TRIPLE, got $manifest_target" >&2
  exit 1
}
[[ "$manifest_platform" == "$EXPECTED_PLATFORM" ]] || {
  echo "package platform mismatch: expected $EXPECTED_PLATFORM, got $manifest_platform" >&2
  exit 1
}
actual_binary_sha=$(sha256sum "$packaged_binary" | awk '{print $1}')
[[ "$actual_binary_sha" == "$manifest_binary_sha" ]] || {
  echo "packaged binary SHA256 does not match release manifest" >&2
  exit 1
}

install -d -m 0755 "$RELEASE_BIN_DIR" "$ROLLBACK_DIR"
install -d -m 0750 "$STATE_DIR"

current_binary="$RELEASE_BIN_DIR/reading-mcp"
rollback_binary="$RELEASE_BIN_DIR/reading-mcp-$ROLLBACK_SHA"
if [[ -e "$current_binary" && ! -e "$rollback_binary" ]]; then
  [[ -x "$current_binary" ]] || {
    echo "current production binary is not executable: $current_binary" >&2
    exit 1
  }
  install -m 0755 "$current_binary" "$rollback_binary"
fi
[[ -x "$rollback_binary" ]] || {
  echo "known-good rollback binary is not available: $rollback_binary" >&2
  exit 1
}

timestamp=$(date -u +%Y%m%dT%H%M%SZ)
checkpoint="$ROLLBACK_DIR/$timestamp-$EXPECTED_SHA"
install -d -m 0750 "$checkpoint"
if [[ -d "$STATE_DIR" ]]; then
  tar --one-file-system -C "$(dirname "$STATE_DIR")" -czf "$checkpoint/state.tar.gz" "$(basename "$STATE_DIR")"
fi
if [[ -e "/etc/systemd/system/$SERVICE_NAME" ]]; then
  install -m 0644 "/etc/systemd/system/$SERVICE_NAME" "$checkpoint/$SERVICE_NAME"
fi

release_binary="$RELEASE_BIN_DIR/reading-mcp-$EXPECTED_SHA"
install -m 0755 "$packaged_binary" "$release_binary"
installed_binary_sha=$(sha256sum "$release_binary" | awk '{print $1}')
[[ "$installed_binary_sha" == "$manifest_binary_sha" ]] || {
  echo "installed binary SHA256 changed during installation" >&2
  exit 1
}

ln -sfn "$(basename "$release_binary")" "$RELEASE_BIN_DIR/reading-mcp"
printf '%s  %s\n' "$actual_archive_sha" "$package_name" > "$checkpoint/package.sha256"
printf '%s  %s\n' "$installed_binary_sha" "$(basename "$release_binary")" > "$checkpoint/binary.sha256"
printf '%s\n' "$EXPECTED_SHA" > "$checkpoint/release.sha"
printf '%s\n' "$EXPECTED_VERSION" > "$checkpoint/release.version"
install -m 0644 "$manifest" "$checkpoint/release-manifest.json"

systemctl daemon-reload
systemctl restart "$SERVICE_NAME"
systemctl is-active --quiet "$SERVICE_NAME" || {
  echo "service failed after rollout; use scripts/rollback-production.sh with a saved SHA" >&2
  exit 1
}

echo "deployed package version=$EXPECTED_VERSION source_sha=$EXPECTED_SHA"
echo "archive_sha256=$actual_archive_sha"
echo "binary_sha256=$installed_binary_sha"
echo "rollback_checkpoint=$checkpoint"
