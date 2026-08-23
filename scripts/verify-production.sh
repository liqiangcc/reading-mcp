#!/usr/bin/env bash
set -euo pipefail

: "${RELEASE_BIN_DIR:?set RELEASE_BIN_DIR to the binary directory}"
: "${EXPECTED_SHA:?set EXPECTED_SHA to the exact deployed SHA}"
: "${SERVICE_NAME:=reading-mcp-tunnel.service}"
: "${TUNNEL_CLIENT:?set TUNNEL_CLIENT to tunnel-client}"
: "${TUNNEL_PROFILE_DIR:?set TUNNEL_PROFILE_DIR to the tunnel profile directory}"
: "${TUNNEL_PROFILE:?set TUNNEL_PROFILE to the configured profile name}"
: "${STATE_DIR:?set STATE_DIR to persistent Reading MCP state}"
: "${ENV_FILE:?set ENV_FILE to the service environment file}"

[[ "$EXPECTED_SHA" =~ ^[0-9a-f]{40}$ ]] || {
  echo "EXPECTED_SHA must be a 40-character lowercase git SHA" >&2
  exit 1
}

command -v systemctl >/dev/null || { echo "systemctl is required" >&2; exit 1; }
systemctl is-active --quiet "$SERVICE_NAME" || { echo "service is not active" >&2; exit 1; }
[[ -r "$ENV_FILE" ]] || { echo "service environment file is not readable" >&2; exit 1; }

binary="$RELEASE_BIN_DIR/reading-mcp"
[[ -x "$binary" ]] || { echo "binary link is not executable" >&2; exit 1; }
target=$(readlink -f "$binary")
[[ "$target" == "$RELEASE_BIN_DIR/reading-mcp-$EXPECTED_SHA" ]] || {
  echo "binary link does not target EXPECTED_SHA" >&2
  exit 1
}

[[ -d "$STATE_DIR" && -r "$STATE_DIR" && -w "$STATE_DIR" ]] || {
  echo "state directory is not accessible" >&2
  exit 1
}

doctor_output=$(mktemp)
trap 'rm -f "$doctor_output"' EXIT
(
  set -a
  # The service manager loads this file for the running tunnel process. Load it
  # only in this short-lived verification subprocess so doctor sees the same
  # credential reference without printing or persisting secret values.
  . "$ENV_FILE"
  set +a
  "$TUNNEL_CLIENT" doctor --json --profile-dir "$TUNNEL_PROFILE_DIR" --profile "$TUNNEL_PROFILE"
) >"$doctor_output" 2>/dev/null || {
  echo "tunnel-client doctor failed" >&2
  exit 1
}

if journalctl -u "$SERVICE_NAME" -n 200 --no-pager 2>/dev/null | rg -i '(authorization:|bearer[[:space:]]+[A-Za-z0-9._-]{16,}|control_plane_api_key|document body|content body)' >/dev/null; then
  echo "recent service logs matched a secret/body redaction guard" >&2
  exit 1
fi

echo "service=active"
echo "binary=$target"
echo "binary_sha256=$(sha256sum "$target" | awk '{print $1}')"
echo "state_dir=$STATE_DIR"
echo "tunnel_doctor=pass"
