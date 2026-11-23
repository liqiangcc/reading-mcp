//! V2-4b PR-2, §5: the driver for the ported `ocr_private_mcp` helper.
//!
//! The v1 helper `tests/support/ocr_private_mcp.rs` drove `open_document` with
//! `READING_MCP_TOOLS=v1`; V2-4b PR-5 deleted it and its include in `tests/ocr_private_runtime.rs`.
//! The v2 helper lives in `tests/support/v2_ocr_private_mcp.rs` and uses `open` + `cite`; this file
//! is its driver.

#[path = "support/v2_mcp.rs"]
mod v2_mcp;

#[path = "support/v2_ocr_private_mcp.rs"]
mod private_mcp;
