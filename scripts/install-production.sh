#!/usr/bin/env bash
set -euo pipefail

: "${SERVICE_NAME:=reading-mcp-tunnel.service}"
: "${SERVICE_USER:?set SERVICE_USER to the non-root production account}"
: "${SERVICE_GROUP:=$SERVICE_USER}"
: "${WORKING_DIRECTORY:?set WORKING_DIRECTORY to the deployment checkout}"
: "${ENV_FILE:?set ENV_FILE to the runtime environment file}"
: "${TUNNEL_CLIENT:?set TUNNEL_CLIENT to the tunnel-client binary}"
: "${TUNNEL_PROFILE_DIR:?set TUNNEL_PROFILE_DIR to the tunnel profile directory}"
: "${TUNNEL_PROFILE:?set TUNNEL_PROFILE to the configured profile name}"
: "${STATE_DIR:?set STATE_DIR to the persistent Reading MCP state directory}"

if [[ ${EUID} -ne 0 ]]; then
  echo "install-production.sh must run as root (use sudo)" >&2
  exit 1
fi

repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
template="$repo_dir/deploy/systemd/reading-mcp-tunnel.service"
unit_dir=/etc/systemd/system
unit_path="$unit_dir/$SERVICE_NAME"
backup_dir="${ROLLBACK_DIR:-$WORKING_DIRECTORY/deploy-rollback}"

[[ -r "$template" ]] || { echo "missing unit template: $template" >&2; exit 1; }
install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_GROUP" "$STATE_DIR" "$TUNNEL_PROFILE_DIR"
install -d -m 0755 "$unit_dir" "$backup_dir"

if [[ -e "$unit_path" ]]; then
  install -m 0644 "$unit_path" "$backup_dir/$SERVICE_NAME.previous"
fi

escaped() {
  printf '%s' "$1" | sed 's/[&|]/\\&/g'
}

sed \
  -e "s|__SERVICE_USER__|$(escaped "$SERVICE_USER")|g" \
  -e "s|__SERVICE_GROUP__|$(escaped "$SERVICE_GROUP")|g" \
  -e "s|__WORKING_DIRECTORY__|$(escaped "$WORKING_DIRECTORY")|g" \
  -e "s|__ENV_FILE__|$(escaped "$ENV_FILE")|g" \
  -e "s|__TUNNEL_CLIENT__|$(escaped "$TUNNEL_CLIENT")|g" \
  -e "s|__TUNNEL_PROFILE_DIR__|$(escaped "$TUNNEL_PROFILE_DIR")|g" \
  -e "s|__TUNNEL_PROFILE__|$(escaped "$TUNNEL_PROFILE")|g" \
  -e "s|__STATE_DIR__|$(escaped "$STATE_DIR")|g" \
  "$template" > "$unit_path"
chmod 0644 "$unit_path"

systemctl daemon-reload
systemctl enable "$SERVICE_NAME" >/dev/null
echo "installed $SERVICE_NAME; start it after the environment file and tunnel profile are verified"
