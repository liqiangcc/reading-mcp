use std::collections::BTreeSet;

use crate::application::ports::{ApplicationError, DocumentReliabilityInspector};
use crate::application::reading_profile::{
    NavigationResolutionCoverage, PublicationCoverage, ReliabilityEvidence, ReliabilityIntegrity,
    ReliabilitySummary, StructureProvenanceCoverage,
};
use crate::domain::{Document, OcrDerivation, ParagraphContentClass, SentenceEligibility};

use super::epub_validator::{
    EPUB_VALIDATION_DEGRADATIONS_METADATA_KEY, EPUB_VALIDATION_ERRORS_METADATA_KEY,
    EPUB_VALIDATION_INTEGRITY_METADATA_KEY, EPUB_VALIDATION_REPORT_METADATA_KEY,
    EPUB_VALIDATION_REPORT_VERSION, EPUB_VALIDATION_REPORT_VERSION_METADATA_KEY,
    EpubValidationIntegrity, EpubValidationReport, EpubValidationSeverity, validate_epub_document,
};

const EPUB_MEDIA_TYPE: &str = "application/epub+zip";
const MAX_PROFILE_DEGRADATION_CODES: usize = 16;

#[derive(Clone, Copy, Debug, Default)]
pub struct PersistedDocumentReliabilityInspector;

impl DocumentReliabilityInspector for PersistedDocumentReliabilityInspector {
    fn inspect(&self, document: &Document) -> Result<ReliabilitySummary, ApplicationError> {
        if let Some(version) = document.metadata.get("pdf_layout_version") {
            let invalid =
                || ApplicationError::ParseFailed("invalid persisted PDF layout evidence".into());
            if version != "pdf-layout/v1"
                || document
                    .media_type
                    .0
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    != "application/pdf"
            {
                return Err(invalid());
            }
            document
                .normalized_block_map()
                .map_err(|_| invalid())?
                .ok_or_else(invalid)?;
            document
                .original_source_binding_map()
                .map_err(|_| invalid())?
                .ok_or_else(invalid)?;
            let count = document
                .metadata
                .get("pdf_layout_preserved_ambiguous_hyphens")
                .and_then(|v| v.parse::<usize>().ok())
                .ok_or_else(invalid)?;
            let missing = document
                .metadata
                .get("pdf_layout_pages_without_text")
                .and_then(|v| v.parse::<usize>().ok())
                .ok_or_else(invalid)?;
            document.validate_ocr_publication().map_err(|_| invalid())?;
            let derived = OcrDerivation::from_metadata(&document.metadata)
                .map_err(|_| invalid())?
                .is_some_and(|derivation| {
                    derivation
                        .selected_word_count
                        .is_some_and(|count| count > 0)
                });
            // Persisted evidence says whether local OCR contributed, not whether
            // the current deployment switch is enabled. Valid provenance does
            // not establish recognition accuracy, even with high confidence.
            let mut codes = vec![
                "pdf_layout_inferred".into(),
                if derived {
                    "pdf_local_ocr_unverified"
                } else {
                    "pdf_local_ocr_not_applied"
                }
                .into(),
            ];
            if count > 0 {
                codes.push("pdf_ambiguous_hyphens_preserved".into());
            }
            if missing > 0 {
                codes.push("pdf_pages_without_text".into());
            }
            let mut summary = ReliabilitySummary::not_applicable();
            summary.evidence = vec![ReliabilityEvidence {
                kind: "pdf_layout_projection".into(),
                schema_version: Some(version.clone()),
                integrity: ReliabilityIntegrity::Valid,
                degradation_count: 2usize.saturating_add(count).saturating_add(missing),
                degradation_codes: codes,
            }];
            return Ok(summary);
        }
        let media_type = document
            .media_type
            .0
            .split(';')
            .next()
            .unwrap_or_default()
            .trim();
        if media_type.eq_ignore_ascii_case("application/pdf") {
            return inspect_fallback_pdf(document);
        }
        if document.media_type.0 != EPUB_MEDIA_TYPE {
            return Ok(ReliabilitySummary::not_applicable());
        }

        let stored = decode_required_report(document)?;
        validate_stored_summary_metadata(document, &stored)?;

        let revalidated = validate_epub_document(document);
        if revalidated.integrity != EpubValidationIntegrity::Valid || revalidated.error_count > 0 {
            return Err(ApplicationError::ParseFailed(format!(
                "persisted EPUB reliability evidence is internally invalid: {} validation error(s)",
                revalidated.error_count
            )));
        }
        if stored != revalidated {
            return Err(ApplicationError::ParseFailed(
                "persisted EPUB validation report does not match revalidation from canonical facts"
                    .into(),
            ));
        }

        Ok(project_epub_report(&stored))
    }
}

