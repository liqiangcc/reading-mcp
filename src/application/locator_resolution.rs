use crate::application::ports::ApplicationError;
use crate::domain::{Document, NormalizedTextRange, TEXT_SEGMENTATION_VERSION, TextLocator};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResolvedLocatorKind {
    Section,
    CharacterRange,
    Paragraph,
    Sentence,
}

impl ResolvedLocatorKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Section => "section",
            Self::CharacterRange => "character_range",
            Self::Paragraph => "paragraph",
            Self::Sentence => "sentence",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedTextLocator {
    pub(crate) locator: TextLocator,
    pub(crate) kind: ResolvedLocatorKind,
    pub(crate) range: NormalizedTextRange,
}

pub(crate) fn resolve_text_locator(
    document: &Document,
    locator: &TextLocator,
) -> Result<ResolvedTextLocator, ApplicationError> {
    if locator.document_id != document.id {
        return Err(ApplicationError::InvalidLocator(format!(
            "locator document {} does not match requested document {}",
            locator.document_id.0, document.id.0
        )));
    }
    if locator.content_hash != document.content_hash {
        return Err(ApplicationError::StaleLocator(format!(
            "raw content hash changed from {} to {}",
            locator.content_hash.0, document.content_hash.0
        )));
    }

    let normalized_hash = document.normalized_document_hash();
    if locator.normalized_document_hash != normalized_hash {
        return Err(ApplicationError::StaleLocator(format!(
            "normalized document hash changed from {} to {}",
            locator.normalized_document_hash.0, normalized_hash.0
        )));
    }

    let section = document
        .find_section(&locator.owner_section_id)
        .ok_or_else(|| {
            ApplicationError::InvalidLocator(format!(
                "locator owner section {} does not exist",
                locator.owner_section_id.0
            ))
        })?;

    match (
        locator.paragraph_index,
        locator.sentence_index,
        locator.normalized_range,
        locator.segmentation_version.as_deref(),
    ) {
        (None, None, None, None) => {
            let range = NormalizedTextRange::new(0, section.normalized_text_len())
                .expect("zero-to-length normalized range is valid");
            Ok(ResolvedTextLocator {
                locator: TextLocator::for_section(document, section),
                kind: ResolvedLocatorKind::Section,
                range,
            })
        }
        (None, None, Some(range), None) => {
            section.validate_normalized_range(range).map_err(|error| {
                ApplicationError::InvalidLocator(format!("invalid character range: {error}"))
            })?;
            Ok(ResolvedTextLocator {
                locator: TextLocator::for_character_range(document, section, range),
                kind: ResolvedLocatorKind::CharacterRange,
                range,
            })
        }
        (Some(paragraph_index), None, Some(range), Some(segmentation_version)) => {
            validate_segmentation_version(segmentation_version)?;
            if paragraph_index == 0 {
                return Err(ApplicationError::InvalidLocator(
                    "paragraph_index must be 1-based".into(),
                ));
            }
            let paragraph = document
                .try_paragraph_text_units()
                .map_err(text_unit_materialization_error)?
                .units
                .into_iter()
                .find(|unit| {
                    unit.owner_section_id == locator.owner_section_id
                        && unit.paragraph_index == paragraph_index
                })
                .ok_or_else(|| {
                    ApplicationError::StaleLocator(format!(
                        "paragraph {paragraph_index} no longer exists in section {}",
                        locator.owner_section_id.0
                    ))
                })?;
            if paragraph.normalized_range != range {
                return Err(ApplicationError::StaleLocator(format!(
                    "paragraph {paragraph_index} normalized range changed"
                )));
            }
            Ok(ResolvedTextLocator {
                locator: TextLocator::for_paragraph(document, section, &paragraph),
                kind: ResolvedLocatorKind::Paragraph,
                range,
            })
        }
        (Some(paragraph_index), Some(sentence_index), Some(range), Some(segmentation_version)) => {
            validate_segmentation_version(segmentation_version)?;
            if paragraph_index == 0 || sentence_index == 0 {
                return Err(ApplicationError::InvalidLocator(
                    "paragraph_index and sentence_index must be 1-based".into(),
                ));
            }
            let sentence = document
                .try_sentence_text_units()
                .map_err(text_unit_materialization_error)?
                .units
                .into_iter()
                .find(|unit| {
                    unit.owner_section_id == locator.owner_section_id
                        && unit.paragraph_index == paragraph_index
                        && unit.sentence_index == sentence_index
                })
                .ok_or_else(|| {
                    ApplicationError::StaleLocator(format!(
                        "sentence {paragraph_index}.{sentence_index} no longer exists in section {}",
                        locator.owner_section_id.0
                    ))
                })?;
            if sentence.normalized_range != range {
                return Err(ApplicationError::StaleLocator(format!(
                    "sentence {paragraph_index}.{sentence_index} normalized range changed"
                )));
            }
            Ok(ResolvedTextLocator {
                locator: TextLocator::for_sentence(document, section, &sentence),
                kind: ResolvedLocatorKind::Sentence,
                range,
            })
        }
        _ => Err(ApplicationError::InvalidLocator(
            "locator Section/CharacterRange/Paragraph/Sentence fields form an invalid shape".into(),
        )),
    }
}

fn validate_segmentation_version(version: &str) -> Result<(), ApplicationError> {
    if version != TEXT_SEGMENTATION_VERSION {
        return Err(ApplicationError::StaleLocator(format!(
            "locator segmentation version {version} is incompatible with {TEXT_SEGMENTATION_VERSION}"
        )));
    }
    Ok(())
}

fn text_unit_materialization_error(error: impl std::fmt::Display) -> ApplicationError {
    ApplicationError::TextUnitIndexFailed(format!(
        "cannot resolve locator against persisted block evidence: {error}"
    ))
}
