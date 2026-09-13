use std::{path::PathBuf, process::Stdio, sync::Arc};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::Semaphore,
};

use super::common::{content_hash, document_id, title_from_metadata};
use super::ocr_systemd::SystemdOcrUnit;
use super::ocr_worker_process::WorkerProcess;
use crate::application::ports::{ApplicationError, OcrEvidenceStore, Parser, RetrievedResource};
use crate::domain::{
    Document, Location, NormalizedBlock, NormalizedBlockKind, NormalizedBlockMap,
    NormalizedBlockProvenance, NormalizedTextRange, OcrConfig, OcrDerivation, OcrEvidenceBlob,
    OcrEvidenceRecord, OcrPageObservations, OcrRuntimeIdentity, OriginalSourceBinding,
    OriginalSourceBindingMap, OriginalSourceTarget, Section, SectionId,
};
use crate::infrastructure::ResourceBudget;

pub const PDF_LAYOUT_CACHE_NAMESPACE: &str = "pdf-layout/v1:pymupdf4llm-layout/1.28.2";
const WORKER: &str = include_str!("pdf_layout_worker.py");
const MAX_OUTPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_OCR_OUTPUT_BYTES: u64 = 32 * 1024 * 1024;

/// Runtime preflight before cache lookup. Direct parser probes may explicitly
/// exercise the unsandboxed adapter, but the MCP runtime has no such fallback.
pub fn require_systemd_ocr_support() -> Result<(), ApplicationError> {
    SystemdOcrUnit::validate_host().map_err(failed)
}

/// Optional layout engine, isolated from the server and bounded by the outer parse timeout.
pub struct LayoutPdfParser {
    python: PathBuf,
    budget: ResourceBudget,
    permit: Arc<Semaphore>,
    evidence_store: Option<Arc<dyn OcrEvidenceStore>>,
    ocr_config: Option<OcrConfig>,
    ocr_identity: Option<OcrRuntimeIdentity>,
    systemd_ocr_sandbox: bool,
    ocr_runtime_root: Option<PathBuf>,
}

impl LayoutPdfParser {
    pub fn new(python: PathBuf, budget: ResourceBudget) -> Self {
        Self {
            python,
            budget,
            permit: Arc::new(Semaphore::new(1)),
            evidence_store: None,
            ocr_config: None,
            ocr_identity: None,
            systemd_ocr_sandbox: false,
            ocr_runtime_root: None,
        }
    }

    pub fn with_evidence_store(mut self, store: Arc<dyn OcrEvidenceStore>) -> Self {
        self.evidence_store = Some(store);
        self
    }
    pub fn with_ocr_config(mut self, config: OcrConfig) -> Self {
        self.ocr_config = Some(config);
        self
    }
    pub fn with_ocr_identity(mut self, identity: OcrRuntimeIdentity) -> Self {
        self.ocr_identity = Some(identity);
        self
    }

    /// Explicit integration entrypoint; never changes native-only layout launch.
    pub fn with_systemd_ocr_sandbox(mut self) -> Self {
        self.systemd_ocr_sandbox = true;
        self
    }

    /// Uses an already verified private runtime; `python` is its internal path.
    /// No host interpreter fallback. Native source-view keeps its own renderer.
    pub fn with_ocr_runtime_root(mut self, root: PathBuf) -> Self {
        self.ocr_runtime_root = Some(root);
        self.systemd_ocr_sandbox = true;
        self
    }
}

fn failed(message: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::ParseFailed(format!("PDF layout: {message}"))
}

fn configure_worker_environment(command: &mut Command, ocr_enabled: bool) {
    if ocr_enabled {
        // This prevents credential/proxy inheritance, not network syscalls.
        // Match dependency discovery; never resolve one library environment
        // and execute the engine under a different one.
        command
            .env_clear()
            .envs(crate::infrastructure::OCR_PROCESS_ENV);
    }
}

