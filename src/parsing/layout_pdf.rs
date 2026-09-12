use std::{path::PathBuf, process::Stdio, sync::Arc};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::Semaphore,
};

use super::common::{content_hash, document_id, title_from_metadata};
use crate::application::ports::{ApplicationError, OcrEvidenceStore, Parser, RetrievedResource};
use crate::domain::{
    Document, Location, NormalizedBlock, NormalizedBlockKind, NormalizedBlockMap,
    NormalizedBlockProvenance, NormalizedTextRange, OcrConfig, OcrDerivation, OcrEvidenceRecord,
    OcrRuntimeIdentity, OriginalSourceBinding, OriginalSourceBindingMap, OriginalSourceTarget,
    Section, SectionId,
};
use crate::infrastructure::ResourceBudget;

pub const PDF_LAYOUT_CACHE_NAMESPACE: &str = "pdf-layout/v1:pymupdf4llm-layout/1.28.2";
const WORKER: &str = include_str!("pdf_layout_worker.py");
const MAX_OUTPUT_BYTES: u64 = 128 * 1024 * 1024;

/// Optional layout engine, isolated from the server and bounded by the outer parse timeout.
pub struct LayoutPdfParser {
    python: PathBuf,
    budget: ResourceBudget,
    permit: Semaphore,
    evidence_store: Option<Arc<dyn OcrEvidenceStore>>,
    ocr_config: Option<OcrConfig>,
    ocr_identity: Option<OcrRuntimeIdentity>,
}

impl LayoutPdfParser {
    pub fn new(python: PathBuf, budget: ResourceBudget) -> Self {
        Self {
            python,
            budget,
            permit: Semaphore::new(1),
            evidence_store: None,
            ocr_config: None,
            ocr_identity: None,
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
}

fn failed(message: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::ParseFailed(format!("PDF layout: {message}"))
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
        let _permit = self.permit.acquire().await.map_err(failed)?;
        if resource.bytes.len() > self.budget.max_document_bytes {
            return Err(ApplicationError::ResourceLimitExceeded(
                "PDF byte limit exceeded".into(),
            ));
        }
        let mut child = Command::new(&self.python)
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
            read_bounded(stdout, MAX_OUTPUT_BYTES),
            read_bounded(stderr, 64 * 1024),
            async { child.wait().await.map_err(failed) },
        )?;
        if !status.success() {
            return Err(failed(String::from_utf8_lossy(&errors)));
        }
        write_result.map_err(failed)?;
        let payload: LayoutResult = serde_json::from_slice(&output).map_err(failed)?;
        let evidence = payload.ocr_evidence.clone();
        let derivation = payload.ocr_derivation.clone();
        let mut document = project(resource, payload, &self.budget)?;
        if !evidence.is_empty() {
            let derivation = derivation.ok_or_else(|| failed("OCR derivation missing"))?;
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
            let bytes = serde_json::to_vec(&evidence).map_err(failed)?;
            let store = self
                .evidence_store
                .as_ref()
                .ok_or_else(|| failed("OCR evidence store is not configured"))?;
            let identity = document.content_hash.0.clone();
            let digest = store.put_immutable(&identity, &bytes).await?;
            document.metadata.insert("ocr_evidence_blob".into(), digest);
            let map = document
                .original_source_binding_map()
                .map_err(failed)?
                .ok_or_else(|| failed("missing binding map"))?;
            let map_bytes = serde_json::to_vec(&map).map_err(failed)?;
            use sha2::{Digest, Sha256};
            document.metadata.insert(
                "original_binding_map_digest".into(),
                format!("sha256:{:x}", Sha256::digest(map_bytes)),
            );
            document.metadata.insert(
                "ocr_derivation".into(),
                serde_json::to_string(&derivation).map_err(failed)?,
            );
        }
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
