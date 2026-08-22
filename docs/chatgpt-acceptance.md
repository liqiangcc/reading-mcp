# ChatGPT / Secure MCP Tunnel v0.1 验收

本文件是生产外部验收 runbook。仓库 CI、stdio E2E、HTTP E2E 和
`tunnel-client doctor` 都不能替代真实 ChatGPT 对生产 Secure MCP Tunnel 的调用。

## 前置条件

- 使用支持自定义 MCP 的 ChatGPT workspace，并启用 Developer Mode；
- Secure MCP Tunnel 已配置并指向生产 stdio 服务；
- 生产部署使用 exact reviewed main SHA，secret 不写入仓库；
- 只记录 SHA、日期、service state 和 tunnel health，不记录 token 或文档正文。

## Tool discovery

ChatGPT 的 `tools/list` 必须恰好返回：

```text
list_documents
open_document
get_document_structure
get_text_units
search_document
get_context
read_document
```

不得出现 admin、session、progress、sentence-specific 或 format-specific Tool。

## 场景

- A discovery/profile：`list_documents` 使用 `discovery-cursor/v1`，随后
  `open_document`，确认 `reading-profile/v1`、canonical coverage 和 format reliability。
- B structure：用小 `max_nodes` 消费 `StructureCursor` 至 complete，确认无 gap/overlap；
  EPUB 不把 nav preorder 当作正文 source order。
- C 单 Section：用 `get_text_units(max_items=1, requested_kind=sentence,
  coverage_policy=preserve_source)`，保存 item 的 `TextLocator`，并直接交给
  `read_document` / `get_context`。
- D 整本/多 Section：按 `body-order/v1` 消费各 body-owning Section 的
  preserve-source stream；coarse Paragraph 和 unsupported gap 必须如实体现。
- E restart/resume：消费 N 后只保存 N 的 TextLocator；新 MCP session/重启后以
  `anchor_locator=N, direction=forward` 必须返回恰好 N+1；identity 变化必须 stale。
- F one-item interaction：追问“这句话是什么意思/为什么/前一句”都保持 N；只有
  “下一句/继续”才恰好推进到 N+1，不能跳过 coarse Paragraph。
- G SearchHit：`SearchHit.text_locator` 直接交给 read/context/anchored units，禁止
  复制 snippet 后重新搜索。
- H–J：代表性 EPUB 记录 nav/spine provenance 和 degradation；原生 PDF 验证 page
  traceability；instruction-like document text 只能作为数据，不能升级为系统指令。

## 记录模板

```text
acceptance_date=<UTC date>
production_sha=<exact main SHA>
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
known_degradations=<codes only>
```
