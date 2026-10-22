use std::collections::{BTreeMap, BTreeSet, HashMap};

use lopdf::content::Content;
use lopdf::{Dictionary, Document as LopdfDocument, Encoding, Object};

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

#[derive(Clone, Debug)]
struct PdfTextLineEvidence<'a> {
    page: u32,
    first_fragment: &'a PdfTextFragmentEvidence,
    text: String,
    font_resources: BTreeSet<&'a str>,
    max_font_size: Option<f32>,
    y: Option<f32>,
}

#[derive(Default)]
struct TextState {
    font_resource: Option<Vec<u8>>,
    font_size: Option<f32>,
    x: Option<f32>,
    y: Option<f32>,
    leading: Option<f32>,
}

/// Per-call memo of resolved font encodings.
///
/// `decode_text_object` used to call `Dictionary::get_font_encoding_with_limit`
/// for every string shown, and lopdf 0.44 caches nothing: a document whose
/// `/ToUnicode` CMap is large re-inflated and re-parsed the same stream once per
/// shown string (256,880 bytes, 1.36 ms, about 39,000 times for 300 pages).
/// Resolving each distinct font once and reusing the result removes that work
/// without changing a single decoded string.
///
/// The key is the address of the `&Dictionary` that `get_page_fonts` returns.
/// Those references borrow from the document, so the address is stable for the
/// whole call, and every page that uses the same font object yields the same
/// address: a font shared by many pages is parsed once for the document.
/// `max_decompressed_bytes` is constant for a call (it is this function's
/// `max_page_decompressed_bytes` argument), so it is not part of the key.
///
/// A resolution that fails is memoised as `None`, which is exactly the old
/// fall-through to the ASCII fallback, so the output cannot change. The memo is
/// local to one call: no global state, no locking, nothing that outlives the
/// document.
struct FontEncodingMemo<'a> {
    resolved: HashMap<usize, Option<Encoding<'a>>>,
    #[cfg(test)]
    resolutions: usize,
}

impl<'a> FontEncodingMemo<'a> {
    fn new() -> Self {
        Self {
            resolved: HashMap::new(),
            #[cfg(test)]
            resolutions: 0,
        }
    }

    /// The font's encoding, resolved at most once per distinct font.
    fn resolve(
        &mut self,
        pdf: &'a LopdfDocument,
        font: &'a Dictionary,
        max_decompressed_bytes: usize,
    ) -> Option<&Encoding<'a>> {
        let key = font as *const Dictionary as usize;
        if self.resolved.get(&key).is_none() {
            #[cfg(test)]
            {
                self.resolutions += 1;
            }
            let resolved = font
                .get_font_encoding_with_limit(pdf, max_decompressed_bytes)
                .ok();
            self.resolved.insert(key, resolved);
        }
        self.resolved.get(&key).and_then(Option::as_ref)
    }

    #[cfg(test)]
    fn resolutions(&self) -> usize {
        self.resolutions
    }
}

pub(super) fn extract_text_fragment_evidence(
    pdf: &LopdfDocument,
    page_numbers: &[u32],
    max_page_decompressed_bytes: usize,
) -> (Vec<PdfTextFragmentEvidence>, Vec<String>) {
    let mut memo = FontEncodingMemo::new();
    extract_text_fragment_evidence_with_memo(
        pdf,
        page_numbers,
        max_page_decompressed_bytes,
        &mut memo,
    )
}

fn extract_text_fragment_evidence_with_memo<'a>(
    pdf: &'a LopdfDocument,
    page_numbers: &[u32],
    max_page_decompressed_bytes: usize,
    memo: &mut FontEncodingMemo<'a>,
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
                            memo,
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
                                    memo,
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
                            memo,
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
                            memo,
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

pub(super) fn abstract_heading_inference_status(
    evidence: &[PdfTextFragmentEvidence],
    first_section_page: u32,
    first_section_title: &str,
) -> &'static str {
    let Some(first_page) = evidence.iter().map(|item| item.page).min() else {
        return "no_layout_evidence";
    };
    if first_section_page != first_page {
        return "first_section_not_on_first_page";
    }

    let lines = layout_lines(evidence);
    let section_label = strip_number_prefix(first_section_title);
    let Some(boundary_index) = lines.iter().position(|line| {
        line.page == first_section_page
            && (same_heading_text(&line.text, first_section_title)
                || same_heading_text(&line.text, section_label))
    }) else {
        return "first_section_layout_not_found";
    };

    let candidates = lines[..boundary_index]
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            line.page == first_page && normalized_heading_text(&line.text) == "abstract"
        })
        .collect::<Vec<_>>();
    let [(candidate_index, candidate)] = candidates.as_slice() else {
        return "abstract_candidate_count_not_one";
    };

    let Some(body) = lines[candidate_index + 1..boundary_index]
        .iter()
        .find(|line| {
            line.page == candidate.page
                && line
                    .text
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .count()
                    >= 12
        })
    else {
        return "abstract_body_layout_not_found";
    };

    if !has_distinct_heading_style(candidate, body) {
        return "abstract_style_not_distinct";
    }
    if let (Some(heading_y), Some(body_y)) = (candidate.y, body.y)
        && heading_y + 0.5 < body_y
    {
        return "abstract_position_invalid";
    }

    "resolved"
}

