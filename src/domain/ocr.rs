use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OcrConfig {
    pub enabled: bool,
    pub engine_path: String,
    pub tessdata_path: String,
    pub languages: Vec<String>,
    pub operator_revision: String,
    pub dpi: u32,
    pub oem: u8,
    pub psm: u8,
    pub detector_version: String,
    pub protocol_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OcrDerivation {
    pub schema: String,
    pub original_sha256: String,
    pub engine_sha256: String,
    pub model_sha256: Vec<String>,
    pub library_sha256: Vec<String>,
    pub languages: Vec<String>,
    pub dpi: u32,
    pub oem: u8,
    pub psm: u8,
    pub detector_version: String,
    pub protocol_version: String,
    pub operator_revision: String,
    pub pages: Vec<OcrPageBinding>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OcrPageBinding {
    pub page: u32,
    pub region_digest: String,
    pub bbox_digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct OcrEvidenceRecord {
    pub page: u32,
    pub block: u32,
    pub paragraph: u32,
    pub line: u32,
    pub text: String,
    pub bbox: [f64; 4],
    pub confidence: Option<f64>,
}

impl OcrDerivation {
    pub fn from_metadata(
        metadata: &std::collections::BTreeMap<String, String>,
    ) -> Result<Option<Self>, String> {
        let Some(raw) = metadata.get("ocr_derivation") else {
            return Ok(None);
        };
        serde_json::from_str(raw)
            .map(Some)
            .map_err(|e| format!("invalid OCR derivation: {e}"))
    }
}
