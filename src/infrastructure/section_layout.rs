//! Compact encoding of one section's stored layout: its unit boundaries and its coverage counters.
//!
//! The store keeps a section's text once and only the offsets of its units. This module encodes those
//! boundaries as a version byte followed by three lists: paragraphs, sentences and sentence coverage.
//! Every integer is an unsigned LEB128 varint; offsets and source orders are ZigZag-encoded deltas
//! from the same field of the previous record in their list, starting from zero.
//!
//! The same helpers encode the two coverage values `section_layout_v2` keeps per section
//! (`paragraph_coverage` and the four `coverage_*` counters). Those were JSON objects with their key
//! names spelled out on every row, which cost 40–70× the bytes of the numbers they held; they are
//! varint lists now, and the section id a paragraph coverage repeats is elided when it is the row's
//! own `section_id`.

use crate::application::get_text_units::TextUnitEnumerationCoverage;
use crate::application::ports::ApplicationError;
use crate::domain::{ParagraphSectionCoverage, SectionId};

/// Number of Unicode scalars stored per `section_text_v3` chunk.
pub(crate) const CHUNK_CHARS: usize = 16_384;

/// The payload budget of one `section_layout_chunk_v2` row: a chunk stops before the next paragraph
/// or sentence would make its encoded layout larger than this.
pub(crate) const LAYOUT_CHUNK_BYTES: usize = 8 * 1024;

