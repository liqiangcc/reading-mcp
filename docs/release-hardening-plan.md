# Release Hardening Plan

## 目标

本阶段不再扩展 MCP Tool 数量，而是把 Reading MCP 从“功能闭环 MVP”提升为可以长期使用的 `v0.1.0` 候选内核。

所有工作继续遵循：

```text
按变化原因拆职责
不同能力通过 Port / Adapter / Composition 组合
MCP Tool Contract 保持少而稳定
```

## 完成矩阵

### Resource Budget

- [x] 本地文件读取前大小限制；
- [x] HTTP 响应大小限制；
- [x] PDF 总页数限制；
- [x] PDF 单页解压文本限制；
- [x] ZIP entry 数量限制；
- [x] ZIP 单 entry 解压大小限制；
- [x] ZIP 总解压大小限制；
- [x] Parser 超时；
- [x] Normalized Document 最大字符数；
- [x] 最大 Section 数量；
- [x] 最大 Section 深度。

`Parser` 超时使用 Tokio cooperative timeout。它可以取消正常异步 Parser 流程，但不能硬抢占一个长时间不 yield 的同步 CPU 调用。因此 PDF/ZIP 等格式同时使用解析前的确定性资源上限，避免只依赖 timeout。

### Runtime Configuration

- [x] 配置从 `ReadingMcpServer` 移出到 `RuntimeConfig`；
- [x] 环境变量启动配置；
- [x] 配置错误启动时 fail-fast；
- [x] HTTPS-only 默认值；
- [x] local file default-deny；
- [x] 显式 local root allowlist；
- [x] 可切换 persistent / memory runtime；
- [x] telemetry 可关闭。

### Persistent State / Search

- [x] 默认持久化 Raw Cache；
- [x] 默认持久化 Parsed Cache；
- [x] SQLite `DocumentRepository`；
- [x] SQLite FTS5 `SearchIndex`；
- [x] Repository 与 SearchIndex 保持两个独立 Port；
- [x] MCP 进程重启后旧 `document_id` 仍可 read/search。

底层可以共享同一个 SQLite 文件，但职责不合并：

```text
DocumentRepository = 规范化文档事实来源
SearchIndex         = 可重建派生数据
```

### HTTP Cache Freshness

- [x] 保存 ETag；
- [x] 保存 Last-Modified；
- [x] `If-None-Match`；
- [x] `If-Modified-Since`；
- [x] `304 Not Modified` 复用缓存 body；
- [x] redirect 后不透传旧 validator；
- [x] `force_refresh=true` 强制重新获取。

### Authentication Profile

- [x] MCP 只接受 `auth_profile` 名称；
- [x] Bearer Token 仅从部署环境读取；
- [x] Profile 必须绑定 host allowlist；
- [x] redirect 每跳重新校验 profile/host；
- [x] 私有 Raw Cache 按 auth profile 隔离；
- [x] 不接受模型提供任意 `Authorization` / `Cookie` Header。

### Error Taxonomy

- [x] Stable error code；
- [x] `retryable` 标记；
- [x] 资源超限单独分类；
- [x] 认证失败单独分类；
- [x] 内部 Store/Index 错误与用户错误分开。

### Observability

所有结构化事件写 stderr，不写 stdout：

- [x] retrieve duration / bytes / media type；
- [x] parse duration / section count / PDF pages；
- [x] raw cache hit/miss；
- [x] parsed cache hit/miss；
- [x] index duration；
- [x] search duration / hit count；
- [x] 只记录 query 字符数，不记录搜索正文；
- [x] 不记录 Token、Authorization 或文档正文。

### Acceptance

- [x] Rust unit/integration tests；
- [x] real MCP stdio subprocess test；
- [x] Text/Markdown/HTML/PDF stdio acceptance matrix；
- [x] persistent MCP state restart test；
- [x] HTTP conditional revalidation test；
- [x] auth redirect isolation test；
- [x] resource budget tests；
- [x] SQLite reopen/FTS test。

### Extended Formats

新增格式仍复用同一 5 Tool 和同一 Normalized Document：

- [x] EPUB：bounded ZIP + OPF spine + existing HtmlParser；
- [x] DOCX：bounded ZIP + WordprocessingML heading tree；
- [x] OpenAPI / Swagger：JSON/YAML → path/operation/schema Sections；
- [x] GitHub README/Wiki：复用 Markdown / HTML；
- [x] Javadoc：复用 HTML；
- [x] MkDocs / Docusaurus / GitBook：静态输出复用 HTML。

这里刻意**不**增加 `GitHubReadmeParser`、`JavadocParser`、`MkDocsParser` 等语义重复实现，因为这些是来源/站点形态，不是新的文档格式。

## 明确不属于 v0.1.0 的范围

以下能力需要新的安全模型或新的来源 Adapter，不应继续塞进本次 release-hardening：

- OCR / 扫描 PDF；
- JavaScript-heavy 页面浏览器渲染；
- Confluence / Notion / 飞书 / 语雀等产品 API；
- OAuth / Cookie 登录流程；
- 公网多租户 MCP 服务；
- 通用 Web crawler；
- AI 总结 / 问答 / 笔记 / 向量 RAG。

这些不是“没做完的 MVP bug”，而是未来独立演进的关注点。

## v0.1.0 Release Gate

```text
[ ] cargo fmt --all -- --check
[ ] cargo clippy --all-targets --all-features -- -D warnings
[ ] cargo test --all-features
[ ] real stdio acceptance matrix green
[ ] README / runtime config docs aligned
[ ] no accidental cross-layer dependency
```

所有 Gate 通过后，代码才适合进入 PR / merge / tag 流程。创建 PR、合并和打 tag 仍是独立发布动作。
