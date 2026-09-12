# #95 executor handoff / stage gate

Status: Stage A discovery and Stage B design proposal only.

## Authority

Read [#95](https://github.com/liqiangcc/reading-mcp/issues/95) and
[Coordinator comment 5643642213](https://github.com/liqiangcc/reading-mcp/issues/95#issuecomment-5643642213)
and [static review 5643665196](https://github.com/liqiangcc/reading-mcp/issues/95#issuecomment-5643665196)
again before continuing. Latest Coordinator/user authorization supersedes old
withdrawal. External Coordinator owns goals/design acceptance; this existing r1
is the sole new development/deployment executor. Do not create another session,
spawn subagents, send work to old sessions, or interfere with their files/processes.

Read applicable AGENTS and `docs/CODEX_EXECUTION.md`, then both mandated full
specifications (`docs/codex-development-production-rollout-plan.md` and
`docs/codex-sentence-reading-closure-amendment.md`). Apply current release/package/
production-deployment protocols. Historical seven-tool/v0.1/local-build examples
are not live facts: current surface is nine and production never compiles.

## Current deliverable and stop

Review [design.md](design.md) and [evidence.md](evidence.md). Return the design PR
URL and exact head SHA, decisions and unresolved gates to Coordinator. **Stop
before runtime implementation.** Approval must identify the reviewed design head;
do not infer approval from CI, bot review, or previous deployment authorization.
No merge/release/deployment at this stage.

## After explicit design approval

1. Recheck live main/PR96/production identities/resources and old-task activity.
   Preserve unrelated/dirty work; work only in isolated short-lived worktrees.
2. Freeze fixture bytes/fonts/gold/manifest in a fixture-only reviewed PR, using
   design's exact logical corpus and thresholds, before implementing/tuning OCR.
   A different corpus/metric/budget requires a reviewed design amendment.
3. Integrate deployed PDF layout and preserve main #92 via an independently
   reviewed prerequisite PR; audit PR96's own delta, not its whole branch.
4. Implement OCR through application Ports, immutable original source and
   versioned canonical evidence, bounded whole-tree cancellation, persistent
   cache/single-flight, atomic publication and explicit errors. Preserve nine
   tools and external ownership of reading progress.
5. Compile, Format, Clippy, full tests, real-engine fixtures and package builds
   only on GitHub-hosted Actions. No production cargo build/test, self-hosted
   runner, private text/raster upload, paid/cloud OCR, or network inference.
6. Return exact implementation head/diff/CI for Coordinator review. Merge only
   current reviewed CI-green head. Then separate Release/Package/Deployment
   Issues, exact unused version/tag, immutable artifact/dependency checksums.
7. Recheck connector deadline, cold scan runtime and host admission. Current
   capacity is insufficient; do not delete canonical or other sessions' data.
   Fail synchronous design back to Coordinator if actual connector cannot fit it.
8. Preserve verified binary/dependencies/config/consistent state rollback tuple;
   deploy final reviewed main artifact only. Test original scan and existing
   capabilities through the actual connector. Report measurements, not inferred
   completion. Coordinator gives final business acceptance before #95 closes.

Private naturebp.pdf may be probed locally with isolated state; publish only
hashes/versions/statistics/pass-fail. No paper-reading-lab progress continuation
or READY claims. Existing historical private pilot annotations are not gold.

## Evidence discipline

Separate proposed, implemented, CI-passed, reviewed, merged, released, packaged,
deployed and live-accepted states. Record SHA-specific URLs and artifact hashes;
never mark future stages complete in advance. Do not move tags, overwrite release
assets, wipe canonical data, or roll production back to main's older PDF behavior.
