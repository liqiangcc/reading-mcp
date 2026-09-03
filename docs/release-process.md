# Reading MCP Release Protocol

本文定义 Reading MCP 的长期正式发布协议。它不是某一次版本的临时 checklist，而是所有正式版本必须遵循的仓库级规则。

## 1. 目标

发布流程必须满足以下目标：

1. 每个正式版本都能追溯到唯一的 `main` commit。
2. 发布前的功能、测试、文档、运行时契约必须一致。
3. 已发布 tag 不可移动、不可重写。
4. 发布失败时必须 fail closed；不能用“看起来差不多”的状态代替证据。
5. 一个没有当前聊天上下文的新会话，只读取仓库和 Release Issue，也能继续执行发布。

## 2. 版本语义

`Cargo.toml` 中的 `[package].version` 是代码版本的唯一事实来源。

正式 Git tag 使用相同版本并加 `v` 前缀，例如：

```text
Cargo.toml: 0.1.0
Git tag:    v0.1.0
Release:    v0.1.0
```

版本遵循 SemVer，并补充 Reading MCP 的 pre-1.0 规则：

- Patch：向后兼容的 bug、security、correctness、文档或运行时修复，例如 `0.1.0 -> 0.1.1`。
- Minor：新增向后兼容能力，或 pre-1.0 阶段发生明确的用户可见破坏性 contract 变化，例如 `0.1.x -> 0.2.0`。
- Major：进入 `1.0.0` 后，Tool contract、locator/cursor identity、持久化兼容性等发生破坏性变化时升级 major。

禁止仅为了“重新发布同一版本”修改已有版本内容。

## 3. Release Issue 驱动

每个正式版本必须创建一个独立 Release Issue，例如：

```text
[Release] reading-mcp v0.1.0
```

Release Issue 是该版本的源码发布状态机和审计记录，至少包含：

- target branch；
- baseline / candidate / frozen commit；
- Cargo version；
- Release Gate checklist；
- freeze 证据；
- tag / GitHub Release 状态；
- post-release verification；
- blocker 与修复记录。

禁止只依赖聊天上下文、人工记忆或临时 TODO 发布。

正式可部署二进制由独立 Package Issue 驱动，规则见 [`package-process.md`](package-process.md)。Production Deployment 再由独立 Deployment Issue 驱动。

## 4. 发布状态机

源码正式发布固定为：

```text
Issue / PR development
        ↓
merge to main
        ↓
Release Issue
        ↓
Release Gate
        ↓
freeze exact main commit
        ↓
immutable Git tag
        ↓
GitHub Release
        ↓
post-release verification
```

只有前一阶段有明确证据通过，才能进入下一阶段。

完整交付链在源码 Release 完成后继续：

```text
Source Release
        ↓
Package Issue
        ↓
verified immutable artifact
        ↓
Deployment Issue
        ↓
production verification
```

Source Release、Package 与 Deployment 不能合并成一个含糊的“发布”动作。

## 5. Release Gate

进入 Release Gate 后，该版本停止加入非 blocker 新功能。

默认 Gate 至少包括：

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

此外必须验证当前公开 contract，而不是只验证编译成功。当前 Reading MCP 至少要求：

- real MCP stdio acceptance；
- Streamable HTTP lifecycle acceptance；
- 当前 Tool surface 与文档一致；
- authorized source workspace / directory navigation contract；
- TextLocator / cursor / continuation identity contract；
- PDF original source-view / precise page binding；
- source-view worker 的 timeout、kill、资源隔离；
- persistence / restart 相关稳定定位行为；
- README、requirements、runtime configuration、Release 文档和真实 runtime 对齐；
- 无 `target/`、临时发布 workflow、调试产物或未解决 release blocker。

具体版本可以在 Release Issue 中增加 Gate，但不能删除仓库当前能力所要求的关键验收。

## 6. Freeze 规则

Release Gate 全绿后，必须记录一个确定的 `main` commit SHA 作为 frozen release commit。

冻结要求：

1. commit 必须位于 `main`；
2. 该 commit 的 CI 必须成功；
3. Cargo version 必须与目标版本一致；
4. 文档和 Tool surface 必须与该 commit 的实际 runtime 一致。

Freeze 后：

- 禁止加入新功能；
- 只允许 release blocker 修复；
- blocker 修复必须走 PR / merge；
- merge 后原 frozen commit 自动失效；
- 必须对新的 `main` commit 完整重跑 Release Gate 并重新 freeze。

