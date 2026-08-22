#!/usr/bin/env bash
set -euo pipefail

: "${ROLLBACK_SHA:?set ROLLBACK_SHA to a retained known-good SHA}"
: "${RELEASE_BIN_DIR:?set RELEASE_BIN_DIR to the versioned binary directory}"
: "${SERVICE_NAME:=reading-mcp-tunnel.service}"

[[ "$ROLLBACK_SHA" =~ ^[0-9a-f]{40}$ ]] || {
  echo "ROLLBACK_SHA must be a 40-character lowercase git SHA" >&2
  exit 1
}

if [[ ${EUID} -ne 0 ]]; then
  echo "rollback-production.sh must run as root (use sudo)" >&2
  exit 1
fi

old="$RELEASE_BIN_DIR/reading-mcp-$ROLLBACK_SHA"
[[ -x "$old" ]] || { echo "retained rollback binary not found: $old" >&2; exit 1; }
actual_sha=$(sha256sum "$old" | awk '{print $1}')
ln -sfn "$(basename "$old")" "$RELEASE_BIN_DIR/reading-mcp"
systemctl daemon-reload
systemctl restart "$SERVICE_NAME"
systemctl is-active --quiet "$SERVICE_NAME" || {
  echo "rollback service is not active" >&2
  exit 1
}
echo "rolled back binary target to $ROLLBACK_SHA (sha256=$actual_sha); canonical state was preserved"
