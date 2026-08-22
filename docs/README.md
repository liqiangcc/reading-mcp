# Reading MCP 文档导航

- [需求文档](requirements.md)：项目目标、当前功能范围、安全要求、非目标和验收标准。
- [设计原则](design-principles.md)：关注点分离（SoC）、单一职责（SRP）、变化原因矩阵、依赖方向、禁止耦合和架构评审清单。
- [架构设计](architecture.md)：领域模型、Retriever/Parser/Search/Cache 边界、稳定定位与 SSRF 设计。
- [Text Index & Source Locator Architecture](text-index-and-locator-design.md)：精确阅读的五级寻址、TextUnit/Locator、字符坐标、切分版本与 continuation 契约。
- [Normalized Document Identity and Text Range Contract](normalized-text-range-contract.md)：已实现的 normalized-document hash、normalization version、Section-relative Unicode-scalar range、坐标空间隔离和 Parsed Cache 版本约束。
- [Paragraph TextUnit Index Contract](paragraph-text-unit-index.md)：已实现的 Paragraph v1 确定性分段、stable TextUnit identity、source order、coverage 与 SQLite/InMemory derived TextUnitIndex。
- [Sentence Locator and Coverage Contract](sentence-locator-contract.md)：已实现的 deterministic Sentence locator foundation、Paragraph ownership、technical-punctuation protection 与 non-prose coarse coverage。
- [TextUnit Enumeration Contract](text-unit-enumeration-contract.md)：已实现的 `get_text_units`、TextLocator、TextUnitCursor、Paragraph/Sentence source-order pagination 与 coverage/completion 语义。
- [Context Granularity Contract](context-granularity-contract.md)：已实现的 TextLocator-driven `neighbor / container / structural` context、stale validation、non-prose coarse context 与 legacy Section compatibility。
- [Precise Read Locator Contract](precise-read-locator-contract.md)：已实现的 TextLocator → exact `read_document`、CharacterRange、exact-target ReadCursor continuation 与 returned source locator。
- [Search Locator Handoff Contract](search-locator-contract.md)：已实现的 SearchHit → canonical Section TextLocator direct handoff、shared locator resolver，以及当前 SearchIndex 精度边界。
- [EPUB-First Structure Reliability Design](epub-structure-reliability-design.md)：EPUB 优先的目录/阅读顺序/章节/块结构可靠性、provenance、validator 与 coverage 设计。
- [Use-Case-First Tool Contract Design](tool-contract-use-case-design.md)：从 Actor/Goal 和阅读 Use Case 推导 Capability、状态机与最终 Tool Contract；包含逐句枚举、SearchHit handoff、stale、non-prose 和 reliability/coverage。
- [ADR 0002：Text Index、Locator Identity 与 Precise Reading](adr/0002-text-index-locator-identity.md)：规范化身份、TextLocator、ReadCursor、搜索候选与派生索引的稳定决策。
- [ADR 0003：EPUB-First Structure Reliability](adr/0003-epub-first-structure-reliability.md)：EPUB 结构优先级、provenance、degradation、validator 和 coverage 的稳定决策。
- [ADR 0004：Use-Case-First MCP Tool Contracts](adr/0004-use-case-first-tool-contracts.md)：从 6 Tool 推导出的第 7 个独立职责 `get_text_units`，以及 read/enumeration/context/search 的责任边界。
- [MVP 实施计划](mvp.md)：从工程骨架到 Markdown/Text、搜索、HTML、PDF、安全缓存和真实 Agent 验证的阶段计划。
- [Phase 5：HTTP、安全与缓存](phase5-security-cache.md)：HTTP Retriever、SSRF/DNS/redirect 安全证据链和缓存边界。
- [Phase 6：MCP stdio 与真实调用验证](phase6-mcp-stdio.md)：真实 `reading-mcp` binary、当前 7 个 Tool 和 stdio 子进程端到端测试。
- [MVP Hardening Review](mvp-review.md)：发布前架构、安全、契约和真实使用 Review。
- [Runtime Configuration](runtime-configuration.md)：持久化状态、资源预算、HTTP、auth profile、telemetry 和错误语义配置。
- [Release Hardening Plan](release-hardening-plan.md)：v0.1.0 前的 hardening 完成矩阵、扩展格式和 Release Gate。

## 推荐阅读顺序

