//! PDF text extraction with the spacing semantics of the PDF number type.
//!
//! This is lopdf 0.44's content walk, kept in process and byte-for-byte compatible except for
//! one correction: numeric entries in a `TJ` array are spacing adjustments whether the lexer
//! spelled them as integers (`-300`) or reals (`-300.004`). pdfTeX writes justified text both
//! ways. Treating only the integer spelling as a possible word gap joins the words around every
//! real-valued adjustment.

use std::collections::BTreeMap;

use lopdf::content::Content;
use lopdf::{Document as LopdfDocument, Encoding, Error, Object, Result};

/// lopdf's long-standing word-gap threshold, in thousandths of a text-space unit. A negative
/// adjustment moves the next glyph forward, so `-300` is a 0.3 em extra gap; small positive and
/// negative values are ordinary kerning and never reach this threshold.
const TJ_WORD_GAP_THRESHOLD: f32 = 100.0;

/// The page text chunks of `page_numbers`, with the same chunk boundaries, error ordering and
/// decompression limit as `Document::extract_text_chunks_with_limit`.
pub(super) fn extract_text_chunks_with_limit(
    pdf: &LopdfDocument,
    page_numbers: &[u32],
    max_decompressed_size: usize,
) -> Vec<Result<String>> {
    let pages = pdf.get_pages();
    page_numbers
        .iter()
        .flat_map(|page_number| {
            match extract_text_chunks_from_page(pdf, &pages, *page_number, max_decompressed_size) {
                Ok(chunks) => chunks,
                Err(error) => vec![Err(error)],
            }
        })
        .collect()
}

fn extract_text_chunks_from_page(
    pdf: &LopdfDocument,
    pages: &BTreeMap<u32, lopdf::ObjectId>,
    page_number: u32,
    max_decompressed_size: usize,
) -> Result<Vec<Result<String>>> {
    let mut chunks = Vec::new();
    let page_id = *pages
        .get(&page_number)
        .ok_or(Error::PageNumberNotFound(page_number))?;
    let fonts = pdf.get_page_fonts(page_id)?;
    let encodings: BTreeMap<Vec<u8>, Encoding> = fonts
        .into_iter()
        .filter_map(|(name, font)| {
            match font.get_font_encoding_with_limit(pdf, max_decompressed_size) {
                Ok(encoding) => Some((name, encoding)),
                Err(error) => {
                    chunks.push(Err(error));
                    None
                }
            }
        })
        .collect();
    let content_data = pdf.get_page_content_with_limit(page_id, max_decompressed_size)?;
    let content = Content::decode(&content_data)?;

    let mut current_encoding = None;
    let mut current_text = String::new();
    for operation in &content.operations {
        match operation.operator.as_ref() {
            "Tf" => {
                let current_font = operation
                    .operands
                    .first()
                    .ok_or_else(|| Error::Syntax("missing font operand".to_string()))?
                    .as_name();
                current_encoding = match current_font {
                    Ok(font) => encodings.get(font),
                    Err(error) => {
                        chunks.push(Err(error));
                        None
                    }
                };
                if !current_text.is_empty() {
                    chunks.push(Ok(std::mem::take(&mut current_text)));
                }
            }
            "Tj" | "TJ" => {
                if let Some(encoding) = current_encoding
                    && let Err(error) =
                        collect_text(&mut current_text, encoding, &operation.operands)
                {
                    chunks.push(Err(error));
                }
            }
            "'" => {
                if let Some(encoding) = current_encoding {
                    if !current_text.ends_with('\n') {
                        current_text.push('\n');
                    }
                    if let Err(error) =
                        collect_text(&mut current_text, encoding, &operation.operands)
                    {
                        chunks.push(Err(error));
                    }
                }
            }
            "\"" => {
                if let Some(encoding) = current_encoding {
                    if !current_text.ends_with('\n') {
                        current_text.push('\n');
                    }
                    if let Some(string_operand) = operation.operands.get(2)
                        && let Err(error) = collect_text(
                            &mut current_text,
                            encoding,
                            std::slice::from_ref(string_operand),
                        )
                    {
                        chunks.push(Err(error));
                    }
                }
            }
            "T*" | "ET" if !current_text.ends_with('\n') => current_text.push('\n'),
            _ => {}
        }
    }
    if !current_text.is_empty() {
        chunks.push(Ok(current_text));
    }
    Ok(chunks)
}

