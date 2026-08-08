# Reading MCP 设计原则：关注点分离与单一职责

本文定义 Reading MCP 的最高级架构约束。实现细节、技术选型和格式支持都必须服从这些约束。

## 1. 两个原则的关系

### 关注点分离（Separation of Concerns, SoC）

回答：**系统应该在哪些地方切开？**

Reading MCP 中至少要分离这些关注点：

```text
来源获取
≠
安全策略
≠
格式解析
≠
规范化文档模型
≠
持久化
≠
缓存
≠
索引/搜索
≠
位置/引用
≠
阅读
≠
MCP 协议适配
≠
AI 理解与推理
```

### 单一职责原则（Single Responsibility Principle, SRP）

回答：**一个模块应该因为什么原因而变化？**

一个模块应只有一个主要变化原因。如果两个变化轴彼此独立，就不应长期放在同一个模块中。

例如：

```text
增加 EPUB 支持
→ parsing 变化

增加 S3 来源
→ retrieval 变化

更换全文索引实现
→ search/index 变化

MCP schema 升级
→ mcp adapter 变化

修改 SSRF 策略
→ security policy 变化

增加 AI 总结方式
→ Reading MCP 不应变化
```

核心判断：

> 按变化原因划分职责，按数据流组合能力。

---

## 2. 顶层职责边界

```text
                    MCP Adapter
                         │
                         ▼
                  Application Layer
                         │
          ┌──────────────┼──────────────┐
          │              │              │
       Retrieval       Search          Read
          │              │              │
          ▼              ▼              ▼
      RawResource   SearchResult     Document
          │                              │
          ▼                              ▼
        Parsing                    Location/Citation
          │
          ▼
   Normalized Document
          │
     ┌────┼────┐
     │    │    │
 Storage Cache Index
```

AI 的解释、总结、推理、教学能力位于 Reading MCP 边界之外。

---

## 3. 变化原因矩阵

| 模块 | 唯一主要职责 | 主要变化原因 | 不应承担 |
|---|---|---|---|
| `mcp` | MCP 协议适配 | MCP schema/transport 变化 | 下载、解析、搜索实现 |
| `application` | 用例编排 | 阅读流程变化 | HTTP/PDF/SQLite 细节 |
| `domain` | Document/Section/Location 语义 | 核心文档语义变化 | 网络、数据库、MCP |
| `retrieval` | 获取原始资源 | 新来源/传输方式 | 文档结构解析 |
| `security` | 访问策略决策 | SSRF/权限/资源限制策略 | HTTP 客户端实现 |
| `parsing` | RawResource → Document | 新格式/解析策略 | 下载、认证、搜索 |
| `storage` | 保存规范化文档 | 存储介质变化 | 搜索排序 |
| `cache` | 避免重复获取/计算 | 缓存策略变化 | 事实数据建模 |
| `index` | 建索引与检索 | 搜索算法/索引引擎变化 | 作为事实来源 |
| `location` | 稳定位置解析 | 定位模型变化 | 格式下载 |
| `citation` | 来源表达 | 引用输出格式变化 | 解析原文 |

如果一个模块持续出现两个以上互相独立的变化原因，应优先重新划分边界。

---

## 4. 必须守住的分离点

### 4.1 MCP Transport ≠ Application Logic

MCP Tool 只负责：

```text
参数解析
参数校验
调用 UseCase
DTO 映射
错误映射
响应大小控制
```

禁止在 MCP handler 中直接：

```text
HTTP 请求
PDF 解析
SQL 查询细节
索引构建
缓存失效逻辑
```

---

### 4.2 Retrieval ≠ Parsing

统一流程：

```text
Source
  ↓
Retriever
  ↓
RawResource
  ↓
Parser
  ↓
NormalizedDocument
```

禁止：

```text
PdfParser.download(url)
HttpRetriever.parseHtml()
```

扩展应正交：

```text
Retriever: HTTP / File / GitHub / S3
Parser: HTML / Markdown / PDF / EPUB
```

不能按组合创建：

```text
HttpPdfReader
LocalPdfReader
GitHubMarkdownReader
HttpHtmlReader
```

否则来源数 × 格式数会导致组合爆炸。

---

### 4.3 Security Policy ≠ HTTP Implementation

HTTP 客户端负责发送请求；安全策略负责判断请求是否允许。

建议独立策略：

```text
SchemePolicy
HostPolicy
IpPolicy
RedirectPolicy
ResourceLimitPolicy
CredentialPolicy
```

这样修改“是否允许私网”时，不需要改 HTTP 核心实现。

---

### 4.4 Section ≠ Chunk

`Section` 是作者定义的逻辑阅读结构。

`Chunk` 是系统为搜索、索引或 Token 限制产生的技术结构。

```text
Chapter 7
└── 7.3 Page Tables        ← Section
    ├── search chunk 1
    ├── search chunk 2
    └── search chunk 3
```

规则：

- 阅读 API 优先暴露 Section；
- 搜索内部可以使用 Chunk；
- Chunk 必须能映射回所属 Section 和稳定 Location；
- Chunk 不应成为核心 Domain Model 的替代品。

---

### 4.5 Search ≠ Read

搜索回答：**在哪里？**

读取回答：**那里是什么？**

