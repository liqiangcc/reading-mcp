use serde::{Deserialize, Serialize};

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

impl OcrDerivation {
    pub fn from_metadata(metadata: &std::collections::BTreeMap<String, String>) -> Option<Self> {
        serde_json::from_str(metadata.get("ocr_derivation")?).ok()
    }
}
