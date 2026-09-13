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

## 2026-09-13 批准后执行：候选模型与专用隔离

用户已批准继续有界本地模型评估与 OCR 专用隔离实现；此前等待该授权的
上下文不再是阻塞。仍不部署中间版本，最终精确 SHA/发布部署审查保留。

### 固定 ONNX 候选的实际结果（不是 F11 最终验收）

固定 `PaddlePaddle/PP-DocLayout_plus-L_onnx` revision
`feb74619326f634e0e883218598096a3733ad9f7`，模型文件 SHA256
`77afb2caa74dd13240d087d2eced91d7fcd2caebd16006a0a66162fc8707ff0e`。
模型卡声明 Apache-2.0；完整离线交付仍需许可证/文件清单审查。

精确代码 `c03fb61ea544be6be6f39b6d462a01da7787053f` 的
[hosted run 34702814921](https://github.com/liqiangcc/reading-mcp/actions/runs/34702814921)
在 MemoryMax=768 MiB、swap=0、网络隔离下成功执行全部 8 个候选样本。
原始输出张量保留在该 run artifact；仓库
`evidence/layout-model-onnx-c03fb61-summary.json` 是日志派生摘要，不伪称原始
artifact 字节相同。没有读取 gold，也没有改变 canonical。

| case | 总秒数 | 进程峰值 KiB | 候选区域 |
| --- | ---: | ---: | --- |
| F02 | 2.9114 | 576860 | 6 text |
| F06 | 2.8645 | 576744 | 6 text |
| F07 | 2.8676 | 576852 | 4 text |
| F08 | 2.8763 | 576608 | 4 text |
| F11 | 2.8813 | 576720 | 6 text + image + formula |
| F12 | 2.8921 | 577572 | 6 text + number |
| F13 | 2.8827 | 576940 | 1 text |
| F14 | 2.8967 | 576896 | 1 text |

候选输出按分数排序，**不得**将其数组顺序直接作为原页阅读顺序。F11 分类
存在不等于 100% 图表/公式原页区域保留；还需验证真实几何覆盖、typed 来源、
正文不丢失/不重复、coarse 项不可变成 Sentence。未批准修改 gold 或阈值。
下一诊断保持 native layout 会话、候选会话同时驻留并运行真实 primary OCR，
记录实际加载库 hash 与整个 cgroup 的累计峰值，检验组合后的资源需求。

### 实际解析器隔离接线

`38fcdae777ab695762dfa474560ab35ce8704b89` 引入专用 systemd transient
unit 的真实 LayoutPdfParser 入口：768 MiB 聚合内存、禁 swap、64 PIDs、
512 MiB 私有 tmpfs、网络 namespace、60 秒执行上限及 1 秒 TERM 宽限。
原有 native-only 解析启动方式不变。Rust 不调用 sudo；hosted 测试只对编译后
的测试二进制使用 sudo，所有 Cargo 编译/测试构建仍在 GitHub-hosted runner。

`987a9b918a7651ccbd2ad0243cd751abb5959f2c` 用 Rust RAII 所有的独立管道
绑定 service main 生命周期：校验真实 pipe dev/inode 与随机 token，EOF 时
不能启动新 worker；所有者消失后退出 service main，由 systemd 回收整个
cgroup。它覆盖 Tokio 清理 future 无法运行的情况，不仅依赖杀 systemd-run。
[run 34703921658](https://github.com/liqiangcc/reading-mcp/actions/runs/34703921658)
的 `Rust OCR systemd launch and cancellation integration` step 已 success，
包括真实 F07、抗 TERM 后代、启动前取消、没有 async cleanup/systemctl 的
owner-pipe 回收。此记录不把当时仍在运行的整个 run 写成已完成。

`f2a94a006319f66e29745a6077a49781c4f145d2` 将该入口接入实际 MCP runtime：
OCR 配置先纯验证，缓存查找前检查专用隔离支持，OCR enabled 时没有普通
process-group 回退。对应 stdio/HTTP 真实入口 hosted 验证尚待结果。
生产未变更；剩余部署级故障矩阵、组合模型资源/区域验收、完整离线安装和
精确版本最终审查仍不能省略。

### 同一入口的限额故障与私有解释器候选后续结果

`a430ac69c838c4f54e9e6aa21a7c25a2c9427ff3` 的两条 hosted Rust CI
`34705356078` / `34705352640` 及两条真实 OCR
`34705356128` / `34705352642` 均 success。真实入口包含 stdio/HTTP、原页
locator 重启复用、F07 隔离解析和五项 systemd 故障测试。内存故障要求 manager
实际 `oom-kill`，不是“任意非零退出”；泄漏抗 TERM 后代要求实际 PID 拒绝、
manager `timeout` 和所有 PID 消失，不能当作正常成功解析。取消时同步关闭
owner pipe，测试故意不轮询 Tokio 清理 future，仍须在 2 秒内回收。
后续代码另设置 `LimitCORE=0`，防止私有 worker 内存落入系统 core dump。

联合模型资源报告已实际下载，原 JSON SHA256
`385402a8b3597df224d130c2d44d57dec926cfc8258f3ec02db12ca26ef65954`；
派生摘要保存为 `evidence/layout-model-joint-f17ca91-summary.json`。全部八样本
成功执行 native layout + 固定 ONNX candidate + 真实 primary OCR，但没有
改变 canonical。累计 cgroup 峰值 **803176448 bytes**，接近 768 MiB 的
805306368 bytes 限额；这是累计值，不伪称每页独立峰值，也不证明生产余量。
每个 case 留有 76 个实际映射依赖文件摘要及对应 Tesseract 依赖摘要。

私有解释器候选 `d429be14f0da10f904feeeb128df5f31b50d0223` 的
[run 34706491682](https://github.com/liqiangcc/reading-mcp/actions/runs/34706491682)
与 `34706489552` 均 success：空 dpkg 状态解析完整依赖，固定所选版本/hash，
提取独立 Python，断网 RootDirectory 内安装 hash-locked wheels，再运行冻结
F07 完整 Python worker，得到四段和完整两次尝试/derivation。没有使用宿主
Python/库来替代候选路径。此前 native import 的 SIGSEGV 已用 hosted GDB
定位到 `fclose`，在子进程启动后发生；提取树缺失 POSIX shell 入口。显式
固定 dash 包及 `usr/bin/sh -> dash` 组装输入后 import 与完整 worker 均通过，
该组装同时纳入重建校验，未执行生产 apt 或任意 maintainer scripts。

此候选不是完成的 companion 安装器：根目录约 584989993 bytes，仍包含
bootstrap wheels/测试材料；正式包还要处理冻结分类模型、namespace 路径身份
接线、完整文件清单、原子安装/回滚与实测空间。ORT 的可选硬件/遥测辅助命令
缺失警告在报告中原样保留；网络已禁用，不能把这些 warning 默认为完整依赖
交付证据。生产仍为 `reading-mcp-1569c68d6220d129beb1f3218cd22d57f26f331c`，
未重启/部署。只读磁盘快照 available=606978048 bytes，不构成准入结论。

**仍需 Coordinator 明确 F11 100% 区域保留的评估定义。** 冻结 gold 的图表/
公式框是包含空白的宽区域带，模型给实际对象框。不能用 gold 反推生产框，
也不能未经裁决把“每个语义区域均保留并可回看原页”与“覆盖整个标注矩形
（含空白）”混为同一指标。问题已发到 PR99 comment `5647122033` 和当前会话；
没有改 gold、门槛或正式 F11 gate，也未宣称 F11 已通过。

### 依赖发现的启动限额补齐（待当前 head hosted 验证）

此前 Rust 缓存查询前与 Python PDF 处理前的 `ldd` 均使用无界 output/capture。
现改为固定 `/usr/bin/ldd`、原有清空后的允许环境、5 秒发现时限和 stdout /
stderr 各 64 KiB 上限；非零状态与 missing-library 检查仍明确拒绝。两端均
建立独立进程组，先终止所拥有的组再 reap，使用 WNOWAIT 观察退出，避免提前
reap 后复用 PID 的清理风险。只限制依赖发现，不改变 fingerprint 编码、OCR
参数、缓存身份或质量阈值，也不代替正式 OCR systemd 隔离。

新增 Rust/Python 的双流/非零状态、两种输出溢出和退出父进程后仍由子进程
持有输出管道的超时测试；后者证明无可执行后代/打开管道，孤儿 zombie 的
最终 reap 仍由 init 负责，不把它当成完整 OCR cgroup 回收测试。Python 测试
接入 hosted workflow，Rust 随全量 tests；本机仅 fmt/py_compile。

`b80f93f4cf6728b89c594ae013d91d38d86e389f` 的 hosted
[Rust run 34736375280](https://github.com/liqiangcc/reading-mcp/actions/runs/34736375280)
已 success，包含 Format/Clippy/全量 Test、真实 PDF/layout 和 package smoke。
[真实 OCR run 34736375290](https://github.com/liqiangcc/reading-mcp/actions/runs/34736375290)
及 `34736374033` success；已读前者日志：Python 三个依赖进程故障测试
0.215s，实际 stdio 15.58s、HTTP 8.13s、F07 systemd 5.08s、五项隔离故障
1.83s。离线私有环境 `34736375237` 同样 success。此记录不借用旧 SHA 的
结果，也不把各套测试累计耗时当作单次 ingestion 性能。

后续新增冻结 F05 的真实 stdio 性能测试（待该测试提交的 hosted 结果）：
五个独立空状态分别冷打开，逐轮重新启动 server 后暖打开。计时含启动/
身份验证/initialize/open，不含之后的测试清理，分别要求 <=45s / <=5s。
`force_refresh=true` 不跳过源重新获取；观察现有实际 parsed-cache telemetry，
冷启动两次 miss（入队前/后）且一次 put，暖启动一次 hit、零 put，并要求
raw/normalized identity 相同。成功 hit 在现有 CachingParser 中直接返回、
不进入底层 OCR Parser；报告称 cache bypass 证据，不捏造额外的引擎计数器。
此项仅是冻结四页混合 F05 的 hosted stdio 验收，不代替生产 connector 的
真实 deadline、10 秒余量、私有原始四页论文或最终 package 的本机实测。

### 可重放的私有 runtime 归档（当前提交待 hosted 验证）

此前私有解释器验证使用刚安装的工作目录，不能据此证明可交付归档。
新增 `scripts/ocr/runtime_archive.py` 的 build/unpack 链路，当前 scope 是
已安装的解释器、engine/models/libraries 与 hash-locked Python 环境；F11
额外分类模型和 Rust runtime RootDirectory 配置尚未完成，不称最终 companion。

归档以 `ocr-private-runtime-archive/v1` 保存实际 checkout SHA、每个 regular
file 的 bytes/SHA256/mode、目录 mode、symlink 原始 target 及四份来源清单
摘要。完整 Debian notices、wheel dist-info notices、单独冻结的 flatbuffers
notice、原始 engine/Python manifests、requirements.lock 和 apt-source-uris
保留在运行树中。bootstrap wheels 不重复交付；tmp/dev/proc/sys 和公开测试
挂载点保留为空目录，测试 PDF/脚本不混作运行依赖。gzip/tar 的 uid/gid/mtime
固定；同一输入树的重复打包须字节相同，不外推成跨时间重新安装也必然相同。

解包先以单一 regular-file FD 核验外部归档摘要，再对实际消耗的压缩字节
二次核验，避免预检后文件替换/更改绕过授权。只在自有 0700 staging 下写入，
禁止重复/越界路径、hardlink/special file、symlink 祖先写入、清单外成员、
错误 source SHA/类型/mode/内容摘要。symlink 最后创建，绝对路径只保留
RootDirectory 内部语义，绝不在宿主跟随它来读写验证。文件 sync、目录 sync
后使用 Linux renameat2(RENAME_NOREPLACE) 发布新目录；已有目录（包括空目录）
和并发抢先创建均不能被覆盖。失败清理仅限本次所有的 staging；不切服务、
不改 canonical state、不移 tag、不上传 Release asset。

防御性解包上限为 compressed/regular total 各 1 GiB、单文件 256 MiB、
manifest 32 MiB/50000 entries；这些是输入上限，不是生产容量准入结论。
生产仍需基于实际归档/解包/备份/回滚同时占用和安全余量计算容量。

Hosted workflow 增加 deterministic roundtrip、payload 单字节篡改、外摘要/
source 不符、预检后输入变化、路径/链接拒绝、已有目标与原子发布竞争测试。
随后实际打包安装树、解包至全新 root，在 network-disabled RootDirectory
内通过只读挂载的公开 F07 再跑真实 worker，要求解释器/worker/dependencies
身份和四段 canonical 文本与打包前一致。只在这些步骤全部成功后上传 verified
candidate archive；执行结果尚待该 head，不以 py_compile 代替测试。

`77a0d6f2a92eae8e3268b5b3d3a2b81e942c1a1d` 的
[push run 34738431660](https://github.com/liqiangcc/reading-mcp/actions/runs/34738431660)
及 PR run `34738433269` 的全部离线组件均 success。已读取 push 日志：7 个
归档/原子发布测试通过，真实重建后的 F07 解释器/worker/dependencies/四段
canonical 与打包前一致。前一 `c3834c8` 的真实重建和 OCR 对比其实已成功，
但普通 runner 的 `du` 无权遍历保留下来的 root/private 目录，使 gate failure；
修正为 sudo 只读统计，没有 chmod 放宽权限。回溯仅在实际 smoke 失败时触发，
不再把统计或比较失败当原生崩溃。

该 push 的 source SHA 就是上述分支 SHA，不借用 PR merge checkout identity。
内层 `ocr-private-runtime.tar.gz` SHA256：
`19b0544ea72e54678d868a19752ae5cdc67363a6d7b77ff968ab5bd28818ae1b`，
200838180 bytes；9766 inventory entries、regular bytes 473153410，解包目录
apparent size 473161882。Artifact `10312136205`
(`ocr-private-runtime-verified-candidate`) 是外层 GitHub ZIP，202595936 bytes，
其 digest `cdeef95af8176a4766094f1be1cecdbff4ec8efd75997a8fab0d20d84b316b0f`
与内层 tar.gz 身份分开记录。没有将候选下载/安装到生产，也没有发布 Release。
这些 apparent/regular 字节还不是完整分类 companion 的最终容量准入。

### 真实解析进程降权（待该提交 hosted 验证）

审查发现原 supervisor 的 Popen 未设置 user/group，PDF/native-library child
继承了 root 身份；此前普通 socket 外连拒绝不能证明它无法利用 root 权限
绕过 namespace。此处不增加任务框架，只收紧已有 OCR unit 的权限边界：
可信小型 supervisor 保留读取 root owner pipe 所需的 SYS_PTRACE 和设置
child credentials 的 SETUID/SETGID，其他 capability 从 bounding set 移除。
实际 PDF worker 改为 UID/GID 65534、空 supplementary groups、umask 0077；
NoNewPrivileges 持续生效。tmpfs 在创建时归此 UID/GID 所有且保持 0700/
512MiB，不对宿主 /tmp 做 chmod/chown。另隐藏 home、只读系统/控制组/内核
参数、隔离设备、禁止创建 namespace，保留原有资源和取消门槛。

新增实际 Rust launcher 测试必须证明 child 三种 UID/GID 都为 65534、
effective/permitted/inheritable/ambient capabilities 都为零，无法恢复 UID0、
读取 supervisor environ/stdin 或打开 PID1 network namespace，且私有 scratch
可正常使用。原 OOM/PID/temp/network/取消/真实 stdio/HTTP/#92 测试不删除。
归档重建后的 F07 同时改用无 capability 的非 root 用户执行，与原输出作
严格比较；是否兼容真实库由 hosted 结果决定，不通过降门槛或回退 root 掩盖。

### 私有归档接入真实 Rust Parser（本提交等待 hosted 验证）

c0a5455a69c12aa7df55232155e8e67f2ab67a17 全部 checks 已通过；
34739031437 六项 systemd 权限/资源/取消测试通过，34739029484 归档
重组非 root F07 通过。后者内层归档 200838495 bytes，解包 regular files
473153410 bytes，不是最终含分类模型的发布包。

本包补齐 LayoutPdfParser 的显式 private-root 入口：使用同一个真实
systemd launcher、owner pipe、降权与 cgroup 清理，只增加 RootDirectory /
MountAPIVFS；Python 路径按根目录内部解释，绝不回退宿主解释器。拒绝
非绝对、宿主根、非规范目录路径，以及未启用 OCR 的 private-root 请求。
不更改 native source-view 的独立 Python 配置。

离线 workflow 在已校验重组目录上编译并执行真实 Rust F07：内部 Python
在宿主必须不存在；正文四段逐字等于归档 smoke、实际 raw hash、完整 typed
derivation/evidence 均验证，并保存后重新打开 SQLite 比对 normalized identity
和原页 map。身份由该归档内前序真实 smoke 提供并由 worker 重算验证；
这不是任意环境 fingerprint。仅测试路径使用 report 注入，不新增生产环境
捷径。RuntimeConfig 的最终包验证/启动前身份构造与完整 MCP 私有根目录
接线仍需下一包，不能把本测试当作已经部署或完整 cache/package 验收。

### 启动包校验 → runtime/cache → worker → Document（本提交待 hosted）

92826a2cb5f1dd2d8d68e0fa918dc01b21b1be7a 所有 checks success；
34739785623 真实归档 Rust F07 / evidence / SQLite reopen 为 1 passed，3.59s。
本包继续接通 RuntimeConfig 的 root + manifest 成对路径，不新增任意 fingerprint
环境变量。配置先校验，再使用可信既有解释器的标准库逐项核验完整安装清单，
所有文件流式 SHA256/mode/size、目录、symlink 和额外/缺失项都必须匹配。
os.open 的目录 fd + NOFOLLOW 保持读取不穿过包内 symlink 到宿主。
随后同一 systemd launcher 在私有根内获取真实 engine/model/library 集合；
启动发现共用 5 秒有界子进程预算，owner pipe 关闭处理超时/延迟启动。

新增 typed OcrRuntimePackageIdentity，其真实清单摘要绑定所有包内 Python、
wheel、模型和库文件及 header/version；与配置/依赖集合共同计算身份。
非 package 编码仍为原四元组；有 package 为明确第五 typed 分量，原历史
evidence 可反序列化验证，不静默改已有 normalized hash。新 identity 实际进入
CachingParser、worker expected identity、证据 blob 与 Document derivation。
worker 读同一只读挂载清单并检查内部解释器与实际依赖；版本目录按部署契约
不可原地热修改。完整字节校验发生在启动前，不伪称每次 cache hit 重扫文件。

新测试包括库存内容/权限/链接/路径/FIFO/额外模块失败，以及实际 MCP 的同包
重启 cache hit、revision 变化 miss、合法新清单身份变化 miss、真实模型损坏时
启动失败且旧 SQLite Document 保留。使用独立 hosted 解包目录，模型损坏注入
有 RAII 恢复并串行测试；冻结资产与原归档不动。保留 45s cold / 5s warm 门槛。
原页 renderer 仍使用原外部 layout Python，未宣称私有 OCR 测试替代 #92 验收。
全套结果待 hosted。F11 正式分类、最终 Package/Deployment、存储扩容与实际
生产验收仍未完成，不合并/部署。

### OCR admission / deadline / Rust 输出上限错误（本提交待 hosted）

按设计错误表修复已证实的 retryability 缺口：单飞队列满或 2 秒队列等待超时
返回 typed OcrBusy → OCR_RESOURCE_LIMIT/retryable=true；共享解析/已知 PDF
ingestion deadline 返回 OcrTimeout → OCR_TIMEOUT/true。已知 PDF 的 90 秒
whole-open 同样正确映射；媒体类型尚未知或非 PDF 不冒充已开始 OCR，保留
原通用超时语义。OCR 关闭与非 PDF 的 30 秒 parser budget 不变。
Rust 侧已知 OCR byte/output/normalized-size caps 使用 OCR_RESOURCE_LIMIT/false。
所有新增错误消息是静态有界文本，不从 stderr 解析类别，不泄露正文/路径。

实际队列测试保留一个 active + 两个 queued、第三 waiting 拒绝、两个 waiting
2 秒后不运行等断言，并精确断言 typed busy。新增 60 秒虚拟时间超时证明
底层解析被取消、无缓存发布、许可恢复。既有 OpenDocument delayed-save 测试
加强为精确 OCR_TIMEOUT，并保留不发布/不索引断言。
真实 MCP transport 测试经实际 OpenDocument/BudgetedParser 产生 timeout/cap
错误，检查公开 code/retryability、不回传 source 路径、repository.save 为零；
busy 在该 transport 测试通过 Parser port 注入，实际 admission 产生路径由上述
单飞测试覆盖，不伪称对正式引擎制造了并发 overload。

尚不等于全错误表关闭：worker 的 typed failure 分类、systemd 真实终止原因、
OCR_REQUIRED/OCR_UNAVAILABLE 和 F11/最终部署验收仍需后续处理。

### Worker typed failure 到真实 MCP（本提交待 hosted）

98cf5749002fcfe43df7129512e32593439122d3 全 checks success，34741863303
日志已确认真实 MCP error transport、60 秒单飞取消、2 秒 admission 和
OpenDocument shared-deadline 测试成功。此包继续补 worker，而非仅新增错误名。

新增 `ocr-worker-failure/v2`，仅显式异常生产点可给出 OCR_UNAVAILABLE /
OCR_TIMEOUT / OCR_RESOURCE_LIMIT。Dependency 阶段必须无原文 hash 声明；
ingestion 阶段必须匹配 Rust 实际完整输入 hash；两阶段均匹配本次 expected
runtime identity，未知 schema/stage/code/额外字段或不一致声明退为 OCR_FAILED。
旧 v1 无支持正文/未解决几何观察协议不改。明确 page/raster/text/依赖输出上限、
实际 subprocess timeout、依赖缺失/变化/import 失败接入 typed 分类；无根据的
普通异常不升级为“可重试”或猜成 OOM。错误公开文本保持静态，隐藏 OSError 路径。

Hosted 新增四个 Python 协议/真实预算生产点测试、Rust 篡改/空 hash 负例；
原真实 model-tamper probe 增加完整 v2 envelope 断言。私有运行包测试额外实际
执行 F07 字符上限失败，Rust 必须收到 OcrResourceLimit；真实 MCP 在正确启动
后、一个全新 revision 的 cache miss 前故障注入实际模型变化，必须返回
OCR_UNAVAILABLE/retryable=false，保存前失败，旧 SQLite Document 保留。
故障仅作用于 disposable hosted root、RAII 恢复，不改原归档或冻结输入/gold。
不声称每次 cache hit 检查运行中被管理员热改的包；版本目录仍须不可变。

顺手去掉 worker 已验证 dependencies 之后的无用 engine/tessdata/sha 定义，
以及持有整份 manifest 到 OCR 结束的 `_` 引用；不改实际正向识别/投影结果。
仍未关闭 OCR_REQUIRED、真实 systemd 终止分类、F11 与最终发布/生产验收。

### OCR disabled 的必需检查与防止部分发布（本包待 hosted 验证）

对无原生正文、但包含实际 PDF image 对象的页，disabled worker 返回
`pdf-layout-ocr-required/v1`，绑定完整原始字节 SHA256；Rust 仅在 disabled
且 schema/code/raw hash 全匹配时映射 `OCR_REQUIRED`（不可重试）。
这是要求开启图像检查，不承诺图像一定有可读文字；纯原生空白/矢量页不受影响。
在整份文档发布前检查每页，避免 F05 只发布 native 子集；页脚不算正文。
未在此包扩展“有正文的同页图片是否含更多文字”的判断，混合区域仍为后续项。

PDF parsed cache namespace 升级为 required-inspection/v1，旧解析结果不命中；
不删除旧 canonical/evidence，成功原生正文的 normalized hash 规则不变。
缓存测试证明相同 raw 的旧 namespace miss、新 namespace 重复 hit。
真实 MCP 测试配置 OCR disabled 和不存在的引擎/模型路径：F01/F03 正常发布，
F02/F05/F12 返回 OCR_REQUIRED、无路径泄漏，已有两个 SQLite Document 原样保留。
该测试由现有 hosted real-engine workflow 执行，未本机运行。

### systemd 真实终止结果通道（本包待 hosted 验证）

保留 `--collect` 与 owner-pipe 取消回收。新增短小可信 ExecStopPost callback，
读取 systemd 自己设置的 SERVICE_RESULT/EXIT_CODE/EXIT_STATUS，通过另一个
Rust 持有的 nonblocking FIFO 写单个不足 512 bytes 的定长上限记录。
管道由设备号/inode/随机单元 token 绑定，PDF 子进程仍为 nobody、无 root 权限；
callback 不读取正文、不写文件、不经 OCR stdout 返回，不留下失败单元等待查询。
Rust 在 systemd-run --wait 结束后读结果，只有真实 oom-kill/timeout 映射
OCR_RESOURCE_LIMIT/OCR_TIMEOUT；退出 137 或单独 SIGKILL 不推定 OOM，
缺失/非法/未知控制记录不生成专用分类。普通引擎失败仍走严格 worker 协议。

依据 systemd v255 官方 ExecStopPost 语义：
https://raw.githubusercontent.com/systemd/systemd/v255/man/systemd.service.xml
（官方 freedesktop 网页此时返回 403，读取官方 Git 仓库文档确认）。

新增 hosted root 真故障测试经生产 command/WorkerProcess：850 MiB 分配触发
768 MiB cgroup OOM；仅测试将 RuntimeMaxSec 缩短为 1s 触发真实 manager timeout；
exit(137) 保持普通失败。逐例要求失败单元仍被自动回收；既有无 runtime 取消、
晚启动 owner EOF、进程树回收与非特权隔离测试保持执行。生产60秒限制不变。
本机仅 fmt/py_compile，所有故障执行与 Rust 构建均交 GitHub-hosted Actions。

### 同页 mixed 顺序缺口的独立实测（新增 diagnostic，未声称修复）

当前 worker 在 native boxes 后追加 OCR boxes。仅 F05 跨页/F12 页脚成功不能
证明同页 mixed 的阅读顺序。新增 mixed_order_probe.py 在 hosted 使用自撰四段
英文生成六种布局：上下两种、左右两种、单列交替、双列交替；原生字与扫描字
分布互换。只使用公开自撰文本/内建 Helvetica，不改冻结语料、字体、gold、
manifest 或正式门槛，不把新样本声称为 Coordinator 已冻结验收。

实际 worker 只收到 PDF bytes/配置/实物 identity；识别返回后才读取独立
authored expectation。完整 canonical、native 区域、primary/retry 原始词、
selected/excluded 引用、CER/WER 分母和错误数公开输出日志/artifact，失败逐例记录。
诊断结果不重排 canonical，不用 expectation 引导算法，不以 workflow green
冒称 mixed order 已支持。用真实观察决定是否能沿原引擎 source refs 合并 native
锚点；不能从简单 y/x 排序或尚未证明的顺序关系捏造完整成功。

### 实际图像与 native 栅格覆盖接线（待 hosted 验证）

5a41962157bd41d8ba229faf85bf838fd8f5ee34 的完整 mixed-order.json 已实际读取，
SHA256 2ebe17917b6ccc4152330726fd9eeba34e8ad5e5f6f76619cb91314bee2c45c8。
六组 attempts 均为空：layout 只返回 native 框，没有返回实际存在的底层 image。
全部漏掉两段扫描正文、WER22/44，不是排序成功或 OCR 准确率通过。

本包 enabled 检查真实 page.get_image_info，不依赖 layout 是否分类出 image。
对有 native 的图像页，在共享15秒页预算、16M/页和64M总栅格预分配门禁内，
复用同一次原始300DPI RGB渲染；不改送给Tesseract的像素。只在副本上将可唯一
绑定 native text+bbox 的真实 texttrace span bbox 遮白，记录原bbox和精确pixel矩形，
向外2渲染像素且裁剪至原页边界。仅检查完后整个副本严格全白才复用native；
任何剩余非白sample都要求OCR，不以字数、invisibility或confidence代替覆盖证据。
旋转/无效文字/不唯一来源不能证明覆盖，不使用gold，未知内容不做字符过滤。

新增 typed native_coverage：source/masked样本hash、尺寸、未覆盖sample数、
native source_box、真实text/bbox与pixel transform。Rust拒绝来源/文本不匹配、
变换篡改、非法hash/计数；零未知sample时重算全白masked digest。它不是“原页
空白”证据，不能与blank_raster混用。已覆盖页存完整evidence但不声称engine调用。
正向 parser→不可变store→Document derivation路径保持不变。

inspection policy v2→v3 进入统一runtime identity/cache/hash；旧策略的OCR派生
须显式reopen，不删除旧canonical/evidence，native normalization与#92不改。
本包尚不宣称 mixed阅读顺序修复或OCR disabled同页native+image覆盖闭环。
新增真实F03/F04-form/F04-flat Rust测试锁住零engine调用及coverage落盘；
原正式CER/WER/页执行门禁、补充六组混合诊断继续跑。补充诊断完整raw留artifact，
日志打印紧凑覆盖/原引擎顺序/排除来源，避免重复多MB输出，不删除原始观察。

852d58b hosted 真测试进一步暴露 F03 reliability 分类错误：以前只要存在
derivation 就认为 local OCR 被采用。覆盖检查会产生 derivation 但没有选入OCR词，
不能把它标成OCR正文。修复为 Rust 对验证后的 blob.selected_words 计数、写入
typed selected_word_count，发布/加载拒绝缺计数，reliability按真实采用词数分类。
不修改既有F03/F04“not_applied”断言。inspection policy再升v4以隔离中间v3缓存，
不让缺计数的中间缓存混入新结果，旧对象保留且要求显式reopen。
