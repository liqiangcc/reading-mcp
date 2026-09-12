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
pub struct DependencyFingerprint {
    pub name: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OcrRuntimeIdentity {
    pub config: OcrConfig,
    pub dependencies: Vec<DependencyFingerprint>,
    pub sha256: String,
}

impl OcrRuntimeIdentity {
    pub fn build(
        config: OcrConfig,
        mut dependencies: Vec<DependencyFingerprint>,
    ) -> Result<Self, String> {
        if !config.engine_path.starts_with('/') || !config.tessdata_path.starts_with('/') {
            return Err("OCR paths must be absolute".into());
        }
        if config.dpi != 300
            || config.oem != 1
            || config.psm != 3
            || config.protocol_version != "pdf-layout/v1"
            || config.detector_version != "pdf-layout/v1"
        {
            return Err("unsupported OCR configuration".into());
        }
        if config.languages.is_empty()
            || config
                .languages
                .iter()
                .any(|l| l != "eng" && l != "chi_sim")
            || {
                let mut s = config.languages.clone();
                s.sort();
                s.windows(2).any(|w| w[0] == w[1])
            }
        {
            return Err("invalid OCR languages".into());
        }
        if dependencies.is_empty() {
            return Err("missing OCR dependencies".into());
        }
        dependencies.sort_by(|a, b| a.name.cmp(&b.name));
        let bytes = serde_json::to_vec(&(config.clone(), dependencies.clone()))
            .map_err(|e| e.to_string())?;
        use sha2::{Digest, Sha256};
        Ok(Self {
            config,
            dependencies,
            sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
        })
    }
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
