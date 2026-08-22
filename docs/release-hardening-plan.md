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
- [x] StructureCursor and DiscoveryCursor continuation
- [x] whole-document body-order composition and multi-Section evidence
- [x] TextLocator restart/resume after MCP/server restart
- [x] Streamable HTTP full reading lifecycle E2E

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
[x] current runtime exposes exactly seven MCP Tools
[x] no build artifacts or temporary release workflows
```

PR creation、merge、tag 与 GitHub Release 仍是独立授权动作。

## Future / 非 v0.1 阻断项

OCR、浏览器渲染、企业产品 API、OAuth/Cookie 登录、公网多租户 transport、通用 crawler、AI 总结/问答/笔记/Vector RAG，以及真实需求出现后再考虑的统一 range-read。