禁止在旧 commit 和新 commit 之间模糊选择。

## 7. Tag 规则

正式 tag 只能从 frozen release commit 创建。

规则：

- tag 名必须精确匹配 `v<semver>`；
- tag 创建后视为 immutable；
- 禁止 force move、删除后重建或让同名 tag 指向不同 commit；
- 发布后发现问题，只能发布新版本，例如 `v0.1.1`；
- 不允许为了修正文档或二进制而重写 `v0.1.0`。

Tag 是正式 Source identity，不是“最新代码”的别名。

## 8. GitHub Release 规则

GitHub Release 必须基于已经验证的正式 tag 创建。

Release Notes 至少包含：

- 版本定位；
- 核心能力；
- 新增或变化的 Tool contract；
- stdio / HTTP transport 状态；
- locator / persistence / source fidelity 等重要兼容性信息；
- 安全与资源边界；
- 已知限制；
- 必要的升级说明。

正式版本默认不得标记为 draft 或 prerelease；只有 Release Issue 明确声明为 RC / beta 时例外。

GitHub Release 创建时可以暂时只有 source archive。正式可部署二进制 asset 由 Package Protocol 生成并验证后加入；Production Deployment 只能消费已经通过 Package Issue 验证的 artifact。

## 9. Post-release Verification

创建 Release 后不能立即关闭 Release Issue。至少执行：

1. GitHub Release 可见且状态正确；
2. tag 指向 frozen release commit；
3. 从 tag 读取 `Cargo.toml`，版本与 tag 一致；
4. 从 tag 验证 README / Tool surface 与 Release Notes 一致；
5. 记录最终 tag、release、commit、CI 证据；
6. 无 blocker 后关闭 Release Issue。

如果 post-release verification 发现问题：

- 禁止移动已有 tag；
- 判断是否仅为 Release metadata 可修正问题；
- 若代码或版本内容有问题，创建新的 patch/minor Release Issue 处理。

## 10. Release、Package 与部署分离

三个生命周期必须显式分离：

```text
Source Release
≠
Package
≠
Production Deployment
```

Source Release 证明：

- 某个源码版本已经冻结；
- tag / version / frozen commit 身份一致；
- Release Gate 已通过。

Package 证明：

- 从该 frozen source 构建了一个确定的 deployable artifact；
- archive SHA256、manifest、binary SHA256 已验证；
- 生产可以复用同一个二进制而无需重新编译。

Production Deployment 证明：

- 某个具体环境安装了该已验证 artifact；
- 生产 binary SHA256 与 package manifest 精确一致；
- service、transport、真实 MCP capability 与 rollback 都已验证。

禁止通过“GitHub main 已更新”推断生产环境已经更新；禁止通过“生产从同一 SHA 重新 cargo build”推断运行的是同一个正式 artifact。

## 11. Release Blocker

以下情况默认属于 blocker：

- Release Gate 任一必需步骤失败；
- runtime Tool surface 与公开文档不一致；
- locator/cursor/identity 可能错误重绑或 fuzzy rebase；
- 安全边界 fail-open；
- source fidelity 返回错误页或把派生/OCR 内容冒充 original source；
- timeout 不能实际终止受限工作；
- 持久化升级导致未声明的数据/身份不兼容；
- 目标 version、tag、commit 不一致；
- 已知 regression 尚未处理。

对 blocker 必须修复或明确取消发布，不能仅在 Release Notes 中降级成已知问题后继续发布。

Package blocker 与 Deployment blocker 分别由 Package Protocol 和 Production Deployment 文档定义，不应该回写或移动已经正确发布的旧 tag。

## 12. 非 blocker 与版本后移

以下内容通常可以后移到下一版本：

- 新 feature；
- 与当前公开 contract 无关的性能优化；
- 未进入当前版本目标的格式/平台；
- 不影响正确性、安全性、兼容性的重构。

Release Gate 的目标是冻结已经完成的版本，而不是继续扩张版本范围。

## 13. v0.1.0 首次执行

`v0.1.0` 的正式 Source Release 由 Release Issue #59 驱动并已完成。

本协议的首次落地由 Issue #60 驱动。一次性的 hardening 完成矩阵继续保存在 `docs/release-hardening-plan.md`。

`v0.1.0` 的首次正式 Package 由 Issue #64 驱动。它属于 bootstrap：Source Release 已经完成后才建立 Package Protocol，因此允许在不移动 `v0.1.0` tag、不修改 frozen source 的前提下首次补充 deployable Release assets。
