# Reading MCP 文档导航

- [需求文档](requirements.md)：项目目标、当前功能范围、安全要求、非目标和验收标准。
- [设计原则](design-principles.md)：关注点分离（SoC）、单一职责（SRP）、变化原因矩阵、依赖方向、禁止耦合和架构评审清单。
- [架构设计](architecture.md)：领域模型、Retriever/Parser/Search/Cache 边界、稳定定位与 SSRF 设计。
- [Text Index & Source Locator Architecture](text-index-and-locator-design.md)：精确阅读的五级寻址、TextUnit/Locator、字符坐标、切分版本与 continuation 契约。
- [Normalized Document Identity and Text Range Contract](normalized-text-range-contract.md)：已实现的 normalized-document hash、normalization version、Section-relative Unicode-scalar range、坐标空间隔离和 Parsed Cache 版本约束。
- [Paragraph TextUnit Index Contract](paragraph-text-unit-index.md)：已实现的 block-aware Paragraph 确定性分段、stable TextUnit identity、source order、coverage 与 SQLite/InMemory derived TextUnitIndex。
- [Sentence Locator and Coverage Contract](sentence-locator-contract.md)：已实现的 deterministic Sentence locator、Paragraph ownership、technical-punctuation protection 与 native/coarse coverage。
- [TextUnit Enumeration Contract](text-unit-enumeration-contract.md)：已实现的 `get_text_units`、TextLocator、TextUnitCursor、Paragraph/Sentence source-order pagination 与 coverage/completion 语义。
- [Context Granularity Contract](context-granularity-contract.md)：已实现的 TextLocator-driven `neighbor / container / structural` context、stale validation、coarse source-preserving context 与 legacy Section compatibility。
- [Precise Read Locator Contract](precise-read-locator-contract.md)：已实现的 TextLocator → exact `read_document`、CharacterRange、exact-target ReadCursor continuation 与 returned source locator。
- [Search Locator Handoff Contract](search-locator-contract.md)：已实现的 SearchHit → TextLocator direct handoff 与 shared locator resolver。
- [Lexical TextUnit Index Contract](lexical-text-unit-index.md)：canonical Section/Paragraph/Sentence lexical candidates、`lexical-tokenizer/v1`、CJK/技术标识检索、SQLite lexical-index v3 migration/rebuild。
- [EPUB Navigation Map Contract](epub-navigation-map-contract.md)：已实现的 EPUB 3 `properties=nav` discovery、TOC/NCX hierarchy、href/fragment resolution 与 provenance parser facts。
- [EPUB Structure Reconciliation Contract](epub-structure-reconciliation-contract.md)：已实现的 nav/NCX/heading/spine precedence、spine-authoritative source order、`linear=no` 与 structural provenance。
- [Normalized Block Model Contract](normalized-block-model-contract.md)：已实现的 HTML/XHTML native body-block kinds、exact Section-relative ranges、EPUB remap、持久化/重启验证与 block-aware identity 输入。
- [EPUB Structure Validator Contract](epub-structure-validator-contract.md)：已实现的 persisted-fact integrity validation、error/degradation taxonomy、spine/navigation/structure/block/TextUnit coverage 与 SQLite reopen revalidation。
- [Block-Aware TextUnit Identity Migration](block-aware-text-unit-identity-migration.md)：已实现的 native-block-aware Paragraph/Sentence、`text-segmentation/v2`、`normalized-document-hash/v2`、stale 与 lexical v3 migration。
- [EPUB-First Structure Reliability Design](epub-structure-reliability-design.md)：EPUB 优先的目录/阅读顺序/章节/块结构可靠性、provenance、validator 与 coverage 设计。
- [Use-Case-First Tool Contract Design](tool-contract-use-case-design.md)：从 Actor/Goal 和阅读 Use Case 推导 Capability、状态机与 Tool Contract。
- [ADR 0002：Text Index、Locator Identity 与 Precise Reading](adr/0002-text-index-locator-identity.md)：规范化身份、TextLocator、ReadCursor、搜索候选与派生索引的稳定决策。
- [ADR 0003：EPUB-First Structure Reliability](adr/0003-epub-first-structure-reliability.md)：EPUB 结构优先级、provenance、degradation、validator 和 coverage 的稳定决策。
- [ADR 0004：Use-Case-First MCP Tool Contracts](adr/0004-use-case-first-tool-contracts.md)：从 6 Tool 推导出的第 7 个独立职责 `get_text_units`，以及 read/enumeration/context/search 的责任边界。
- [ADR 0005：Block-Aware TextUnit Identity Migration](adr/0005-block-aware-text-unit-identity.md)：已实现 block-aware segmentation/hash identity、旧 locator/cursor fail-closed、Parsed Cache v6 与 lexical-index/v3 rebuild 决策。
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
lexical-text-unit-index.md
      ↓
epub-navigation-map-contract.md
      ↓
epub-structure-reconciliation-contract.md
      ↓
normalized-block-model-contract.md
      ↓
epub-structure-validator-contract.md
      ↓
block-aware-text-unit-identity-migration.md
      ↓
epub-structure-reliability-design.md
      ↓
adr/0003-epub-first-structure-reliability.md
      ↓
adr/0004-use-case-first-tool-contracts.md
      ↓
adr/0005-block-aware-text-unit-identity.md
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

当前 runtime Tool surface 仍是 7 个：

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

当前 precise-reading / retrieval foundation：

