# Phase 5：HTTP、安全与缓存边界

## 1. 目标

Phase 5 的目标不是“给 Reader 加网络能力”，而是证明三个独立关注点可以组合而不互相侵入：

```text
HTTP 获取
   ≠
URL / SSRF 安全决策
   ≠
缓存生命周期
   ≠
文档解析
```

最终组合保持为：

```text
OpenDocumentUseCase
        │
        ├── SourcePolicy
        │       └── SourcePolicyRouter
        │              ├── LocalFileSourcePolicy
        │              └── PublicHttpAccessPolicy
        │
        ├── Retriever
        │       └── CachingRetriever
        │              └── RetrieverRouter
        │                     ├── FileRetriever
        │                     └── HttpRetriever
        │
        ├── Parser
        │       └── CachingParser
        │              └── ParserRouter
        │                     ├── TextParser
        │                     ├── MarkdownParser
        │                     ├── HtmlParser
        │                     └── PdfParser
        │
        ├── DocumentRepository
        └── SearchIndex
```

`OpenDocumentUseCase` 不包含 HTTP、SSRF、缓存命中、JSON 或文件系统逻辑。

---

## 2. HTTP Retriever 的职责

`HttpRetriever` 只负责受约束的 HTTP 获取：

- 发起请求；
- 手动处理 redirect；
- 读取 response body；
- 限制响应大小；
- 设置连接/请求超时；
- 限制并发；
- 读取 `Content-Type`；
- 保存 `ETag` / `Last-Modified`；
- 返回最终 URL；
- 返回 `RetrievedResource`。

它不负责：

- 判断业务上是否应该允许访问某个内网；
- 解析 HTML/PDF/Markdown；
- 保存 Document；
- 建立搜索索引；
- 执行 AI 逻辑。

---

## 3. SSRF 安全策略

### 3.1 默认协议

默认只允许 HTTPS。

HTTP 必须显式启用：

```text
PublicHttpAccessPolicy::https_only()   # 默认
PublicHttpAccessPolicy::allow_http()   # 显式选择
```

拒绝其他 scheme，例如：

```text
file://
ftp://
gopher://
```

URL 中嵌入 username/password 也被拒绝。

### 3.2 默认阻断本地地址

策略拒绝典型非公网地址，包括：

- loopback；
- RFC1918 private IPv4；
- link-local；
- IPv6 ULA/link-local；
- CGNAT；
- benchmark/documentation/reserved ranges；
- IPv4-mapped IPv6 对应的非公网地址；
- `localhost` / `.localhost` / `.local` / `.internal` / `.home.arpa`。

这也覆盖云 metadata 常见的 `169.254.169.254`。

### 3.3 DNS 与 DNS rebinding

安全判断不只发生在 `open_document` 的入口。

实际请求前：

```text
URL
 ↓
validate URL
 ↓
DNS resolve
 ↓
validate resolved IPs
 ↓
pin validated endpoint
 ↓
HTTP request
```

当前采用严格策略：如果一个域名的 DNS 结果同时包含公网和被阻断地址，则整个解析结果被拒绝，而不是挑一个公网地址继续。

这样安全策略不会依赖“第一次解析结果之后 DNS 不变化”的假设。

### 3.4 Redirect

HTTP client 不自动跟随重定向。

每一跳都重新执行：

```text
Location
 ↓
URL validation
 ↓
DNS resolution
 ↓
IP validation
 ↓
request
```

因此：

```text
public URL
   ↓ redirect
private / metadata endpoint
```

仍会被拒绝。

### 3.5 Proxy 边界

HTTP Retriever 禁用环境/system proxy。

原因是当前安全模型依赖：

```text
validated DNS result
        ↓
pinned direct endpoint
```

如果请求随后交给外部 proxy 再解析目标域名，会破坏这条安全证据链。

未来如果确实需要 proxy，应建立独立的受信 Proxy Policy，而不是直接继承环境代理。

---

## 4. 资源限制

当前 `HttpRetrieverConfig` 提供：

```text
max_redirects        = 5
max_response_bytes   = 16 MiB
request_timeout      = 20 s
connect_timeout      = 8 s
max_concurrency      = 8
```

