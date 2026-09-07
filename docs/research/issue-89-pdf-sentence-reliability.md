# Issue #89：PDF sentence reliability 预研报告

状态：`[RESEARCH COMPLETE — READY FOR REVIEW]`（research-only；不包含生产修复）。

## 基线与边界

本研究固定使用已部署 v0.3.0：source SHA
`1cfbc4ca032a8bab7b59134a9b03c2526a34b7f4`，normalization
`reading-mcp-normalization/v8`。#87 原始 PDF 只保留 URL、raw SHA
`48c6ab4b1e17360ab32be09d0042b43630d3afcd8da7d453c17042a959ab6061`、
document/normalized hashes 和人工 annotation；论文正文没有进入仓库，也不再分发。
来源为外部 URL，后续实现只应在受许可/用户提供的本地输入上运行。

所有命令在 `/tmp` 隔离 worktree 运行；MCP probe 使用独立 state/root。没有修改、重启、部署 production service，也没有写 `/root/.reading-mcp`。

正式阈值在 `research/issue89/fixtures/scoring-thresholds.json` 中冻结于评分前。原始 probe JSON 仅为 harness debugging；正式结果为 `research/issue89/results/scored-summary.json`。

## 故障归因链

```text
PDF extraction → reading order/layout → normalization
             → paragraph/block → sentence boundary
             → SourceUnit/locator → MCP transport
```

对 v0.3.0 源码核对并以 #87 实测交叉验证：`pdf.rs` canonical page text 来自 lopdf high-level `extract_text(_chunks)_with_limit`，`normalize_pdf_text` 只有 trim；`pdf_v8.rs` 只有在 base parser provenance 为 `inferred_numbered_headings` 时才进入 low-level layout evidence/Abstract split。#87 是 `page_fallback`，所以没有走该 layout evidence。`pdf_layout.rs` 对 TJ 数组只 decode/concat 文本，忽略 numeric displacement/kerning，且 text-show 后没有按 glyph advance 更新 x；它不是完整 body reading-order/word-spacing reconstruction。

复现结果将责任层收敛到 extraction/layout/line/paragraph reconstruction 及其 provenance gate：v8 把 front matter 与 Abstract body 放进首个 sentence/paragraph fallback。MCP live stdio transport 实测忠实返回该结果：10 sections、4 sentence items、首项 513 chars、`degradation=null`，用同一 locator read 回来的 513 chars 与首项完全相等。因此 transport 不是根因；exact read equality 也不等于语义边界正确。

## 数据集与人工边界

仓库内 fixture 为可分发的 synthetic PDF：single-column、double-column、cross-page、hyphenated、space-loss、punctuation-identifiers；另有 EPUB regression control（native paragraph）。annotation manifest 记录 class、gold count、标签和 validation subset。synthetic 的 Abstract 边界用 range/hash 对照，正文 fixture 不含外部论文内容。

#87 的人工 gold 使用 `pdf-extract 0.12.0` 加确定性 soft-wrap/dehyphen projection：projection 长度 821，SHA
`3fc5d020d5c014b3601cbb2d554ecf2e3b73cfa7618c18d15a87dc43475a4d67`，7 个 sentence range/hash 详见 annotation manifest。该坐标是实验 projection coordinate，不是 canonical/native source coordinate，不能直接作为生产 locator；source map 是必需实现项。

## 冻结指标与结果

阈值为：overall boundary P/R 各至少 0.99；#87 high-confidence wrong merge/split、metadata contamination、omission、duplication 均为 0；exact normalized read equality 为 1.0；determinism 为 3/3 exact hash；supported prose sentence-readable coverage 至少 0.95；所有非精确区域必须显式 degradation。句数相等不构成边界通过条件。

| 方法 | #87 结果 | 结论 |
|---|---|---|
| deployed v8 baseline | P/R 0；wrong merge 1；metadata contamination 1；body coverage 0；MCP exact read 1 | FAIL |
| low-level lopdf prototype | debug snapshot 曾为 7 句但 body 1789 chars；当前 v2 为 16 句；均不匹配 gold 821/projected ranges | FAIL |
| pdf-extract raw + syntok | 21 physical-line segments 对 7 gold；P≈0.333、R=1、wrong split=14 | FAIL |
| pdf-extract + projection + syntok | projection 上 7/7，P/R=1、wrong merge/split=0、metadata/omission/duplication=0、3/3 deterministic | partial proof；full acceptance FAIL |

