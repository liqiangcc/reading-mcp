# ADR-0001：实现语言与核心边界

- Status: Accepted
- Date: 2026-08-07

## Context

Reading MCP 的 Phase 0 目标不是尽快支持 PDF/HTML，而是先把领域模型、用例、端口和 MCP Contract 固定下来，并验证关注点分离（SoC）和单一职责（SRP）可以在代码结构中成立。

核心约束：

```text
MCP protocol adapter
≠
application orchestration
≠
document domain
≠
retrieval
≠
parsing
≠
security policy
≠
storage/cache/index
```

## Decision

Reading MCP 核心实现采用 Rust（edition 2024）。

Phase 0 只依赖通用序列化/Schema/错误处理库，不直接绑定 MCP SDK、HTTP Client、PDF Parser、HTML Parser、SQLite 或搜索引擎。

MCP Adapter 后续优先采用官方 Rust MCP SDK `rmcp`，但 `rmcp` 只能存在于协议适配边界，不能进入 `domain` 与 `application`。

## Why Rust

1. 适合构建单二进制本地工具，部署和 MCP 客户端配置简单；
2. 类型系统适合表达稳定的 Domain Model、Port 和错误边界；
3. trait 可以自然表达 Retriever / Parser / Repository / SearchIndex / Policy 等抽象端口；
4. 可以把网络、解析、索引等高风险依赖隔离在 adapter/infrastructure；
5. 官方 MCP Rust SDK 已提供 server、stdio、Streamable HTTP 等能力，后续不需要自行实现协议栈。

## Why not bind rmcp in Phase 0

MCP SDK 是协议适配关注点，不应决定核心模型和用例。

因此 Phase 0 先定义 SDK-independent DTO / Contract：

```text
MCP JSON request
      ↓
contract DTO
      ↓
adapter mapping
      ↓
application use case
```

后续即使 MCP SDK API 变化，只修改 adapter。

## Consequences

### Positive

- Domain/Application 可以脱离 MCP SDK 独立测试；
- 可以使用 Fake Retriever / Fake Parser 验证完整用例；
- HTTP/PDF/SQLite 等库的选择不会污染核心；
- 更容易检查 SRP：某一技术依赖变化时，只应影响一个边界。

### Trade-offs

- Rust 文档解析生态不一定在所有格式上都比 Node/Python 丰富；
- 某些复杂 PDF/OCR 能力未来可能需要调用外部库或 sidecar；
- Phase 0 会多一层 DTO → Application Model 映射。

这些代价可以接受，因为 Reading MCP 的主要长期风险是边界腐化，而不是第一版解析器代码量。

## Revisit conditions

只有出现以下证据时才重新评估语言：

- 核心格式的解析能力在 Rust 中长期无法达到最低可用质量；
- 关键依赖无法安全分发；
- 性能/兼容性问题无法通过 adapter/sidecar 解决。

不能因为“某个格式在另一语言里有更方便的库”就直接推翻核心语言选择。
