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