独立 `pdf-extract 0.12.0` 实测输出 49,963 chars，SHA
`4624c5978fa10017bd229dcbdb18f674cc5dfb9e2c9e96fb7c40163019a9f24f`，重复运行 hash 相同；输出仍含物理行双换行和断词，说明 extraction 优于 v8 但不是 sentence-ready。syntok 使用官方 HEAD
`371e6ca0d4e307271eaf439f912fad94efdaa8e9` 与 Ubuntu `python3-regex` 隔离运行；保护缩写/小数/技术标识符示例正确分 3 句。#87 raw 输入产生 21 个伪句；投影后 3 次均为 7 句，输出 SHA
`51a4348f12c89a821db6f097857bb928dd0d23bc9a5961d2c99905cce2bb80f6`。这证明顺序应为“先 projection，再 splitter”，不证明 source map 已完成。

synthetic 的 v2 layout probe 在 Abstract 范围上通过单栏、双栏、跨页、断词、空格丢失、标点/标识符样例的已标注 count/range smoke gate；其 coverage 仅代表 fixture scope，不能替代 #87 external gold。EPUB control 保留为非 PDF regression control。

## 候选比较与推荐

推荐候选是 source-preserving structural projection：从 low-level glyph/text fragment evidence 恢复 page/column/line/paragraph，采用明确 whitespace、soft-wrap、断词策略，再交给成熟的离线 deterministic splitter（syntok 或等价实现）。每次 join/normalize 必须保存 native fragment → normalized range map，并将 map/version 纳入 cache/index identity。

`pdf-extract 0.12.0` 是已实测的成熟 external text extractor，成本低、可离线、重复输入稳定；syntok 是已实测的成熟 deterministic splitter，但不能修复错误 paragraph。GROBID（结构/布局候选）和 Poppler（CLI text/layout comparator）本轮未执行，不能把静态能力描述写成实测结果；它们的服务/系统包版本会增加 identity、部署和维护成本。模型辅助方案不作为生产候选：模型权重/提示词/服务身份和 cache invalidation 难以 fail closed，且可能引入不可复现边界。

## fallback、版本与 locator 策略

若 glyph/font/layout evidence 不足、column/line confidence 不足、source map 不完整，必须显式 degradation 并退回 page/paragraph；不得把 page fallback 宣称为 sentence success。建议结构 projection 与 sentence segmentation 分别 bump（例如 normalization v9、segmentation v3），并把 extractor/layout policy、splitter identity、source-map schema 纳入 cache key。版本变化或 map 缺失时 cache/index 必须失效并要求 explicit source reopen；旧 locator/cursor 对新 normalized coordinate 一律返回 STALE/fail-closed，禁止 fuzzy/automatic reinterpret。

## 限制与实施门禁

本轮 lopdf candidate 是研究 prototype，不是生产实现；其 #87 1789/821 mismatch 明确说明“7 句”不能作为成功。projection 改变 normalized text，缺少 native map 时即使 projection 上 P/R=1 也只能 partial proof。GROBID、Poppler 未执行；外部论文不入库；synthetic fixture 规模小。

后续 implementation Issue 必须以 #87、上述 projection range/hash metadata、synthetic corpus、EPUB control 为 regression gates，并要求 native source map、exact locator read equality、determinism 3/3、显式 degradation 和所有冻结阈值全绿后才可考虑 production。#87/#88 保持 open。

## 本地验证记录

research harness（含 synthetic、#87 raw extraction、隔离 MCP 以及三次 exact-hash determinism）通过；`cargo fmt --all -- --check`、`git diff --check` 和仓库 `cargo clippy --locked --all-targets --all-features -- -D warnings` 通过。仓库 `cargo test --locked --all-features` 未完成：并行链接阶段 `rust-lld/collect2` 以 `SIGBUS` 失败；清理隔离 worktree 可再生 target 后，`-j1` 重跑又因文件系统耗尽（约 313 MB 可用）无法进入测试执行。该环境失败不会被写成候选通过，CI 是后续代码门禁的 authoritative gate。
