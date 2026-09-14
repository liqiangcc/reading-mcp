use crate::domain::{DependencyFingerprint, OcrConfig, OcrRuntimeIdentity};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::process::Command;

pub(crate) const OCR_PROCESS_ENV: [(&str, &str); 4] = [
    ("PATH", "/usr/bin:/bin"),
    ("LANG", "C.UTF-8"),
    ("OMP_THREAD_LIMIT", "1"),
    ("PYTHONDONTWRITEBYTECODE", "1"),
];

// `-I` ignores every PYTHON* variable, including PYTHONDONTWRITEBYTECODE, so
// the explicit `-B` is the only guarantee that no interpreter launched inside
// the private runtime rootfs can alter the verified inventory with .pyc files.
pub(crate) const OCR_INTERPRETER_ARGS: [&str; 2] = ["-I", "-B"];

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
    let mut command = Command::new("/usr/bin/ldd");
    command
        .env_clear()
        .envs(OCR_PROCESS_ENV)
        .arg(&config.engine_path);
    let output = super::ocr_identity_process::dependency_output(
        &mut command,
        std::time::Duration::from_secs(5),
    )?;
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
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Do not block opening a FIFO before checking the owned descriptor.
        // Symlinks to regular packaged libraries remain supported.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("OCR dependency must be a regular file".into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_hash_streams_all_bytes_and_tracks_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("model");
        let mut bytes = vec![42_u8; 3 * 1024 * 1024 + 7];
        std::fs::write(&path, &bytes).unwrap();
        let before = hash_file(&path).unwrap();
        assert_eq!(before, format!("{:x}", Sha256::digest(&bytes)));
        *bytes.last_mut().unwrap() = 43;
        std::fs::write(&path, &bytes).unwrap();
        assert_ne!(before, hash_file(&path).unwrap());
        assert!(hash_file(directory.path()).is_err());
        assert!(hash_file(&directory.path().join("missing")).is_err());
        #[cfg(unix)]
        {
            let link = directory.path().join("library.so");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert_eq!(hash_file(&path).unwrap(), hash_file(&link).unwrap());
        }
    }

    #[cfg(unix)]
    #[test]
    fn dependency_fifo_is_rejected_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("model-fifo");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: owned NUL-terminated path in the test's unique directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert_eq!(
            hash_file(&path).unwrap_err(),
            "OCR dependency must be a regular file"
        );
    }

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
