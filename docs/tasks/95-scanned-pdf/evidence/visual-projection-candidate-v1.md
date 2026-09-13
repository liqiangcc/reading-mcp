# F11：保留来源的分类投影候选

本包新增 worker 中的纯几何 helper，并让固定本地模型的 hosted 诊断实际调用该 helper 与现有 project。**尚未在 main/runtime 启用**：先检验真实 canonical/non-prose 输出，后续仍须完整 typed 模型证据、配置/实物 fingerprint、缓存和 Rust 发布接线，不将此包宣称为已完成 F11。

输入仅包含实际 page/raster 变换、完整已选 OCR boxes 与模型的固定阈值观察。图像/公式/table 仅在一个模型区域覆盖 box 所有 word 中心时赋予非正文类别；部分或多重匹配明确失败。投影 bbox 为真实模型框与 OCR 原框的并集，保留两套原始坐标与索引，不裁掉模型框边缘外的真实 OCR 字符。原始 words、confidence、block/par/line、输入 boxes/model 数组和引擎先后次序不变。无可绑定文字的模型对象保留为 unanchored，不能声称完整或制造占位正文。

hosted 候选使用既有 primary + 有界区域 retry，不再将 primary-only raw 分数冒充最终 canonical。模型全部 raw outputs、regional primary/retry、视觉选择、实际 canonical 块均留报告；识别及投影结束后才读冻结 gold，按既有 NFC/空白规则记录 CER/English WER。F08 沿用 Latin-only actual 段与 mixed ambiguity 规则，F07 无 English WER。F11 stdout 显示每个实际块的文本/kind/原页region。候选文本门槛不是最终区域保留100%或 Rust/MCP发布验收；原正式 quality gate 不变。

资源诊断仍保留768MiB cgroup及原固定依赖。此前并驻模型峰值接近上限，本轮如果实测超限会明确失败，不能扩大阈值；后续按已提出的短生命周期分类子进程拆开驻留。

单元测试覆盖原始观察不变、原序正文/粗块输出、并集保留边缘词、部分/多重覆盖拒绝、无文字对象不伪造、非法模型框/score。执行验证全部在 GitHub-hosted Actions，本机仅 py_compile/fmt。

## c633d1f 原始结果与段落缺口

固定 run34752668168/artifact10315867721 的完整 layout-model-report.json 已实际下载；SHA256 为 c8e93825fc2d807cd111996a921bf27d5f3d85a26e0a146ffd7c83647a516853。八个样本文字指标均在原门槛内；F11 CER0/548、WER0/91，实际却有7段prose和两个preformatted（真实 `12` 与 `x?+y?=2?`）。图表/公式不再进入prose，但一段被Tesseract拆为两个连续片段，不能以CER0宣布段落正确。最大cgroup累计峰值763998208bytes；并驻诊断不是最终服务预算验收。

追加的候选修复只合并连续原序片段，并同时要求：两个片段全部word中心均唯一归属于同一真实模型text区域、前文未结束、后文为小写续文或CJK连续字符、行间距及左沿偏移不超过实际line高度中位数。不使用gold、固定行数或特定词；完整句/标题、远距离、没有模型支持或模型重叠都不合并。保留原始line/spans的全部block/par/line ID，另记source groups和paragraph_merges。

候选评分增加既有独立对齐的paragraph boundary F1及非单调paragraph order计算，仍在识别完成后读取gold，避免文字分数隐藏多段/漏段。sentence与真正Rust发布仍待接线，不伪造sentence范围。原c633报告不改写。

## 6187a60 已下载结果

准确 head `6187a60a8a9d9be5326adcead22bb645d1803013`，
[hosted run 34753118571](https://github.com/liqiangcc/reading-mcp/actions/runs/34753118571)，
artifact `10316113862`。实际下载的完整 `layout-model-report.json` SHA256：
`bf224d25622cb7d33c5a6686a568925319fd1bd6a6b3719d0b970686ce3b6e9f`。
以下为原始报告的人工摘要，不伪称完整报告字节。

| 样本 | CER errors/denominator | English WER errors/denominator | 正文段数 |
| --- | --- | --- | --- |
| F02 | 0/548 | 0/91 | 6 |
| F06 | 0/548 | 0/91 | 6 |
| F07 | 1/110 | not applicable | 4 |
| F08 | 0/217 | 0/26 | 4 |
| F11 | 0/548 | 0/91 | 6 |
| F12 | 0/548 | 0/91 | 6 |
| F13 | 0/81 | 0/12 | 1 |
| F14 | 1/2102 | 1/384 | 1 |

八个样本 paragraph boundary F1=1、独立正文 order score=1，F08 无 mixed
段。F11 前次七段缺口已在候选闭合。该 head 九项 checks 全部 success（Rust
34753119964/34753118566、真实 OCR 34753119943/34753118576、离线包
34753119953/34753118560、fixture 34753119945、raft 34753119954、模型上述
run）。正文顺序分数不证明粗块与正文之间的完整交错顺序，也不证明尚未接入
main 的模型身份、typed evidence、Rust 原子发布或生产部署。

日志修复只停止逐 case 输出巨大的 raw tensor/word 全量行，保留所有八个
有界摘要以及 F11 canonical blocks。完整 raw 观察继续保存在各 case JSON
及汇总 artifact，未删减评分、原始证据或正式门槛。

## 完整粗块位置（待 hosted）

design.md 第7节明确 F11 的 unsupported regions 在两栏正文之后。此前
正文 order=1 不覆盖此要求；6187 实际粗块仍插在左右栏之间。新增有界规则
只对模型已绑定且真实框完全低于所有正文的对象作末尾移动，保留正文顺序、
粗块之间原引擎顺序及完整 input/output source-group permutation。不作全页
x/y 排序，遇到粗块之间逆序/重叠或需越过其他未证明对象时明确失败。

纯几何测试使用非 fixture 坐标覆盖双栏、正文不可按 y 排序、原观察不变、
部分纵向重叠不得移到末尾，以及粗块逆序拒绝。实际 hosted 诊断在识别结束
后才读取 gold，另报两个对象的原页/class/中心对应及是否在全部正文之后。
此 coarse-block 评分不是 Rust Sentence/原页回看验收，也不要求覆盖 gold
矩形内全部空白。默认运行时尚未启用模型；后续身份及发布接线必须绑定本
投影策略和原始 model/retry/source-group 证据。
