# #95 layout prerequisite 审查交接

设计批准：`76f449061b20b6c8d79b57b871ce67ac1cfa7c6b`，PR #97 已 match-head
合并，main 基线 `525c02f9745ab6163ba7f93d26b5d676b098dd44`。
设计精确 head 的 hosted CI：
[34675744827](https://github.com/liqiangcc/reading-mcp/actions/runs/34675744827)、
[34675747627](https://github.com/liqiangcc/reading-mcp/actions/runs/34675747627) 均成功。

本 PR 按文件整合 production `1569c68d6220d129beb1f3218cd22d57f26f331c`
相对旧 main `de4306a3b486a8aee69cf159dcf2c1f545d1e732` 的 layout 前置差异。
没有 cherry-pick 整个候选分支，没有纳入 PR96 的 OCR-only 改动。

| 文件组 | 审查目的 |
| --- | --- |
| domain / identity tests | PdfLayout provenance、normalization v9、segmentation v3、既有 etc. 回归 |
| parsing / runtime / cache | opt-in 路由、固定 worker 协议、scalar/page bindings、独立 PDF cache namespace |
| source_view worker / renderer | 原始 PDF 页渲染与预算，不从解析文本生成页面 |
| Python fixtures / hosted CI | 保留原投影测试，实际配置 pinned Python；新增 real native-PDF worker 调用 |
| package / runtime docs | 保留必要依赖资产，重新说明主线状态；不带旧 RC 版本号或历史发布成功声明 |

`src/mcp/contracts.rs`、#92 anchor tests 和完成语义文档保持 main 原样；
`src/mcp/server.rs` 不带旧候选版本号。Cargo 0.3.0/Cargo.lock 不变，只有 tokio
process feature 恢复线上所需配置。既有 #92 与完整 Rust suite 由当前 hosted CI 检验。

新 head/CI 链接在 PR 中报告。没有本机编译/测试、OCR 实现、安装依赖或服务变更。
此 PR 交 Coordinator 审查，不自行合并、发布或部署；最终 #95 发布再测实际生产。