pub(super) fn infer_abstract_heading<'a>(
    evidence: &'a [PdfTextFragmentEvidence],
    first_section_page: u32,
    first_section_title: &str,
) -> Option<&'a PdfTextFragmentEvidence> {
    if abstract_heading_inference_status(evidence, first_section_page, first_section_title)
        != "resolved"
    {
        return None;
    }

    let lines = layout_lines(evidence);
    let section_label = strip_number_prefix(first_section_title);
    let boundary_index = lines.iter().position(|line| {
        line.page == first_section_page
            && (same_heading_text(&line.text, first_section_title)
                || same_heading_text(&line.text, section_label))
    })?;
    lines[..boundary_index]
        .iter()
        .find(|line| normalized_heading_text(&line.text) == "abstract")
        .map(|line| line.first_fragment)
}

fn layout_lines(evidence: &[PdfTextFragmentEvidence]) -> Vec<PdfTextLineEvidence<'_>> {
    let mut lines = Vec::<PdfTextLineEvidence<'_>>::new();
    for fragment in evidence {
        let can_join = lines.last().is_some_and(|line| {
            line.page == fragment.page
                && match (line.y, fragment.y) {
                    (Some(left), Some(right)) => (left - right).abs() <= 1.0,
                    _ => false,
                }
        });

        if can_join {
            let line = lines.last_mut().expect("joinable line must exist");
            if !line.text.is_empty() && !fragment.text.starts_with(char::is_whitespace) {
                line.text.push(' ');
            }
            line.text.push_str(fragment.text.trim());
            if let Some(font) = fragment.font_resource.as_deref() {
                line.font_resources.insert(font);
            }
            if let Some(size) = fragment.font_size {
                line.max_font_size =
                    Some(line.max_font_size.map_or(size, |current| current.max(size)));
            }
            continue;
        }

        let mut font_resources = BTreeSet::new();
        if let Some(font) = fragment.font_resource.as_deref() {
            font_resources.insert(font);
        }
        lines.push(PdfTextLineEvidence {
            page: fragment.page,
            first_fragment: fragment,
            text: fragment.text.trim().to_string(),
            font_resources,
            max_font_size: fragment.font_size,
            y: fragment.y,
        });
    }
    lines
}

