# #119 follow-up: kill the `:memory:.ses` writer at the source

Status: review finding + execution plan for the r1 executor. Written by the
parallel reviewer session on 2026-09-14 after auditing PR #119 head `7aa5004`.

## What PR #119 already established (keep all of it)

- `-I -B` on every interpreter launched inside the private runtime
  (`OCR_INTERPRETER_ARGS`), so no `.pyc` can ever reach the verified rootfs.
- `SystemdOcrUnit::isolation_properties()` is a pure helper, unit-tested
  without `validate_host()`, and now pins `WorkingDirectory=/tmp`.
- CI `offline-private-runtime` snapshots the staging rootfs inventory right
  before the smoke and asserts an exact `0 added / 0 removed / 0 changed`
  diff afterwards, for both the staging root and the verified archive root.
  The failure message prints the first 20 offending paths.
- Hosted head `7aa5004` is green on all five gates.

Nothing in this plan loosens the gate or exempts any path from the manifest.

## Review finding: the writer is onnxruntime's embedded 1DS telemetry SDK

`WorkingDirectory=/tmp` is correct defence in depth, but it only *relocates*
the write. The reviewer confirmed the actual source in the scratch rootfs
`/tmp/rt-test/rootfs` (same wheels as CI, `onnxruntime==1.29.0`):

- `strings libonnxruntime.so.1.29.0` contains `.ses` immediately next to
  `sessionfirstlaunchtime`, `sessionsdkuid` and `MATSDK.PAL`; that is the
  Microsoft 1DS/MAT telemetry SDK's offline session store, whose storage path
  is `:memory:` and whose companion file is `<path>.ses`, written relative to
  cwd.
- `import onnxruntime` alone (no `InferenceSession`) already writes
  `:memory:.ses` (51 bytes) into cwd, spawns `blkid` and `hostname`, probes
  `/etc/ssl/*` CA bundles, and drops `mat-debug-<pid>.log` into **absolute**
  `/tmp`.
- `onnxruntime.disable_telemetry_events()` does **not** prevent the file: the
  SDK initialises at import time, before any Python API call can run.
- `ORT_DISABLE_TELEMETRY=1` in the process environment prevents everything:
  no `:memory:.ses`, no `mat-debug-*.log`, no `blkid`/`hostname` children,
  no "Failed to persist telemetry device ID" warning. `InferenceSession` on
  `/opt/ocr-layout-model/inference.onnx` still works.

Empirical matrix (cwd = fresh empty directory inside the rootfs):

| run | env | API call | `:memory:.ses` in cwd |
|---|---|---|---|
| import only | default | – | yes |
| session create | default | – | yes |
| session create | `ORT_DISABLE_TELEMETRY=1` | – | **no** |
| session create | default | `disable_telemetry_events()` | yes |

Consequences for the current design:

1. Telemetry init forks `blkid`/`hostname` inside the sandbox; both are absent
   in the rootfs (`sh: blkid: not found`), so today it is noise, but it is an
   unplanned exec surface under `TasksMax=64`.
2. The SDK tries to open a network channel; under `PrivateNetwork=yes` this is
   the most plausible cause of the `import onnxruntime` stall that the
   gdb "native backtrace" debug step in `ocr-python-component.yml` exists for.
3. `mat-debug-*.log` lands in `/tmp`. In production that is the 512M tmpfs and
   is discarded, but it competes with OCR scratch space and is never inspected.

## Plan (separation of concerns, one PR after #119 merges)

### 1. Source fix: add `ORT_DISABLE_TELEMETRY=1` to the allowlisted process env

- `src/infrastructure/ocr_identity.rs`: `OCR_PROCESS_ENV` gains
  `("ORT_DISABLE_TELEMETRY", "1")` (array length 4 -> 5) with a comment that
  onnxruntime's 1DS SDK initialises at import and only honours the env var.
- `src/parsing/pdf_layout_worker.py`: mirror it in the Python `OCR_PROCESS_ENV`
  so children spawned by the worker (`subprocess.run(..., env=OCR_PROCESS_ENV)`)
  inherit it.
- `SystemdOcrUnit::command_with_root` already forwards every `OCR_PROCESS_ENV`
  pair through both `--setenv` and `/usr/bin/env -i`; no change needed there.
- Update the hosted-only assertion in `ocr_systemd.rs`
  (`assert set(os.environ) <= {...}`) and any equivalent env allowlist in
  tests to include `ORT_DISABLE_TELEMETRY`. The `layout_pdf.rs` test that
  compares the child's environment against `OCR_PROCESS_ENV` will follow
  automatically.
