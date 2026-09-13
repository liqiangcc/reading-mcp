# PDF layout 主线前置整合

本页记录已发布的 v0.4.1：source `a9c811b5d9e11e6d6be9fef37f99dd8b6701cddd`。
PDF layout / original-page renderer 与 #95 本地 OCR 均通过 hosted 构建、解包和真实引擎
验证；OCR 仍是 opt-in，生产仅使用经过 manifest/hash 校验的私有 runtime Release 资产。
历史 v0.3.0 与 v0.4.0-rc.1 tag/asset 保持不变，不覆盖旧版本。

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
证据，模糊连字符保留；图像页 OCR 仅在显式 enabled 且私有 runtime 完整校验后执行。
`integrity=valid` 只证明内部映射；v0.4.1 的生产 E2E 已验证 F07 的 open、structure、
sentence units、exact read、original source view，以及重启后的 cache/locator 复用。
冻结质量报告中的 F07/F08 CER 仍按原门槛记录为失败，未修改 gold 或阈值。

最终部署前保留已校验旧二进制、旧 Python 环境、service config/drop-ins 和一致性
state snapshot；整体恢复原 tuple，不让旧 binary 读取不兼容 state。不 tar 活跃
SQLite/WAL 作为唯一一致性备份。不覆盖 tag/asset、不删除 canonical/raw 数据。

## 许可与验证

保留依赖分发包的全部许可/版权说明。PyMuPDF/MuPDF、PyMuPDF4LLM/Layout
适用其 AGPL/commercial 条款，仓库 MIT 不重新许可它们；本次不购买商业授权。
原候选说明与证据见 [历史源码](https://github.com/liqiangcc/reading-mcp/blob/2f31f95c8f16b8c64ee015254bed6864c2a97ddb/docs/issue89-review.md)，
其中旧测试结果是历史证据，不能替代本 PR 精确 head 的 hosted CI。

CI 执行 Format/Clippy/full Test、结构化投影 fixtures、真实 pinned engine 的原生
PDF worker 和原页渲染/预算测试、package smoke，以及私有 runtime 的 hosted 离线
重建/解包 smoke。生产部署使用 Release v0.4.1 的持久资产和不可变版本目录；不对
私有论文上传或云 OCR，准确率门槛仍以冻结 synthetic 质量报告独立验收。
