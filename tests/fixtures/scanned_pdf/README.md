# #95 fixture / gold 冻结审查包（无 OCR 实现）

依据批准设计 `76f449061b20b6c8d79b57b871ce67ac1cfa7c6b` 第 7 节构造。
这是 OCR 实现分支首个独立 fixture commit；等待 Coordinator 按精确 SHA 冻结，
后续 commit 才可加入 OCR 实现/调参。本包不执行识别，不证明 OCR 准确率。

## 审查入口

- [manifest.json](manifest.json)：15 个变体、18 个物理页，PDF/gold/font/preview/
  生成器/源语料逐文件 bytes + SHA256。F04 的 Form/flat 是两个变体。
- [corpus.json](corpus.json)：设计原始语料、独立逐句 gold、质量阈值，不由 OCR
  输出反推。中文句间不插入人为英文空格，E5 保留 `-8.8` 和 `+8.8`。
- `gold/*.json`：全文 Unicode scalar 坐标、段落/自然句范围、原始页号、行范围、
  字体/基线/bbox、原页模式及非正文区域。bbox 为原 PDF 左上角 point 坐标；
  没有捏造的引擎置信度或应用 Section identity。
- [总览一](previews/contact-1.png)、[总览二](previews/contact-2.png)、
  [总览三](previews/contact-3.png)；`previews/*-pN.png` 可逐页放大。
  总览只放各变体第一页；F05 四页必须另查 [p1](previews/F05-p1.png)、
  [p2](previews/F05-p2.png)、[p3](previews/F05-p3.png)、[p4](previews/F05-p4.png)。

覆盖 native、scan、正确隐藏层、Form/flat、混合页、双栏、中文、中英混合、blank、
100-DPI 降质再升采样、图表/公式、扫描正文+可见页脚、短页、超过12行的单句。
F12 的正文是纯图片，只有 footer 是可见文字对象；不能被 footer 误判为 native body。
F11 的 chart annotation 包含 y=635 的标签，因此 gold bbox 扩至 y=650；
实际柱形/标签位置保持设计定义，不能漏掉标签而声称区域保留完整。
F04-flat 是相同 gold/坐标直接写入 page content streams 的扁平控制，
不是一个未经验证的 flatten 工具结果；F04-form 确实用 Form XObject 承载隐藏层。

## 固定依赖与权利

| 文件 | 来源/版本 | SHA256 |
| --- | --- | --- |
| DejaVuSans.ttf | Ubuntu fonts-dejavu-core 2.37-8；未修改的 DejaVu Sans 2.37 | ae7b7855e115a5966d8b1b3f80f254ccc117ec86f9965e202ee2940453837280 |
| NotoSansCJKsc-Regular.otf | noto-cjk Sans2.004，commit 523d033d6cb47f4a80c58a35753646f5c3608a78 | 2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b |

Noto 原文件：[上游固定来源](https://github.com/notofonts/noto-cjk/blob/523d033d6cb47f4a80c58a35753646f5c3608a78/Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf)。
字体保留原始内嵌版权信息及 [许可全文](fonts/LICENSES.txt)：Noto SIL OFL 1.1，
DejaVu Bitstream Vera 条款/DejaVu 公有领域改动。字体不是仓库 MIT 重新许可。
合成文字/几何由本项目创作；无真实论文、私有正文或历史 OCR 输出。

构造只使用 PyMuPDF/MuPDF 1.28.2 和以上字体，不安装/调用 Tesseract、layout
模型、reading-mcp 或任何识别/评分算法。PyMuPDF 依赖沿用批准许可边界。
字体/每个生成资产都按实际 hash 固定，不依赖系统字体替代。

## 构造与验证边界

`generate.py` 以 corpus 的 gold 先排版，再构造 native/raster/hidden-layer PDF；
不会从提取文字或模型结果生成 gold。300 DPI 栅格，A4 595x842 points，12 pt、
18 pt 行距/段间距，按设计列宽换行。96 DPI PNG 是原页机械预览，不是 OCR 图。
生成器拒绝覆盖已有 pdf/manifest；对新目录构造以保持已审查字节不被静默重写。

本机执行的只是公开 fixture 资产创作，并做肉眼预览；无本机编译、测试或识别。
`verify.py` 与再生成对比在 GitHub-hosted `OCR fixture integrity` workflow 执行：
hash、范围/源语料/字体、真 PDF 文字层与 Form/flat、scan 无正文 text objects、
F14 长句和 F13 短页、原页图像一致性以及独立再生成逐字节一致性。
此 workflow 不调用 OCR，也不替代 Coordinator gold 审查或未来真实引擎验收。

作者已看三张总览及放大的中文页：未见缺字/裁切，双栏、短页、长句、图表/公式
如设计呈现；这只是预览观察，不是人工最终 gold 批准。F02/F03/F04 两变体的
96-DPI 预览 SHA256 相同：`56c5448e7903d5ff5eb1aceab019589b639000c688963e9f136b80bbb81bacc5`。
精确 commit、manifest SHA 和 hosted 检查链接在终端/Issue 交接，不写伪自引用 SHA。
