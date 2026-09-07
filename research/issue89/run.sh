#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
out=${ISSUE89_RESULT_DIR:-/tmp/reading-mcp-issue-89-results}
inputs="$out/inputs"
mkdir -p "$inputs" "$out"
cp /root/.reading-mcp/cache/raw/2fb3e1dd2bd7bb8c224c1afa021a4bba0c50466d89979b31980d96e5be6e6353.bin "$inputs/issue87.pdf"

cargo run --offline --locked --manifest-path "$root/research/issue89/Cargo.toml" --bin issue89-fixture-generator -- "$inputs/synthetic"
cargo run --offline --locked --manifest-path "$root/research/issue89/Cargo.toml" --bin issue89-pdf-probe -- lopdf-layout "$inputs/issue87.pdf" > "$out/issue87-lopdf.json"
cargo run --offline --locked --manifest-path "$root/research/issue89/Cargo.toml" --bin issue89-pdf-probe -- pdf-extract "$inputs/issue87.pdf" > "$out/issue87-pdf-extract.json"
for pdf in "$inputs"/synthetic/*.pdf; do
  name=$(basename "$pdf" .pdf)
  cargo run --offline --locked --manifest-path "$root/research/issue89/Cargo.toml" --bin issue89-pdf-probe -- lopdf-layout "$pdf" > "$out/$name-lopdf.json"
  cargo run --offline --locked --manifest-path "$root/research/issue89/Cargo.toml" --bin issue89-pdf-probe -- pdf-extract "$pdf" > "$out/$name-pdf-extract.json"
done

binary=${READING_MCP_BINARY:-/root/reading-mcp/target/release/reading-mcp-1cfbc4ca032a8bab7b59134a9b03c2526a34b7f4}
READING_MCP_BINARY="$binary" \
READING_MCP_LOCAL_ROOTS="$inputs" \
READING_MCP_STATE_DIR="$out/mcp-state" \
python3 "$root/research/issue89/mcp_probe.py" "$inputs/issue87.pdf" > "$out/issue87-mcp.json"

cmp "$out/issue87-mcp.json" <(READING_MCP_BINARY="$binary" READING_MCP_LOCAL_ROOTS="$inputs" READING_MCP_STATE_DIR="$out/mcp-state-repeat" python3 "$root/research/issue89/mcp_probe.py" "$inputs/issue87.pdf")
cmp "$out/issue87-mcp.json" <(READING_MCP_BINARY="$binary" READING_MCP_LOCAL_ROOTS="$inputs" READING_MCP_STATE_DIR="$out/mcp-state-repeat-3" python3 "$root/research/issue89/mcp_probe.py" "$inputs/issue87.pdf")
printf 'results=%s\n' "$out"
