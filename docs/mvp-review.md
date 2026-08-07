# Reading MCP MVP Hardening Review

## 1. Review 结论

本次 Review 的目标不是继续增加格式或 Tool，而是判断当前 Reading MCP 是否已经形成一个边界清晰、默认安全、可以由真实 MCP Client 使用的 MVP。

结论：

> 当前实现已经可以作为 **单用户、本地 stdio 场景的 MVP** 使用；核心架构边界成立，5 个 Tool 的真实协议链路已打通。它还不应被描述为“远程、多租户、恶意输入环境下已经完全 hardening 的文档服务”。

Review 中发现的两个高优先级问题已经直接修复：

1. 本地文件读取从“stdio 默认可读任意宿主机路径”改为 **default-deny + allowlisted roots**；
2. Domain 中已有的来源/定位事实在 MCP 边界被丢失，已补齐 `source / content_hash / parent_id / title / section_id` 等可追溯字段。

同时把早期需求中与当前实现不一致的“任意 page/range 读取”承诺收敛为：MVP 以 `Section` 为读取单元，页码和格式特有位置统一保留在 `Location` 中，后续如有真实需求再扩展统一 `read_document`。

---

## 2. Review 范围

本次按以下 7 个维度检查：

```text
需求覆盖
  ↓
SoC / SRP
  ↓
MCP Tool Contract
  ↓
Security Defaults
  ↓
Cache / State Lifecycle
  ↓
Error Semantics
  ↓
真实 MCP Client 体验
```

判断标准不是“代码能跑”，而是：

> 新需求是否只改变预期模块；安全边界是否默认成立；返回结果是否足以形成可验证证据链；实现与文档是否描述同一套真实能力。

---

## 3. 已通过的核心架构验收

### 3.1 Source 与 Format 正交

```text
File / HTTPS
      ↓
Retriever
      ↓
RetrievedResource
      ↓
ParserRouter
      ↓
Text / Markdown / HTML / PDF
```

没有出现：

```text
HttpPdfReader
LocalMarkdownReader
HttpHtmlReader
```

新增格式主要改变 `parsing`；新增来源主要改变 `retrieval/security`。

### 3.2 MCP Transport 与 Application 分离

```text
rmcp
 ↓
mcp adapter
 ↓
Application UseCases
 ↓
Domain + Ports
```

`rmcp` 不进入 Domain、Parser、Retriever 或 SearchIndex。

### 3.3 Search 与 Read 分离

SearchIndex 使用 paragraph/search-unit 粒度定位；`read_document` 与 `get_context` 从 DocumentRepository 读取规范化文档。

```text
Search Unit ≠ Section
Search ≠ Read
Index ≠ Document
```

### 3.4 Storage / Cache / Index 分离

当前仍保持：

```text
RawResourceCache       → 避免重复获取
ParsedDocumentCache    → 避免重复解析
DocumentRepository     → 运行时规范化事实来源
SearchIndex            → 可重建派生状态
```

没有合并成职责模糊的 `CacheManager` 或通用 Store。

### 3.5 AI 能力没有下沉

MCP 中不存在：

- summarize；
- question answering；
- explanation；
- quiz；
- notes；
- embedding / vector RAG。

Reading MCP 继续只提供可验证的文档上下文。

---

## 4. Review 中已修复的问题

### P0/P1：本地文件访问默认过宽

Review 前默认 stdio runtime 直接装配：

```text
LocalFileSourcePolicy
        +
FileRetriever
```

并允许任意本地路径。这意味着拥有 MCP Tool 调用能力的 Agent 可以尝试读取当前进程权限范围内的文档文件。

现已改为：

```text
未配置 local roots
       ↓
所有本地文件拒绝

READING_MCP_LOCAL_ROOTS
       ↓
canonical allowed roots
       ↓
requested canonical path
       ↓
必须位于某个 root 下
       ↓
FileRetriever
```

默认 stdio runtime 只开放通过 SSRF Policy 校验的公共 HTTPS 来源。

本地目录必须由部署者显式授权。

### P1：可追溯性字段在 MCP 边界丢失

Review 前 Normalized Document 已保存：

```text
source
content_hash
section parent
section title
location
```

但 MCP Response 没有完整返回这些事实。

现在：

`open_document` 返回：

```text
document_id
source
title
media_type
content_hash
section_count
```

`get_document_structure` 节点返回：

```text
section_id
parent_id
title
level
location
children
```

`search_document` hit 返回：

```text
section_id
title
source
snippet
score
location
```

`read_document` 返回：

```text
document_id
source
section_id
content
location
truncated
```

`get_context` 返回：

```text
document_id
source
owner_section_id
content
location
truncated
```

因此上层 AI 不需要依靠隐含状态猜测“这段文字来自哪里”。

### P1：需求文档与真实契约漂移

早期需求要求 page/logical range 读取，但 Phase 4 已有意选择：

```text
Read Unit = Section
PDF page = Location evidence
```

Review 后需求文档已与实际设计对齐：

- 不声明尚未实现的 page-range Tool；
- 不增加 PDF 专属读取接口；
- 后续范围读取只在真实使用证明必要时扩展统一 `read_document`。

