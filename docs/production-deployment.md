# Production deployment

Production topology for v0.1:

```text
ChatGPT
  ↓
OpenAI Secure MCP Tunnel
  ↓
tunnel-client
  ↓
reading-mcp stdio binary
  ↓
persistent SQLite/cache state + explicitly allowed local roots
```

The GitHub Actions tunnel workflow is a smoke test only. Production is a
systemd-supervised `tunnel-client` process with a checked-out exact `main` SHA.
The tunnel profile owns the stdio command; Reading MCP is not exposed on
`0.0.0.0`.

## Discover before changing a server

Do not assume service name, user, checkout, state directory, profile directory,
binary path, or local roots. Record non-sensitive facts first:

```bash
systemctl show <service> -p Id -p ActiveState -p SubState -p User \
  -p WorkingDirectory -p ExecStart -p EnvironmentFiles
systemctl status <service> --no-pager
readlink -f <binary>
sha256sum <binary>
find <state-dir> -maxdepth 2 -type f -name '*.sqlite' -o -name 'reading-mcp.sqlite'
tunnel-client --version
tunnel-client doctor --json --profile-dir <profile-dir> --profile <profile>
```

Never print the environment file, API key, Bearer token, profile secret, or
private document content. Inspect only variable names or redact values.

## Install the service asset

`deploy/systemd/reading-mcp-tunnel.service` is a template. Install it with
explicit deployment facts; the script refuses to invent a service user/path:

```bash
sudo SERVICE_USER=<service-user> \
  SERVICE_GROUP=<service-group> \
  WORKING_DIRECTORY=<checkout> \
  ENV_FILE=<environment-file> \
  TUNNEL_CLIENT=<tunnel-client-path> \
  TUNNEL_PROFILE_DIR=<profile-directory> \
  TUNNEL_PROFILE=<profile-name> \
  STATE_DIR=<persistent-state-directory> \
  scripts/install-production.sh
```

The environment file and tunnel profile must be provisioned separately with
permissions appropriate to the service user. `.env.example` contains only
placeholders. The profile should reference the control-plane key through an
environment variable or file reference and launch the versioned/symlinked
Reading MCP binary.

## Deploy an exact reviewed SHA

Create a rollback checkpoint before changing the active link. The deploy script
builds with locked dependencies, retains a versioned binary, preserves the
canonical SQLite/cache state, atomically switches the `reading-mcp` symlink, and
restarts systemd:

```bash
sudo RELEASE_SHA=<exact-reviewed-main-sha> \
  REPO_DIR=<checkout-at-that-sha> \
  RELEASE_BIN_DIR=<versioned-binary-directory> \
  SERVICE_NAME=<actual-service-name> \
  STATE_DIR=<persistent-state-directory> \
  scripts/deploy-production.sh
```

The checkout must be clean and `HEAD` must equal `RELEASE_SHA`. Never deploy a
feature branch or an unreviewed later `main` commit. Derived index changes may
rebuild from canonical state; deployment must not delete `reading-mcp.sqlite`,
Raw Cache, Parsed Cache, or canonical Documents.

## Verify

```bash
sudo RELEASE_BIN_DIR=<binary-directory> \
  EXPECTED_SHA=<exact-reviewed-main-sha> \
  SERVICE_NAME=<actual-service-name> \
  TUNNEL_CLIENT=<tunnel-client-path> \
  TUNNEL_PROFILE_DIR=<profile-directory> \
  TUNNEL_PROFILE=<profile-name> \
  STATE_DIR=<persistent-state-directory> \
  scripts/verify-production.sh
```

Verification checks service activity, exact binary target, state accessibility,
`tunnel-client doctor`, and recent-log redaction guards. It does not print log
bodies or secrets. Separately run the production acceptance scenarios in
[`chatgpt-acceptance.md`](chatgpt-acceptance.md) through the real Secure MCP
Tunnel.

## Rollback

Known-good binaries and service-unit backups live in the rollback directory
created by deployment. On failure:

```bash
sudo ROLLBACK_SHA=<previous-known-good-sha> \
  RELEASE_BIN_DIR=<binary-directory> \
  SERVICE_NAME=<actual-service-name> \
  scripts/rollback-production.sh
```

Then rerun `verify-production.sh`, check the tunnel doctor result, and confirm
the old service can open/read an already persisted document after restart. Do
not restore by deleting canonical state. If the old binary cannot be restored,
stop rollout, retain bounded logs without secrets, and fix through a branch/PR
before attempting another exact-SHA deployment.
