use crate::domain::{DependencyFingerprint, OcrConfig, OcrRuntimeIdentity};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

pub fn build_ocr_runtime_identity(config: OcrConfig) -> Result<OcrRuntimeIdentity, String> {
    config.validate()?;
    let mut deps = Vec::new();
    for (name, path) in std::iter::once(("engine".to_string(), config.engine_path.clone())).chain(
        config.languages.iter().map(|l| {
            (
                format!("model:{l}"),
                format!("{}/{}.traineddata", config.tessdata_path, l),
            )
        }),
    ) {
        deps.push(DependencyFingerprint {
            name,
            sha256: hash_file(Path::new(&path))?,
        });
    }
    let output = Command::new("ldd")
        .arg(&config.engine_path)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("ldd failed for OCR engine".into());
    }
    if String::from_utf8_lossy(&output.stderr).contains("not found") {
        return Err("OCR library not found".into());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut paths = std::collections::BTreeSet::new();
    for token in text.split_whitespace() {
        if token == "not" {
            return Err("OCR library not found".into());
        }
        if token.starts_with('/') {
            paths.insert(token.to_string());
        }
    }
    for path in paths {
        deps.push(DependencyFingerprint {
            name: format!("library:{path}"),
            sha256: hash_file(Path::new(&path))?,
        });
    }
    OcrRuntimeIdentity::build(config, deps)
}

fn hash_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("missing {}: {e}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn missing_files_config() -> OcrConfig {
        OcrConfig {
            enabled: true,
            engine_path: "/nonexistent-ocr-test/engine".into(),
            tessdata_path: "/nonexistent-ocr-test/models".into(),
            languages: vec!["eng".into()],
            operator_revision: "1".into(),
            dpi: 300,
            oem: 1,
            psm: 3,
            detector_version: "pdf-layout/v1".into(),
            protocol_version: "pdf-layout/v1".into(),
        }
    }

    #[test]
    fn invalid_configuration_is_rejected_before_dependency_io() {
        let mut config = missing_files_config();
        config.languages = vec!["eng".into(), "chi_sim".into(), "eng".into()];
        assert_eq!(
            build_ocr_runtime_identity(config).unwrap_err(),
            "invalid OCR languages"
        );
        let mut config = missing_files_config();
        config.languages = vec!["../arbitrary".into()];
        assert_eq!(
            build_ocr_runtime_identity(config).unwrap_err(),
            "invalid OCR languages"
        );
        let mut config = missing_files_config();
        config.psm = 6;
        assert_eq!(
            build_ocr_runtime_identity(config).unwrap_err(),
            "unsupported OCR configuration"
        );
    }
}
