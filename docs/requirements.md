# Reading MCP 需求文档

## 1. 项目目标

Reading MCP 是一个面向 AI 的统一文档阅读上下文服务。

它解决的不是“让模型生成总结”，而是让模型能够可靠地完成以下动作：

1. 打开文档；
2. 识别文档结构；
3. 搜索文档内容；
4. 精确读取章节或位置范围；
5. 展开命中位置的上下文；
6. 返回可追溯的位置和来源。

最终目标：

> 让 AI 能够和用户精确地阅读同一份文档或同一本书，而不是依赖整篇文本注入或无来源的二手知识。

---

## 2. 目标用户

主要面向：

- 使用 ChatGPT / Codex / Claude 等 Agent 阅读技术资料的开发者；
- 需要 AI 辅助阅读教材、技术书和论文的人；
- 需要 AI 阅读 RFC、KIP、设计文档、API 文档的工程人员；
- 需要统一 HTML / PDF / Markdown / EPUB 等格式读取方式的 MCP 客户端。

---

## 3. 核心场景

### 3.1 在线技术文档

用户提供文档 URL，AI 打开文档后先查看结构，再搜索并读取相关章节。

### 3.2 长 PDF / 教材

AI 不一次读取整本书，而是：

```text
open_document
→ get_document_structure
→ read_document(section)
→ search_document(query)
→ get_context(location)
```

### 3.3 证据链学习

AI 回答某个概念时，需要能够返回：

- 原始文档；
- 章节；
- 页码或逻辑位置；
- 原文上下文。

上层模型再基于这些证据解释、推理和设计实验。

---

## 4. 功能需求

### FR-001 打开文档

系统必须支持通过统一入口打开文档。

MVP source 类型：

- HTTP/HTTPS URL；
- 本地文件路径（由部署模式决定是否开启）。

返回至少包含：

- `document_id`；
- title；
- media type；
- content hash；
- source；
- 是否存在目录；
- 页数/章节数（可获取时）。

### FR-002 获取文档结构

必须支持返回结构化目录，而不是只返回平铺 chunk。

结构至少包含：

- section id；
- title；
- level；
- parent id；
- location；
- children。

### FR-003 文档内搜索

必须支持对已经打开或缓存的文档进行全文搜索。

搜索结果至少返回：

- section id；
- title；
- snippet；
- location；
- score。

MVP 优先全文检索，不要求向量搜索。

### FR-004 按章节读取

必须支持通过 `section_id` 读取完整章节或受限长度正文。

### FR-005 按范围读取

必须支持通过位置范围读取正文，例如：

- page range；
- logical location range；
- paragraph/anchor range。

### FR-006 上下文展开

必须支持围绕搜索命中位置向前/向后展开上下文，以避免孤立 chunk。

### FR-007 来源定位

所有正文返回结果必须尽可能携带可追溯位置。

示例：

```json
{
  "document_id": "doc_xxx",
  "source": "...",
  "chapter": "7",
  "section": "7.3 Page Tables",
  "page": 183,
  "location": "section://7/3#p12",
  "content": "..."
}
```

### FR-008 缓存

同一文档在未变化时不应重复下载和解析。

缓存至少考虑：

- normalized source；
- ETag；
- Last-Modified；
- content hash；
- parsed document；
- search index。

### FR-009 格式路由

系统必须通过 Content-Type / 文件特征选择解析器，并将结果统一为 Normalized Document。

---

## 5. MCP 工具范围

MVP 只暴露以下工具：

```text
open_document
get_document_structure
search_document
read_document
get_context
```

不在 MVP 增加语义重复的小工具。

---

## 6. 非功能需求

### NFR-001 Token 效率

默认响应必须遵循按需读取原则，禁止在没有明确请求时返回整份长文档。

### NFR-002 可追溯性

正文返回值必须保留来源和位置。

### NFR-003 确定性

相同文档版本应产生尽可能稳定的 section id 和 location。

### NFR-004 可扩展性

新增格式解析器不应要求修改 MCP Tool 接口。

### NFR-005 关注分离

Retriever、Parser、Index、Cache、MCP Adapter 必须职责独立。

### NFR-006 安全默认

外部 URL 获取必须默认启用 SSRF 防护和资源限制。

### NFR-007 无模型依赖

Reading MCP 核心运行不依赖 LLM。

---

## 7. 安全需求

### SR-001 协议限制

默认仅允许 `https`，可配置开启 `http`。

禁止 `file://`、`ftp://`、`gopher://` 等远程输入协议。

### SR-002 SSRF 防护

默认阻止：

- loopback；
- localhost；
- RFC1918 私网；
- link-local；
- cloud metadata 地址；
- IPv6 loopback/link-local/private ranges。

### SR-003 Redirect 校验

每一次 redirect 后必须重新：

1. 校验协议；
2. DNS 解析；
3. 校验目标 IP；
4. 应用 redirect 次数限制。

### SR-004 资源限制

必须可配置：

- HTTP timeout；
- 最大响应体；
- 最大文档页数；
- 最大解析时长；
- 最大 redirect；
- 最大并发；
- Content-Type allowlist。

### SR-005 凭据隔离

模型不应直接传递明文 Authorization/Cookie。

使用：

```text
auth_profile
```

引用本地或外部 Secret Provider。

---

## 8. 非目标

首期明确不做：

- Web 搜索；
- 搜索引擎；
- 通用网页爬虫；
- 浏览器自动化；
- 网站登录和交互；
- 表单提交；
- AI 总结；
- AI 问答；
- AI 教学；
- 自动笔记；
- 自动出题；
- 通用知识库；
- 通用 RAG 平台。

原则：

> Reading MCP 是 Context Provider，不是 AI Application。

---

## 9. 格式范围

### MVP

- HTML
- Markdown
- Plain Text
- PDF

### Phase 2

- EPUB
- DOCX
- GitHub README/Wiki
- OpenAPI / Swagger
- Javadoc

### Phase 3

- JavaScript-heavy docs
- Confluence
- Notion
- 飞书
- 语雀
- 企业认证文档

---

## 10. 验收标准

MVP 完成必须至少证明：

1. 可以打开 HTML、Markdown、Text、PDF；
2. 能抽取标题和基本章节结构；
3. 能通过同一 `search_document` 搜索不同格式；
4. 能按 section/location 读取正文；
5. 搜索命中后可以展开上下文；
6. 返回结果包含来源定位；
7. 重复打开相同文档会命中缓存；
8. localhost / 私网 / metadata URL 默认被拒绝；
9. redirect 到私网地址仍会被拒绝；
10. MCP 不包含任何 LLM 总结或问答逻辑。
