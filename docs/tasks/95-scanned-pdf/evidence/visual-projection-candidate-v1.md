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
