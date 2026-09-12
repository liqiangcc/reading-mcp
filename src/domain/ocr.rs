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

impl OcrConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !self.engine_path.starts_with('/')
            || !self.tessdata_path.starts_with('/')
            || self.engine_path.contains('\0')
            || self.tessdata_path.contains('\0')
        {
            return Err("OCR paths must be absolute and NUL-free".into());
        }
        if self.dpi != 300
            || self.oem != 1
            || self.psm != 3
            || self.protocol_version != "pdf-layout/v1"
            || self.detector_version != "pdf-layout/v1"
        {
            return Err("unsupported OCR configuration".into());
        }
        let mut languages = std::collections::BTreeSet::new();
        if self.languages.is_empty()
            || self
                .languages
                .iter()
                .any(|l| (l != "eng" && l != "chi_sim") || !languages.insert(l))
        {
            return Err("invalid OCR languages".into());
        }
        if self.operator_revision.trim().is_empty() {
            return Err("OCR operator revision must be nonempty".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DependencyFingerprint {
    pub name: String,
    pub sha256: String,
}

/// Versioned constants for the reviewed geometric retry, never caller tuning.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OcrRetryPolicy {
    pub version: String,
    pub primary_psm: u8,
    pub retry_psm: u8,
    pub max_retries_per_page: u8,
    pub overlap_percent: u8,
    pub vertical_overlap_percent: u8,
    pub roi_padding_pixels: u8,
}

impl Default for OcrRetryPolicy {
    fn default() -> Self {
        Self {
            version: "ocr-regional-retry/v1".into(),
            primary_psm: 3,
            retry_psm: 6,
            max_retries_per_page: 1,
            overlap_percent: 90,
            vertical_overlap_percent: 50,
            roi_padding_pixels: 2,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OcrRuntimeIdentity {
    pub config: OcrConfig,
    pub retry_policy: OcrRetryPolicy,
    pub inspection_policy: String,
    pub dependencies: Vec<DependencyFingerprint>,
    pub sha256: String,
}

impl OcrRuntimeIdentity {
    pub fn build(
        config: OcrConfig,
        mut dependencies: Vec<DependencyFingerprint>,
    ) -> Result<Self, String> {
        config.validate()?;
        if dependencies.is_empty() {
            return Err("missing OCR dependencies".into());
        }
        dependencies.sort_by(|a, b| a.name.cmp(&b.name));
        if dependencies.iter().any(|d| !valid_sha256(&d.sha256))
            || dependencies.windows(2).any(|w| w[0].name == w[1].name)
        {
            return Err("invalid/duplicate OCR dependency fingerprint".into());
        }
        let names: std::collections::BTreeSet<_> =
            dependencies.iter().map(|d| d.name.as_str()).collect();
        let models: std::collections::BTreeSet<_> = config
            .languages
            .iter()
            .map(|l| format!("model:{l}"))
            .collect();
        if !names.contains("engine")
            || !names.iter().any(|n| n.starts_with("library:/"))
            || models.iter().any(|m| !names.contains(m.as_str()))
            || names
                .iter()
                .any(|n| *n != "engine" && !n.starts_with("library:/") && !models.contains(*n))
        {
            return Err("incomplete/unexpected OCR dependency set".into());
        }
        let retry_policy = OcrRetryPolicy::default();
        let inspection_policy = "ocr-white-raster-inspection/v1".to_owned();
        let bytes = serde_json::to_vec(&(
            config.clone(),
            &retry_policy,
            &inspection_policy,
            dependencies.clone(),
        ))
        .map_err(|e| e.to_string())?;
        use sha2::{Digest, Sha256};
        Ok(Self {
            config,
            retry_policy,
            inspection_policy,
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
    pub retry_policy: OcrRetryPolicy,
    pub runtime_identity_sha256: String,
    pub inspection_policy: String,
    #[serde(default)]
    pub binding_map_sha256: Option<String>,
    #[serde(default)]
    pub evidence_blob: Option<String>,
}

impl OcrDerivation {
    pub fn validate_against(
        &self,
        identity: &OcrRuntimeIdentity,
        original_sha256: &str,
    ) -> Result<(), String> {
        if self.schema != "ocr-derivation/v3" || self.original_sha256 != original_sha256 {
            return Err("OCR derivation source/schema mismatch".into());
        }
        if self.retry_policy != identity.retry_policy
            || self.retry_policy != OcrRetryPolicy::default()
            || self.runtime_identity_sha256 != identity.sha256
            || self.inspection_policy != identity.inspection_policy
        {
            return Err("OCR retry policy/runtime identity mismatch".into());
        }
        if self.engine_sha256
            != identity
                .dependencies
                .iter()
                .find(|d| d.name == "engine")
                .map(|d| d.sha256.as_str())
                .unwrap_or("")
        {
            return Err("OCR engine identity mismatch".into());
        }
        let mut actual_models = self.model_sha256.clone();
        actual_models.sort();
        let mut expected_models = identity
            .dependencies
            .iter()
            .filter(|d| d.name.starts_with("model:"))
            .map(|d| d.sha256.clone())
            .collect::<Vec<_>>();
        expected_models.sort();
        let mut actual_libs = self.library_sha256.clone();
        actual_libs.sort();
        let mut expected_libs = identity
            .dependencies
            .iter()
            .filter(|d| d.name.starts_with("library:"))
            .map(|d| d.sha256.clone())
            .collect::<Vec<_>>();
        expected_libs.sort();
        if actual_models != expected_models
            || actual_libs != expected_libs
            || self.languages != identity.config.languages
            || self.dpi != identity.config.dpi
            || self.oem != identity.config.oem
            || self.psm != identity.config.psm
            || self.detector_version != identity.config.detector_version
            || self.protocol_version != identity.config.protocol_version
            || self.operator_revision != identity.config.operator_revision
        {
            return Err("OCR derivation configuration/dependency mismatch".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structured_identity_binds_retry_policy_and_dependency_changes() {
        let seed = identity();
        let built =
            OcrRuntimeIdentity::build(seed.config.clone(), seed.dependencies.clone()).unwrap();
        let mut reversed = seed.dependencies.clone();
        reversed.reverse();
        assert_eq!(
            built,
            OcrRuntimeIdentity::build(seed.config.clone(), reversed).unwrap()
        );
        let mut sorted = seed.dependencies.clone();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        use sha2::{Digest, Sha256};
        let old = serde_json::to_vec(&(seed.config.clone(), sorted)).unwrap();
        assert_ne!(
            built.sha256,
            format!("sha256:{:x}", Sha256::digest(old)),
            "pre-retry identity must miss the new cache namespace"
        );
        for index in 0..seed.dependencies.len() {
            let mut changed = seed.dependencies.clone();
            changed[index].sha256 = "0".repeat(64);
            assert_ne!(
                built.sha256,
                OcrRuntimeIdentity::build(seed.config.clone(), changed)
                    .unwrap()
                    .sha256
            );
        }
        let mut revised = seed.config;
        revised.operator_revision = "2".into();
        assert_ne!(
            built.sha256,
            OcrRuntimeIdentity::build(revised, seed.dependencies)
                .unwrap()
                .sha256
        );
    }
    #[test]
    fn dependency_set_requires_only_selected_models_and_actual_digests() {
        let seed = identity();
        assert!(OcrRuntimeIdentity::build(seed.config.clone(), seed.dependencies.clone()).is_ok());
        for index in 0..seed.dependencies.len() {
            let mut missing = seed.dependencies.clone();
            missing.remove(index);
            assert!(OcrRuntimeIdentity::build(seed.config.clone(), missing).is_err());
        }
        let mut invalid = seed.dependencies.clone();
        invalid[0].sha256 = "unknown".into();
        assert!(OcrRuntimeIdentity::build(seed.config.clone(), invalid).is_err());
        let mut duplicate = seed.dependencies.clone();
        duplicate.push(duplicate[0].clone());
        assert!(OcrRuntimeIdentity::build(seed.config.clone(), duplicate).is_err());
        let mut extra = seed.dependencies.clone();
        extra.push(DependencyFingerprint {
            name: "model:chi_sim".into(),
            sha256: "c".repeat(64),
        });
        assert!(OcrRuntimeIdentity::build(seed.config, extra).is_err());
    }
    fn identity() -> OcrRuntimeIdentity {
        OcrRuntimeIdentity {
            retry_policy: OcrRetryPolicy::default(),
            inspection_policy: "ocr-white-raster-inspection/v1".into(),
            config: OcrConfig {
                enabled: true,
                engine_path: "/e".into(),
                tessdata_path: "/t".into(),
                languages: vec!["eng".into()],
                operator_revision: "1".into(),
                dpi: 300,
                oem: 1,
                psm: 3,
                detector_version: "pdf-layout/v1".into(),
                protocol_version: "pdf-layout/v1".into(),
            },
            dependencies: vec![
                DependencyFingerprint {
                    name: "engine".into(),
                    sha256: "e".repeat(64),
                },
                DependencyFingerprint {
                    name: "model:eng".into(),
                    sha256: "a".repeat(64),
                },
                DependencyFingerprint {
                    name: "library:/lib".into(),
                    sha256: "b".repeat(64),
                },
            ],
            sha256: "x".into(),
        }
    }
    #[test]
    fn derivation_schema_and_classed_digests_are_strict() {
        let i = identity();
        let mut d = OcrDerivation {
            schema: "ocr-derivation/v3".into(),
            original_sha256: "raw".into(),
            engine_sha256: "e".repeat(64),
            model_sha256: vec!["a".repeat(64)],
            library_sha256: vec!["b".repeat(64)],
            languages: vec!["eng".into()],
            dpi: 300,
            oem: 1,
            psm: 3,
            detector_version: "pdf-layout/v1".into(),
            protocol_version: "pdf-layout/v1".into(),
            operator_revision: "1".into(),
            pages: vec![],
            retry_policy: OcrRetryPolicy::default(),
            runtime_identity_sha256: i.sha256.clone(),
            inspection_policy: i.inspection_policy.clone(),
            binding_map_sha256: None,
            evidence_blob: None,
        };
        assert!(d.validate_against(&i, "raw").is_ok());
        d.schema = "ocr-evidence/v1".into();
        assert!(d.validate_against(&i, "raw").is_err());
        d.schema = "ocr-derivation/v3".into();
        d.model_sha256[0] = "b".repeat(64);
        d.library_sha256[0] = "a".repeat(64);
        assert!(d.validate_against(&i, "raw").is_err());
    }
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
        let value: Self =
            serde_json::from_str(raw).map_err(|e| format!("invalid OCR derivation: {e}"))?;
        if value.schema != "ocr-derivation/v3"
            || value.inspection_policy != "ocr-white-raster-inspection/v1"
            || value.retry_policy != OcrRetryPolicy::default()
            || !valid_sha256(&value.original_sha256)
            || !valid_sha256(&value.engine_sha256)
            || value.model_sha256.is_empty()
            || value.library_sha256.is_empty()
            || !value
                .model_sha256
                .iter()
                .chain(&value.library_sha256)
                .all(|s| valid_sha256(s))
            || !valid_prefixed_sha256(&value.runtime_identity_sha256)
            || !value
                .binding_map_sha256
                .as_deref()
                .is_some_and(valid_prefixed_sha256)
            || !value
                .evidence_blob
                .as_deref()
                .is_some_and(valid_prefixed_sha256)
        {
            return Err(
                "invalid/incomplete OCR published derivation; explicit reopen required".into(),
            );
        }
        Ok(Some(value))
    }
}

pub(super) fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_prefixed_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(valid_sha256)
}

impl super::Document {
    pub fn validate_ocr_publication(&self) -> Result<(), String> {
        let Some(derivation) = OcrDerivation::from_metadata(&self.metadata)? else {
            if self.metadata.contains_key("ocr_evidence_blob") {
                return Err("OCR evidence has no derivation".into());
            }
            return Ok(());
        };
        let map = self
            .original_source_binding_map()
            .map_err(|e| e.to_string())?
            .ok_or("OCR original binding map missing")?;
        use sha2::{Digest, Sha256};
        let digest = format!(
            "sha256:{:x}",
            Sha256::digest(serde_json::to_vec(&map).map_err(|e| e.to_string())?)
        );
        if derivation.binding_map_sha256.as_ref() != Some(&digest)
            || derivation.evidence_blob.as_ref() != self.metadata.get("ocr_evidence_blob")
            || self.content_hash.0 != format!("sha256:{}", derivation.original_sha256)
        {
            return Err("OCR published source/evidence/binding identity mismatch".into());
        }
        Ok(())
    }
}