```text
normalized-document-hash/v2 / normalized range
ReadCursor continuation
block-aware Paragraph TextUnit + Paragraph TextUnitIndex
Sentence locator + Paragraph ownership + native/coarse coverage
TextLocator + shared resolver
TextUnitCursor + source-order pagination
get_text_units Paragraph/Sentence enumeration
TextLocator → get_context
TextLocator → exact read_document
SearchHit → candidate_kind + TextLocator
canonical Section title lexical candidates
canonical Paragraph lexical candidates
canonical eligible Sentence lexical candidates
lexical-tokenizer/v1
CJK + mixed technical lexical projection
lexical-search-index/v3 SQLite rebuildable state
search → precise TextLocator → read/context direct handoff
EPUB navigation-map/v1 parser facts
EPUB structure-reconciliation/v1 canonical hierarchy
normalized-block-model/v1 persisted exact body-block ranges
epub-structure-validator/v1 integrity + factual coverage evidence
INVALID_LOCATOR / STALE_LOCATOR fail-closed validation
```

当前运行时身份、解析策略与检索版本继续分离：

```text
normalized-document-hash/v2 + text-segmentation/v2
+ normalized-block-model/v1 identity projection
→ 当前 Paragraph/Sentence/TextLocator identity

normalized-block-model/v1
→ persisted HTML/XHTML native block evidence
→ identity-bearing kind/range/order 进入 hash v2 与 segmentation v2

epub-structure-validator/v1
→ persisted-fact validation / coverage evidence
→ 不成为 source identity

reading-mcp-normalization/v6
→ Parsed Document policy/cache invalidation
→ v5 cache 不可复用，因为 EPUB validator 的 persisted TextUnit coverage 已切换到 segmentation v2

lexical-search-index/v3
→ precise lexical derived-state schema/version

lexical-tokenizer/v1
→ search projection / rebuild only
```

Block-aware v2 的 source-first 规则：

```text
native paragraph    → exact sentence-eligible Paragraph
native blockquote   → typed coarse Paragraph-level item, no Sentence
native list_item    → typed coarse Paragraph-level item, no Sentence
native preformatted → coarse Paragraph-level item, no Sentence
native table        → coarse Paragraph-level item, no Sentence

uncovered whitespace-only gap → separator coverage
uncovered non-whitespace gap  → deterministic fallback Paragraph segmentation
```

`blockquote/list_item` 之所以保持 coarse，不是因为它们必然不是 prose，而是 `normalized-block-model/v1` 使用 flat maximal projection：外层容器可能吞掉嵌套 `<p>/<pre>/<table>` 的 leaf boundary。没有持久化证据时不恢复、不猜测这些边界。

旧 `text-segmentation/v1` Paragraph/Sentence locator 必须 `STALE_LOCATOR`，旧 TextUnitCursor 必须 `STALE_CURSOR`；旧 normalized-hash-bound Section/CharacterRange locator 与 ReadCursor 也通过 normalized identity mismatch fail closed。不会因为文本仍匹配就静默重解释或 fuzzy rebase。

Persistent lexical state 从 `lexical-search-index/v2` 升级为 v3。旧 v2 derived rows 被丢弃并从 canonical persisted Document 重建；`lexical-tokenizer/v1` 保持不变，不重新下载/解析来源。

EPUB 当前已经完成 navigation extraction、canonical structure reconciliation、native body-block 持久化、persisted-fact validator 与 block-aware TextUnit materialization：

```text
manifest properties=nav
→ EPUB 3 toc nav
→ legacy NCX fallback
→ archive-safe href / fragment resolution
→ epub-navigation-map/v1
        ↓
spine-authoritative source order
+ nav/NCX hierarchy/labels
+ XHTML heading fallback
+ spine-item fallback
→ epub-structure-reconciliation/v1
→ canonical Section tree
        ↓
HTML/XHTML p / blockquote / li / pre / table
→ exact Section.content ranges
→ normalized-block-model/v1
        ↓
normalized-document-hash/v2 + text-segmentation/v2
→ block-aware Paragraph/Sentence identity
        ↓
persisted navigation / structure / block / current TextUnit evidence
→ epub-structure-validator/v1
→ integrity errors + readable degradations + factual coverage
```

Publisher navigation 只有在能映射到真实 canonical Section boundary 时才可升级 title/parentage；navigation 顺序不能反转 spine/source order。`linear=no` 作为 auxiliary content 仍保持可寻址，不被静默删除。

NormalizedBlock v1 不复制正文：每个 block 都是 owner Section 的 exact normalized range。Heading 继续由 canonical `Section.title/id/parent/level` 表示，因为 heading label 当前不属于 `Section.content`，所以不会伪造 heading body range。Block source order 是 parser/spine source order，独立于 reconciliation 后的 Section-tree DFS。

Validator 不重新打开 ZIP/DOM，只消费持久化事实。内部一致性冲突是 `error` 并使 EPUB parser fail closed；missing fragment、unsupported media、fallback 或能力覆盖不足属于 `degradation`，保留 readable Document 与明确 coverage。报告可随 Document 持久化并在 SQLite reopen 后得到相同 revalidation 结果。

当前 direct handoff：

```text
get_text_units ─→ TextLocator ─┬→ read_document
                               └→ get_context

search_document → SearchHit.text_locator ─┬→ read_document
                                         └→ get_context
```

SearchHit 可以真实返回：

```text
candidate_kind = section | paragraph | sentence
```

Section title candidates 始终保留；coarse structural/non-prose region 可以是 Paragraph candidate，但不会伪造 Sentence candidate。Legacy `location/search-unit` 只作为 preview/provenance，canonical identity 始终来自 `text_locator`。

Sentence persistence 仍不是正确性依赖：Sentence enumeration/context/read/search facts 都可以从 canonical persisted Document + deterministic segmentation 重建。只有真实性能证据出现时才增加 Sentence derived persistence。

当前 `get_text_units` 仍从 Section 边界起读；anchor-based `before/after(locator)` 是独立后续扩展。

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
