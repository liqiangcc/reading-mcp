# Reading MCP 文档导航

- [需求文档](requirements.md)：项目目标、功能范围、安全要求、非目标和验收标准。
- [架构设计](architecture.md)：关注分离、领域模型、Retriever/Parser/Search/Cache 边界、稳定定位与 SSRF 设计。
- [MVP 实施计划](mvp.md)：从工程骨架到 Markdown/Text、搜索、HTML、PDF、安全缓存和真实 Agent 验证的阶段计划。

## 推荐阅读顺序

```text
requirements.md
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

实现过程中如果出现新的能力需求，优先判断它属于“文档上下文层”还是“上层 AI 能力”，避免破坏项目边界。
