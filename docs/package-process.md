# Reading MCP Package Protocol

本文定义 Reading MCP 的正式打包协议。Packaging 是 Source Release 与 Production Deployment 之间的独立生命周期。

```text
Source Release
    ↓
immutable tag / frozen source identity
    ↓
Package
    ↓
immutable deployable artifact
    ↓
Production Deployment
```

核心原则：**build once, deploy the same artifact**。

## 1. 三类身份必须分离

正式链路同时保存三类身份，禁止互相替代：

1. Source identity：`tag + git_sha`，说明 artifact 来自哪一份冻结源码。
2. Package identity：archive SHA256、manifest、binary SHA256，说明具体交付物是什么。
3. Deployment identity：生产实际 binary SHA256，说明当前运行的是否就是那个已发布 artifact。

“源码 SHA 相同”不能替代“二进制相同”的证据。

## 2. Package Issue 驱动

每个需要正式可部署 artifact 的版本必须建立 Package Issue，例如：

```text
[Package] reading-mcp v0.1.0
```

Package Issue 至少记录：

- source tag；
- frozen git SHA；
- package target/platform；
- packaging infrastructure commit；
- package workflow/run；
- archive SHA256；
- binary SHA256；
- manifest 验证；
- GitHub Release assets 状态。

禁止只依赖聊天上下文或某台构建机的临时文件。

## 3. Package 前置条件

正式 package 必须满足：

- source tag 已存在且按 Release Protocol 视为 immutable；
- tag 精确解析到 Package Issue 记录的 frozen SHA；
- checkout `HEAD` 精确等于 frozen SHA；
- checkout clean；
- `Cargo.toml` version 与目标版本一致；
- `Cargo.lock` 存在；
- 使用 `cargo build --release --locked`；
- packaging 环境不读取 production secret、production state 或私有文档。

任一 identity 证据不一致都必须 fail closed。

## 4. 正式 artifact

Linux x86_64 的当前首个正式格式：

```text
reading-mcp-v<version>-linux-x86_64.tar.gz
SHA256SUMS
```

archive 内至少包含：

```text
reading-mcp
release-manifest.json
Cargo.toml
```

仓库目前由 `Cargo.toml` 声明 `license = "MIT"`。根 LICENSE 文件由独立 Issue #65 补齐；Packaging 不在构建时临时生成法律文本。LICENSE 文件进入仓库后，打包脚本会自动将其包含在 archive 中。

## 5. release-manifest.json

manifest 使用版本化 schema：

```text
reading-mcp-release-manifest/v1
```

至少记录：

```json
{
  "schema": "reading-mcp-release-manifest/v1",
  "version": "0.1.0",
  "tag": "v0.1.0",
  "git_sha": "<40-char source sha>",
  "target": "x86_64-unknown-linux-gnu",
  "platform": "linux-x86_64",
  "binary_sha256": "<sha256>",
  "cargo_lock_sha256": "<sha256>",
  "packaging_commit_sha": "<packaging infrastructure sha>"
}
```

`packaging_commit_sha` 是构建基础设施身份，不参与 Source identity，也不能把新的 main commit 冒充为旧 tag 的源码。

## 6. Package 生成

统一入口：

```bash
REPO_DIR=<clean-checkout-at-tag> \
RELEASE_VERSION=0.1.0 \
RELEASE_TAG=v0.1.0 \
RELEASE_SHA=<frozen-sha> \
TARGET_TRIPLE=x86_64-unknown-linux-gnu \
PLATFORM=linux-x86_64 \
OUTPUT_DIR=<dist-dir> \
PACKAGING_COMMIT_SHA=<packaging-infra-sha> \
bash scripts/package-release.sh
```

脚本负责：

1. 验证 source identity；
2. locked release build；
3. 计算 binary / Cargo.lock checksum；
4. 生成 manifest；
5. 生成 archive；
6. 生成 `SHA256SUMS`；
7. 立即重新解包并自验证 archive、manifest 和 binary checksum。

## 7. CI / GitHub Release assets

`.github/workflows/package-release.yml` 是长期 Package workflow。

它把 packaging infrastructure 与 release source 分开 checkout：

```text
main packaging infrastructure
        +
immutable release tag source
        ↓
package-release.sh
```

正式发布 asset 时：

- archive 与 `SHA256SUMS` 上传到对应 GitHub Release；
- 已存在同名 asset 时 fail closed，不使用 `--clobber` 静默重写；
- 同一版本 artifact 不通过“重新跑一次再覆盖”修复；
- 若正式 artifact 内容需要改变，应进入新的 patch/minor 版本，除非只是尚未发布过的首次 Package 生成。

`v0.1.0` 是 Package Protocol 建立前已经完成 Source Release 的 bootstrap 版本，因此允许在不移动 tag、不修改 frozen source 的前提下首次补充正式 assets；Package Issue #64 记录完整证据。

## 8. Production Deployment 不得重新编译

生产部署禁止运行：

```text
cargo build
```

生产机器只允许：

```text
download / receive exact artifact
        ↓
verify archive SHA256
        ↓
extract
        ↓
verify release-manifest identity
        ↓
verify binary SHA256
        ↓
install exact binary
        ↓
atomic switch
        ↓
restart + verify
```

因此生产机器不需要 Rust/Cargo 作为部署前置。

## 9. Deployment handoff

Package Issue 完成后，把以下事实交给 Deployment Issue：

- version；
- tag；
- source git SHA；
- archive file name；
- archive SHA256；
- binary SHA256；
- target/platform；
- GitHub Release asset identity。

Production Verification 必须重新计算生产 binary SHA256，并与 Package Issue / manifest 中的 `binary_sha256` 精确一致。

## 10. Failure / repair

以下情况属于 Package blocker：

- tag 与 frozen SHA 不一致；
- Cargo version 不一致；
- dirty source checkout；
- locked build 失败；
- archive checksum 不一致；
- manifest identity 不一致；
- 解包后 binary checksum 不一致；
- 同名正式 asset 已存在但内容身份无法证明一致。

遇到 blocker 时停止 Packaging。禁止跳过 checksum、修改 tag、现场替换 Release asset 后继续部署。