- Workflows: add `ORT_DISABLE_TELEMETRY=1` to every `/usr/bin/env -i ...`
  argument list that runs the private runtime (`ocr-python-component.yml`
  staging smoke, verified-archive smoke, gdb debug unit;
  `package-ocr-runtime.yml` smoke). Keep `WorkingDirectory=/tmp` in all of
  them.
- Do **not** add the variable to the probe scripts that intentionally run the
  host interpreter with their own env (`first_engine_probe.py`,
  `page_selection_probe.py`) unless they import onnxruntime; check first.

### 2. Make the gate prove the source fix, not just the cwd mask

Add one negative-space check to `ocr-python-component.yml`, immediately after
the staging smoke and before the inventory diff:

```bash
test -z "$(sudo find "$runtime_root/tmp" -mindepth 1 -print -quit)"
```

Rationale: with `WorkingDirectory=/tmp` and a `TemporaryFileSystem=/tmp`
overlay, anything the worker writes to `/tmp` vanishes with the unit, so the
inventory diff can no longer see telemetry side files. The verified archive
smoke step must therefore also assert that the smoke's stderr does not contain
`Failed to persist telemetry device ID`, `blkid: not found` or
`hostname: not found`; capture stderr to `$RUNNER_TEMP/*-smoke.stderr` and
`grep -q` on it. This is what turns "0/0/0" from a mask into evidence.

### 3. Runtime identity and archive contents

- `OCR_PROCESS_ENV` is not hashed into `OcrRuntimeIdentity` (it only shapes
  how `ldd`/worker probes are executed), so no reopen/`operator_revision`
  bump is required. Confirm by reading `build_ocr_runtime_identity`; if a
  test snapshot embeds the env allowlist, update the snapshot in the same
  commit and say so in the PR body.
- The v0.4.1 private runtime archive already contains `/:memory:.ses` from
  build-time pollution. Do not edit that archive or its manifest. The next
  `package-ocr-runtime.yml` run on the fixed head produces a clean archive;
  the fresh inventory gate plus the `/tmp`-empty check guarantee it. Record
  the new archive SHA in `docs/tasks/95-scanned-pdf/implementation-evidence.md`
  when it exists.

### 4. Ordering and gates

1. Merge #119 as-is once you have re-read the five green checks on the exact
   head (`7aa5004`). It is correct and self-contained; do not fold this plan
   into it.
2. Open a new branch from the merged `main` for steps 1-2. Version bump only
   if the packaged runtime archive changes (it will), i.e. `0.4.5`.
3. Local verification is allowed only in a throwaway copy or in the existing
   `/tmp/rt-test/rootfs` scratch with `cd` into a fresh subdirectory of its
   `/tmp` that you delete afterwards; do not touch the production runtime.
4. Hosted gates that must be green on the exact head before merge:
   `rust`, `offline-engine`, `offline-python`, `offline-private-runtime`
   (including the new `/tmp`-empty and stderr assertions), `real-engine-probe`.
5. After merge: tag, `package-release.yml`, `package-ocr-runtime.yml`, deploy
   via `scripts/deploy-production.sh`, then `scripts/verify-production.sh`.
   Startup `--verify-runtime` inventory verification against the new archive
   is the production acceptance signal.

### 5. Out of scope (do not do)

- Do not exempt `:memory:.ses`, `mat-debug-*.log` or any path from the
  inventory manifest or gate.
- Do not replace `WorkingDirectory=/tmp`; keep both controls.
- Do not modify existing tags (`v0.4.3`, `v0.4.4`).
- Do not pin a different onnxruntime version to dodge the issue; the wheel set
  is frozen by `requirements.lock` and the layout model identity.

## Evidence commands (reviewer, reproducible)

```bash
R=/tmp/rt-test/rootfs
strings $R/opt/ocr-python/lib/python3.12/site-packages/onnxruntime/capi/libonnxruntime.so.1.29.0 \
  | grep -n -B3 -A3 '^\.ses$'          # sessionfirstlaunchtime / MATSDK.PAL context
mkdir -p $R/tmp/rv
chroot $R /usr/bin/env -i PATH=/usr/bin:/bin LANG=C.UTF-8 /bin/sh -c \
  'cd /tmp/rv && /opt/ocr-python/bin/python -I -B -c "import onnxruntime"; ls -A /tmp/rv'
# -> :memory:.ses
chroot $R /usr/bin/env -i PATH=/usr/bin:/bin LANG=C.UTF-8 ORT_DISABLE_TELEMETRY=1 /bin/sh -c \
  'cd /tmp/rv && rm -f /tmp/rv/*; /opt/ocr-python/bin/python -I -B -c "import onnxruntime"; ls -A /tmp/rv'
# -> (empty)
rm -rf $R/tmp/rv $R/tmp/mat-debug-*.log
```
