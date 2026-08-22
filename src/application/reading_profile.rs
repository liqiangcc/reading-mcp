use crate::application::ports::ApplicationError;
use crate::domain::{
    Document, ParagraphTextUnitSet, SentenceEligibility, SentenceTextUnitSet,
    TEXT_SEGMENTATION_VERSION,
};

pub const READING_PROFILE_SCHEMA_VERSION: &str = "reading-profile/v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadingCapabilityAvailability {
    Available,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructureCapability {
    pub availability: ReadingCapabilityAvailability,
    pub section_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentedReadingCapability {
    pub availability: ReadingCapabilityAvailability,
    pub segmentation_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentenceFirstCapability {
    pub availability: ReadingCapabilityAvailability,
    pub segmentation_version: String,
    pub source_preserving_coarse_regions: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimpleReadingCapability {
    pub availability: ReadingCapabilityAvailability,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexicalSearchCapability {
    pub availability: ReadingCapabilityAvailability,
    pub precise_candidates: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadingCapabilities {
    pub structural_navigation: StructureCapability,
    pub paragraph_enumeration: SegmentedReadingCapability,
    pub sentence_first_enumeration: SentenceFirstCapability,
    pub exact_locator_read: SimpleReadingCapability,
    pub locator_context: SimpleReadingCapability,
    pub lexical_search: LexicalSearchCapability,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CanonicalTextCoverage {
    pub owner_chars: usize,
    pub paragraph_chars: usize,
    pub paragraph_separator_chars: usize,
    pub paragraph_count: usize,
    pub native_paragraph_chars: usize,
    pub native_structural_container_chars: usize,
    pub native_non_prose_chars: usize,
    pub fallback_chars: usize,
    pub sentence_eligible_paragraphs: usize,
    pub coarse_paragraphs: usize,
    pub sentence_count: usize,
    pub sentence_chars: usize,
    pub sentence_separator_chars: usize,
    pub sentence_coarse_only_chars: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReliabilityIntegrity {
    Valid,
    Invalid,
    NotApplicable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReliabilityEvidence {
    pub kind: String,
    pub schema_version: Option<String>,
    pub integrity: ReliabilityIntegrity,
    pub degradation_count: usize,
    pub degradation_codes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationCoverage {
    pub source_units_total: usize,
    pub source_units_represented: usize,
    pub source_units_missing: usize,
    pub source_units_unsupported: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructureProvenanceCoverage {
    pub native_navigation_sections: usize,
    pub legacy_navigation_sections: usize,
    pub heading_fallback_sections: usize,
    pub source_item_fallback_sections: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationResolutionCoverage {
    pub targets_total: usize,
    pub targets_resolved: usize,
    pub targets_unresolved_or_unsupported: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReliabilitySummary {
    pub evidence: Vec<ReliabilityEvidence>,
    pub publication_coverage: Option<PublicationCoverage>,
    pub structure_provenance: Option<StructureProvenanceCoverage>,
    pub navigation_resolution: Option<NavigationResolutionCoverage>,
}

impl ReliabilitySummary {
    pub fn not_applicable() -> Self {
        Self {
            evidence: vec![ReliabilityEvidence {
                kind: "format_validator".into(),
                schema_version: None,
                integrity: ReliabilityIntegrity::NotApplicable,
                degradation_count: 0,
                degradation_codes: Vec::new(),
            }],
            publication_coverage: None,
            structure_provenance: None,
            navigation_resolution: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadingProfile {
    pub schema_version: String,
    pub capabilities: ReadingCapabilities,
    pub canonical_text_coverage: CanonicalTextCoverage,
    pub reliability: ReliabilitySummary,
}

pub fn build_reading_profile(
    document: &Document,
    paragraphs: &ParagraphTextUnitSet,
    sentences: &SentenceTextUnitSet,
    reliability: ReliabilitySummary,
    precise_lexical_candidates: bool,
) -> Result<ReadingProfile, ApplicationError> {
    let mut coverage = CanonicalTextCoverage::default();

    for section in &paragraphs.coverage {
        coverage.owner_chars += section.owner_chars;
        coverage.paragraph_chars += section.paragraph_chars;
        coverage.paragraph_separator_chars += section.separator_chars;
        coverage.paragraph_count += section.paragraph_count;
        coverage.native_paragraph_chars += section.native_paragraph_chars;
        coverage.native_structural_container_chars += section.native_structural_container_chars;
        coverage.native_non_prose_chars += section.native_non_prose_chars;
        coverage.fallback_chars += section.fallback_chars;
    }

    for paragraph in &sentences.coverage {
        match paragraph.eligibility {
            SentenceEligibility::Eligible => coverage.sentence_eligible_paragraphs += 1,
            SentenceEligibility::CoarseParagraphOnly => coverage.coarse_paragraphs += 1,
        }
        coverage.sentence_count += paragraph.sentence_count;
        coverage.sentence_chars += paragraph.sentence_chars;
        coverage.sentence_separator_chars += paragraph.separator_chars;
        coverage.sentence_coarse_only_chars += paragraph.coarse_only_chars;
    }

    validate_coverage(&coverage, sentences.coverage.len())?;

    let coarse_regions = coverage.coarse_paragraphs > 0 || coverage.sentence_coarse_only_chars > 0;
    let available = ReadingCapabilityAvailability::Available;

    Ok(ReadingProfile {
        schema_version: READING_PROFILE_SCHEMA_VERSION.into(),
        capabilities: ReadingCapabilities {
            structural_navigation: StructureCapability {
                availability: available,
                section_count: document.section_count(),
            },
            paragraph_enumeration: SegmentedReadingCapability {
                availability: available,
                segmentation_version: TEXT_SEGMENTATION_VERSION.into(),
            },
            sentence_first_enumeration: SentenceFirstCapability {
                availability: available,
                segmentation_version: TEXT_SEGMENTATION_VERSION.into(),
                source_preserving_coarse_regions: coarse_regions,
            },
            exact_locator_read: SimpleReadingCapability {
                availability: available,
            },
            locator_context: SimpleReadingCapability {
                availability: available,
            },
            lexical_search: LexicalSearchCapability {
                availability: available,
                precise_candidates: precise_lexical_candidates,
            },
        },
        canonical_text_coverage: coverage,
        reliability,
    })
}

fn validate_coverage(
    coverage: &CanonicalTextCoverage,
    sentence_paragraphs: usize,
) -> Result<(), ApplicationError> {
    let partitioned_owner_chars = coverage
        .paragraph_chars
        .checked_add(coverage.paragraph_separator_chars)
        .ok_or_else(|| {
            ApplicationError::TextUnitIndexFailed(
                "reading profile Paragraph coverage overflowed".into(),
            )
        })?;
    if partitioned_owner_chars != coverage.owner_chars {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "reading profile Paragraph coverage does not partition canonical Section content: owner_chars={}, paragraph_chars={}, separator_chars={}",
            coverage.owner_chars, coverage.paragraph_chars, coverage.paragraph_separator_chars
        )));
    }

    if sentence_paragraphs != coverage.paragraph_count {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "reading profile Sentence coverage does not contain one record per Paragraph: paragraph_count={}, sentence_coverage={sentence_paragraphs}",
            coverage.paragraph_count
        )));
    }

    let classified_paragraphs = coverage
        .sentence_eligible_paragraphs
        .checked_add(coverage.coarse_paragraphs)
        .ok_or_else(|| {
            ApplicationError::TextUnitIndexFailed(
                "reading profile Sentence eligibility coverage overflowed".into(),
            )
        })?;
    if classified_paragraphs != coverage.paragraph_count {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "reading profile Sentence eligibility does not classify every Paragraph: paragraph_count={}, classified={classified_paragraphs}",
            coverage.paragraph_count
        )));
    }

    let represented_paragraph_chars = coverage
        .sentence_chars
        .checked_add(coverage.sentence_separator_chars)
        .and_then(|value| value.checked_add(coverage.sentence_coarse_only_chars))
        .ok_or_else(|| {
            ApplicationError::TextUnitIndexFailed(
                "reading profile Sentence coverage overflowed".into(),
            )
        })?;
    if represented_paragraph_chars != coverage.paragraph_chars {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "reading profile Sentence/coarse coverage does not partition Paragraph content: paragraph_chars={}, sentence_chars={}, sentence_separator_chars={}, coarse_only_chars={}",
            coverage.paragraph_chars,
            coverage.sentence_chars,
            coverage.sentence_separator_chars,
            coverage.sentence_coarse_only_chars
        )));
    }

    Ok(())
}
