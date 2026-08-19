#!/usr/bin/env bash
set -Eeuo pipefail

PROJECT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
CONFIG_FILE="${CONFIG_FILE:-/root/.env}"
export PATH="/root/.local/bin:$PATH"

# Load trusted local configuration without printing its contents.
if [[ -f "$CONFIG_FILE" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$CONFIG_FILE"
  set +a
fi

TUNNEL_ID="${TUNNEL_ID:-tunnel_6a8590404d5081918ac6e07ed032c75a}"
PROFILE="${PROFILE:-reading-mcp}"
MCP_BINARY="${MCP_BINARY:-$PROJECT_DIR/target/release/reading-mcp}"

die() {
  echo "错误: $*" >&2
  exit 1
}

command -v tunnel-client >/dev/null 2>&1 || \
  die "找不到 tunnel-client，请先安装 OpenAI tunnel-client"

[[ -n "${CONTROL_PLANE_API_KEY:-}" ]] || \
  die "配置文件中缺少 CONTROL_PLANE_API_KEY: $CONFIG_FILE"

[[ -n "$TUNNEL_ID" ]] || die "TUNNEL_ID 不能为空"

if [[ ! -x "$MCP_BINARY" ]]; then
  echo "正在构建 reading-mcp..."
  cargo build --release --locked --bin reading-mcp --manifest-path "$PROJECT_DIR/Cargo.toml"
fi

echo "初始化 Tunnel profile: $PROFILE"
tunnel-client init \
  --sample sample_mcp_stdio_local \
  --profile "$PROFILE" \
  --tunnel-id "$TUNNEL_ID" \
  --mcp-command "$MCP_BINARY"

echo "执行连接诊断..."
tunnel-client doctor --profile "$PROFILE" --explain

echo "启动 reading-mcp Tunnel: $TUNNEL_ID"
exec tunnel-client run --profile "$PROFILE"
