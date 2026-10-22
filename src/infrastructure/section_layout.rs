//! Compact encoding of one section's unit boundaries.
//!
//! The store keeps a section's text once and only the offsets of its units. This module encodes those
//! boundaries as a version byte followed by three lists: paragraphs, sentences and sentence coverage.
//! Every integer is an unsigned LEB128 varint; offsets and source orders are ZigZag-encoded deltas
//! from the same field of the previous record in their list, starting from zero.

use crate::application::ports::ApplicationError;

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
}
