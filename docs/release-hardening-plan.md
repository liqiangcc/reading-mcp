# v0.1 Release Hardening

## 完成矩阵

### Resource Budget

- [x] local file max bytes
- [x] HTTP response limit / timeout / redirects / concurrency
- [x] PDF total pages + per-page extraction limit
- [x] ZIP entry count / entry bytes / total bytes
- [x] Parser timeout
- [x] normalized chars / sections / depth

### Runtime / Persistence

- [x] RuntimeConfig 与 MCP adapter 分离
- [x] HTTPS-only 默认
- [x] local file default-deny + allowed roots
- [x] persistent / memory runtime switch
- [x] persistent Raw/Parsed Cache
- [x] SQLite DocumentRepository
- [x] SQLite FTS5 SearchIndex
- [x] restart 后旧 document_id 可 read/search

### HTTP Freshness / Auth

- [x] ETag / Last-Modified
- [x] If-None-Match / If-Modified-Since / 304
- [x] force_refresh
- [x] auth_profile host binding
- [x] redirect 每跳 credential isolation
- [x] private Raw Cache profile isolation

### Error / Observability

- [x] stable MCP error code
- [x] retryable
- [x] stderr structured telemetry
- [x] no content/secret logging

### Formats / Acceptance

- [x] Text / Markdown / HTML / PDF
- [x] EPUB / DOCX / OpenAPI-Swagger JSON/YAML
- [x] real MCP stdio subprocess E2E
- [x] Text/Markdown/HTML/PDF stdio acceptance matrix
- [x] persistent restart test
- [x] HTTP revalidation/auth tests
- [x] resource budget tests
- [x] SQLite reopen/FTS tests
- [x] architecture dependency-boundary test

## Merge Review

- [x] remove accidentally committed `target/`
- [x] add `/target/` to `.gitignore`
- [x] remove temporary release workflows
- [x] align requirements/Phase5/Phase6/MVP review docs

## v0.1 Release Gate

```text
[x] cargo fmt --all -- --check
[x] cargo clippy --locked --all-targets --all-features -- -D warnings
[x] cargo test --locked --all-features
[x] real stdio acceptance matrix
[x] architecture boundary tests
[x] README/runtime/requirements aligned
[x] no build artifacts or temporary release workflows
```

PR creation、merge、tag 与 GitHub Release 仍是独立授权动作。

## Post-v0.1 Convergence Hardening

当前 `main` 之后的收敛目标不再横向增加格式，而是解决真实运行边界：

### MCP Response

- [x] `read_document` 默认/硬字符上限
- [x] `get_context` 默认/硬字符上限
- [x] `max_chars=0` 参数拒绝
- [x] `get_document_structure` 可见节点上限
- [x] 行为测试证明默认响应会截断，超大结构会拒绝

### Search

- [x] FTS5 / BM25 保持主检索路径
- [x] CJK 自然语言查询的 bounded canonical fallback
- [x] 主索引召回不足时 relaxed fallback
- [x] snippet 围绕真实命中位置
- [x] 不引入 Vector DB / 通用 RAG

### Parser Runtime

- [x] 同步格式解析移入 Tokio blocking pool
- [x] async runtime worker 不再直接承担长同步 parser 工作
- [x] 保留 Parser timeout
- [x] 文档明确 blocking task 仍不能被 Tokio 硬抢占，资源 budget 仍是主防线

### Streamable HTTP

- [x] HTTP transport 收敛到 MCP adapter
- [x] loopback-only bind
- [x] mandatory Bearer Token
- [x] Host validation 默认开启
- [x] Origin validation 默认开启
- [x] 随机隧道关闭 Host validation 必须显式 opt-out
- [x] `/healthz` / `/readyz` + legacy `/health`
- [x] 集成测试覆盖 401、hostile Host、hostile Origin
- [x] architecture boundary test 禁止 Axum 泄漏到 core layers

### Repository Gate

- [x] convergence branch `cargo fmt --all -- --check`
- [x] convergence branch `cargo clippy --locked --all-targets --all-features -- -D warnings`
- [x] convergence branch `cargo test --locked --all-features`
- [ ] 外部 ChatGPT / Secure MCP Tunnel 产品验收（Issue #3）

外部 ChatGPT 验收仍然是独立产品环境证据，不能由仓库 CI 替代。

## Future / 非 v0.1 阻断项

OCR、浏览器渲染、企业产品 API、OAuth/Cookie 登录、公网多租户 transport、通用 crawler、AI 总结/问答/笔记/Vector RAG，以及真实需求出现后再考虑的统一 range-read。