---

## 5. 当前仍存在的 Hardening 项

以下问题没有被隐藏或包装成“已完成”，而是明确保留为后续工作。

### P1：统一解析资源预算

当前已经有：

- HTTP response size limit；
- request/connect timeout；
- redirect limit；
- HTTP concurrency limit；
- PDF 单页解压文本上限。

仍缺少：

```text
PDF total page limit
Parser global timeout
Parser cancellation
local file max bytes
统一 parse resource budget
```

对单用户本地 MVP 可接受；如果未来变成远程服务或读取不可信大文件，应优先完成。

### P1：HTTP Cache 自动新鲜度

当前 Raw Cache 保存：

```text
ETag
Last-Modified
```

但尚未实现：

```text
If-None-Match
If-Modified-Since
304 Not Modified
```

因此默认 cache 命中不会主动发现远端内容变化。

当前正确语义是：

```text
普通 open → 可使用 cache
force_refresh=true → 重新获取来源
```

条件 HTTP 重验证应作为 Retriever/Raw Cache 的后续优化完成，不应污染 Parser/MCP Tool。

### P1：默认 runtime 状态仍主要在内存

Phase 5 已有持久化 Raw/Parsed Cache adapter，但默认 stdio runtime 仍使用：

```text
InMemoryRawResourceCache
InMemoryParsedDocumentCache
InMemoryDocumentRepository
InMemorySearchIndex
```

进程重启后 opened-document runtime state 不保留。

这不是 SoC 问题，但真实长期使用后应引入明确的 runtime config/composition layer，而不是继续把配置逻辑堆在 `ReadingMcpServer::with_local_roots()`。

### P2：HTTP auth_profile

MCP Contract 保留 `auth_profile`，但 HTTP Retriever 当前明确拒绝使用。

在实现以下能力前，不应注入任意 Authorization/Cookie：

```text
Credential Store
Profile → Host binding
Secret redaction
Explicit policy
```

### P2：MCP Error taxonomy

当前 `RetrievalFailed` / `ParseFailed` 等错误主要映射为 MCP `invalid_params`。

对 MVP 可工作，但语义还可以进一步细分：

```text
用户参数错误
访问策略拒绝
来源不可达
格式不可解析
内部状态故障
```

后续应在不泄漏敏感内部信息的前提下统一错误代码和可恢复性提示。

### P2：真实 stdio fixture 覆盖面

真实 subprocess MCP E2E 当前使用 Markdown，已经证明 transport + Tool composition 成立。

HTML/PDF 已有独立集成测试，但还没有全部通过 stdio subprocess 跑一遍。

在发布第一个稳定版本前可以补成小型 acceptance matrix，但不需要复制业务逻辑测试。

---

## 6. 安全定位

当前 MVP 的推荐定位：

```text
单用户
本地 stdio
受信部署者
公共 HTTPS
显式授权本地目录
```

不应直接定位为：

```text
公网多租户服务
不可信用户任意上传
企业统一认证网关
通用浏览器/爬虫平台
```

如果未来进入远程/多人场景，需要重新进行 threat model，而不是假设 stdio 的安全边界自然延伸过去。

---

## 7. SoC / SRP Review 结论

当前核心依赖方向仍然成立：

```text
mcp ─────────→ application ─────────→ domain
                    ↑
                    │ ports
                    │
retrieval ──────────┤
security ───────────┤
parsing ────────────┤
infrastructure ─────┤
search/index ───────┘
```

本次 hardening 本身也验证了这个设计：

```text
修改本地文件授权
→ retrieval policy + composition + tests/docs
→ Parser/Domain/Search 不需要重写

补充 traceability response
→ Application result + MCP mapping + Search derived metadata
→ Retriever/Parser 格式逻辑不需要重写

收敛 range promise
→ Contract/docs
→ 不需要制造 PDF 专属 UseCase
```

说明系统目前是按“变化原因”拆分，而不是按“功能页面/使用场景”堆模块。

---

## 8. MVP Release Gate

在当前单用户 stdio 定位下，Review Gate：

```text
[PASS] Text / Markdown / HTML / PDF 统一 Document Model
[PASS] open / structure / search / context / read 五 Tool
[PASS] real MCP stdio subprocess E2E
[PASS] SSRF + redirect + DNS/IP policy
[PASS] HTTP response / timeout / concurrency limits
[PASS] local file default-deny + root allowlist
[PASS] Search Unit ≠ Read Unit
[PASS] Document ≠ Index
[PASS] source/location traceability through MCP
[PASS] no LLM dependency
[PASS] fmt + clippy + test CI

[DEFER] PDF total page / parser global resource budget
[DEFER] conditional HTTP cache revalidation
[DEFER] persistent default runtime state/config
[DEFER] auth_profile credential provider
[DEFER] refined MCP error taxonomy
```

因此当前建议状态是：

> **MVP core ready for controlled single-user stdio use; hardening backlog remains before remote or multi-user deployment.**

下一阶段应优先做真实使用反馈和资源预算 hardening，而不是继续增加格式数量。