```text
requirements.md
      ↓
design-principles.md
      ↓
architecture.md
      ↓
text-index-and-locator-design.md
      ↓
tool-contract-use-case-design.md
      ↓
adr/0002-text-index-locator-identity.md
      ↓
normalized-text-range-contract.md
      ↓
paragraph-text-unit-index.md
      ↓
sentence-locator-contract.md
      ↓
text-unit-enumeration-contract.md
      ↓
context-granularity-contract.md
      ↓
precise-read-locator-contract.md
      ↓
search-locator-contract.md
      ↓
epub-structure-reliability-design.md
      ↓
adr/0003-epub-first-structure-reliability.md
      ↓
adr/0004-use-case-first-tool-contracts.md
      ↓
mvp.md
      ↓
phase5-security-cache.md
      ↓
phase6-mcp-stdio.md
      ↓
mvp-review.md
      ↓
runtime-configuration.md
      ↓
release-hardening-plan.md
```

## 核心共识

```text
Reading MCP = 文档上下文基础设施

负责：获取 / 解析 / 结构化 / 搜索 / 定位 / 读取 / 引用 / 缓存
不负责：总结 / 问答 / 推理 / 教学 / 通用 Web 搜索 / 通用 RAG
```

当前推荐部署定位：

```text
单用户
本地 stdio
公共 HTTPS
本地文件 default-deny
显式授权 local roots
默认持久化状态
```

当前 runtime Tool surface：

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

当前 precise-reading foundation 已实现：

```text
normalized_document_hash / normalized range
ReadCursor continuation
Paragraph TextUnit + Paragraph TextUnitIndex
Sentence locator + Paragraph ownership + non-prose coverage
TextLocator output
TextUnitCursor + source-order pagination
get_text_units Paragraph/Sentence enumeration
shared TextLocator resolver
TextLocator → get_context
neighbor / container / structural tagged context
TextLocator → exact read_document
Section / Paragraph / Sentence / CharacterRange exact targets
exact-target ReadCursor + returned source-range locator
SearchHit → candidate_kind + TextLocator
search → TextLocator → read/context direct handoff
INVALID_LOCATOR / STALE_LOCATOR fail-closed validation
```

Sentence persistence 仍未实现，也不是当前正确性的依赖：`get_text_units`、precise context 与 precise read 都以 canonical persisted Document 和 deterministic TextUnit facts 为事实基础。后续只有在实际性能证据需要时才增加 Sentence derived persistence。

当前 direct handoff：

```text
get_text_units ─→ TextLocator ─┬→ read_document
                               └→ get_context

search_document → SearchHit.text_locator ─┬→ read_document
                                         └→ get_context
```

当前 SearchIndex 的 paragraph-like retrieval row 并不等于 canonical Paragraph TextUnit：其 legacy split/location 不携带 normalized range + segmentation identity。因此当前 SearchHit 只诚实返回 `candidate_kind=section` 和 owning Section TextLocator。下一依赖步骤是 `feat/lexical-text-unit-index`；只有新的 lexical index 真正保存/证明 canonical Paragraph/Sentence locator facts 后，搜索才允许升级 candidate kind。

当前 `get_text_units` v1 仍从 Section 边界起读；anchor-based `before/after(locator)` 是独立后续扩展。

格式能力分为两类：

```text
独立格式 Parser
├── Text
├── Markdown
├── HTML
├── PDF
├── EPUB
├── DOCX
└── OpenAPI / Swagger JSON/YAML

复用现有格式
├── GitHub README / Wiki → Markdown / HTML
├── Javadoc             → HTML
└── MkDocs / Docusaurus / GitBook static output → HTML
```

不为站点品牌创建重复 Parser；只有文档格式产生新的解析职责。

实现过程中如果出现新的能力需求，必须先判断：

1. Actor 需要完成的阅读目标和 Use Case 是什么？
2. 成功、失败和降级如何判定？
3. 它属于哪个独立 Capability / 变化原因？
4. 现有 Tool 是否能自然表达，还是会造成语义混淆、参数组合爆炸或多余 round-trip？
5. 是否破坏来源、格式、索引、读取、协议和 AI 能力之间的边界？

如果一个需求导致多个原本正交的模块同时修改，或仅为了保持 Tool 数量而把多个状态机塞入一个 Tool，应先检查关注点分离是否失效，而不是直接扩展实现。