fn collect_text(text: &mut String, encoding: &Encoding, operands: &[Object]) -> Result<()> {
    for operand in operands {
        match operand {
            Object::String(bytes, _) => encoding.write_to_string(bytes, text)?,
            Object::Array(array) => {
                collect_text(text, encoding, array)?;
                text.push(' ');
            }
            Object::Integer(adjustment) if *adjustment < -TJ_WORD_GAP_THRESHOLD as i64 => {
                text.push(' ')
            }
            Object::Real(adjustment) if *adjustment < -TJ_WORD_GAP_THRESHOLD => text.push(' '),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn winansi_text(operands: Vec<Object>) -> String {
        let mut text = String::new();
        collect_text(
            &mut text,
            &Encoding::SimpleEncoding(b"WinAnsiEncoding"),
            &operands,
        )
        .expect("WinAnsi text decodes");
        text
    }

    fn ucs2_text(operands: Vec<Object>) -> String {
        let mut text = String::new();
        collect_text(
            &mut text,
            &Encoding::SimpleEncoding(b"UniGB-UCS2-H"),
            &operands,
        )
        .expect("UCS-2 text decodes");
        text
    }

    fn literal_ascii(text: &str) -> Object {
        Object::String(text.as_bytes().to_vec(), lopdf::StringFormat::Literal)
    }

    fn literal_ucs2(text: &str) -> Object {
        Object::String(
            text.encode_utf16()
                .flat_map(|unit| unit.to_be_bytes())
                .collect(),
            lopdf::StringFormat::Literal,
        )
    }

    fn tj(items: Vec<Object>) -> Vec<Object> {
        vec![Object::Array(items)]
    }

    #[test]
    fn a_real_valued_word_gap_gets_a_space() {
        assert_eq!(
            winansi_text(tj(vec![
                literal_ascii("economic"),
                Object::Real(-314.006),
                literal_ascii("and"),
            ])),
            "economic and "
        );
    }

    #[test]
    fn an_integer_word_gap_keeps_the_old_space() {
        assert_eq!(
            winansi_text(tj(vec![
                literal_ascii("economic"),
                Object::Integer(-314),
                literal_ascii("and"),
            ])),
            "economic and "
        );
    }

    #[test]
    fn a_kerned_pair_does_not_gain_a_space() {
        for adjustment in [
            Object::Integer(27),
            Object::Real(27.001),
            Object::Real(-27.001),
        ] {
            assert_eq!(
                winansi_text(tj(vec![
                    literal_ascii("wo"),
                    adjustment,
                    literal_ascii("rld")
                ])),
                "world "
            );
        }
    }

    #[test]
    fn a_ligature_does_not_gain_a_space() {
        assert_eq!(
            winansi_text(tj(vec![
                literal_ascii("fi"),
                Object::Real(-1.0),
                literal_ascii("nal"),
            ])),
            "final "
        );
    }

    #[test]
    fn a_letter_spaced_line_does_not_gain_spaces() {
        assert_eq!(
            winansi_text(tj(vec![
                literal_ascii("A"),
                Object::Real(-80.0),
                literal_ascii("B"),
                Object::Real(-80.0),
                literal_ascii("C"),
            ])),
            "ABC "
        );
    }

    #[test]
    fn a_cjk_run_does_not_gain_spaces() {
        assert_eq!(
            ucs2_text(tj(vec![
                literal_ucs2("中"),
                Object::Real(-40.0),
                literal_ucs2("文"),
                Object::Real(-40.0),
                literal_ucs2("字"),
            ])),
            "中文字 "
        );
    }

    #[test]
    fn a_mixed_cjk_latin_line_only_marks_the_word_gap() {
        assert_eq!(
            ucs2_text(tj(vec![
                literal_ucs2("中文"),
                Object::Real(-40.0),
                literal_ucs2("字"),
                Object::Real(-320.0),
                literal_ucs2("Latin"),
            ])),
            "中文字 Latin "
        );
    }
}