const SECTION_LAYOUT_VERSION: u8 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SectionLayout {
    pub paragraphs: Vec<LayoutParagraph>,
    pub sentences: Vec<LayoutSentence>,
    pub coverage: Vec<LayoutCoverage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LayoutParagraph {
    pub paragraph_index: usize,
    pub source_order: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LayoutSentence {
    pub paragraph_index: usize,
    pub sentence_index: usize,
    pub source_order: usize,
    pub start: usize,
    pub end: usize,
    pub parent_start: usize,
    pub parent_end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LayoutCoverage {
    pub paragraph_index: usize,
    pub paragraph_start: usize,
    pub paragraph_end: usize,
    pub content_class: u8,
    pub eligibility: u8,
    pub paragraph_chars: usize,
    pub sentence_chars: usize,
    pub separator_chars: usize,
    pub coarse_only_chars: usize,
    pub sentence_count: usize,
}

impl SectionLayout {
    pub(crate) fn encode(&self) -> Result<Vec<u8>, ApplicationError> {
        let mut out = Vec::new();
        out.push(SECTION_LAYOUT_VERSION);

        encode_count(&mut out, self.paragraphs.len())?;
        let mut previous_source_order = 0usize;
        let mut previous_start = 0usize;
        let mut previous_end = 0usize;
        for paragraph in &self.paragraphs {
            encode_unsigned(&mut out, paragraph.paragraph_index)?;
            encode_delta(&mut out, paragraph.source_order, &mut previous_source_order)?;
            encode_delta(&mut out, paragraph.start, &mut previous_start)?;
            encode_delta(&mut out, paragraph.end, &mut previous_end)?;
        }

        encode_count(&mut out, self.sentences.len())?;
        let mut previous_source_order = 0usize;
        let mut previous_start = 0usize;
        let mut previous_end = 0usize;
        let mut previous_parent_start = 0usize;
        let mut previous_parent_end = 0usize;
        for sentence in &self.sentences {
            encode_unsigned(&mut out, sentence.paragraph_index)?;
            encode_unsigned(&mut out, sentence.sentence_index)?;
            encode_delta(&mut out, sentence.source_order, &mut previous_source_order)?;
            encode_delta(&mut out, sentence.start, &mut previous_start)?;
            encode_delta(&mut out, sentence.end, &mut previous_end)?;
            encode_delta(&mut out, sentence.parent_start, &mut previous_parent_start)?;
            encode_delta(&mut out, sentence.parent_end, &mut previous_parent_end)?;
        }

        encode_count(&mut out, self.coverage.len())?;
        let mut previous_paragraph_start = 0usize;
        let mut previous_paragraph_end = 0usize;
        for coverage in &self.coverage {
            encode_unsigned(&mut out, coverage.paragraph_index)?;
            encode_delta(
                &mut out,
                coverage.paragraph_start,
                &mut previous_paragraph_start,
            )?;
            encode_delta(
                &mut out,
                coverage.paragraph_end,
                &mut previous_paragraph_end,
            )?;
            encode_unsigned(&mut out, usize::from(coverage.content_class))?;
            encode_unsigned(&mut out, usize::from(coverage.eligibility))?;
            encode_unsigned(&mut out, coverage.paragraph_chars)?;
            encode_unsigned(&mut out, coverage.sentence_chars)?;
            encode_unsigned(&mut out, coverage.separator_chars)?;
            encode_unsigned(&mut out, coverage.coarse_only_chars)?;
            encode_unsigned(&mut out, coverage.sentence_count)?;
        }

        Ok(out)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, ApplicationError> {
        let mut input = bytes;
        let version = take_byte(&mut input, "layout version")?;
        if version != SECTION_LAYOUT_VERSION {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "unsupported section layout version {version}"
            )));
        }

        let paragraph_count = decode_count(&mut input, "paragraph count")?;
        let mut paragraphs = Vec::with_capacity(paragraph_count);
        let mut previous_source_order = 0usize;
        let mut previous_start = 0usize;
        let mut previous_end = 0usize;
        for _ in 0..paragraph_count {
            paragraphs.push(LayoutParagraph {
                paragraph_index: decode_unsigned(&mut input, "paragraph index")?,
                source_order: decode_delta(
                    &mut input,
                    &mut previous_source_order,
                    "paragraph source order",
                )?,
                start: decode_delta(&mut input, &mut previous_start, "paragraph start")?,
                end: decode_delta(&mut input, &mut previous_end, "paragraph end")?,
            });
        }

        let sentence_count = decode_count(&mut input, "sentence count")?;
        let mut sentences = Vec::with_capacity(sentence_count);
        let mut previous_source_order = 0usize;
        let mut previous_start = 0usize;
        let mut previous_end = 0usize;
        let mut previous_parent_start = 0usize;
        let mut previous_parent_end = 0usize;
        for _ in 0..sentence_count {
            sentences.push(LayoutSentence {
                paragraph_index: decode_unsigned(&mut input, "sentence paragraph index")?,
                sentence_index: decode_unsigned(&mut input, "sentence index")?,
                source_order: decode_delta(
                    &mut input,
                    &mut previous_source_order,
                    "sentence source order",
                )?,
                start: decode_delta(&mut input, &mut previous_start, "sentence start")?,
                end: decode_delta(&mut input, &mut previous_end, "sentence end")?,
                parent_start: decode_delta(
                    &mut input,
                    &mut previous_parent_start,
                    "sentence parent start",
                )?,
                parent_end: decode_delta(
                    &mut input,
                    &mut previous_parent_end,
                    "sentence parent end",
                )?,
            });
        }

        let coverage_count = decode_count(&mut input, "coverage count")?;
        let mut coverage = Vec::with_capacity(coverage_count);
        let mut previous_paragraph_start = 0usize;
        let mut previous_paragraph_end = 0usize;
        for _ in 0..coverage_count {
            let paragraph_index = decode_unsigned(&mut input, "coverage paragraph index")?;
            let paragraph_start = decode_delta(
                &mut input,
                &mut previous_paragraph_start,
                "coverage paragraph start",
            )?;
            let paragraph_end = decode_delta(
                &mut input,
                &mut previous_paragraph_end,
                "coverage paragraph end",
            )?;
            let content_class = decode_u8(&mut input, "coverage content class")?;
            let eligibility = decode_u8(&mut input, "coverage eligibility")?;
            coverage.push(LayoutCoverage {
                paragraph_index,
                paragraph_start,
                paragraph_end,
                content_class,
                eligibility,
                paragraph_chars: decode_unsigned(&mut input, "coverage paragraph chars")?,
                sentence_chars: decode_unsigned(&mut input, "coverage sentence chars")?,
                separator_chars: decode_unsigned(&mut input, "coverage separator chars")?,
                coarse_only_chars: decode_unsigned(&mut input, "coverage coarse chars")?,
                sentence_count: decode_unsigned(&mut input, "coverage sentence count")?,
            });
        }

        if !input.is_empty() {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "section layout has {} trailing bytes",
                input.len()
            )));
        }

        Ok(Self {
            paragraphs,
            sentences,
            coverage,
        })
    }
}

