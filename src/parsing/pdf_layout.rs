use std::collections::BTreeMap;

use lopdf::content::Content;
use lopdf::{Dictionary, Document as LopdfDocument, Object};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PdfTextFragmentEvidence {
    pub page: u32,
    pub sequence_index: usize,
    pub text: String,
    pub font_resource: Option<String>,
    pub font_size: Option<f32>,
    pub x: Option<f32>,
    pub y: Option<f32>,
}

#[derive(Default)]
struct TextState {
    font_resource: Option<Vec<u8>>,
    font_size: Option<f32>,
    x: Option<f32>,
    y: Option<f32>,
    leading: Option<f32>,
}

pub(super) fn extract_text_fragment_evidence(
    pdf: &LopdfDocument,
    page_numbers: &[u32],
    max_page_decompressed_bytes: usize,
) -> (Vec<PdfTextFragmentEvidence>, Vec<String>) {
    let pages = pdf.get_pages();
    let mut evidence = Vec::new();
    let mut errors = Vec::new();
    let mut sequence_index = 0_usize;

    for page_number in page_numbers {
        let Some(page_id) = pages.get(page_number).copied() else {
            continue;
        };
        let content_bytes =
            match pdf.get_page_content_with_limit(page_id, max_page_decompressed_bytes) {
                Ok(bytes) => bytes,
                Err(error) => {
                    errors.push(format!("page {page_number}: {error}"));
                    continue;
                }
            };
        let content = match Content::decode(&content_bytes) {
            Ok(content) => content,
            Err(error) => {
                errors.push(format!(
                    "page {page_number}: cannot decode content stream: {error}"
                ));
                continue;
            }
        };
        let fonts = pdf.get_page_fonts(page_id).unwrap_or_default();
        let mut state = TextState::default();

        for operation in content.operations {
            match operation.operator.as_str() {
                "BT" => {
                    state.x = None;
                    state.y = None;
                }
                "Tf" => {
                    if let Some(name) = operation
                        .operands
                        .first()
                        .and_then(|value| value.as_name().ok())
                    {
                        state.font_resource = Some(name.to_vec());
                    }
                    if let Some(size) = operation
                        .operands
                        .get(1)
                        .and_then(|value| value.as_float().ok())
                    {
                        state.font_size = Some(size.abs());
                    }
                }
                "Tm" => {
                    if operation.operands.len() >= 6 {
                        state.x = operation.operands[4].as_float().ok();
                        state.y = operation.operands[5].as_float().ok();
                    }
                }
                "Td" | "TD" => {
                    if operation.operator == "TD" {
                        state.leading = operation
                            .operands
                            .get(1)
                            .and_then(|value| value.as_float().ok())
                            .map(|value| -value);
                    }
                    apply_translation(&mut state, &operation.operands);
                }
                "TL" => {
                    state.leading = operation
                        .operands
                        .first()
                        .and_then(|value| value.as_float().ok())
                        .map(f32::abs);
                }
                "T*" => move_to_next_line(&mut state),
                "Tj" => {
                    if let Some(text) = operation.operands.first().and_then(|value| {
                        decode_text_object(
                            pdf,
                            &fonts,
                            state.font_resource.as_deref(),
                            value,
                            max_page_decompressed_bytes,
                        )
                    }) {
                        push_evidence(
                            &mut evidence,
                            *page_number,
                            &mut sequence_index,
                            &state,
                            text,
                        );
                    }
                }
                "TJ" => {
                    if let Some(array) = operation
                        .operands
                        .first()
                        .and_then(|value| value.as_array().ok())
                    {
                        let text = array
                            .iter()
                            .filter_map(|value| {
                                decode_text_object(
                                    pdf,
                                    &fonts,
                                    state.font_resource.as_deref(),
                                    value,
                                    max_page_decompressed_bytes,
                                )
                            })
                            .collect::<String>();
                        push_evidence(
                            &mut evidence,
                            *page_number,
                            &mut sequence_index,
                            &state,
                            text,
                        );
                    }
                }
                "'" => {
                    move_to_next_line(&mut state);
                    if let Some(text) = operation.operands.first().and_then(|value| {
                        decode_text_object(
                            pdf,
                            &fonts,
                            state.font_resource.as_deref(),
                            value,
                            max_page_decompressed_bytes,
                        )
                    }) {
                        push_evidence(
                            &mut evidence,
                            *page_number,
                            &mut sequence_index,
                            &state,
                            text,
                        );
                    }
                }
                "\"" => {
                    move_to_next_line(&mut state);
                    if let Some(text) = operation.operands.get(2).and_then(|value| {
                        decode_text_object(
                            pdf,
                            &fonts,
                            state.font_resource.as_deref(),
                            value,
                            max_page_decompressed_bytes,
                        )
                    }) {
                        push_evidence(
                            &mut evidence,
                            *page_number,
                            &mut sequence_index,
                            &state,
                            text,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    (evidence, errors)
}

pub(super) fn infer_abstract_heading<'a>(
    evidence: &'a [PdfTextFragmentEvidence],
    first_section_page: u32,
    first_section_title: &str,
) -> Option<&'a PdfTextFragmentEvidence> {
    let first_page = evidence.iter().map(|item| item.page).min()?;
    if first_section_page != first_page {
        return None;
    }
    let section_label = strip_number_prefix(first_section_title);
    let boundary_index = evidence.iter().position(|item| {
        item.page == first_section_page
            && (same_heading_text(&item.text, first_section_title)
                || same_heading_text(&item.text, section_label))
    })?;

    let candidates = evidence[..boundary_index]
        .iter()
        .filter(|item| item.page == first_page && item.text.trim().eq_ignore_ascii_case("Abstract"))
        .collect::<Vec<_>>();
    let [candidate] = candidates.as_slice() else {
        return None;
    };

    let candidate_index = evidence
        .iter()
        .position(|item| item.sequence_index == candidate.sequence_index)?;
    let next_text = evidence[candidate_index + 1..boundary_index]
        .iter()
        .find(|item| item.page == candidate.page && item.text.trim().chars().count() >= 12)?;

    if !has_distinct_heading_style(candidate, next_text) {
        return None;
    }
    if let (Some(heading_y), Some(body_y)) = (candidate.y, next_text.y)
        && heading_y + 0.5 < body_y
    {
        return None;
    }

    Some(candidate)
}

fn has_distinct_heading_style(
    heading: &PdfTextFragmentEvidence,
    body: &PdfTextFragmentEvidence,
) -> bool {
    let font_differs = match (&heading.font_resource, &body.font_resource) {
        (Some(left), Some(right)) => left != right,
        _ => false,
    };
    let size_is_larger = match (heading.font_size, body.font_size) {
        (Some(left), Some(right)) => left >= right + 0.25,
        _ => false,
    };
    font_differs || size_is_larger
}

fn strip_number_prefix(title: &str) -> &str {
    title
        .trim_start()
        .trim_start_matches(|character: char| character.is_ascii_digit() || character == '.')
        .trim_start_matches(|character: char| {
            character.is_whitespace() || matches!(character, ':' | '-' | '–' | '—')
        })
}

fn same_heading_text(left: &str, right: &str) -> bool {
    left.split_whitespace()
        .map(|part| part.to_ascii_lowercase())
        .eq(right
            .split_whitespace()
            .map(|part| part.to_ascii_lowercase()))
}

fn apply_translation(state: &mut TextState, operands: &[Object]) {
    if operands.len() < 2 {
        return;
    }
    let Some(tx) = operands[0].as_float().ok() else {
        return;
    };
    let Some(ty) = operands[1].as_float().ok() else {
        return;
    };
    state.x = Some(state.x.unwrap_or(0.0) + tx);
    state.y = Some(state.y.unwrap_or(0.0) + ty);
}

fn move_to_next_line(state: &mut TextState) {
    if let (Some(y), Some(leading)) = (state.y, state.leading) {
        state.y = Some(y - leading);
    }
}

fn push_evidence(
    output: &mut Vec<PdfTextFragmentEvidence>,
    page: u32,
    sequence_index: &mut usize,
    state: &TextState,
    text: String,
) {
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    output.push(PdfTextFragmentEvidence {
        page,
        sequence_index: *sequence_index,
        text,
        font_resource: state
            .font_resource
            .as_deref()
            .map(|name| String::from_utf8_lossy(name).into_owned()),
        font_size: state.font_size,
        x: state.x,
        y: state.y,
    });
    *sequence_index = sequence_index.saturating_add(1);
}

fn decode_text_object(
    pdf: &LopdfDocument,
    fonts: &BTreeMap<Vec<u8>, &Dictionary>,
    font_resource: Option<&[u8]>,
    object: &Object,
    max_decompressed_bytes: usize,
) -> Option<String> {
    let bytes = object.as_str().ok()?;
    if let Some(font_resource) = font_resource
        && let Some(font) = fonts.get(font_resource)
        && let Ok(encoding) = font.get_font_encoding_with_limit(pdf, max_decompressed_bytes)
        && let Ok(text) = LopdfDocument::decode_text(&encoding, bytes)
    {
        return Some(text);
    }

    bytes
        .iter()
        .all(|byte| byte.is_ascii_graphic() || byte.is_ascii_whitespace())
        .then(|| String::from_utf8_lossy(bytes).into_owned())
}
