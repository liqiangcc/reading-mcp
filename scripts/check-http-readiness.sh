#!/usr/bin/env sh
set -eu

BASE_URL="${1:-http://127.0.0.1:8000}"

printf 'Checking Reading MCP HTTP health at %s/healthz\n' "$BASE_URL"
curl --fail --silent --show-error "$BASE_URL/healthz"
printf '\n'

printf 'Checking Reading MCP HTTP readiness at %s/readyz\n' "$BASE_URL"
curl --fail --silent --show-error "$BASE_URL/readyz"
printf '\n'

printf 'Local MCP endpoint is ready for protocol/tunnel validation: %s/mcp\n' "$BASE_URL"
