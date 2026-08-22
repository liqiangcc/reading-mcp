use crate::application::reading_profile::{
    ReadingCapabilityAvailability, ReadingProfile, ReliabilityIntegrity,
};

use super::contracts::{
    CanonicalTextCoverageDto, LexicalSearchCapabilityDto, NavigationResolutionCoverageDto,
    PublicationCoverageDto, ReadingCapabilitiesDto, ReadingCapabilityAvailabilityDto,
    ReadingProfileDto, ReliabilityEvidenceDto, ReliabilityIntegrityDto, ReliabilitySummaryDto,
    SegmentedReadingCapabilityDto, SentenceFirstCapabilityDto, SimpleReadingCapabilityDto,
    StructureCapabilityDto, StructureProvenanceCoverageDto,
};

impl From<ReadingProfile> for ReadingProfileDto {
    fn from(profile: ReadingProfile) -> Self {
        let capabilities = profile.capabilities;
        let coverage = profile.canonical_text_coverage;
        let reliability = profile.reliability;

        Self {
            schema_version: profile.schema_version,
            capabilities: ReadingCapabilitiesDto {
                structural_navigation: StructureCapabilityDto {
                    availability: availability_dto(
                        capabilities.structural_navigation.availability,
                    ),
                    section_count: capabilities.structural_navigation.section_count,
                },
                paragraph_enumeration: SegmentedReadingCapabilityDto {
                    availability: availability_dto(
                        capabilities.paragraph_enumeration.availability,
                    ),
                    segmentation_version: capabilities.paragraph_enumeration.segmentation_version,
                },
                sentence_first_enumeration: SentenceFirstCapabilityDto {
                    availability: availability_dto(
                        capabilities.sentence_first_enumeration.availability,
                    ),
                    segmentation_version: capabilities
                        .sentence_first_enumeration
                        .segmentation_version,
                    source_preserving_coarse_regions: capabilities
                        .sentence_first_enumeration
                        .source_preserving_coarse_regions,
                },
                exact_locator_read: SimpleReadingCapabilityDto {
                    availability: availability_dto(capabilities.exact_locator_read.availability),
                },
                locator_context: SimpleReadingCapabilityDto {
                    availability: availability_dto(capabilities.locator_context.availability),
                },
                lexical_search: LexicalSearchCapabilityDto {
                    availability: availability_dto(capabilities.lexical_search.availability),
                    precise_candidates: capabilities.lexical_search.precise_candidates,
                },
            },
            canonical_text_coverage: CanonicalTextCoverageDto {
                owner_chars: coverage.owner_chars,
                paragraph_chars: coverage.paragraph_chars,
                paragraph_separator_chars: coverage.paragraph_separator_chars,
                paragraph_count: coverage.paragraph_count,
                native_paragraph_chars: coverage.native_paragraph_chars,
                native_structural_container_chars: coverage.native_structural_container_chars,
                native_non_prose_chars: coverage.native_non_prose_chars,
                fallback_chars: coverage.fallback_chars,
                sentence_eligible_paragraphs: coverage.sentence_eligible_paragraphs,
                coarse_paragraphs: coverage.coarse_paragraphs,
                sentence_count: coverage.sentence_count,
                sentence_chars: coverage.sentence_chars,
                sentence_separator_chars: coverage.sentence_separator_chars,
                sentence_coarse_only_chars: coverage.sentence_coarse_only_chars,
            },
            reliability: ReliabilitySummaryDto {
                evidence: reliability
                    .evidence
                    .into_iter()
                    .map(|evidence| ReliabilityEvidenceDto {
                        kind: evidence.kind,
                        schema_version: evidence.schema_version,
                        integrity: integrity_dto(evidence.integrity),
                        degradation_count: evidence.degradation_count,
                        degradation_codes: evidence.degradation_codes,
                    })
                    .collect(),
                publication_coverage: reliability.publication_coverage.map(|coverage| {
                    PublicationCoverageDto {
                        source_units_total: coverage.source_units_total,
                        source_units_represented: coverage.source_units_represented,
                        source_units_missing: coverage.source_units_missing,
                        source_units_unsupported: coverage.source_units_unsupported,
                    }
                }),
                structure_provenance: reliability.structure_provenance.map(|coverage| {
                    StructureProvenanceCoverageDto {
                        native_navigation_sections: coverage.native_navigation_sections,
                        legacy_navigation_sections: coverage.legacy_navigation_sections,
                        heading_fallback_sections: coverage.heading_fallback_sections,
                        source_item_fallback_sections: coverage.source_item_fallback_sections,
                    }
                }),
                navigation_resolution: reliability.navigation_resolution.map(|coverage| {
                    NavigationResolutionCoverageDto {
                        targets_total: coverage.targets_total,
                        targets_resolved: coverage.targets_resolved,
                        targets_unresolved_or_unsupported: coverage
                            .targets_unresolved_or_unsupported,
                    }
                }),
            },
        }
    }
}

fn availability_dto(value: ReadingCapabilityAvailability) -> ReadingCapabilityAvailabilityDto {
    match value {
        ReadingCapabilityAvailability::Available => ReadingCapabilityAvailabilityDto::Available,
        ReadingCapabilityAvailability::Unavailable => ReadingCapabilityAvailabilityDto::Unavailable,
    }
}

fn integrity_dto(value: ReliabilityIntegrity) -> ReliabilityIntegrityDto {
    match value {
        ReliabilityIntegrity::Valid => ReliabilityIntegrityDto::Valid,
        ReliabilityIntegrity::Invalid => ReliabilityIntegrityDto::Invalid,
        ReliabilityIntegrity::NotApplicable => ReliabilityIntegrityDto::NotApplicable,
    }
}