```text
search_document("virtual memory")
→ section_id
→ location
→ score
→ short snippet

read_document(section_id)
→ 完整逻辑阅读单元
```

禁止让搜索接口不断膨胀为大段正文返回接口。

---

### 4.6 Document ≠ Index

`Document` 是规范化事实数据；`Index` 是派生数据。

必须满足：

```text
删除 Index
→ 可以由 Document 重建

删除 Document
→ 不能依赖 Index 恢复完整事实
```

因此 SearchIndex 不得成为 Document Repository。

---

### 4.7 Storage ≠ Cache ≠ Index

三者目的不同：

```text
Storage
→ 保存规范化事实

Cache
→ 避免重复下载/解析/计算

Index
→ 加速检索
```

不要创建一个无边界的 `DocumentCacheManager` 同时承担持久化、索引和缓存。

---

### 4.8 Location ≠ Citation

`Location` 表达内部稳定位置：

```text
page
chapter
section_path
anchor
paragraph
char_range
native_location
```

`Citation` 负责把 Location 转换为 AI/用户可理解的来源表达。

Parser 负责产生位置事实，不负责拼接最终引用文案。

---

### 4.9 Reading MCP ≠ AI Reading Product

Reading MCP 只负责提供可靠文档上下文。

允许：

```text
open
structure
search
read
context
location
citation
```

不允许把以下能力逐步塞入核心：

```text
summarize_book
explain_section
generate_quiz
generate_notes
answer_question_with_llm
```

原因不是这些能力没有价值，而是它们属于上层 Agent/学习产品的变化轴。

---

## 5. 依赖方向

核心依赖规则：

```text
mcp ────────→ application
                  │
                  ▼
               domain

infrastructure ─→ application/domain ports
```

具体实现依赖抽象，而不是反过来。

例如：

```text
OpenDocumentUseCase
    depends on
RetrieverPort
ParserPort
DocumentRepositoryPort
SearchIndexPort
```

而不是：

```text
OpenDocumentUseCase
    depends directly on
reqwest / axios / pdf.js / sqlite
```

### 禁止依赖

```text
parsing    → mcp
retrieval  → mcp
search     → mcp
security   → parsing
index      → MCP DTO
parser     → HTTP concrete client
application→ specific PDF/HTML library
```

---

## 6. 用例编排与领域能力分离

Application 层可以编排多个职责，但不应拥有具体实现细节。

例如：

```text
OpenDocumentUseCase
  1. normalize source
  2. security policy check
  3. retrieve resource
  4. select parser
  5. parse
  6. persist document
  7. update index
  8. return metadata
```

这里“负责流程”不等于“负责每一步的实现”。

这是 SRP 最容易被误解的地方：

> UseCase 的单一职责可以是“完成一个业务用例的编排”，而不是只能调用一个函数。

---

## 7. 扩展性验算

任何新需求进入实现前，先做影响范围预测。

### 新增 EPUB

期望主要影响：

```text
parsing/epub
parser router/config
parser fixtures
```

不应修改：

```text
MCP tools
HTTP retriever
search API
Document read API
```

### 新增 S3 来源

期望主要影响：

```text
retrieval/s3
auth/config
```

不应修改：

```text
PdfParser
MarkdownParser
search model
MCP read tool
```

### 从 SQLite FTS5 换成其他全文索引

期望主要影响：

```text
index implementation
index configuration
index integration tests
```

不应修改：

```text
Parser
Retriever
MCP schema
Domain Document
```

### 新增“总结章节”产品能力

期望影响：

```text
上层 Agent / AI application
```

Reading MCP：

```text
无需修改
```

如果实际影响范围明显大于预期，应先检查边界是否泄漏，而不是立即打补丁。

---

## 8. 架构评审检查表

每个新增模块、Tool、格式或基础设施能力都检查：

```text
[ ] 这个模块唯一的主要变化原因是什么？
[ ] 是否混合了两个独立关注点？
[ ] 是否可以通过接口替换实现，而不影响上层？
[ ] Source 和 Format 是否仍然正交？
[ ] Section 和 Chunk 是否仍然分离？
[ ] Search 和 Read 是否仍然分离？
[ ] Document 是否仍然是事实来源，Index 只是派生数据？
[ ] Security Policy 是否独立于 HTTP 实现？
[ ] MCP 层是否仍然只是协议适配？
[ ] 是否把 AI 能力错误地下沉到 Reading MCP？
[ ] 新需求是否只修改预期的少量模块？
```

出现以下信号时，应停止扩展并重构边界：

```text
一个类/模块同时包含 fetch + parse + index
增加一种格式需要修改 MCP Tool
增加一种来源需要修改 Parser
search_document 开始返回越来越大的正文
Chunk 逐渐替代 Section
Index 变成唯一可读取数据源
MCP handler 出现大量业务逻辑
security if/else 散落在 HTTP 客户端各处
```

---

## 9. 最终原则

```text
按变化原因拆职责
按数据流组合能力
正交扩展来源与格式
结构是阅读语义，Chunk 只是技术实现
事实数据与派生索引分离
协议适配与业务逻辑分离
策略与机制分离
文档上下文与 AI 智能分离
```

一句话：

> Reading MCP 要做的是稳定的“文档读取能力内核”，而不是不断吸收所有与阅读有关的功能。
