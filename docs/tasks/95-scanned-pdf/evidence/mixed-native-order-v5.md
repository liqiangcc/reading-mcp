# 混合页原序接线：inspection v5

本包以真实 primary/retry 引擎顺序为基础，将唯一覆盖同一组 word 中心的原生区域作为顺序锚点。保留原生正文、原区域编号、全部 raw attempts；不使用 gold、坐标排序或文字匹配来修正文案。原生正文锚点的相对顺序必须与已有 layout 顺序一致；缺失、重复、矛盾或跨区域锚点显式失败，不发布不完整结果。

新增 `ocr-native-anchor-order/v1` typed evidence，记录原始区域数、每个 native/OCR 来源与投影编号。Rust 根据 raw attempts 和原生区域重算顺序，并核对实际 canonical regions 的顺序、编号及 OCR bbox/class。证据仍通过既有不可变 blob、真实 binding map 和原子 Document 发布路径绑定身份。

inspection policy 从 v4 改为 v5，使旧候选 parsed cache/derivation 不被新策略复用。norm v11/hash v3 不变；旧数据不删除，需要明确 reopen。

新增验证（提交时尚待 GitHub-hosted Actions 执行）：

- Python 几何测试：交替顺序、原始观察不变、矛盾/缺失锚点失败、共享页预算。
- Rust typed evidence/实际 regions 测试：交换、缺失、伪造编号或 bbox 均拒绝。
- 真实 Rust parser → evidence → SQLite 重启：三个公开单列混合样本均需保持四段实际全文及原页绑定。
- 真实 MCP open/get_text_units → 服务重启：四段原序、normalized identity 和 locator 一致。

补充样本不是冻结语料或替代验收。此前三个短双栏样本因 PSM3 将 native 与 scanned 内容混入同一 box 而失败；本包不声称解决该缺口。正式冻结质量门槛、F11 未决验收、最终包/部署和外部检查仍保留。