fn worker_failure(ocr_enabled: bool, stderr: &[u8]) -> ApplicationError {
    if ocr_enabled {
        // OCR errors can contain input text or local paths. Keep bounded stderr
        // only inside this parse operation, never expose it via MCP/telemetry.
        ApplicationError::OcrFailed
    } else {
        failed(String::from_utf8_lossy(stderr))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OcrWorkerFailure {
    schema: String,
    original_sha256: String,
    runtime_identity_sha256: String,
    error: String,
    // Failure observations are diagnostic only, never published as canonical evidence.
    ocr_attempts: Vec<OcrPageObservations>,
}

fn validated_worker_failure(
    output: &[u8],
    original_sha256: &str,
    identity_sha256: &str,
) -> ApplicationError {
    let Ok(failure) = serde_json::from_slice::<OcrWorkerFailure>(output) else {
        return ApplicationError::OcrFailed;
    };
    if failure.schema == "ocr-worker-failure/v1"
        && failure.original_sha256 == original_sha256
        && failure.runtime_identity_sha256 == identity_sha256
        && failure.error == "OCR_NO_SUPPORTED_PROJECTION"
        && !failure.ocr_attempts.is_empty()
        && failure.ocr_attempts.iter().all(|page| {
            page.schema == "ocr-regional-observations/v3"
                && page.page > 0
                && page.page_bounds.iter().all(|value| value.is_finite())
                && page.page_bounds[0] < page.page_bounds[2]
                && page.page_bounds[1] < page.page_bounds[3]
        })
    {
        ApplicationError::OcrNoSupportedProjection
    } else {
        ApplicationError::OcrFailed
    }
}

async fn read_bounded(
    reader: impl AsyncRead + Unpin,
    limit: u64,
) -> Result<Vec<u8>, ApplicationError> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(failed)?;
    if bytes.len() as u64 > limit {
        return Err(ApplicationError::ResourceLimitExceeded(
            "PDF layout worker output limit exceeded".into(),
        ));
    }
    Ok(bytes)
}

#[async_trait]
impl Parser for LayoutPdfParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let ocr_enabled = self
            .ocr_config
            .as_ref()
            .is_some_and(|config| config.enabled);
        if self.ocr_runtime_root.is_some() && !ocr_enabled {
            return Err(failed("private OCR runtime requires enabled OCR"));
        }
        let permit = self.permit.clone().acquire_owned().await.map_err(failed)?;
        if resource.bytes.len() > self.budget.max_document_bytes {
            return Err(ApplicationError::ResourceLimitExceeded(
                "PDF byte limit exceeded".into(),
            ));
        }
        let (mut command, unit) = if ocr_enabled && self.systemd_ocr_sandbox {
            let (command, unit) = match &self.ocr_runtime_root {
                Some(root) => SystemdOcrUnit::command_in_root(&self.python, root),
                None => SystemdOcrUnit::command(&self.python),
            }
            .map_err(failed)?;
            (command, Some(unit))
        } else {
            (Command::new(&self.python), None)
        };
        configure_worker_environment(&mut command, ocr_enabled);
        #[cfg(unix)]
        command.process_group(0);
        let child = command
            .args(["-I", "-c", WORKER])
            .arg(self.budget.max_pdf_pages.to_string())
            .arg(self.budget.max_document_bytes.to_string())
            .arg(self.budget.max_normalized_chars.to_string())
            .arg(
                self.ocr_config
                    .as_ref()
                    .and_then(|c| serde_json::to_string(c).ok())
                    .unwrap_or_default(),
            )
            .arg(
                self.ocr_identity
                    .as_ref()
                    .and_then(|i| serde_json::to_string(i).ok())
                    .unwrap_or_default(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                failed(format!(
                    "cannot start configured Python: {error}; run setup-pdf-layout.sh"
                ))
            })?;
        let mut process = WorkerProcess::new(child, permit).with_systemd_unit(unit);
        let child = process.child_mut();
        let mut stdin = child.stdin.take().ok_or_else(|| failed("missing stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| failed("missing stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| failed("missing stderr"))?;
        let write = async {
            // A worker may reject dependencies before consuming input. Its exit
            // status/stderr is more useful than a resulting broken pipe.
            let result = stdin.write_all(&resource.bytes).await;
            drop(stdin);
            Ok::<_, ApplicationError>(result)
        };
        let (write_result, output, errors, status) = tokio::try_join!(
            write,
            read_bounded(
                stdout,
                if ocr_enabled {
                    MAX_OCR_OUTPUT_BYTES
                } else {
                    MAX_OUTPUT_BYTES
                }
            ),
            read_bounded(stderr, 64 * 1024),
            async { process.wait().await.map_err(failed) },
        )?;
        if !status.success() {
            if ocr_enabled && let Some(identity) = &self.ocr_identity {
                use sha2::{Digest, Sha256};
                return Err(validated_worker_failure(
                    &output,
                    &format!("{:x}", Sha256::digest(&resource.bytes)),
                    &identity.sha256,
                ));
            }
            return Err(worker_failure(ocr_enabled, &errors));
        }
        write_result.map_err(failed)?;
        let payload: LayoutResult = serde_json::from_slice(&output).map_err(failed)?;
        let evidence = payload.ocr_evidence.clone();
        let derivation = payload.ocr_derivation.clone();
        let attempts = payload.ocr_attempts.clone();
        for page in &attempts {
            for native in &page.native_regions {
                let expected = serde_json::json!({"page":page.page,"box":native.source_box,
                    "bbox":native.bbox,"class":native.source_class});
                if !payload
                    .regions
                    .as_array()
                    .is_some_and(|regions| regions.contains(&expected))
                {
                    return Err(failed(
                        "native exclusion has no matching original layout region",
                    ));
                }
            }
        }
        let page_count = payload.page_count;
        let mut document = project(resource, payload, &self.budget)?;
        if !evidence.is_empty() || !attempts.is_empty() {
            let mut derivation = derivation.ok_or_else(|| failed("OCR derivation missing"))?;
            let identity = self
                .ocr_identity
                .as_ref()
                .ok_or_else(|| failed("OCR expected identity missing"))?;
            derivation
                .validate_against(
                    identity,
                    document.content_hash.0.trim_start_matches("sha256:"),
                )
                .map_err(failed)?;
            if derivation.original_sha256 != document.content_hash.0.trim_start_matches("sha256:")
                || derivation.engine_sha256.len() != 64
                || derivation.model_sha256.is_empty()
                || derivation.library_sha256.is_empty()
                || derivation
                    .model_sha256
                    .iter()
                    .chain(derivation.library_sha256.iter())
                    .any(|v| v.len() != 64)
            {
                return Err(failed("invalid OCR derivation fingerprint"));
            }
            validate_ocr_evidence(
                &evidence,
                document
                    .metadata
                    .get("pdf_pages")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(u32::MAX),
            )?;
            let blob = OcrEvidenceBlob {
                schema: "ocr-evidence/v3".into(),
                original_sha256: derivation.original_sha256.clone(),
                runtime_identity: identity.clone(),
                pages: attempts,
                selected_words: evidence,
            };
            blob.validate(page_count).map_err(failed)?;
            let bytes = serde_json::to_vec(&blob).map_err(failed)?;
            let store = self
                .evidence_store
                .as_ref()
                .ok_or_else(|| failed("OCR evidence store is not configured"))?;
            let identity = document.content_hash.0.clone();
            let digest = store.put_immutable(&identity, &bytes).await?;
            document
                .metadata
                .insert("ocr_evidence_blob".into(), digest.clone());
            derivation.evidence_blob = Some(digest);
            let map = document
                .original_source_binding_map()
                .map_err(failed)?
                .ok_or_else(|| failed("missing binding map"))?;
            let map_bytes = serde_json::to_vec(&map).map_err(failed)?;
            use sha2::{Digest, Sha256};
            let binding_digest = format!("sha256:{:x}", Sha256::digest(map_bytes));
            derivation.binding_map_sha256 = Some(binding_digest.clone());
            document
                .metadata
                .insert("original_binding_map_digest".into(), binding_digest);
            document.metadata.insert(
                "ocr_derivation".into(),
                serde_json::to_string(&derivation).map_err(failed)?,
            );
        }
        document.validate_ocr_publication().map_err(failed)?;
        Ok(document)
    }
}