响应大小同时进行：

1. `Content-Length` 预检查；
2. 流式读取时的实际字节上限检查。

第二层检查用于处理没有可信 `Content-Length` 或解压后变大的响应。

HTTP 只接受 MVP 格式对应的 Content-Type：

```text
text/plain
text/markdown
text/x-markdown
text/html
application/xhtml+xml
application/pdf
```

无 Content-Type 时，仅在 URL 后缀可以可靠推断时继续。

---

## 5. Cache 分层

缓存不是 Repository，也不是 SearchIndex。

```text
RawResourceCache
    ↓ 避免重复获取

ParsedDocumentCache
    ↓ 避免重复解析

DocumentRepository
    ↓ 当前运行时规范化文档事实来源

SearchIndex
    ↓ 可重新构建的派生搜索状态
```

四者有不同变化原因，因此不合并为一个 `CacheManager`。

### 5.1 Raw cache

Key：规范输入 `DocumentSource` 的哈希。

保存：

- raw bytes；
- final source；
- media type；
- ETag；
- Last-Modified；
- retrieval metadata。

提供：

- `InMemoryRawResourceCache`；
- `FileRawResourceCache`。

文件缓存使用：

```text
<cache-root>/raw/<key>.bin
<cache-root>/raw/<key>.json
```

body 先写、metadata 后写；metadata 不存在时视为 cache miss，避免把中断写入当成完整缓存。

### 5.2 Parsed cache

Key：

```text
final_source + raw_sha256
```

因此同一个 URL 内容发生变化时，会自然得到新的 Parsed Cache entry。

提供：

- `InMemoryParsedDocumentCache`；
- `FileParsedDocumentCache`。

文件缓存使用：

```text
<cache-root>/parsed/<key>.json
```

序列化 DTO 位于 infrastructure adapter 内部；Domain Model 不依赖 serde/JSON 文件格式。

### 5.3 Decorator

缓存通过 Port Decorator 接入：

```text
Retriever
   ↑
CachingRetriever
   ↑
RetrieverRouter

Parser
   ↑
CachingParser
   ↑
ParserRouter
```

而不是修改 `OpenDocumentUseCase`。

这意味着未来改成 SQLite、RocksDB 或其他缓存实现时，只替换 adapter。

---

## 6. Refresh 语义

`force_refresh=true` 当前只绕过 Raw Cache：

```text
force_refresh
     ↓
重新 Retriever
     ↓
raw bytes hash
     ↓
如果内容未变化
     ↓
Parsed Cache 仍然可以命中
```

因此“重新确认来源”不等于“强制重复解析”。

---

## 7. 当前明确未实现

### 条件 HTTP 重验证

当前已经保存：

```text
ETag
Last-Modified
```

但尚未发送：

```text
If-None-Match
If-Modified-Since
```

因此还没有实现 `304 Not Modified` 条件重验证。

这是 Retriever/Raw Cache 之间未来可以增加的优化，不要求修改 Parser、Domain 或 MCP Tool Contract。

### HTTP auth profile

虽然 `RetrievalOptions` 已有 `auth_profile`，但 HTTP Retriever 当前会明确拒绝它。

尚未实现：

- credential store；
- profile → host/domain binding；
- Authorization header 注入。

在安全模型完成前，不接受模型直接传入任意 Authorization header。

### OCR

扫描 PDF 的 OCR 仍不属于 Phase 5；无可提取文本时继续返回明确错误。

---

## 8. SoC / SRP 验收

Phase 5 应满足：

```text
修改 SSRF 规则
→ security 变化
→ Parser 不变

更换 HTTP client
→ retrieval 变化
→ Domain / Parser 不变

更换文件缓存为 SQLite
→ infrastructure/cache adapter 变化
→ OpenDocumentUseCase 不变

新增 EPUB
→ parsing 变化
→ HTTP / SSRF / Cache Port 不变

新增 S3 来源
→ retrieval/source policy 变化
→ PDF/HTML Parser 不变
```

最终判断标准仍然是：

> 一个模块应该只有一个主要变化原因；不同关注点通过 Port 组合，而不是通过条件分支互相渗透。