/// PDFs parsed without persisted layout evidence (lopdf native-ToC / inferred-heading /
/// page-fallback paths) have no per-block provenance, so fallback paragraphs are
/// classified from text alone. Surface the count of paragraphs that were kept coarse
/// because their merged/corrupted text cannot be trusted to produce clean sentences;
/// otherwise `degradation_count=0` would masquerade as a clean extraction.
fn inspect_fallback_pdf(document: &Document) -> Result<ReliabilitySummary, ApplicationError> {
    let invalid = || ApplicationError::ParseFailed("invalid persisted PDF evidence".into());
    let sentences = document.try_sentence_text_units().map_err(|_| invalid())?;
    let unreliable = sentences
        .coverage
        .iter()
        .filter(|coverage| {
            coverage.eligibility == SentenceEligibility::CoarseParagraphOnly
                && coverage.content_class == ParagraphContentClass::ProseOrUnknown
        })
        .count();
    let extraction_errors = document
        .metadata
        .get("pdf_text_extraction_errors")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);

    if unreliable == 0 && extraction_errors == 0 {
        return Ok(ReliabilitySummary::not_applicable());
    }

    let mut codes = Vec::new();
    if unreliable > 0 {
        codes.push("pdf_fallback_paragraph_unreliable".to_string());
    }
    if extraction_errors > 0 {
        codes.push("pdf_text_extraction_errors".to_string());
    }

    let mut summary = ReliabilitySummary::not_applicable();
    summary.evidence = vec![ReliabilityEvidence {
        kind: "pdf_text_extraction".into(),
        schema_version: None,
        integrity: ReliabilityIntegrity::Valid,
        degradation_count: unreliable.saturating_add(extraction_errors),
        degradation_codes: codes,
    }];
    Ok(summary)
}

fn decode_required_report(document: &Document) -> Result<EpubValidationReport, ApplicationError> {
    let json = required_metadata(document, EPUB_VALIDATION_REPORT_METADATA_KEY)?;
    let report = serde_json::from_str::<EpubValidationReport>(json).map_err(|error| {
        ApplicationError::ParseFailed(format!(
            "persisted EPUB validation report cannot be decoded: {error}"
        ))
    })?;

    if report.schema_version != EPUB_VALIDATION_REPORT_VERSION {
        return Err(ApplicationError::ParseFailed(format!(
            "persisted EPUB validation report schema mismatch: expected {EPUB_VALIDATION_REPORT_VERSION}, got {}",
            report.schema_version
        )));
    }
    if report.integrity != EpubValidationIntegrity::Valid || report.error_count > 0 {
        return Err(ApplicationError::ParseFailed(format!(
            "persisted EPUB validation report is invalid: {} error(s)",
            report.error_count
        )));
    }

    Ok(report)
}

fn validate_stored_summary_metadata(
    document: &Document,
    report: &EpubValidationReport,
) -> Result<(), ApplicationError> {
    let version = required_metadata(document, EPUB_VALIDATION_REPORT_VERSION_METADATA_KEY)?;
    if version != report.schema_version {
        return Err(ApplicationError::ParseFailed(format!(
            "persisted EPUB validation version summary mismatch: metadata={version}, report={}",
            report.schema_version
        )));
    }

    let integrity = required_metadata(document, EPUB_VALIDATION_INTEGRITY_METADATA_KEY)?;
    if integrity != report.integrity.as_str() {
        return Err(ApplicationError::ParseFailed(format!(
            "persisted EPUB validation integrity summary mismatch: metadata={integrity}, report={}",
            report.integrity.as_str()
        )));
    }

    validate_count_metadata(
        document,
        EPUB_VALIDATION_ERRORS_METADATA_KEY,
        report.error_count,
    )?;
    validate_count_metadata(
        document,
        EPUB_VALIDATION_DEGRADATIONS_METADATA_KEY,
        report.degradation_count,
    )?;

    Ok(())
}

fn validate_count_metadata(
    document: &Document,
    key: &str,
    expected: usize,
) -> Result<(), ApplicationError> {
    let value = required_metadata(document, key)?;
    let parsed = value.parse::<usize>().map_err(|error| {
        ApplicationError::ParseFailed(format!(
            "persisted EPUB validation summary {key} is not a count: {error}"
        ))
    })?;
    if parsed != expected {
        return Err(ApplicationError::ParseFailed(format!(
            "persisted EPUB validation summary {key} mismatch: metadata={parsed}, report={expected}"
        )));
    }
    Ok(())
}

fn required_metadata<'a>(document: &'a Document, key: &str) -> Result<&'a str, ApplicationError> {
    document
        .metadata
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| {
            ApplicationError::ParseFailed(format!(
                "persisted EPUB document is missing required reliability evidence: {key}"
            ))
        })
}

fn project_epub_report(report: &EpubValidationReport) -> ReliabilitySummary {
    let degradation_codes = report
        .findings
        .iter()
        .filter(|finding| finding.severity == EpubValidationSeverity::Degradation)
        .map(|finding| finding.code.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(MAX_PROFILE_DEGRADATION_CODES)
        .collect();

    let package = &report.coverage.package_spine;
    let structure = &report.coverage.structure;
    let navigation = &report.coverage.navigation;

    ReliabilitySummary {
        evidence: vec![ReliabilityEvidence {
            kind: "epub_structure_validator".into(),
            schema_version: Some(report.schema_version.clone()),
            integrity: ReliabilityIntegrity::Valid,
            degradation_count: report.degradation_count,
            degradation_codes,
        }],
        publication_coverage: Some(PublicationCoverage {
            source_units_total: package.spine_items_total,
            source_units_represented: package.spine_items_parsed,
            source_units_missing: package.spine_items_missing_manifest,
            source_units_unsupported: package.spine_items_unsupported_media,
        }),
        structure_provenance: Some(StructureProvenanceCoverage {
            native_navigation_sections: structure.sections_epub_nav,
            legacy_navigation_sections: structure.sections_epub_ncx,
            heading_fallback_sections: structure.sections_xhtml_heading,
            source_item_fallback_sections: structure.sections_spine_item,
        }),
        navigation_resolution: Some(NavigationResolutionCoverage {
            targets_total: navigation.nodes_total,
            targets_resolved: navigation.resolved_nodes,
            targets_unresolved_or_unsupported: navigation
                .nodes_total
                .saturating_sub(navigation.resolved_nodes),
        }),
    }
}
