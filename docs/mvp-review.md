# Reading MCP v0.1 最终 Hardening Review

## 结论

当前实现已经达到 **单用户、本地 stdio 场景的 v0.1 release candidate**。核心功能、默认安全、持久化状态、资源预算、HTTP 新鲜度、认证隔离、错误语义、可观察性和真实 MCP E2E 均已落地。

推荐定位仍是：

```text
单用户
本地 stdio
公共 HTTPS
显式授权本地 roots
部署侧 Secret Provider
```

它不应被直接宣传为公网多租户或任意恶意上传场景的 hardened 文档服务；那需要新的 threat model。

## 架构 Review

关键边界成立：

```text
Source ≠ Format
SourcePolicy ≠ Retriever
Retriever ≠ Parser
Parser ≠ Repository
Repository ≠ SearchIndex
Search Unit ≠ Read Unit
Runtime Composition ≠ MCP Adapter
Reading MCP ≠ AI Application
```

依赖方向由 `tests/architecture_boundaries.rs` 自动约束，防止 Domain/Application 引入 rmcp/reqwest/lopdf/rusqlite 等具体实现依赖。

## 已完成 Hardening

### Security / Resource Budget

- HTTPS-only 默认；
- SSRF hostname/DNS/IP/redirect 防护；
- endpoint pinning + no proxy；
- local file default-deny + canonical root allowlist；
- HTTP timeout/redirect/concurrency/body limits；
- local file max bytes；
- PDF total pages + per-page extraction limits；
- EPUB/DOCX bounded ZIP；
- Parser timeout；
- normalized chars/section count/depth limits。

### State / Search

- 默认持久化 Raw/Parsed Cache；
- SQLite DocumentRepository；
- SQLite FTS5 SearchIndex；
- Repository 与 Index 保持独立 Port；
- 进程重启后旧 document_id 可继续 read/search。

### HTTP Freshness / Auth

- ETag / Last-Modified；
- If-None-Match / If-Modified-Since；
- 304 cache reuse；
- force_refresh；
- host-bound auth_profile；
- redirect 每跳认证隔离；
- authenticated raw cache profile isolation。

### MCP / Operations

- 稳定 `error code + retryable`；
- stderr structured telemetry；
- stdout 仅 MCP JSON-RPC；
- Cargo.lock + CI `--locked`；
- real stdio subprocess E2E；
- Text/Markdown/HTML/PDF stdio acceptance；
- EPUB/DOCX/OpenAPI integration tests。

## 格式范围

独立 Parser：Text、Markdown、HTML/XHTML、PDF、EPUB、DOCX、OpenAPI/Swagger JSON/YAML。

GitHub README/Wiki、Javadoc、MkDocs/Docusaurus/GitBook 静态输出复用 Markdown/HTML，不制造重复 Parser。

## 合并前 Review 发现并修复

- 移除误提交的 `target/` Rust 构建产物；
- 新增 `.gitignore` 的 `/target/`；
- 移除临时 release helper workflows，只保留正常 `ci.yml`；
- 删除仅用于发布过程的临时 gate/RC 证明文件；
- 将 requirements、Phase 5、Phase 6、MVP Review 与当前实现重新对齐。

## Release Gate

合并前必须保持：

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
```

同时要求：无 `target/`、无临时 release workflow、架构边界测试通过、README/requirements/runtime docs 与实现一致。

## Future，不属于 v0.1 未完成项

- OCR / 扫描 PDF；
- JavaScript-heavy browser retriever；
- Confluence/Notion/飞书/语雀等产品 API；
- OAuth/Cookie 交互登录；
- Streamable HTTP / 公网多租户；
- 通用 Web crawler；
- AI 总结/问答/笔记；
- 通用 Vector RAG；
- 只有真实使用证明需要后再扩展的统一 range-read。