fn encode_count(out: &mut Vec<u8>, count: usize) -> Result<(), ApplicationError> {
    encode_unsigned(out, count)
}

fn decode_count(input: &mut &[u8], field: &str) -> Result<usize, ApplicationError> {
    let count = decode_unsigned(input, field)?;
    // Every record is at least one byte, so a count larger than the remaining input is corrupt.
    if count > input.len() {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "section layout {field} {count} exceeds the remaining input"
        )));
    }
    Ok(count)
}

fn encode_unsigned(out: &mut Vec<u8>, value: usize) -> Result<(), ApplicationError> {
    put_uvarint(
        out,
        u64::try_from(value).map_err(|_| {
            ApplicationError::TextUnitIndexFailed(format!(
                "section layout value {value} exceeds u64 range"
            ))
        })?,
    );
    Ok(())
}

fn decode_unsigned(input: &mut &[u8], field: &str) -> Result<usize, ApplicationError> {
    let value = get_uvarint(input, field)?;
    usize::try_from(value).map_err(|_| {
        ApplicationError::TextUnitIndexFailed(format!(
            "section layout {field} {value} is outside usize range"
        ))
    })
}

fn encode_delta(
    out: &mut Vec<u8>,
    value: usize,
    previous: &mut usize,
) -> Result<(), ApplicationError> {
    let value_i64 = i64::try_from(value).map_err(|_| {
        ApplicationError::TextUnitIndexFailed(format!(
            "section layout value {value} exceeds i64 range"
        ))
    })?;
    let previous_i64 = i64::try_from(*previous).map_err(|_| {
        ApplicationError::TextUnitIndexFailed(format!(
            "section layout previous value {previous} exceeds i64 range"
        ))
    })?;
    let delta = value_i64.checked_sub(previous_i64).ok_or_else(|| {
        ApplicationError::TextUnitIndexFailed("section layout delta overflow".into())
    })?;
    *previous = value;
    put_uvarint(out, zigzag(delta));
    Ok(())
}

fn decode_delta(
    input: &mut &[u8],
    previous: &mut usize,
    field: &str,
) -> Result<usize, ApplicationError> {
    let delta = unzigzag(get_uvarint(input, field)?);
    let base = i64::try_from(*previous).map_err(|_| {
        ApplicationError::TextUnitIndexFailed(format!(
            "section layout previous {field} {previous} exceeds i64 range"
        ))
    })?;
    let value = base.checked_add(delta).ok_or_else(|| {
        ApplicationError::TextUnitIndexFailed(format!("section layout {field} delta overflow"))
    })?;
    if value < 0 {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "section layout {field} is negative ({value})"
        )));
    }
    let value = usize::try_from(value).map_err(|_| {
        ApplicationError::TextUnitIndexFailed(format!(
            "section layout {field} {value} is outside usize range"
        ))
    })?;
    *previous = value;
    Ok(value)
}

fn decode_u8(input: &mut &[u8], field: &str) -> Result<u8, ApplicationError> {
    let value = get_uvarint(input, field)?;
    u8::try_from(value).map_err(|_| {
        ApplicationError::TextUnitIndexFailed(format!(
            "section layout {field} {value} is outside u8 range"
        ))
    })
}

fn take_byte(input: &mut &[u8], field: &str) -> Result<u8, ApplicationError> {
    let Some((&byte, rest)) = input.split_first() else {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "section layout is missing its {field}"
        )));
    };
    *input = rest;
    Ok(byte)
}

