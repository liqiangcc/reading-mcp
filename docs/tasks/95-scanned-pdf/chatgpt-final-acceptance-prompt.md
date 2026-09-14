# #95 最终外部验收交接

这份文件只用于把机器侧已完成的生产交接交给一个新的 ChatGPT
workspace 会话。它不包含私有论文正文、raster、hOCR 或 OCR 输出。

## 机器侧冻结快照（2026-09-14）

- production source/tag：`a9c811b5d9e11e6d6be9fef37f99dd8b6701cddd` / `v0.4.1`
- production binary SHA256：`8550bada64cbf342454f18a12d691f6db5d7bb272d76562ea38fc32ab7995a63`
- OCR runtime root：`/opt/reading-mcp/ocr-private-runtime-v0.4.1-a9c811b5d9e11e6d6be9fef37f99dd8b6701cddd`
- OCR runtime manifest SHA256：`e562abb37bd3575fa28553d0280ce953e595e9f5f71e1272d2d6e67a58790863`
- service：`reading-mcp-tunnel.service` active/running，`ExecMainStatus=0`，`NRestarts=0`
- tunnel health：`/healthz=live`，`/readyz=ready`
- private original `naturebp.pdf` SHA256：`d26997baf588222109d32545604a2a2ed400dc769a21fd49a5acdc4a955396ae`

private four-page local cold/full-scope handoff statistics（只记录统计）：

- pages `4`，sections `4`，sentence units `4`
- structure complete `true`，truncated `false`
- exact reads `4/4`
- first unit original-page binding：page `1`，source-view PNG `pass`
- restart/reopen content and normalized identity reuse：`pass`
- normalized identity：`sha256:23bad08869ea643ab7411a11cb0fa588f5500c7e7f27b9fd3c0730f20e1a1e1a`
- unsupported/quality limitation：只记录 runtime profile 的代码和覆盖统计；不得据此宣称论文转录准确率，也不得回显正文。

上述是 production v0.4.1 与同一 private runtime 的本机隔离交接，不是
ChatGPT 外部验收。外部验收必须通过真实 Secure MCP Tunnel 完成。

## 可直接复制到新 ChatGPT 会话的提示词

```text
你正在执行 reading-mcp #95 的最终外部验收。请先实际调用 tools/list，记录返回的
完整工具名；期望保持九工具边界：
list_documents、list_directory、open_document、get_document_structure、
get_text_units、search_document、get_context、read_document、get_source_view。
不得出现 admin、session、progress、sentence-specific 或 format-specific 工具。

你必须通过当前 production Secure MCP Tunnel 调用，不要用本机 stdio、HTTP、CI、
tunnel-client doctor 或仓库测试替代外部验收。按仓库
docs/chatgpt-acceptance.md 的 A–J 顺序执行并逐项记录 pass/fail：

A discovery/profile；B 小 max_nodes structure continuation 至 complete；
C 单 Section sentence-first + 直接 read_document/get_context；D 全部 body-owning
Section 的 preserve_source 遍历并记录 coarse/unsupported gap；E 保存 N 的
TextLocator 后重启/新 session，以 anchor_locator=N、direction=forward 验证恰好
N+1 且 identity 变化 stale；F 对当前项反复提问不推进，只有“下一句/继续”推进一项；
G SearchHit.text_locator 直接交 read/context/anchored units；H–J 分别完成 EPUB
provenance/degradation、原生 PDF page traceability、instruction-like 文本保持数据。

同时验收 private four-page naturebp.pdf：只公开原始文件 hash、source/runtime
版本、page/section/unit/coverage/unsupported-gap 统计和 pass/fail。不得输出、复制、
总结或上传论文正文、raster、hOCR、OCR 字符串，也不要更新
paper-reading-lab 的阅读进度或宣称 #47 READY。至少证明一个自然句子的可靠边界、
exact read 和 original-page binding，再遍历全部 supported scope；若任一范围不能
证明，明确写入 unsupported-gap，而不是用 source_complete 掩盖。

最后只输出以下非敏感记录（不要输出 token、路径中的秘密或正文）：
acceptance_date=<UTC date>
production_sha=a9c811b5d9e11e6d6be9fef37f99dd8b6701cddd
tool_discovery=pass|fail
scenario_A=pass|fail
scenario_B=pass|fail
scenario_C=pass|fail
scenario_D=pass|fail
scenario_E=pass|fail
scenario_F=pass|fail
scenario_G=pass|fail
scenario_H=pass|fail
scenario_I=pass|fail
scenario_J=pass|fail
private_naturebp=pass|fail
private_naturebp_pages=<count>/<total>
private_naturebp_sections=<count>
private_naturebp_units=<count>
private_naturebp_coverage=<statistics only>
private_naturebp_unsupported_gaps=<codes/count only>
known_degradations=<codes only>
``` 

真实 ChatGPT workspace 返回上述记录后，Coordinator 再决定是否在 #95 写最终
acceptance 并关闭 Issue；在此之前不得关闭 #95、创建新 release/tag 或修改生产。