fn has_distinct_heading_style(
    heading: &PdfTextLineEvidence<'_>,
    body: &PdfTextLineEvidence<'_>,
) -> bool {
    let font_differs = heading.font_resources.len() == 1
        && body.font_resources.len() == 1
        && heading.font_resources != body.font_resources;
    let size_is_larger = match (heading.max_font_size, body.max_font_size) {
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
    normalized_heading_text(left) == normalized_heading_text(right)
}

fn normalized_heading_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
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

fn decode_text_object<'a>(
    pdf: &'a LopdfDocument,
    fonts: &BTreeMap<Vec<u8>, &'a Dictionary>,
    font_resource: Option<&[u8]>,
    object: &Object,
    max_decompressed_bytes: usize,
    memo: &mut FontEncodingMemo<'a>,
) -> Option<String> {
    let bytes = object.as_str().ok()?;
    if let Some(font_resource) = font_resource
        && let Some(font) = fonts.get(font_resource)
        && let Some(encoding) = memo.resolve(pdf, *font, max_decompressed_bytes)
        && let Ok(text) = LopdfDocument::decode_text(encoding, bytes)
    {
        return Some(text);
    }

    bytes
        .iter()
        .all(|byte| byte.is_ascii_graphic() || byte.is_ascii_whitespace())
        .then(|| String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        FontEncodingMemo, PdfTextFragmentEvidence, abstract_heading_inference_status,
        extract_text_fragment_evidence, extract_text_fragment_evidence_with_memo,
        infer_abstract_heading,
    };
    use lopdf::content::{Content, Operation};
    use lopdf::{Dictionary, Document as LopdfDocument, Object, Stream, StringFormat, dictionary};

    fn fragment(
        sequence_index: usize,
        text: &str,
        font: &str,
        size: f32,
        y: f32,
    ) -> PdfTextFragmentEvidence {
        PdfTextFragmentEvidence {
            page: 1,
            sequence_index,
            text: text.into(),
            font_resource: Some(font.into()),
            font_size: Some(size),
            x: Some(72.0),
            y: Some(y),
        }
    }

    #[test]
    fn operator_split_abstract_line_is_reconstructed_before_inference() {
        let evidence = vec![
            fragment(0, "Ab", "FHeading", 12.0, 700.0),
            fragment(1, "stract", "FHeading", 12.0, 700.0),
            fragment(
                2,
                "This is a sufficiently long abstract body line.",
                "FBody",
                10.0,
                682.0,
            ),
            fragment(3, "1", "FHeading", 12.0, 640.0),
            fragment(4, "Introduction", "FHeading", 12.0, 640.0),
        ];

        assert_eq!(
            abstract_heading_inference_status(&evidence, 1, "1 Introduction"),
            "resolved"
        );
        let candidate = infer_abstract_heading(&evidence, 1, "1 Introduction")
            .expect("split Abstract layout line should resolve");
        assert_eq!(candidate.sequence_index, 0);
    }

    #[test]
    fn same_style_abstract_line_still_fails_closed() {
        let evidence = vec![
            fragment(0, "Abstract", "FBody", 10.0, 700.0),
            fragment(
                1,
                "This is a sufficiently long abstract body line.",
                "FBody",
                10.0,
                682.0,
            ),
            fragment(2, "1 Introduction", "FHeading", 12.0, 640.0),
        ];

        assert_eq!(
            abstract_heading_inference_status(&evidence, 1, "1 Introduction"),
            "abstract_style_not_distinct"
        );
        assert!(infer_abstract_heading(&evidence, 1, "1 Introduction").is_none());
    }

    // PDF-2b: the memo below must be a pure memoisation.  These fixtures are
    // built in memory with lopdf so they exercise the real
    // `get_font_encoding_with_limit` path, including a Type0 font whose text
    // only decodes through its `/ToUnicode` CMap.

    const MAX_PAGE_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;

    /// A two-byte codespace CMap mapping <0001> to U+4E00, <0002> to U+4E01 and
    /// <0003> to U+4E02.
    fn identity_cmap() -> Vec<u8> {
        b"/CIDInit /ProcSet findresource begin
\
12 dict begin
\
begincmap
\
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
\
/CMapName /Adobe-Identity-UCS def
\
/CMapType 2 def
\
1 begincodespacerange
\
<0000> <FFFF>
\
endcodespacerange
\
2 beginbfrange
\
<0001> <0002> <4E00>
\
<0003> <0003> <4E02>
\
endbfrange
\
endcmap
\
CMapName currentdict /CMap defineresource pop
\
end
\
end
"
        .to_vec()
    }

    /// The same CMap with the `endcmap` trailer removed: lopdf's CMap parser
    /// rejects it and the font falls back to its standard encoding.
    fn truncated_cmap() -> Vec<u8> {
        b"/CIDInit /ProcSet findresource begin
\
12 dict begin
\
begincmap
\
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
\
1 begincodespacerange
\
<0000> <FFFF>
\
endcodespacerange
\
2 beginbfrange
\
<0001> <0002> <4E00>
\
endbfrange
\
end
"
        .to_vec()
    }

    fn type0_font(cmap: Vec<u8>) -> Dictionary {
        dictionary! {
            "Type" => "Font",
            "Subtype" => "Type0",
            "BaseFont" => "FixtureCJK",
            "Encoding" => "Identity-H",
            "ToUnicode" => Stream::new(dictionary! {}, cmap),
        }
    }

    fn pdf_with_font(font: Dictionary, pages: Vec<Vec<Operation>>) -> LopdfDocument {
        let mut document = LopdfDocument::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(font);
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => font_id,
            },
        });

        let mut page_ids = Vec::new();
        for operations in pages {
            let content_id = document.add_object(Stream::new(
                dictionary! {},
                Content { operations }
                    .encode()
                    .expect("fixture content should encode"),
            ));
            let page_id = document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
            });
            page_ids.push(page_id);
        }

        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => page_ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(),
                "Count" => page_ids.len() as i64,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        document
    }

    /// One `BT`/`ET` block showing `strings` top to bottom, 14 pt apart.
    fn page_operations(font_name: &str, size: i64, strings: &[Vec<u8>]) -> Vec<Operation> {
        let mut operations = vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![font_name.into(), size.into()]),
            Operation::new(
                "Tm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    72.into(),
                    700.into(),
                ],
            ),
        ];
        for (index, bytes) in strings.iter().enumerate() {
            if index > 0 {
                operations.push(Operation::new("Td", vec![0.into(), (-14).into()]));
            }
            operations.push(Operation::new(
                "Tj",
                vec![Object::String(bytes.clone(), StringFormat::Hexadecimal)],
            ));
        }
        operations.push(Operation::new("ET", vec![]));
        operations
    }

    fn page_numbers(document: &LopdfDocument) -> Vec<u32> {
        document.get_pages().keys().copied().collect()
    }

    fn hex(text: &[u8]) -> Vec<u8> {
        text.to_vec()
    }

    fn cjk_fragment(
        page: u32,
        sequence_index: usize,
        text: &str,
        font_size: f32,
        x: f32,
        y: f32,
    ) -> PdfTextFragmentEvidence {
        PdfTextFragmentEvidence {
            page,
            sequence_index,
            text: text.into(),
            font_resource: Some("F1".into()),
            font_size: Some(font_size),
            x: Some(x),
            y: Some(y),
        }
    }

    #[test]
    fn one_type0_font_is_resolved_once_for_the_whole_document() {
        // 10 pages x 20 shown strings in one shared font: 200 strings, but the
        // encoding must be resolved once, not 200 times.
        let strings = (0..20)
            .map(|index| match index % 3 {
                0 => hex(&[0x00, 0x01]),
                1 => hex(&[0x00, 0x02]),
                _ => hex(&[0x00, 0x01, 0x00, 0x02]),
            })
            .collect::<Vec<_>>();
        let pages = (0..10)
            .map(|_| page_operations("F1", 12, &strings))
            .collect::<Vec<_>>();
        let document = pdf_with_font(type0_font(identity_cmap()), pages);

        let mut memo = FontEncodingMemo::new();
        let (evidence, errors) = extract_text_fragment_evidence_with_memo(
            &document,
            &page_numbers(&document),
            MAX_PAGE_DECOMPRESSED_BYTES,
            &mut memo,
        );

        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(evidence.len(), 200);
        assert_eq!(memo.resolutions(), 1, "one resolution per distinct font");
        assert_eq!(evidence[0].text, "\u{4e00}");
        assert_eq!(evidence[2].text, "\u{4e00}\u{4e01}");
        assert_eq!(evidence[199].page, 10);
        assert_eq!(evidence[199].sequence_index, 199);
    }

    #[test]
    fn type0_font_evidence_matches_the_hand_written_list() {
        let document = pdf_with_font(
            type0_font(identity_cmap()),
            vec![
                page_operations(
                    "F1",
                    12,
                    &[hex(&[0x00, 0x01]), hex(&[0x00, 0x01, 0x00, 0x02])],
                ),
                page_operations("F1", 10, &[hex(&[0x00, 0x02])]),
            ],
        );

        let mut memo = FontEncodingMemo::new();
        let (evidence, errors) = extract_text_fragment_evidence_with_memo(
            &document,
            &page_numbers(&document),
            MAX_PAGE_DECOMPRESSED_BYTES,
            &mut memo,
        );

        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(
            evidence,
            vec![
                cjk_fragment(1, 0, "\u{4e00}", 12.0, 72.0, 700.0),
                cjk_fragment(1, 1, "\u{4e00}\u{4e01}", 12.0, 72.0, 686.0),
                cjk_fragment(2, 2, "\u{4e01}", 10.0, 72.0, 700.0),
            ]
        );
        assert_eq!(memo.resolutions(), 1);
        // The public entry point returns the same fragments.
        let (again, again_errors) = extract_text_fragment_evidence(
            &document,
            &page_numbers(&document),
            MAX_PAGE_DECOMPRESSED_BYTES,
        );
        assert!(again_errors.is_empty());
        assert_eq!(again, evidence);
    }

    #[test]
    fn broken_to_unicode_keeps_the_text_and_is_resolved_once() {
        // lopdf degrades a CMap it cannot parse to the font's standard encoding;
        // the memo caches that outcome instead of re-parsing per shown string.
        let strings = (0..20)
            .map(|_| Object::string_literal(b"Hello".to_vec()))
            .collect::<Vec<_>>();
        let pages = (0..10)
            .map(|_| {
                let mut operations = vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 12.into()]),
                    Operation::new(
                        "Tm",
                        vec![
                            1.into(),
                            0.into(),
                            0.into(),
                            1.into(),
                            72.into(),
                            700.into(),
                        ],
                    ),
                ];
                for (index, string) in strings.iter().enumerate() {
                    if index > 0 {
                        operations.push(Operation::new("Td", vec![0.into(), (-14).into()]));
                    }
                    operations.push(Operation::new("Tj", vec![string.clone()]));
                }
                operations.push(Operation::new("ET", vec![]));
                operations
            })
            .collect::<Vec<_>>();
        let document = pdf_with_font(type0_font(truncated_cmap()), pages);

        let mut memo = FontEncodingMemo::new();
        let (evidence, errors) = extract_text_fragment_evidence_with_memo(
            &document,
            &page_numbers(&document),
            MAX_PAGE_DECOMPRESSED_BYTES,
            &mut memo,
        );

        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(evidence.len(), 200);
        assert!(evidence.iter().all(|item| item.text == "Hello"));
        assert_eq!(memo.resolutions(), 1, "one resolution per distinct font");
    }

    #[test]
    fn font_without_a_font_type_keeps_the_ascii_fallback_and_is_resolved_once() {
        // No /Type /Font: `get_font_encoding_with_limit` returns Err, which the
        // memo stores as None and reuses for every shown string.
        let font = dictionary! {
            "Subtype" => "Type0",
            "BaseFont" => "NotAFont",
        };
        let strings = (0..20)
            .map(|_| Object::string_literal(b"Hello".to_vec()))
            .collect::<Vec<_>>();
        let pages = (0..10)
            .map(|_| {
                let mut operations = vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 12.into()]),
                    Operation::new(
                        "Tm",
                        vec![
                            1.into(),
                            0.into(),
                            0.into(),
                            1.into(),
                            72.into(),
                            700.into(),
                        ],
                    ),
                ];
                for (index, string) in strings.iter().enumerate() {
                    if index > 0 {
                        operations.push(Operation::new("Td", vec![0.into(), (-14).into()]));
                    }
                    operations.push(Operation::new("Tj", vec![string.clone()]));
                }
                operations.push(Operation::new("ET", vec![]));
                operations
            })
            .collect::<Vec<_>>();
        let document = pdf_with_font(font, pages);

        let mut memo = FontEncodingMemo::new();
        let (evidence, errors) = extract_text_fragment_evidence_with_memo(
            &document,
            &page_numbers(&document),
            MAX_PAGE_DECOMPRESSED_BYTES,
            &mut memo,
        );

        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(evidence.len(), 200);
        assert!(evidence.iter().all(|item| item.text == "Hello"));
        assert_eq!(memo.resolutions(), 1, "the failed resolution is cached");
    }

    #[test]
    fn font_name_missing_from_the_page_resources_keeps_the_output_unchanged() {
        // The content selects /F9; the page resources only define /F1.
        let document = pdf_with_font(
            type0_font(identity_cmap()),
            vec![page_operations("F9", 12, &[hex(&[0x00, 0x01])])],
        );

        let mut memo = FontEncodingMemo::new();
        let (evidence, errors) = extract_text_fragment_evidence_with_memo(
            &document,
            &page_numbers(&document),
            MAX_PAGE_DECOMPRESSED_BYTES,
            &mut memo,
        );

        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        // The bytes are not printable ASCII, so the old fall-through produced no
        // fragment at all; the memo must not invent one.
        assert!(evidence.is_empty());
        assert_eq!(
            memo.resolutions(),
            0,
            "nothing to resolve for a missing font"
        );

        // Printable ASCII keeps the old ASCII fallback, unchanged.
        let document = pdf_with_font(
            type0_font(identity_cmap()),
            vec![vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F9".into(), 12.into()]),
                Operation::new(
                    "Tm",
                    vec![
                        1.into(),
                        0.into(),
                        0.into(),
                        1.into(),
                        72.into(),
                        700.into(),
                    ],
                ),
                Operation::new("Tj", vec![Object::string_literal(b"Missing font".to_vec())]),
                Operation::new("ET", vec![]),
            ]],
        );
        let (evidence, errors) = extract_text_fragment_evidence(
            &document,
            &page_numbers(&document),
            MAX_PAGE_DECOMPRESSED_BYTES,
        );
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(
            evidence,
            vec![PdfTextFragmentEvidence {
                page: 1,
                sequence_index: 0,
                text: "Missing font".into(),
                font_resource: Some("F9".into()),
                font_size: Some(12.0),
                x: Some(72.0),
                y: Some(700.0),
            }]
        );
    }
}
