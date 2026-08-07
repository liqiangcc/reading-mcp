# Reading MCP 文档导航

- [需求文档](requirements.md)：项目目标、功能范围、安全要求、非目标和验收标准。
- [设计原则](design-principles.md)：关注点分离（SoC）、单一职责（SRP）、变化原因矩阵、依赖方向、禁止耦合和架构评审清单。
- [架构设计](architecture.md)：领域模型、Retriever/Parser/Search/Cache 边界、稳定定位与 SSRF 设计。
- [MVP 实施计划](mvp.md)：从工程骨架到 Markdown/Text、搜索、HTML、PDF、安全缓存和真实 Agent 验证的阶段计划。

## 推荐阅读顺序

```text
requirements.md
      ↓
design-principles.md
      ↓
architecture.md
      ↓
mvp.md
      ↓
开始实现
```

## 核心共识

```text
Reading MCP = 文档上下文基础设施

负责：获取 / 解析 / 结构化 / 搜索 / 定位 / 读取 / 引用 / 缓存
不负责：总结 / 问答 / 推理 / 教学 / 通用 Web 搜索 / 通用 RAG
```

实现过程中如果出现新的能力需求，必须先判断：

1. 它属于哪个关注点？
2. 哪个模块应该因它而变化？
3. 是否引入了新的独立变化原因？
4. 是否破坏了来源、格式、索引、读取、协议和 AI 能力之间的边界？

如果一个需求导致多个原本正交的模块同时修改，应先检查关注点分离是否失效，而不是直接扩展实现。
