# Phase 5：HTTP、安全、认证与缓存边界

## 目标

```text
HTTP 获取 ≠ SSRF 安全策略 ≠ Credential Provider ≠ Cache ≠ Parser
```

`OpenDocumentUseCase` 只编排 Port，不知道 reqwest、DNS、缓存文件、SQLite 或具体 Parser。

## HTTP 安全链路

默认 HTTPS-only；HTTP 只能显式开启。URL 中 username/password 被拒绝。

每次请求都执行：

```text
URL validation
→ DNS resolve
→ validate every resolved IP
→ choose validated endpoint
→ pin host to endpoint
→ direct request (no proxy)
```

阻止 loopback、RFC1918、link-local、metadata、CGNAT、benchmark/documentation/reserved IPv4，以及 IPv6 loopback/ULA/link-local/multicast/documentation 等范围。一个域名只要解析结果中包含被阻止地址，整体拒绝。

Redirect 不由 reqwest 自动处理；每一跳重新执行 URL/DNS/IP 校验。条件 validator 不跨 redirect 盲目透传。

## HTTP 资源限制

默认配置包括 redirect、connect/request timeout、并发和最大响应体限制。响应大小同时检查 `Content-Length` 和流式解压后的实际字节数。

HTTP Content-Type allowlist 覆盖 Text/Markdown/HTML/PDF/EPUB/DOCX/OpenAPI JSON/YAML；缺失 Content-Type 时仅在 URL 扩展名可可靠推断时继续。

## Credential Provider

MCP 只接受 `auth_profile`。`EnvironmentCredentialProvider` 从部署环境读取：

```text
READING_MCP_AUTH_<PROFILE>_HOSTS
READING_MCP_AUTH_<PROFILE>_BEARER_TOKEN
```

Profile 名只允许 ASCII 字母/数字/`-`/`_`。Token 仅在当前 URL host 命中 allowlist 时注入；redirect 每一跳重新验证，因此不会把凭据自动带到未授权 host。

## Cache

```text
RawResourceCache      = 下载结果
ParsedDocumentCache   = raw content hash 对应的 Normalized Document
DocumentRepository    = 事实来源
SearchIndex           = 可重建派生索引
```

默认持久化 Raw/Parsed Cache。HTTP Raw Cache 保存 ETag/Last-Modified；普通 reopen 会发送 `If-None-Match` / `If-Modified-Since`，304 时复用缓存。`force_refresh=true` 重新获取来源；若 raw bytes 未变化，Parsed Cache 仍可复用。

Raw Cache（`<state>/cache/raw`）按条目整体限长：`READING_MCP_RAW_CACHE_MIB`（默认 1024 MiB，`0` 不设上限）以下，超限时按最近使用时间淘汰整个条目（body 与 metadata 一起，先删 metadata 再删 body），所以并发读者只会看到完整条目或干净的 miss。读取（`get`）与 `source_view` 的 `locate` 命中会显式刷新两个文件的访问时间，不依赖挂载的 atime 选项。被淘汰的条目只多付一次重新下载：`source_view` 回到 revalidating 检索路径，正确性不变。

所有以「同目录临时文件 + rename」发布的缓存——raw（`cache/raw`）、rendered source-view（`cache/source-view`）、OCR checkpoint（`ocr-checkpoints/<key>`）与 OCR evidence（`ocr-evidence`）——在启动时删除残留的临时文件；由于 state dir 可能被两个进程共用，只删修改时间超过 1 小时的临时文件，更新的临时文件可能属于进行中的写入。Parsed Cache 不在此列：它原地覆盖单个文件，不写临时文件。

认证缓存 key 包含 auth profile，避免不同身份共享私有响应。

## 统一资源预算

除 HTTP 限制外，Runtime 还约束：

- 本地文件最大字节数；
- PDF 总页数与单页文本解压；
- EPUB/DOCX ZIP entries、单 entry、总解压量；
- Parser timeout；
- Normalized Document 最大字符数、section 数和深度。

## SoC 验收

```text
修改 SSRF → security
更换 HTTP client → retrieval
更换 Cache → infrastructure
新增格式 → parsing
更换 Repository/SearchIndex → infrastructure ports
```

以上变化均不要求修改 MCP Tool Contract。
