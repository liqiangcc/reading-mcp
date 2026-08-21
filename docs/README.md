# Reading MCP 文档导航

- [需求文档](requirements.md)：项目目标、当前功能范围、安全要求、非目标和验收标准。
- [设计原则](design-principles.md)：关注点分离（SoC）、单一职责（SRP）、变化原因矩阵、依赖方向、禁止耦合和架构评审清单。
- [架构设计](architecture.md)：领域模型、Retriever/Parser/Search/Cache 边界、稳定定位与 SSRF 设计。
- [Text Index & Source Locator Architecture](text-index-and-locator-design.md)：精确阅读的五级寻址、TextUnit/Locator、字符坐标、切分版本与 continuation 契约。
- [EPUB-First Structure Reliability Design](epub-structure-reliability-design.md)：EPUB 优先的目录/阅读顺序/章节/块结构可靠性、provenance、validator 与 coverage 设计。
- [Use-Case-First Tool Contract Design](tool-contract-use-case-design.md)：从 Actor/Goal 和阅读 Use Case 推导 Capability、状态机与最终 Tool Contract；包含逐句枚举、SearchHit handoff、stale、non-prose 和 reliability/coverage。
- [ADR 0002：Text Index、Locator Identity 与 Precise Reading](adr/0002-text-index-locator-identity.md)：规范化身份、TextLocator、ReadCursor、搜索候选与派生索引的稳定决策。
- [ADR 0003：EPUB-First Structure Reliability](adr/0003-epub-first-structure-reliability.md)：EPUB 结构优先级、provenance、degradation、validator 和 coverage 的稳定决策。
- [ADR 0004：Use-Case-First MCP Tool Contracts](adr/0004-use-case-first-tool-contracts.md)：当前 6 Tool 与未来 7 Tool 的边界、`get_text_units` 决策及 additive contract evolution。
- [MVP 实施计划](mvp.md)：从工程骨架到 Markdown/Text、搜索、HTML、PDF、安全缓存和真实 Agent 验证的阶段计划。
- [Phase 5：HTTP、安全与缓存](phase5-security-cache.md)：HTTP Retriever、SSRF/DNS/redirect 安全证据链和缓存边界。
- [Phase 6：MCP stdio 与真实调用验证](phase6-mcp-stdio.md)：真实 `reading-mcp` binary、当前 6 个 Tool 和 stdio 子进程端到端测试。
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
epub-structure-reliability-design.md
      ↓
tool-contract-use-case-design.md
      ↓
adr/0002-text-index-locator-identity.md
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
search_document
get_context
read_document
```

Use-Case-First 设计接受的未来 surface 在底层 TextUnit/locator/cursor invariant 实现后增加一个通用 `get_text_units`。当前文档必须明确区分“已实现 6 Tool”和“已接受但尚未实现的未来第 7 Tool”。

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
