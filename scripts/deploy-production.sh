#!/usr/bin/env bash
set -euo pipefail

: "${RELEASE_SHA:?set RELEASE_SHA to an exact reviewed main SHA}"
: "${REPO_DIR:?set REPO_DIR to the checked-out repository}"
: "${RELEASE_BIN_DIR:?set RELEASE_BIN_DIR to the versioned binary directory}"
: "${SERVICE_NAME:=reading-mcp-tunnel.service}"
: "${STATE_DIR:?set STATE_DIR to persistent Reading MCP state}"

[[ "$RELEASE_SHA" =~ ^[0-9a-f]{40}$ ]] || {
  echo "RELEASE_SHA must be a 40-character lowercase git SHA" >&2
  exit 1
}
: "${ROLLBACK_DIR:=$RELEASE_BIN_DIR/rollback}"

if [[ ${EUID} -ne 0 ]]; then
  echo "deploy-production.sh must run as root (use sudo)" >&2
  exit 1
fi

cd "$REPO_DIR"
actual_sha=$(git rev-parse HEAD)
[[ "$actual_sha" == "$RELEASE_SHA" ]] || {
  echo "checkout SHA mismatch: expected $RELEASE_SHA, got $actual_sha" >&2
  exit 1
}
[[ -z "$(git status --porcelain)" ]] || {
  echo "refusing to deploy a dirty checkout" >&2
  exit 1
}

command -v cargo >/dev/null || { echo "cargo is required to build the release" >&2; exit 1; }
command -v systemctl >/dev/null || { echo "systemctl is required" >&2; exit 1; }
install -d -m 0755 "$RELEASE_BIN_DIR" "$ROLLBACK_DIR"
install -d -m 0750 "$STATE_DIR"

cargo build --release --locked --bin reading-mcp
built="$REPO_DIR/target/release/reading-mcp"
[[ -x "$built" ]] || { echo "release binary was not produced" >&2; exit 1; }

timestamp=$(date -u +%Y%m%dT%H%M%SZ)
checkpoint="$ROLLBACK_DIR/$timestamp-$RELEASE_SHA"
install -d -m 0750 "$checkpoint"
if [[ -d "$STATE_DIR" ]]; then
  tar --one-file-system -C "$(dirname "$STATE_DIR")" -czf "$checkpoint/state.tar.gz" "$(basename "$STATE_DIR")"
fi
if [[ -e "/etc/systemd/system/$SERVICE_NAME" ]]; then
  install -m 0644 "/etc/systemd/system/$SERVICE_NAME" "$checkpoint/$SERVICE_NAME"
fi

release_binary="$RELEASE_BIN_DIR/reading-mcp-$RELEASE_SHA"
install -m 0755 "$built" "$release_binary"
ln -sfn "$(basename "$release_binary")" "$RELEASE_BIN_DIR/reading-mcp"
sha256sum "$release_binary" > "$checkpoint/binary.sha256"
printf '%s\n' "$RELEASE_SHA" > "$checkpoint/release.sha"

systemctl daemon-reload
systemctl restart "$SERVICE_NAME"
systemctl is-active --quiet "$SERVICE_NAME" || {
  echo "service failed after rollout; use scripts/rollback-production.sh with a saved SHA" >&2
  exit 1
}
echo "deployed exact SHA $RELEASE_SHA; rollback checkpoint: $checkpoint"
