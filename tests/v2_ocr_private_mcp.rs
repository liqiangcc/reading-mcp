//! V2-4b PR-2, §5: the driver for the ported `ocr_private_mcp` helper.
//!
//! `tests/ocr_private_runtime.rs` includes `tests/support/ocr_private_mcp.rs`, whose one MCP test
//! drove `open_document` with `READING_MCP_TOOLS=v1`. The v2 helper lives in
//! `tests/support/v2_ocr_private_mcp.rs` and uses `open` + `cite`; this file is its driver, so the
//! v1 pair stays untouched until PR-5 deletes it.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

#[path = "support/v2_ocr_private_mcp.rs"]
mod private_mcp;
