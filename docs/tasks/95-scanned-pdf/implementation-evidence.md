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

## 区域重试与发布身份（实现中，待当前 head hosted 验证和主控审查）

`dce7ebc6241a35f97fa3f92900ac8298b022d4cd` 的
[真实 worker / Rust integration / canonical gate](https://github.com/liqiangcc/reading-mcp/actions/runs/34689505621)
通过：F02/F06 CER 0/548、WER 0/91；F07 CER 1/110；F08 CER 0/217、WER 0/26；
F14 CER 1/2102、WER 1/384。这是五例已接入质量门槛，非整个第 7 节验收完成。

后续代码把 `ocr-regional-retry/v1` 的固定主/重试 PSM、重试上限、几何阈值和
像素容差纳入 runtime identity 的结构化序列化。旧的不含 policy 的 lookup key
自然 miss；全局 normalization v11/hash v3 不复用另一含义，新增 OCR 子协议明确
使用 `ocr-derivation/v2` 与 `ocr-evidence/v2`。历史未发布 OCR v1 Document 需要
显式 reopen，不删除旧 blob、原始 PDF 或 canonical 数据。

完整 typed blob 保存 source hash、runtime identity、按页 primary/retry attempts、
组件 ROI、稳定 box/line/word 引用及 selected words。Rust 拒绝无效坐标/置信度、
缺失/重叠组件、不存在的引用、未覆盖原词中心的候选，以及与 raw attempts 不一致
的 selected evidence。先写完整 immutable blob，再将实际 blob digest 与真实
OriginalSourceBindingMap digest 填入 typed derivation；normalized hash v3 绑定这些字段。
OpenDocument/缓存/仓库发布和加载路径拒绝坏 derivation 或 binding mismatch。

本节新增代码须由当前 head Actions 验证。仍未完成：所有冻结 fixture 的完整 MCP
质量/边界/locator 验收、混合页与 visual 分类闭环、共享预算/进程树隔离与取消、
离线依赖包、生产资源和 connector 时限实测、最终主控审查及 release/package/deploy。

## 执行边界增量（2026-09-12，未部署）

- `e8f219d707d918e00141ff917ccab9fa9fb42dd2` 的
  [hosted real-engine run](https://github.com/liqiangcc/reading-mcp/actions/runs/34690947189)
  全部通过，包含共享 15 秒页预算与光栅预检测试、真实 Rust OCR 和原 canonical 门槛。
  PSM3 与最多一次 PSM6 共用页截止时间；最多 8 个需 OCR 页、1600 万像素/次、
  累计 6400 万像素。重试的第二次实际渲染也消耗累计像素额度。
- `27dd7a4dc68ae5eaf59d9f273dc9f0642a682c42` 的
  [Rust CI](https://github.com/liqiangcc/reading-mcp/actions/runs/34691144363) 与
  [real-engine run](https://github.com/liqiangcc/reading-mcp/actions/runs/34691144309)
  全部通过。layout worker 使用独立进程组；取消后 TERM、1 秒 grace、KILL，并保留
  admission permit 至父进程清理结束。Linux WNOWAIT 在发信号前保留父 PID 身份。
  测试验证忽略 TERM 的后代停止、父进程回收及取消清理期间许可不释放。
  **这不证明 orphan 后代已全部 reaped，也不证明内存/网络隔离。**
- `9ac02c6042fe384c295e5f7742f7d77694997781` 加入仅 OCR-enabled PDF 使用的
  同完整 ParsedCacheKey single-flight：1 个 active、最多 2 个 waiting key、排队
  最多 2 秒。最后一个 waiter 取消才 abort 共享解析；单个取消不影响其他 waiter。
  新增同 key 缓存重用、取消不发布缓存、队列满和等待超时测试；本节记录时该 head
  [CI](https://github.com/liqiangcc/reading-mcp/actions/runs/34691450539) 仍在执行，
  不据源码宣称通过。

后续输出边界代码把 OCR structured stdout 收紧至 32 MiB，保留 64 KiB stderr
读取上限，并不再向 MCP 返回原始 OCR stderr；native layout 的原有输出额度和
错误诊断保持不变。对应边界/隐私测试需由同提交 hosted CI 验证。

`23fcd1db62b0ccbc2dbcf3e12c234c6b37a2e645` 接入 OpenDocument 的 60 秒
cache→parse→publication 共享截止时间、包含取源的 90 秒 whole-open 上限，
并移除 OCR-enabled PDF 外层普通 30 秒截断；普通 PDF/非 PDF 仍保持原解析额度。
[hosted CI](https://github.com/liqiangcc/reading-mcp/actions/runs/34691737173) 的
Format/Clippy/Test 已通过（记录时打包仍在运行）；新增真实 OpenDocument 调用路径
的虚拟时间测试，覆盖跨阶段累计超时、超时 save 不提交、取源消耗总时间及原额度回归。
[真实 OCR / canonical gate](https://github.com/liqiangcc/reading-mcp/actions/runs/34691737170)
全部通过。这里证明的是异步截止时间和发布前检查，不证明同步 SQLite/CPU 工作可被硬抢占。

仍未闭合：硬资源执行边界、全后代回收、私有临时目录清理、aggregate memory/temp/PID
限制及网络隔离。后续 page_selection_probe 采集公开冻结 F01/F03/F04/F05/F09–F13
的原始 layout、glyph visibility/geometry、image/drawing bounds 与真实 worker 输出，
两个管道结束后才读取 gold；该诊断不取代正式质量门槛，也不将 worker failure 算通过。
混合页/已有层/blank/visual 分类、完整 MCP 门槛、离线包及最终生产验收仍按设计待办。

## 原页分类诊断实测（26baa615，非分类验收通过）

精确代码：`26baa615d8aa6711865925c09e9cdd1d14e7bf72`。
[hosted run 34691988235](https://github.com/liqiangcc/reading-mcp/actions/runs/34691988235)
全部步骤完成；artifact `ocr-first-engine-probe/page-selection-probe.json` 已实际下载读取，
SHA256 `c9b4107535643f116a93e3d7b5b9299a03033e499a4037fe5333eb5e21b21a30`。
同一公开 JSON 同时打印在 job `103548753532` 的 page diagnostics 日志。

| 样本 | worker 退出码 | prose 段数 | 成功 payload 声明的 OCR 页 |
| --- | --- | --- | --- |
| F01 | 0 | 6 | 无 |
| F03 | 0 | 6 | 无 |
| F04-form / F04-flat | 各 0 | 各 6 | 无 |
| F05 | 0 | 6 | 2、4 |
| F09 | 1 | 无成功 payload | 未知，不可写成未执行 |
| F10 | 0 | 6 | 1 |
| F11 | 0 | 9 | 1 |
| F12 | 0 | 7 | 1 |
| F13 | 0 | 1 | 1 |

确定缺口：F05 第 4 页本应 blank，却执行 OCR。F11 把图表数字 `12` 与公式
`x?+y?=2?` 当作 prose，且一处原正文被分成两个段；不得按这些字符串过滤。
F12 OCR 的 `Page l` 成为第 7 个 prose 段，同时原生 `Page 1` 已保留为
page-footer/preformatted。原始 layout box 实际只有 x0/y0/x1/y1，没有 bbox，
现有 native exclusion 读取错误字段，确实未排除原生页脚。

F11 的原始 `to_json(use_ocr=False)` 返回空 boxes，不提供可直接复用的图表区域；
不能虚称已有 layout 分类足够，亦不能用 gold bbox 修剪。后续须验证独立原图 layout
区域来源，并把分类、排除依据和未采用的原始 OCR 观察一起纳入 typed evidence/identity。
以上段数与退出码只是诊断事实，不代替 CER、边界、原序或完整 MCP 验收。

## 全白栅格检查接线（待本提交 hosted 验证）

只在既有逻辑判定需检查 OCR 的页上执行实际 300 DPI RGB/无 alpha 渲染。完整像素
必须全为 255 且原页无 texttrace span 才能记为 blank；一个非白样本或存在隐藏文字
对象都会阻止该判定。不是以空文字、短段、低置信度或 gold 区域当作 blank。
主识别与重试复用该栅格；像素分配累计额度仍为 6400 万，blank 检查不消耗 8 个
required-recognition page 名额。

blank 的 typed observation 保存尺寸、通道数、样本数、glyph span 数及实际像素
SHA256，attempts/selection/components 均为空。Rust 重算同尺寸全白像素摘要，
验证尺寸匹配原页 300 DPI，拒绝矛盾的识别声明，再随完整 evidence blob 持久化。
纯 blank 文档仍以无可支持正文失败返回，公开 worker 失败 JSON 留下检查证据；
不伪造可读正文。混合文档的 blank 事实可随其他 OCR 页一起持久化。

缓存迁移：固定 `ocr-white-raster-inspection/v1` 进入 runtime identity 的结构化
哈希并由 derivation 严格匹配；page observations 为 v2，derivation/evidence blob
为 v3。旧未部署候选 OCR 文档需显式 reopen；旧 blob 不删除、不覆盖。全局
normalization v11/hash v3 及原始 content hash 语义不变。仍需完成 blank 事实向
完整 MCP coverage/reliability 的映射，以及 F11/F12 分类/去重和执行隔离验收。

## 原生区域排除接线（待本提交 hosted 验证）

修复真实 layout 的 x0/y0/x1/y1 读取。原生区域记录原 box index、class、bbox、
原生文本；Rust 要求它匹配 worker 返回的原页 layout region。OCR 单次入口不再
丢弃重叠词；全部 primary/retry raw observations 保留。只在选择完成后，按所有
词中心是否被原生区域覆盖排除整个 OCR box，并记录 excluded_sources 稳定引用。
Rust 从原始词坐标重算被排除集合、剩余顺序与词引用，不接受任意删词/删段。
一个 box 仅部分词重叠时明确 unresolved，整次发布失败，不猜测切分正文。

对应 policy 更新为 `ocr-original-region-inspection/v2` 并进入 runtime/cache/derivation
身份，page observations 为 v3；旧候选需要 reopen。原始 PDF、冻结 gold、全局
normalization v11/hash v3 不改。F12 真实 Rust 测试验证原生页脚保留、OCR 六段选择、
被排除观察仍可读、删除排除引用后验证失败。通用几何测试不使用任何 fixture 字符过滤。
这不宣称任意混合页阅读顺序或 partial-overlap 已支持，F11 visual 分类仍未解决。

### Hosted 验证结果：原生区域排除

精确 head `da657a9b3384e0d7e5b26c1b0ad90b4a1d2b1b5f` 的
[真实引擎 run 34693864668](https://github.com/liqiangcc/reading-mcp/actions/runs/34693864668)
已完成 success。job `103553841971` 的 Rust real OCR integration 明确记录：
F12 原生页脚/排除观察、F07 派生与原页绑定、证据失败保留 SQLite 旧文档、F05
blank 无引擎 attempt 四项均通过（4 passed / 0 failed，8.67s）。同一 run 的
geometry tests 和正式 canonical quality gates 均 success。此证据不代表 F11
图表/公式分类或全部发布验收通过。

下一诊断仅使用已 fingerprint 的 libtesseract 原生 AnalyseLayout / BlockType，
在独立、有超时的 hosted 子进程记录 F11/F12/F13 所有原生 block 类型与像素 bbox。
配置、所选模型、库摘要先重新验证；不从 gold 提取区域，不改变正式 worker/身份/
门槛，不把原生 API 存在当作分类有效的证明。结果随 page-selection-probe JSON
及公开日志保留；调用失败明确使诊断失败，而不是空区域成功。

### 原生 C API 分类诊断结论与部署能力复核

精确 head `21e0315918c21b2dac3f4096e39dd82c13462797` 的
[hosted run 34694150489](https://github.com/liqiangcc/reading-mcp/actions/runs/34694150489)
已完成 success，包括真实 Rust 集成与正式 canonical gate。实际下载的
`page-selection-probe.json` SHA256 为
`8f2a03e0a45a4b8562f69d2d31b21124014e4a2f824cd5e891c84d03134ba1f5`。
F11 原生 AnalyseLayout 返回 8 个 block，全部 type_id=1 / flowing_text；
F12 为 7 个、F13 为 1 个，也全部 flowing_text。因此原生 BlockType 与此前的
原页 layout/hOCR 一样，不能提供通过 F11 所需的图表/公式分类证据。成功执行诊断
不等于分类通过；不得把这些类型直接作为生产非正文过滤规则。

2026-09-12 再次只读核对实际 `reading-mcp-tunnel.service`：MainPID 987247，
ControlGroup `/system.slice/reading-mcp-tunnel.service`，Delegate=no，
MemoryMax=infinity，TasksMax=2261，PrivateNetwork=no，NoNewPrivileges=no。
宿主 cgroup v2 提供 memory/pids/cpu 等控制器，但当前服务没有声明委派；
root 用户会话的 user@0.service Delegate=yes 不证明生产子进程具备相同委派。
`unshare --user --map-root-user --net --fork /usr/bin/true` 实际退出 0，证明当前
执行身份可创建该 namespace，不证明生产 wiring 或完整网络拒绝验收已完成。
未更改线上 unit、未创建生产 cgroup、未重启或部署。

同次快照：根文件系统约 682 MiB 可用，内存 available 702 MiB、swap 使用
3521 MiB。快照不是硬容量门槛，需按离线包实际解压尺寸、回滚快照和负载峰值
计算余量；不能宣称足够，也不能删除 canonical/其他会话数据获取空间。
执行隔离仍需落实子进程树内存/temp/PID 限制、网络拒绝和完整回收测试。
真实 OCR workflow 补充 typed observation/store 文件的触发路径，避免只改
这些发布契约时遗漏真实集成 gate。