fn put_uvarint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn get_uvarint(input: &mut &[u8], field: &str) -> Result<u64, ApplicationError> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let Some((&byte, rest)) = input.split_first() else {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "section layout {field} varint is truncated"
            )));
        };
        *input = rest;
        if shift == 63 && (byte & 0x7f) > 1 {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "section layout {field} varint overflows u64"
            )));
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
        if shift >= 70 {
            return Err(ApplicationError::TextUnitIndexFailed(format!(
                "section layout {field} varint is too long"
            )));
        }
    }
}

fn zigzag(value: i64) -> u64 {
    ((value as u64) << 1) ^ ((value >> 63) as u64)
}

fn unzigzag(value: u64) -> i64 {
    ((value >> 1) as i64) ^ -((value & 1) as i64)
}

fn encode_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), ApplicationError> {
    encode_unsigned(out, bytes.len())?;
    out.extend_from_slice(bytes);
    Ok(())
}

fn decode_bytes(input: &mut &[u8], field: &str) -> Result<Vec<u8>, ApplicationError> {
    let length = decode_unsigned(input, field)?;
    if length > input.len() {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "section layout {field} length {length} exceeds the remaining input"
        )));
    }
    let (bytes, rest) = input.split_at(length);
    *input = rest;
    Ok(bytes.to_vec())
}

// ---------------------------------------------------------------------------------------------
// The header coverages.
//
// `section_layout_v2` keeps one paragraph coverage and four stream coverages per section. Both used
// to be JSON objects that spelled every key name on every row: a stream coverage was ~410 B for 14
// numbers, and a paragraph coverage repeated the section's whole id string. They are now a version
// byte and a varint list, and the repeated owner id is elided when it is the row's own `section_id`.
// ---------------------------------------------------------------------------------------------

/// Version byte of an encoded paragraph coverage (`section_layout_v2.paragraph_coverage`).
const PARAGRAPH_COVERAGE_VERSION: u8 = 1;

/// Version byte of an encoded stream coverage (`section_layout_v2.coverage_*`).
const STREAM_COVERAGE_VERSION: u8 = 1;

/// Flag bit of a paragraph coverage: `owner_section_id` is the row's own `section_id`, so it was not
/// written and the decoder rebuilds it from the row.
const PARAGRAPH_COVERAGE_FLAG_OWNER_IS_SECTION: u8 = 1 << 0;

/// The stored bytes of one paragraph coverage. `section_id` is the section the row belongs to, so a
/// coverage whose owner is that section does not write the id again.
pub(crate) fn encode_paragraph_coverage(
    coverage: &ParagraphSectionCoverage,
    section_id: &SectionId,
) -> Result<Vec<u8>, ApplicationError> {
    let owner_is_section = coverage.owner_section_id == *section_id;
    let mut out = vec![PARAGRAPH_COVERAGE_VERSION];
    out.push(if owner_is_section {
        PARAGRAPH_COVERAGE_FLAG_OWNER_IS_SECTION
    } else {
        0
    });
    if !owner_is_section {
        encode_bytes(&mut out, coverage.owner_section_id.0.as_bytes())?;
    }
    for value in [
        coverage.owner_chars,
        coverage.paragraph_chars,
        coverage.separator_chars,
        coverage.paragraph_count,
        coverage.native_paragraph_chars,
        coverage.native_structural_container_chars,
        coverage.native_non_prose_chars,
        coverage.fallback_chars,
    ] {
        encode_unsigned(&mut out, value)?;
    }
    Ok(out)
}

