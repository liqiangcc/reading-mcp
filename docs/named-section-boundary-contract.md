# Named-section boundary contract

本文定义 Reading MCP 的 named-section structure-only 定位契约，以及 strict no-lookahead caller 应如何在正文 reveal 前执行 scope gate。

## 1. 职责分离

```text
get_document_structure
→ structural navigation / metadata-only named resolution

get_text_units / read_document
→ explicit canonical body reveal

search_document
→ lexical discovery with snippets
```

因此：

```text
boundary discovery != body reveal
structural navigation != lexical search
```

named-section resolution 不会隐式调用 `search_document`，也不会通过正文 snippet 猜 section boundary。

## 2. PDF structure provenance

PDF 结构证据按以下优先级建立 canonical `Section`：

```text
valid native PDF outline / TOC
→ native canonical Section hierarchy

otherwise conservative numbered-heading inference succeeds
→ inferred canonical heading Sections

otherwise
→ Page N fallback
```

推断只使用 deterministic parser text-line / numbering evidence，不使用 LLM、BM25、OCR 或文档特判。

如果证据不足，保持 Page fallback 并让 named-section resolution 返回 `unavailable`；false negative 优先于制造假结构。

## 3. Public API

named-section lookup 是 `get_document_structure` 的 additive mode，不增加新的 MCP Tool。

请求核心字段：

```text
document_id
named_section_query
expected_content_hash
expected_normalized_document_hash
expected_structure_resolution_version? = named-section-resolution/v1
```

例如：

```json
{
  "document_id": "doc:...",
  "named_section_query": "Section 1 Introduction",
  "expected_content_hash": "sha256:...",
  "expected_normalized_document_hash": "sha256:...",
  "expected_structure_resolution_version": "named-section-resolution/v1",
  "max_nodes": 32
}
```

`expected_content_hash` 与 `expected_normalized_document_hash` 来自 caller 已经接受的 `open_document` identity。named query 与 StructureCursor 不能混在同一次 continuation 请求中。

## 4. Resolution semantics

resolver 只匹配 canonical `Section` metadata：

```text
section_id
parent_id
title
level
location / section_path
body_order
```

不得读取或返回：

```text
Section.content
Paragraph / Sentence text
SearchIndex rows
lexical snippets
body previews
```

支持的 query 形式包括：

```text
1 Introduction
Section 1 Introduction
Introduction
```

匹配优先级：

```text
exact normalized full title
> optional Section-prefix full title
> title-only match after stripping one numeric designator
```

不做 fuzzy / synonym / search-score 匹配。

结果状态：

```text
resolved
ambiguous
not_found
unavailable
boundary_unavailable
```

`ambiguous` 返回 metadata-only candidates，不替 caller 猜一个结果。

## 5. Executable boundary

成功解析的 structural scope 是 matched Section + canonical descendants。

响应中的：

```text
named-section-boundary/v1
```

使用 `body-order/v1` 半开区间表示 scope owner：

```text
intervals = [start, end)
```

必要时可以有多个 interval；这对 EPUB 很重要，因为 publication/source body order 不一定等于 structure tree preorder。

caller 判断 prospective owner 是否可 reveal：

```text
next_owner.body_order ∈ any allowed interval
├─ yes → 可以调用 get_text_units / read_document
└─ no  → STOP before body reveal
```

`end_exclusive` 如果存在，只包含下一 body owner 的 structural metadata，不包含正文。

matched node 的 `start_locator` 复用 canonical Section-level `TextLocator`：

```text
document_id
content_hash
normalized_document_hash
owner_section_id
section_path
native_location
```

它没有正文 `normalized_range`，不会建立第二套文本 identity。

## 6. No-lookahead usage

```text
open_document
→ retain document_id + raw/normalized identity
→ get_document_structure(named_section_query = planned_scope)
→ require resolution.status = resolved
→ retain boundary.body_order_version + intervals
→ enumerate/reveal body only for allowed owner Sections
→ finish final allowed SourceUnit
→ inspect next canonical owner metadata
→ next owner is outside intervals
→ STOP
→ do not call get_text_units/read_document for that owner
```

Paper Reading Lab 的典型 gate：

```text
planned_scope = Section 1
→ resolve Section 1 structural boundary
→ reveal Section 1 owned TextUnits
→ next canonical owner = Section 2
→ Section 2 body_order outside boundary
→ STOP before Section 2 body reveal
```

## 7. Identity / stale behavior

当前 addressing-relevant normalization：

```text
reading-mcp-normalization/v7
normalized-document-hash/v2
text-segmentation/v2
```

PDF 从 Page-owned structure 迁移到可信 heading-owned structure 会改变 canonical `Section` facts，因此 normalized identity 会改变。

named-section request 中以下任一不匹配都会 fail closed：

```text
content_hash
normalized_document_hash
named-section-resolution version
```

MCP error code：

```text
STALE_STRUCTURE
```

不会按标题、ordinal、snippet 或相似文本自动 rebase。

SQLite canonical Document persistence 也绑定当前 normalization version。升级后，旧的未版本化或旧 normalization persisted Document 不会被新 runtime 静默解释成当前 canonical facts；caller 需要通过 `open_document` 使用当前 parser 重新建立 source facts。

## 8. Compatibility

历史的：

```text
get_document_structure(document_id, root_section_id?, max_depth?, max_nodes?, cursor?)
```

保持有效。named-section fields 是 additive contract。

已有 EPUB / HTML / Markdown / native-TOC PDF structure navigation 不应因为 named resolution 改变职责；`search_document`、`get_text_units`、`read_document` 的 body/lexical 语义保持显式分离。

## 9. Acceptance invariant

Issue #69 只有在以下 evidence 同时成立时才可 closure：

```text
named structural resolve
+ zero body/snippet leakage
+ executable body-order boundary
+ pre-reveal scope crossing proof
+ stale identity fail closed
+ real Raft 2014 source evidence
+ existing structure/read/search regression green
+ reviewed main SHA
+ formal release/package/deployment identity
+ production MCP acceptance
```

之前因 lexical boundary preflight 泄露 future Source 而 contaminated 的 Raft ReadingSession 仍然是 abandoned。该历史失败不会被本能力整改重新解释为成功；strict Raft Session 必须在完成部署后的 fresh conversation 重新开始。