fn validate_ocr_evidence(
    values: &[OcrEvidenceRecord],
    max_page: u32,
) -> Result<(), ApplicationError> {
    for value in values {
        if value.page == 0
            || value.page > max_page
            || value.block == 0
            || value.paragraph == 0
            || value.line == 0
            || value.text.trim().is_empty()
        {
            return Err(failed("invalid OCR evidence identity"));
        }
        let [x0, y0, x1, y1] = value.bbox;
        if ![x0, y0, x1, y1].iter().all(|v| v.is_finite())
            || x0 < 0.0
            || y0 < 0.0
            || x1 <= x0
            || y1 <= y0
        {
            return Err(failed("invalid OCR evidence coordinates"));
        }
        if let Some(conf) = value.confidence
            && (!conf.is_finite() || !(0.0..=100.0).contains(&conf))
        {
            return Err(failed("invalid OCR confidence"));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct LayoutResult {
    schema_version: String,
    engine: String,
    page_count: u32,
    pages_without_text: u32,
    sections: Vec<LayoutSection>,
    regions: serde_json::Value,
    preserved_ambiguous_hyphens: usize,
    #[serde(default)]
    ocr_evidence: Vec<OcrEvidenceRecord>,
    #[serde(default)]
    ocr_derivation: Option<OcrDerivation>,
    #[serde(default)]
    ocr_attempts: Vec<OcrPageObservations>,
}
#[derive(Deserialize)]
struct LayoutSection {
    title: String,
    blocks: Vec<LayoutBlock>,
}
#[derive(Deserialize)]
struct LayoutBlock {
    text: String,
    kind: NormalizedBlockKind,
    parts: Vec<LayoutPart>,
}
#[derive(Deserialize)]
struct LayoutPart {
    start: usize,
    end: usize,
    page: u32,
}

fn project(
    resource: RetrievedResource,
    layout: LayoutResult,
    budget: &ResourceBudget,
) -> Result<Document, ApplicationError> {
    if layout.schema_version != "pdf-layout/v1" || layout.engine != "pymupdf4llm-layout/1.28.2" {
        return Err(failed("unsupported worker protocol or engine version"));
    }
    if layout.page_count == 0
        || layout.page_count as usize > budget.max_pdf_pages
        || layout.sections.len() > budget.max_sections
    {
        return Err(ApplicationError::ResourceLimitExceeded(
            "PDF layout page/section limit exceeded".into(),
        ));
    }
    let hash = content_hash(&resource.bytes);
    let mut document = Document {
        id: document_id(&resource.final_source, &hash),
        title: title_from_metadata(&resource.metadata, &resource.final_source),
        source: resource.final_source,
        media_type: resource.media_type,
        content_hash: hash,
        metadata: resource.metadata,
        root_sections: vec![],
    };
    let mut blocks = Vec::new();
    let mut bindings: Vec<OriginalSourceBinding> = Vec::new();
    let mut total_chars = 0;
    for (ordinal, input) in layout.sections.into_iter().enumerate() {
        let id = SectionId(format!("section://pdf-layout/{}", ordinal + 1));
        let mut section = Section {
            id: id.clone(),
            parent_id: None,
            title: input.title.clone(),
            level: 1,
            content: String::new(),
            children: vec![],
            location: Location {
                section_path: vec![input.title],
                ..Location::default()
            },
        };
        let mut chars = 0;
        for (block_index, block) in input.blocks.into_iter().enumerate() {
            let length = block.text.chars().count();
            if length == 0 || block.text.trim() != block.text {
                return Err(failed("empty or untrimmed worker block"));
            }
            if chars > 0 {
                section.content.push_str("\n\n");
                chars += 2;
            }
            let start = chars;
            section.content.push_str(&block.text);
            chars += length;
            let mut end = 0;
            for part in &block.parts {
                if part.start < end
                    || part.start >= part.end
                    || part.end > length
                    || part.page == 0
                    || part.page > layout.page_count
                {
                    return Err(failed("invalid worker source range"));
                }
                // Separators may be synthetic spaces; every non-space scalar must
                // be covered by original page evidence.
                if block
                    .text
                    .chars()
                    .skip(end)
                    .take(part.start - end)
                    .any(|c| !c.is_whitespace())
                {
                    return Err(failed("unbound source text"));
                }
                end = part.end;
                let binding_start = start + part.start;
                let target = OriginalSourceTarget::Page {
                    page_number: part.page,
                };
                if let Some(previous) = bindings
                    .last_mut()
                    .filter(|b| b.owner_section_id == id && b.target == target)
                {
                    previous.normalized_range = NormalizedTextRange::new(
                        previous.normalized_range.start(),
                        start + part.end,
                    )
                    .map_err(failed)?;
                } else {
                    bindings.push(OriginalSourceBinding {
                        owner_section_id: id.clone(),
                        normalized_range: NormalizedTextRange::new(binding_start, start + part.end)
                            .map_err(failed)?,
                        target,
                    });
                }
            }
            if end != length {
                return Err(failed("incomplete worker source range"));
            }
            let page = block
                .parts
                .first()
                .filter(|first| block.parts.iter().all(|p| p.page == first.page))
                .map(|p| p.page);
            blocks.push(NormalizedBlock {
                owner_section_id: id.clone(),
                block_index: block_index + 1,
                source_order: blocks.len(),
                kind: block.kind,
                normalized_range: NormalizedTextRange::new(start, chars).map_err(failed)?,
                native_anchor: None,
                native_location: page.map(|p| format!("pdf:page:{p}")),
                provenance: NormalizedBlockProvenance::PdfLayout,
            });
        }
        total_chars += chars;
        if total_chars > budget.max_normalized_chars {
            return Err(ApplicationError::ResourceLimitExceeded(
                "PDF normalized text limit exceeded".into(),
            ));
        }
        // Do not claim a single page for an owner spanning multiple pages.
        let pages: Vec<_> = bindings
            .iter()
            .filter(|b| b.owner_section_id == id)
            .map(|b| match b.target {
                OriginalSourceTarget::Page { page_number } => page_number,
            })
            .collect();
        if let Some(page) = pages
            .first()
            .filter(|first| pages.iter().all(|p| p == *first))
        {
            section.location.page = Some(*page);
            section.location.native_location = Some(format!("pdf:page:{page}"));
        }
        document.root_sections.push(section);
    }
    if !blocks
        .iter()
        .any(|b| b.kind == NormalizedBlockKind::Paragraph)
    {
        return Err(failed("no supported prose blocks"));
    }
    document.metadata.insert(
        "pdf_layout_pages_without_text".into(),
        layout.pages_without_text.to_string(),
    );
    document
        .set_normalized_block_map(NormalizedBlockMap::new(blocks))
        .map_err(failed)?;
    document
        .set_original_source_binding_map(OriginalSourceBindingMap::new(bindings))
        .map_err(failed)?;
    document
        .metadata
        .insert("pdf_layout_engine".into(), layout.engine);
    document
        .metadata
        .insert("pdf_layout_version".into(), layout.schema_version);
    document
        .metadata
        .insert("pdf_layout_regions".into(), layout.regions.to_string());
    document.metadata.insert(
        "pdf_layout_preserved_ambiguous_hyphens".into(),
        layout.preserved_ambiguous_hyphens.to_string(),
    );
    document
        .metadata
        .insert("pdf_structure_provenance".into(), "layout_inferred".into());
    document
        .metadata
        .insert("pdf_pages".into(), layout.page_count.to_string());
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DocumentSource, MediaType};
    use serde_json::json;

    #[cfg(unix)]
    #[tokio::test]
    async fn ocr_worker_environment_is_an_exact_allowlist() {
        let mut command = Command::new("/usr/bin/env");
        command.env("OCR_TEST_SECRET", "synthetic-secret");
        command.env("HTTPS_PROXY", "http://synthetic-proxy.invalid");
        command.env("LD_LIBRARY_PATH", "/synthetic-library-override");
        configure_worker_environment(&mut command, true);
        let output = command.output().await.unwrap();
        assert!(output.status.success());
        let actual: std::collections::BTreeSet<_> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        let expected: std::collections::BTreeSet<_> = crate::infrastructure::OCR_PROCESS_ENV
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn native_worker_environment_configuration_remains_unchanged() {
        let mut command = Command::new("python");
        command.env("NATIVE_LAYOUT_SETTING", "preserve");
        configure_worker_environment(&mut command, false);
        assert!(
            command
                .as_std()
                .get_envs()
                .any(|(key, value)| key == "NATIVE_LAYOUT_SETTING"
                    && value == Some(std::ffi::OsStr::new("preserve")))
        );
    }

    #[test]
    fn failure_protocol_requires_matching_identity_and_typed_observations() {
        let raw = "a".repeat(64);
        let identity = "b".repeat(64);
        let valid = serde_json::json!({
            "schema":"ocr-worker-failure/v1", "original_sha256":raw,
            "runtime_identity_sha256":identity, "error":"OCR_NO_SUPPORTED_PROJECTION",
            "ocr_attempts":[{"schema":"ocr-regional-observations/v3", "page":1,
                "page_bounds":[0,0,595,842], "complete":false,
                "attempts":[], "selection":[], "components":[]}]
        });
        let check = |value: &serde_json::Value| {
            validated_worker_failure(&serde_json::to_vec(value).unwrap(), &raw, &identity)
        };
        assert!(matches!(
            check(&valid),
            ApplicationError::OcrNoSupportedProjection
        ));
        for field in [
            "schema",
            "original_sha256",
            "runtime_identity_sha256",
            "error",
        ] {
            let mut invalid = valid.clone();
            invalid[field] = serde_json::json!("private invalid value");
            assert!(matches!(check(&invalid), ApplicationError::OcrFailed));
            assert!(!check(&invalid).to_string().contains("private"));
        }
        let mut invalid = valid.clone();
        invalid["ocr_attempts"][0]["page"] = serde_json::json!("one");
        assert!(matches!(check(&invalid), ApplicationError::OcrFailed));
        invalid = valid.clone();
        invalid["unexpected"] = serde_json::json!("private diagnostic");
        assert!(matches!(check(&invalid), ApplicationError::OcrFailed));
        assert!(matches!(
            validated_worker_failure(b"invalid JSON", &raw, &identity),
            ApplicationError::OcrFailed
        ));
    }

    #[test]
    fn ocr_stderr_is_not_exposed_but_native_diagnostics_remain_compatible() {
        let diagnostic = b"private document passage at /private/source.pdf";
        let error = worker_failure(true, diagnostic).to_string();
        assert!(error.contains("local OCR worker failed"));
        assert!(!error.contains("private"));
        assert!(
            worker_failure(false, diagnostic)
                .to_string()
                .contains("private document passage")
        );
        assert_eq!(MAX_OCR_OUTPUT_BYTES, 32 * 1024 * 1024);
        assert_eq!(MAX_OUTPUT_BYTES, 128 * 1024 * 1024);
    }

    #[tokio::test]
    async fn worker_output_rejects_the_first_byte_over_limit() {
        assert_eq!(read_bounded(&b"abc"[..], 3).await.unwrap(), b"abc");
        assert!(matches!(
            read_bounded(&b"abcd"[..], 3).await,
            Err(ApplicationError::ResourceLimitExceeded(_))
        ));
    }

    fn resource() -> RetrievedResource {
        let source = DocumentSource("https://example.org/paper.pdf".into());
        RetrievedResource {
            source: source.clone(),
            final_source: source,
            media_type: MediaType("application/pdf".into()),
            bytes: b"original PDF identity".to_vec(),
            etag: None,
            last_modified: None,
            metadata: Default::default(),
        }
    }
    fn payload() -> serde_json::Value {
        json!({"schema_version":"pdf-layout/v1", "engine":"pymupdf4llm-layout/1.28.2", "page_count":2, "pages_without_text":0, "regions":[], "preserved_ambiguous_hyphens":0,
        "sections":[{"title":"Abstract", "blocks":[
          {"text":"甲😀 sentence.", "kind":"paragraph", "parts":[{"start":0,"end":12,"page":1}]},
          {"text":"Next page.", "kind":"paragraph", "parts":[{"start":0,"end":10,"page":2}]}
        ]}]})
    }
    #[test]
    fn projection_preserves_original_identity_scalar_ranges_and_page_bindings() {
        let input = resource();
        let document = project(
            input.clone(),
            serde_json::from_value(payload()).unwrap(),
            &ResourceBudget::default(),
        )
        .unwrap();
        assert_eq!(document.source, input.final_source);
        assert_eq!(document.content_hash, content_hash(&input.bytes));
        assert_eq!(
            document.id,
            document_id(&input.final_source, &document.content_hash)
        );
        let section = &document.root_sections[0];
        assert_eq!(section.content, "甲😀 sentence.\n\nNext page.");
        assert_eq!(section.location.page, None);
        assert_eq!(
            document.normalized_block_map().unwrap().unwrap().blocks[1].normalized_range,
            NormalizedTextRange::new(14, 24).unwrap()
        );
        assert_eq!(
            document
                .original_source_target_for_range(
                    &section.id,
                    NormalizedTextRange::new(0, 12).unwrap()
                )
                .unwrap(),
            Some(OriginalSourceTarget::Page { page_number: 1 })
        );
        assert!(
            document
                .original_source_target_for_range(
                    &section.id,
                    NormalizedTextRange::new(0, 24).unwrap()
                )
                .is_err()
        );
    }
    #[test]
    fn rejects_unbound_text_unknown_version_and_out_of_bounds_pages() {
        for (key, value) in [("start", json!(2)), ("end", json!(999)), ("page", json!(3))] {
            let mut data = payload();
            data["sections"][0]["blocks"][0]["parts"][0][key] = value;
            assert!(
                project(
                    resource(),
                    serde_json::from_value(data).unwrap(),
                    &ResourceBudget::default()
                )
                .is_err()
            );
        }
        let mut data = payload();
        data["schema_version"] = json!("pdf-layout/future");
        assert!(
            project(
                resource(),
                serde_json::from_value(data).unwrap(),
                &ResourceBudget::default()
            )
            .is_err()
        );
    }
    #[tokio::test]
    async fn missing_engine_fails_explicitly() {
        let parser = LayoutPdfParser::new(
            PathBuf::from("/nonexistent/issue89/python"),
            ResourceBudget::default(),
        );
        assert!(
            parser
                .parse(resource())
                .await
                .unwrap_err()
                .to_string()
                .contains("cannot start configured Python")
        );
    }
}
