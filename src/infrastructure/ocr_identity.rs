use crate::domain::{DependencyFingerprint, OcrConfig, OcrRuntimeIdentity};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

pub fn build_ocr_runtime_identity(config: OcrConfig) -> Result<OcrRuntimeIdentity, String> {
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
