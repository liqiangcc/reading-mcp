# Production deployment

Production topology for v0.1:

```text
GitHub Release artifact
  ↓
checksum + release-manifest verification
  ↓
production host
  ↓
systemd-supervised tunnel-client
  ↓
reading-mcp stdio binary
  ↓
OpenAI Secure MCP Tunnel
  ↓
ChatGPT
```

Persistent SQLite/cache state and explicitly allowed local roots remain on the production host.

The GitHub Actions tunnel workflow is a smoke test only. Production is a systemd-supervised `tunnel-client` process. The tunnel profile owns the stdio command; Reading MCP is not exposed on `0.0.0.0`.

Packaging and deployment are deliberately separate. Production **must not rebuild source with Cargo**. Formal deployment consumes the exact archive produced by the Package Protocol in [`package-process.md`](package-process.md).

## Identity chain

A production rollout must preserve all three identities:

```text
source tag + git SHA
        ↓
release-manifest.json
        ↓
archive SHA256 + binary SHA256
        ↓
installed production binary SHA256
```

A matching git SHA alone is not sufficient evidence. The production binary checksum must match the package manifest exactly.

## Discover before changing a server

Do not assume service name, user, state directory, profile directory, binary path, local roots, or currently deployed identity. Record non-sensitive facts first:

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

Never print the environment file, API key, Bearer token, profile secret, or private document content. Inspect only variable names or redact values.

The Deployment Issue must record the current known-good source SHA and binary identity before rollout so rollback does not depend on memory.

## Install the service asset

`deploy/systemd/reading-mcp-tunnel.service` is a template. Install it with explicit deployment facts; the script refuses to invent a service user/path:

```bash
sudo SERVICE_USER=<service-user> \
  SERVICE_GROUP=<service-group> \
  WORKING_DIRECTORY=<stable-working-directory> \
  ENV_FILE=<environment-file> \
  TUNNEL_CLIENT=<tunnel-client-path> \
  TUNNEL_PROFILE_DIR=<profile-directory> \
  TUNNEL_PROFILE=<profile-name> \
  STATE_DIR=<persistent-state-directory> \
  scripts/install-production.sh
```

The environment file and tunnel profile must be provisioned separately with permissions appropriate to the service user. `.env.example` contains only placeholders. The profile should reference the control-plane key through an environment variable or file reference and launch the stable `reading-mcp` symlink in the versioned binary directory.

## Obtain the exact package

For a formal version such as `v0.1.0`, obtain the two Package outputs from the corresponding GitHub Release:

```text
reading-mcp-v0.1.0-linux-x86_64.tar.gz
SHA256SUMS
```

Do not substitute a locally rebuilt binary. Do not copy a binary from a development checkout. Do not overwrite an existing formal Release asset to make deployment convenient.

Before invoking the deployment script, the Deployment Issue should already contain the expected values copied from the completed Package Issue:

- version;
- source SHA;
- platform;
- archive SHA256;
- binary SHA256.

The separately recorded archive SHA256 is a trust input to deployment. `SHA256SUMS` must agree with that recorded value; the deployment script does not treat a self-consistent archive/checksum pair as sufficient identity evidence.

## Deploy the verified artifact

Create a rollback checkpoint before changing the active link. The deployment script verifies the recorded archive identity, checksum file and manifest, verifies the packaged binary checksum, preserves canonical state, retains the known-good binary, atomically switches the `reading-mcp` symlink, and restarts systemd.

```bash
sudo PACKAGE_FILE=<reading-mcp-v0.1.0-linux-x86_64.tar.gz> \
  CHECKSUM_FILE=<SHA256SUMS> \
  EXPECTED_ARCHIVE_SHA256=<package-issue-archive-sha256> \
  EXPECTED_VERSION=0.1.0 \
  EXPECTED_SHA=<frozen-source-sha> \
  EXPECTED_PLATFORM=linux-x86_64 \
  RELEASE_BIN_DIR=<versioned-binary-directory> \
  SERVICE_NAME=<actual-service-name> \
  STATE_DIR=<persistent-state-directory> \
  ROLLBACK_SHA=<current-known-good-deployed-sha> \
  scripts/deploy-production.sh
```

The production host does not need Rust or Cargo for this step.

The script rejects:

- malformed version/SHA/checksum identities;
- checksum file that does not match the Package Issue archive identity;
- archive checksum mismatch;
- unsafe archive paths;
- missing or multiple manifests;
- unsupported manifest schema;
- version/source/platform mismatch;
- packaged binary checksum mismatch;
- missing rollback binary.

Deployment must not delete `reading-mcp.sqlite`, Raw Cache, Parsed Cache, or canonical Documents. Derived indexes may rebuild from canonical state when runtime compatibility rules require it.

## Verify

Use the `binary_sha256` from the completed Package Issue / `release-manifest.json`:

```bash
sudo RELEASE_BIN_DIR=<binary-directory> \
  EXPECTED_VERSION=0.1.0 \
  EXPECTED_SHA=<frozen-source-sha> \
  EXPECTED_BINARY_SHA256=<package-manifest-binary-sha256> \
  SERVICE_NAME=<actual-service-name> \
  TUNNEL_CLIENT=<tunnel-client-path> \
  TUNNEL_PROFILE_DIR=<profile-directory> \
  TUNNEL_PROFILE=<profile-name> \
  STATE_DIR=<persistent-state-directory> \
  ENV_FILE=<service-environment-file> \
  scripts/verify-production.sh
```

Verification checks:

- service activity;
- active symlink points to the expected source-SHA-named binary;
- production binary SHA256 equals the packaged binary SHA256;
- persistent state accessibility;
- `tunnel-client doctor`;
- recent-log redaction guards.

It does not print log bodies or secrets.

After host verification, separately run the production acceptance scenarios in [`chatgpt-acceptance.md`](chatgpt-acceptance.md) through the real Secure MCP Tunnel. For `v0.1.0`, acceptance must include real `tools/list = 9`, `list_directory`, and `get_source_view` evidence before the Deployment Issue can close.

## Rollback

Known-good binaries and service-unit/state checkpoints live in the rollback directory created by deployment. On failure:

```bash
sudo ROLLBACK_SHA=<previous-known-good-sha> \
  RELEASE_BIN_DIR=<binary-directory> \
  SERVICE_NAME=<actual-service-name> \
  scripts/rollback-production.sh
```

Then rerun production verification against the previous known-good identity, check the tunnel doctor result, and confirm the old service can open/read an already persisted document after restart.

Do not restore by deleting canonical state. If the old binary cannot be restored, stop rollout, retain bounded logs without secrets, and fix through a branch/PR before attempting another deployment.

## Production completion evidence

A Deployment Issue is complete only when it records a chain equivalent to:

```text
v0.1.0
  -> frozen source SHA
  -> GitHub Release archive SHA256
  -> release-manifest binary SHA256
  -> installed production binary SHA256 (exact match)
  -> systemd active
  -> tunnel doctor pass
  -> real MCP 9-tool acceptance pass
```

GitHub Release success alone does not prove deployment; package success alone does not prove deployment; systemd activity alone does not prove artifact identity.
