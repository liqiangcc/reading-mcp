# OCR disabled：混合页不发布缺失的正文

旧 disabled 分支仅判断无 native body 的图像页；同页存在部分 native 正文时可能发布遗漏扫描段的子集。本包在 disabled 分支检查真实 PDF image info，图像页缺正文仍返回 OCR_REQUIRED；有正文时使用固定 300 DPI、16M/page、64M/document 和共享 15 秒页预算作本地覆盖检查，不发现/启动 Tesseract 或读取模型。

复用真实 texttrace → native region 的文字/几何绑定遮罩。原生层完整覆盖时允许原路发布。对既有 layout 已分类 image/picture/figure/table，保留原 region 与 Source View 行为，仅其真实边界内部的完整像素可视为已有视觉覆盖，不扩大 bbox、不修改页面或 canonical。未知非白像素仍返回 OCR_REQUIRED；这是要求进一步检查，不是断言未知内容必然是文字。无图像的 native 页面不新增渲染。

不修改 enabled OCR policy、冻结资产、质量标准或 F11 分类，也不宣称此检查实现新的图表识别。parsed cache namespace 的 required-inspection/v1 升为 v2，旧部分文档缓存不能跳过检查；不删除旧数据。缓存测试覆盖原始 namespace、v1、v2 之间 miss 和 v2 hit。

新增 hosted 单元测试覆盖未知像素拒绝、现有视觉区域保留、边界外像素不可忽略、native 全覆盖、无 engine 配置与原输入不变。真实 MCP disabled 测试保留不存在的 engine/model 路径、F01/F03 成功，并要求六种公开补充混合页均 OCR_REQUIRED、旧 SQLite Document 集合不变。提交时所有执行验证尚待 Actions。

## 首轮真实失败与测量范围

7af49710ea2159239c0f2d214b798ca12a8df97b 的 PR run 34751069729 成功；push run 34751068359 的三个功能测试成功（含新增 disabled 六样本拒绝），F05 benchmark 失败：cold 16.112497495 秒通过45秒，但紧接的 restarted warm 在初始化阶段耗尽共享5秒。日志同时显示另外三个独立测试进程在执行解析/渲染。保留该失败，不能声称这种额外多服务负载下符合5秒。

后续将同一 benchmark 按精确测试名独立执行，其他功能测试仍执行且保留其并行性；每个 trial 的完整 startup+open、五次 cold/restart-warm、force_refresh、真实缓存命中/写入断言和45/5秒阈值均不变。此 gate 明确证明单服务基准，不替代生产负载测试或已有专门的队列/取消并发测试。另在没有任何 visual region 时直接消费已算出的 unknown sample 结论拒绝，避免无意义的第二份 raster 副本分配/扫描，不改变分类结果。