/// [`encode_paragraph_coverage`], decoded against the `section_id` of the row it was read from.
pub(crate) fn decode_paragraph_coverage(
    bytes: &[u8],
    section_id: &SectionId,
) -> Result<ParagraphSectionCoverage, ApplicationError> {
    let mut input = bytes;
    let version = take_byte(&mut input, "paragraph coverage version")?;
    if version != PARAGRAPH_COVERAGE_VERSION {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "unsupported paragraph coverage version {version}"
        )));
    }
    let flags = take_byte(&mut input, "paragraph coverage flags")?;
    if flags & !PARAGRAPH_COVERAGE_FLAG_OWNER_IS_SECTION != 0 {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "unsupported paragraph coverage flags {flags}"
        )));
    }
    let owner_section_id = if flags & PARAGRAPH_COVERAGE_FLAG_OWNER_IS_SECTION != 0 {
        section_id.clone()
    } else {
        let owner = decode_bytes(&mut input, "paragraph coverage owner")?;
        SectionId(String::from_utf8(owner).map_err(|error| {
            ApplicationError::TextUnitIndexFailed(format!(
                "paragraph coverage owner is not UTF-8: {error}"
            ))
        })?)
    };
    let coverage = ParagraphSectionCoverage {
        owner_section_id,
        owner_chars: decode_unsigned(&mut input, "paragraph coverage owner chars")?,
        paragraph_chars: decode_unsigned(&mut input, "paragraph coverage paragraph chars")?,
        separator_chars: decode_unsigned(&mut input, "paragraph coverage separator chars")?,
        paragraph_count: decode_unsigned(&mut input, "paragraph coverage paragraph count")?,
        native_paragraph_chars: decode_unsigned(
            &mut input,
            "paragraph coverage native paragraph chars",
        )?,
        native_structural_container_chars: decode_unsigned(
            &mut input,
            "paragraph coverage native structural chars",
        )?,
        native_non_prose_chars: decode_unsigned(
            &mut input,
            "paragraph coverage native non prose chars",
        )?,
        fallback_chars: decode_unsigned(&mut input, "paragraph coverage fallback chars")?,
    };
    if !input.is_empty() {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "paragraph coverage has {} trailing bytes",
            input.len()
        )));
    }
    Ok(coverage)
}

/// The stored bytes of one stream coverage.
pub(crate) fn encode_stream_coverage(
    coverage: &TextUnitEnumerationCoverage,
) -> Result<Vec<u8>, ApplicationError> {
    let mut out = vec![STREAM_COVERAGE_VERSION];
    for value in [
        coverage.owner_chars,
        coverage.section_separator_chars,
        coverage.sentence_separator_chars,
        coverage.paragraph_count,
        coverage.sentence_eligible_paragraphs,
        coverage.non_prose_paragraphs,
        coverage.coarse_structural_paragraphs,
        coverage.represented_paragraphs,
        coverage.represented_sentences,
        coverage.coarse_non_prose_items,
        coverage.coarse_structural_items,
        coverage.intentionally_skipped,
        coverage.unsupported_gaps,
    ] {
        encode_unsigned(&mut out, value)?;
    }
    out.push(u8::from(coverage.source_complete));
    Ok(out)
}

