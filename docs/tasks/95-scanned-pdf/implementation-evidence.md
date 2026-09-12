# #95 实现证据（进行中，未发布/部署）

Coordinator 在 [5644066400](https://github.com/liqiangcc/reading-mcp/issues/95#issuecomment-5644066400)
冻结 fixture commit `7b68a6e7b1a518ef7276f8d3a0ac006639121117`，manifest SHA256
`4e3b1dd9d84a7a401e4f2d5c773b510118f5bba3c5aa7fe8f112704827a17ff2`。
仅 YAML 修复 head `1f949cdbd9d5c9444d40bf6a0d9831818419fdf6` 的
[hosted integrity / 再生成比对](https://github.com/liqiangcc/reading-mcp/actions/runs/34677149290)
成功：15 变体、18 页、70 段、139 个 gold 自然句；F14 一句跨 27 行。
这些数字是 fixture 完整性，不是 OCR 准确率。

已实际读取批准评论，合并 `ca921e0fd6f637b8c6c52659042c5a605390baa4` layout
main 到实现分支，保留原 fixture commit；冻结目录与该 commit 无差异。

## 首轮真实引擎先行

按 Coordinator 建议，先用 hosted Tesseract 5.3.4 fast 对 F02/F06/F07/F08/F11/F14
跑固定 OEM 1、PSM 3、300 DPI、单线程。`first_engine_probe.py` 只负责识别诊断和
评分，不是 MCP ingestion adapter；原始 TSV、word bbox/confidence、CER/WER、
顺序、段边界诊断、耗时、RSS 及实际依赖 hash 存公开 synthetic Actions artifact。
gold 在整页识别完成后才读取，只作评分范围关联，不控制裁剪/输入或提供识别答案。

同时报告未过滤的全引擎输出与 gold-region prose 指标，后者不冒充生产 adapter
支持范围。中文不计算伪空格 WER；英文部分单独 WER。单句边界 F1 待后续 canonical
MCP 集成，不由 OCR 段落数量推断。引擎高置信度不等于准确。

诊断允许单次 60s 以记录超时限数据，但单独按批准的 15s/page 标记 pass/fail；
不是放宽生产预算。每样本启动新 engine，未清 OS 文件缓存，不称 connector 冷开。
阈值失败要报告原始数据，不能修改 fixture/gold/阈值来迎合引擎。

其余 ingestion/identity/cache/监督/MCP 故障门禁尚未实现或验收；本轮先取得真实
引擎指标，再按最小 Port / atomic Document upsert / normalized hash 身份边界完成。
无本机编译测试、私有上传、云 OCR 或生产变更。

## 首轮评分解释修订（保留原始结果）

首轮 head `35653e2363b2cc03707d5dcc0ef94f45832039b0` 的
[hosted probe](https://github.com/liqiangcc/reading-mcp/actions/runs/34677501137)
执行成功仅表示诊断脚本完成，不表示准确率或集成验收通过。原脚本、raw TSV 和
该次 artifact 不追溯改写。

Coordinator 指出：`join_words` 对全页中文连续拼接会丢掉引擎已经识别的段间空白，
而 gold 的 `\n\n` 经既定 NFC + whitespace 归一化后仍是一个空格。因此全页
raw/gold-region CER 同时包含识别错误与投影拼接错误，不能把它全部解释成错字。
后续诊断须同时查看 `per_gold_paragraph` 和实际 engine block/par IDs；即使逐段
准确，也不能用 gold 插入边界、重排或选择文本来冒充 canonical 输出。

首轮日志中的 gold-region CER：F02/F06/F11 为 0，F07 为 5/110（4.55%），
F08 为 9/217（4.15%），F14 为 1/2102（0.048%）。这些是原始诊断数值，
尚未完成上述误差归因，尤其不能宣称中文阈值已通过。英文逐段 WER 除 F14
1/384（0.26%）外为 0；六例诊断顺序分均为 1。单引擎耗时 0.716–1.970s，
引擎进程峰值 RSS 126508–175080 KiB；不是整棵进程树、并发或 connector 冷开验收。

最终 canonical 评分必须读取实际生产 adapter/MCP 输出的段与自然句文本，按
既定 NFC + whitespace 规则与冻结 gold 比较。引擎段边界投影诊断如另加，必须
单独命名、保留原 raw 指标，且不得使用 gold 指导输出边界。raw/gold-region、
逐 gold 段指标以及 Actions green 均不能替代最终集成门禁。
