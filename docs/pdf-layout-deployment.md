# PDF layout 主线前置整合

本次仅整合已在线上候选 `1569c68d6220d129beb1f3218cd22d57f26f331c`
使用的 opt-in layout / original-page renderer，不发布或部署中间版本，不启用 OCR。
后续 #95 按已批准设计实现和验收后，才走独立 Release / Package / Deployment。
Cargo 版本保持当前 main 的 0.3.0；没有重写 v0.3.0 或 v0.4.0-rc.1 tag/asset。

## 依赖与启用

Python 3.12+，PyMuPDF/PyMuPDF4LLM/Layout 均为 1.28.2；其余固定版本见
`scripts/pdf-layout/requirements.txt`。server 内嵌 worker，package 包含依赖清单、
setup/smoke 脚本和本文，不把 Python 环境伪装成 server 二进制的一部分。

`setup-pdf-layout.sh` 仅用于 hosted CI 或显式隔离开发环境，要求新绝对路径，
拒绝覆盖已有环境。生产不执行 cargo build；本次也不在生产安装依赖。最终 #95
需补全所有实际包/模型 hash 与离线 companion artifact，按 Package Issue 审核，
不得把这里的版本列表当作完整 hash lock。

`READING_MCP_PDF_LAYOUT_PYTHON` 指向固定环境解释器；未配置继续默认 PDF backend。
显式配置后缺依赖、版本错误等直接失败，不静默回落并缓存错误 backend 结果。
原页 renderer 使用原始 PDF；不从 canonical text 或 OCR 内容重建页面。
现有线上 `READING_MCP_SOURCE_VIEW_MAX_PIXELS=16000000` 是已发现的环境配置，
不是所有主机的新默认值。保留 source-view 各项限制和显式超预算失败。

## 身份、范围与回滚

normalization v9、segmentation v3、normalized hash v2；PDF parsed cache namespace
`pdf-layout/v1:pymupdf4llm-layout/1.28.2` 与默认 backend 分开。v3 保留 sentence-final
`etc.` 修复。原始 ContentHash/DocumentId 和原页绑定语义不变。
旧 v8 PDF/EPUB 需显式 reopen、重新取得 locator/cursor；不得模糊重绑或删除旧数据。
这不是后续 OCR v11/hash-v3 迁移。

版面分类/平面章节是推断事实，不能宣称已重建全部目录。非正文保留 coarse/visual
证据，模糊连字符保留；图像页/OCR 识别未启用。`integrity=valid` 只证明内部映射，
不证明识别准确率、完整出版物覆盖或 #95 完成。保持 main #92 的方向/作用域完成契约。
现有 worker timeout/kill 不视为未来 OCR 子进程树回收已经验证。

最终部署前保留已校验旧二进制、旧 Python 环境、service config/drop-ins 和一致性
state snapshot；整体恢复原 tuple，不让旧 binary 读取不兼容 state。不 tar 活跃
SQLite/WAL 作为唯一一致性备份。不覆盖 tag/asset、不删除 canonical/raw 数据。

## 许可与验证

保留依赖分发包的全部许可/版权说明。PyMuPDF/MuPDF、PyMuPDF4LLM/Layout
适用其 AGPL/commercial 条款，仓库 MIT 不重新许可它们；本次不购买商业授权。
原候选说明与证据见 [历史源码](https://github.com/liqiangcc/reading-mcp/blob/2f31f95c8f16b8c64ee015254bed6864c2a97ddb/docs/issue89-review.md)，
其中旧测试结果是历史证据，不能替代本 PR 精确 head 的 hosted CI。

CI 执行 Format/Clippy/full Test、结构化投影 fixtures、真实 pinned engine 的原生
PDF worker 和原页渲染/预算测试、package smoke。仅构造字典的测试不算真实 OCR
验证；本 PR 没有 OCR 实现，也不对真实扫描论文作准确率或部署成功声明。