/// [`encode_stream_coverage`], decoded.
pub(crate) fn decode_stream_coverage(
    bytes: &[u8],
) -> Result<TextUnitEnumerationCoverage, ApplicationError> {
    let mut input = bytes;
    let version = take_byte(&mut input, "stream coverage version")?;
    if version != STREAM_COVERAGE_VERSION {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "unsupported stream coverage version {version}"
        )));
    }
    let coverage = TextUnitEnumerationCoverage {
        owner_chars: decode_unsigned(&mut input, "stream coverage owner chars")?,
        section_separator_chars: decode_unsigned(
            &mut input,
            "stream coverage section separator chars",
        )?,
        sentence_separator_chars: decode_unsigned(
            &mut input,
            "stream coverage sentence separator chars",
        )?,
        paragraph_count: decode_unsigned(&mut input, "stream coverage paragraph count")?,
        sentence_eligible_paragraphs: decode_unsigned(
            &mut input,
            "stream coverage sentence eligible paragraphs",
        )?,
        non_prose_paragraphs: decode_unsigned(&mut input, "stream coverage non prose paragraphs")?,
        coarse_structural_paragraphs: decode_unsigned(
            &mut input,
            "stream coverage coarse structural paragraphs",
        )?,
        represented_paragraphs: decode_unsigned(
            &mut input,
            "stream coverage represented paragraphs",
        )?,
        represented_sentences: decode_unsigned(
            &mut input,
            "stream coverage represented sentences",
        )?,
        coarse_non_prose_items: decode_unsigned(
            &mut input,
            "stream coverage coarse non prose items",
        )?,
        coarse_structural_items: decode_unsigned(
            &mut input,
            "stream coverage coarse structural items",
        )?,
        intentionally_skipped: decode_unsigned(
            &mut input,
            "stream coverage intentionally skipped",
        )?,
        unsupported_gaps: decode_unsigned(&mut input, "stream coverage unsupported gaps")?,
        source_complete: match take_byte(&mut input, "stream coverage source complete")? {
            0 => false,
            1 => true,
            other => {
                return Err(ApplicationError::TextUnitIndexFailed(format!(
                    "stream coverage source_complete is {other}"
                )));
            }
        },
    };
    if !input.is_empty() {
        return Err(ApplicationError::TextUnitIndexFailed(format!(
            "stream coverage has {} trailing bytes",
            input.len()
        )));
    }
    Ok(coverage)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_layout() -> SectionLayout {
        SectionLayout {
            paragraphs: vec![
                LayoutParagraph {
                    paragraph_index: 1,
                    source_order: 5,
                    start: 1_000,
                    end: 4_096,
                },
                LayoutParagraph {
                    paragraph_index: 2,
                    source_order: 2,
                    start: 10,
                    end: 50_000,
                },
                LayoutParagraph {
                    paragraph_index: 3,
                    source_order: 100,
                    start: 49_999,
                    end: 50_001,
                },
            ],
            sentences: vec![
                LayoutSentence {
                    paragraph_index: 1,
                    sentence_index: 1,
                    source_order: 8,
                    start: 1_000,
                    end: 1_010,
                    parent_start: 1_000,
                    parent_end: 4_096,
                },
                LayoutSentence {
                    paragraph_index: 1,
                    sentence_index: 2,
                    source_order: 9,
                    start: 1_010,
                    end: 4_096,
                    parent_start: 1_000,
                    parent_end: 4_096,
                },
                LayoutSentence {
                    paragraph_index: 2,
                    sentence_index: 1,
                    source_order: 3,
                    start: 10,
                    end: 50_000,
                    parent_start: 10,
                    parent_end: 50_000,
                },
            ],
            coverage: vec![
                LayoutCoverage {
                    paragraph_index: 1,
                    paragraph_start: 1_000,
                    paragraph_end: 4_096,
                    content_class: 1,
                    eligibility: 0,
                    paragraph_chars: 3_096,
                    sentence_chars: 3_096,
                    separator_chars: 0,
                    coarse_only_chars: 0,
                    sentence_count: 2,
                },
                LayoutCoverage {
                    paragraph_index: 2,
                    paragraph_start: 10,
                    paragraph_end: 50_000,
                    content_class: 6,
                    eligibility: 1,
                    paragraph_chars: 49_990,
                    sentence_chars: 0,
                    separator_chars: 0,
                    coarse_only_chars: 49_990,
                    sentence_count: 0,
                },
            ],
        }
    }

    #[test]
    fn layout_round_trips_large_and_non_monotonic_deltas() {
        let layout = sample_layout();
        let bytes = layout.encode().expect("layout should encode");
        let decoded = SectionLayout::decode(&bytes).expect("layout should decode");
        assert_eq!(decoded, layout);
    }

    #[test]
    fn empty_layout_round_trips() {
        let layout = SectionLayout {
            paragraphs: Vec::new(),
            sentences: Vec::new(),
            coverage: Vec::new(),
        };
        let bytes = layout.encode().expect("layout should encode");
        assert_eq!(
            SectionLayout::decode(&bytes).expect("layout should decode"),
            layout
        );
    }

    #[test]
    fn truncated_and_trailing_bytes_are_rejected() {
        let bytes = sample_layout().encode().expect("layout should encode");
        assert!(SectionLayout::decode(&bytes[..bytes.len() - 1]).is_err());
        let mut extended = bytes;
        extended.push(0);
        assert!(SectionLayout::decode(&extended).is_err());
    }

    fn sample_paragraph_coverage(owner: &SectionId) -> ParagraphSectionCoverage {
        ParagraphSectionCoverage {
            owner_section_id: owner.clone(),
            owner_chars: 7_000,
            paragraph_chars: 6_500,
            separator_chars: 500,
            paragraph_count: 42,
            native_paragraph_chars: 6_000,
            native_structural_container_chars: 100,
            native_non_prose_chars: 0,
            fallback_chars: 400,
        }
    }

    fn sample_stream_coverage() -> TextUnitEnumerationCoverage {
        TextUnitEnumerationCoverage {
            owner_chars: 7_000,
            section_separator_chars: 3,
            sentence_separator_chars: 120,
            paragraph_count: 42,
            sentence_eligible_paragraphs: 40,
            non_prose_paragraphs: 2,
            coarse_structural_paragraphs: 1,
            represented_paragraphs: 42,
            represented_sentences: 300,
            coarse_non_prose_items: 2,
            coarse_structural_items: 1,
            intentionally_skipped: 0,
            unsupported_gaps: 0,
            source_complete: false,
        }
    }

    #[test]
    fn paragraph_coverage_round_trips_and_elides_its_own_section_id() {
        let section_id = SectionId("section://sha256:abcdef/book/section-1".into());
        let own = sample_paragraph_coverage(&section_id);
        let bytes = encode_paragraph_coverage(&own, &section_id).expect("encode");
        assert_eq!(
            decode_paragraph_coverage(&bytes, &section_id).expect("decode"),
            own
        );

        // A coverage owned by another section keeps the id, and comes back with it.
        let foreign = sample_paragraph_coverage(&SectionId("section://other/section".into()));
        let foreign_bytes = encode_paragraph_coverage(&foreign, &section_id).expect("encode");
        assert_eq!(
            decode_paragraph_coverage(&foreign_bytes, &section_id).expect("decode"),
            foreign
        );
        assert!(
            foreign_bytes.len() > bytes.len(),
            "a foreign owner must cost its id: {} vs {}",
            foreign_bytes.len(),
            bytes.len()
        );
        assert!(
            bytes.len() <= 16,
            "an own-section coverage is a version byte, a flag and eight varints: {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn stream_coverage_round_trips_and_costs_far_less_than_its_json() {
        let coverage = sample_stream_coverage();
        let bytes = encode_stream_coverage(&coverage).expect("encode");
        assert_eq!(decode_stream_coverage(&bytes).expect("decode"), coverage);
        // The JSON object this replaced spelled fourteen key names and cost ~410 bytes.
        assert!(
            bytes.len() <= 24,
            "a stream coverage is a version byte, thirteen varints and a bool: {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn coverage_decoders_reject_truncated_trailing_and_unknown_bytes() {
        let section_id = SectionId("section://book/1".into());
        let paragraph =
            encode_paragraph_coverage(&sample_paragraph_coverage(&section_id), &section_id)
                .expect("encode");
        let stream = encode_stream_coverage(&sample_stream_coverage()).expect("encode");
        for bytes in [paragraph.as_slice(), stream.as_slice()] {
            let mut extended = bytes.to_vec();
            extended.push(0);
            let truncated = &bytes[..bytes.len() - 1];
            if bytes == paragraph.as_slice() {
                assert!(decode_paragraph_coverage(truncated, &section_id).is_err());
                assert!(decode_paragraph_coverage(&extended, &section_id).is_err());
                let mut unknown = bytes.to_vec();
                unknown[0] = 9;
                assert!(decode_paragraph_coverage(&unknown, &section_id).is_err());
            } else {
                assert!(decode_stream_coverage(truncated).is_err());
                assert!(decode_stream_coverage(&extended).is_err());
                let mut unknown = bytes.to_vec();
                unknown[0] = 9;
                assert!(decode_stream_coverage(&unknown).is_err());
            }
        }
    }
}
