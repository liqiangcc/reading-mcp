//! The page text walk for the default PDF backend, with the inter-word space rule that
//! lopdf 0.44 misses (#232).
//!
//! This is `lopdf`'s `Document::extract_text_chunks_from_page` walk, copied rather than
//! called so the one rule that drops spaces can be fixed without forking the crate:
//!
//! * a chunk is a font run: `Tf` closes it, and `'`, `"`, `T*` and `ET` append `\n`;
//! * inside a `TJ` array, a numeric adjustment is the pen moving before the next glyph,
//!   in thousandths of the font size, and lopdf calls it a word space when it is an
//!   `Integer` below -100 (`collect_text`, lopdf-0.44 `src/parser_aux.rs:363-380`).
//!
//! pdfTeX writes that same inter-word adjustment as a `Real` whenever the line is
//! justified: the glue stretch is fractional, so a line whose words are separated by
//! `-416.003` gets no space while a ragged line whose gaps are `-416` does. On NBER
//! w24458 that is most of the body text ("China'smarketandinvestorsareseparated from the
//! restoftheworld"). The walk below is lopdf's, unchanged, except that the space test
//! reads the number as a float.
//!
//! Two guards keep the added spaces honest:
//!
//! * a `Real` gap only adds a space when the chunk does not already end in whitespace, so
//!   a string that carries its own space glyph cannot gain a second one (lopdf's
//!   `Integer` arm is left exactly as it was, double spaces included);
//! * a space this walk inserted between two CJK characters is removed again: CJK runs are
//!   set without spaces, and a CJK font's `TJ` adjustments (commonly -250 for a word gap
//!   in mixed text, and the same magnitude between ideographs) are not word boundaries.

use std::collections::BTreeMap;

use lopdf::content::Content;
use lopdf::{Document as LopdfDocument, Encoding, Error, Object, ObjectId, Result};

/// A `TJ` adjustment at or below this many thousandths of an em separates two words.
/// lopdf's own threshold, kept so an `Integer` gap publishes exactly the text it did.
const WORD_GAP_THOUSANDTHS: f32 = 100.0;

pub(super) fn extract_text_chunks_with_spacing(
    pdf: &LopdfDocument,
    page_numbers: &[u32],
    max_decompressed_bytes: usize,
) -> Vec<Result<String>> {
    let pages = pdf.get_pages();
    page_numbers
        .iter()
        .flat_map(|page_number| {
            match extract_page_chunks(pdf, &pages, *page_number, max_decompressed_bytes) {
                Ok(chunks) => chunks,
                Err(error) => vec![Err(error)],
            }
        })
        .collect()
}

fn extract_page_chunks(
    pdf: &LopdfDocument,
    pages: &BTreeMap<u32, ObjectId>,
    page_number: u32,
    max_decompressed_bytes: usize,
) -> Result<Vec<Result<String>>> {
    let mut chunks: Vec<Result<String>> = Vec::new();

    let page_id = *pages
        .get(&page_number)
        .ok_or(Error::PageNumberNotFound(page_number))?;
    let fonts = pdf.get_page_fonts(page_id)?;
    let encodings: BTreeMap<Vec<u8>, Encoding<'_>> = fonts
        .into_iter()
        .filter_map(|(name, font)| {
            match font.get_font_encoding_with_limit(pdf, max_decompressed_bytes) {
                Ok(encoding) => Some((name, encoding)),
                Err(error) => {
                    chunks.push(Err(error));
                    None
                }
            }
        })
        .collect();
    let content_data = pdf.get_page_content_with_limit(page_id, max_decompressed_bytes)?;
    let content = Content::decode(&content_data)?;

    let mut current_encoding = None;
    let mut current_text = String::new();
    let mut inserted_gaps: Vec<usize> = Vec::new();
    for operation in &content.operations {
        match operation.operator.as_str() {
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
                    drop_cjk_gap_spaces(&mut current_text, &inserted_gaps);
                    chunks.push(Ok(current_text));
                    current_text = String::new();
                    inserted_gaps.clear();
                }
            }
            "Tj" | "TJ" => {
                if let Some(encoding) = current_encoding
                    && let Err(error) = collect_text(
                        &mut current_text,
                        &mut inserted_gaps,
                        encoding,
                        &operation.operands,
                    )
                {
                    chunks.push(Err(error));
                }
            }
            "'" => {
                if let Some(encoding) = current_encoding {
                    if !current_text.ends_with('\n') {
                        current_text.push('\n');
                    }
                    if let Err(error) = collect_text(
                        &mut current_text,
                        &mut inserted_gaps,
                        encoding,
                        &operation.operands,
                    ) {
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
                            &mut inserted_gaps,
                            encoding,
                            std::slice::from_ref(string_operand),
                        )
                    {
                        chunks.push(Err(error));
                    }
                }
            }
            "T*" if !current_text.ends_with('\n') => current_text.push('\n'),
            "T*" => {}
            "ET" if !current_text.ends_with('\n') => current_text.push('\n'),
            "ET" => {}
            _ => {}
        }
    }
    if !current_text.is_empty() {
        drop_cjk_gap_spaces(&mut current_text, &inserted_gaps);
        chunks.push(Ok(current_text));
    }

    Ok(chunks)
}

/// lopdf's `collect_text`, with the word-space test widened to `Real` adjustments and
/// every inserted space recorded in `inserted_gaps` (its byte index in `text`).
fn collect_text(
    text: &mut String,
    inserted_gaps: &mut Vec<usize>,
    encoding: &Encoding<'_>,
    operands: &[Object],
) -> Result<()> {
    for operand in operands {
        match operand {
            Object::String(bytes, _) => {
                encoding.write_to_string(bytes, text)?;
            }
            Object::Array(array) => {
                collect_text(text, inserted_gaps, encoding, array)?;
                text.push(' ');
            }
            Object::Integer(value) if *value < -100 => {
                text.push(' ');
            }
            Object::Real(value)
                if *value < -WORD_GAP_THOUSANDTHS
                    && !text.is_empty()
                    && !text.ends_with(char::is_whitespace) =>
            {
                inserted_gaps.push(text.len());
                text.push(' ');
            }
            _ => {}
        }
    }
    Ok(())
}

/// Removes the spaces this walk inserted between two CJK characters. `gaps` holds their
/// byte indices in `text`, in the order they were pushed, so they are removed backwards
/// and no earlier index shifts.
fn drop_cjk_gap_spaces(text: &mut String, gaps: &[usize]) {
    for &index in gaps.iter().rev() {
        if index >= text.len() || !text.is_char_boundary(index) {
            continue;
        }
        let previous = text[..index].chars().next_back();
        let next = text[index..].chars().nth(1);
        if previous.is_some_and(is_cjk) && next.is_some_and(is_cjk) {
            text.remove(index);
        }
    }
}

/// CJK script and CJK-set punctuation: an ideograph, a kana, a hangul syllable, a fullwidth
/// form or CJK punctuation. A space between two of these is never a word boundary.
fn is_cjk(character: char) -> bool {
    let code = character as u32;
    (0x3000..=0x303F).contains(&code)
        || (0x3040..=0x30FF).contains(&code)
        || (0x31F0..=0x31FF).contains(&code)
        || (0x3400..=0x4DBF).contains(&code)
        || (0x4E00..=0x9FFF).contains(&code)
        || (0xAC00..=0xD7AF).contains(&code)
        || (0xF900..=0xFAFF).contains(&code)
        || (0xFE30..=0xFE4F).contains(&code)
        || (0xFF00..=0xFFEF).contains(&code)
        || (0x20000..=0x2FA1F).contains(&code)
}

#[cfg(test)]
mod tests {
    use super::{collect_text, drop_cjk_gap_spaces, extract_text_chunks_with_spacing};
    use lopdf::content::{Content, Operation};
    use lopdf::{Document as LopdfDocument, Encoding, Object, Stream, StringFormat, dictionary};

    const WIN_ANSI: &[u8] = b"WinAnsiEncoding";

    fn string(value: &str) -> Object {
        Object::String(value.as_bytes().to_vec(), StringFormat::Literal)
    }

    /// A UTF-16BE code string, the shape `Encoding::SimpleEncoding(b"UniGB-UCS2-H")` decodes.
    fn utf16_string(value: &str) -> Object {
        Object::String(
            value
                .encode_utf16()
                .flat_map(|unit| unit.to_be_bytes())
                .collect(),
            StringFormat::Literal,
        )
    }

    fn collect(operands: &[Object]) -> String {
        let encoding = Encoding::SimpleEncoding(WIN_ANSI);
        let mut text = String::new();
        let mut gaps = Vec::new();
        collect_text(&mut text, &mut gaps, &encoding, operands).expect("decoding a literal");
        drop_cjk_gap_spaces(&mut text, &gaps);
        text
    }

    fn tj(items: Vec<Object>) -> Vec<Object> {
        vec![Object::Array(items)]
    }

    #[test]
    fn a_real_gap_that_means_a_space_becomes_one() {
        let text = collect(&tj(vec![
            string("financial"),
            Object::Real(-416.003),
            string("systems"),
        ]));
        assert_eq!(text, "financial systems ");
    }

    #[test]
    fn a_kerned_pair_does_not_gain_a_space() {
        let text = collect(&tj(vec![string("V"), Object::Real(-27.001), string("A")]));
        assert_eq!(text, "VA ");
        let text = collect(&tj(vec![string("w"), Object::Real(27.001), string("orld")]));
        assert_eq!(text, "world ");
    }

    #[test]
    fn a_ligature_stays_inside_its_word() {
        // U+FB00 is the `ff` ligature glyph: one code, two characters, never a word boundary.
        let encoding = Encoding::SimpleEncoding(b"UniGB-UCS2-H");
        let operands = tj(vec![
            utf16_string("di"),
            Object::Real(0.0),
            utf16_string("\u{fb00}erent"),
        ]);
        let mut text = String::new();
        let mut gaps = Vec::new();
        collect_text(&mut text, &mut gaps, &encoding, &operands).expect("decoding UTF-16BE");
        drop_cjk_gap_spaces(&mut text, &gaps);
        assert_eq!(text, "di\u{fb00}erent ");
    }

    #[test]
    fn a_letter_spaced_title_does_not_gain_spaces() {
        // Tracking moves the next glyph right, so it is a negative adjustment; a title tracked
        // at 54 thousandths of an em stays below the word-gap threshold.
        let text = collect(&tj(vec![
            string("I"),
            Object::Real(-54.0),
            string("N"),
            Object::Real(-54.0),
            string("T"),
            Object::Real(-54.0),
            string("R"),
            Object::Real(-54.0),
            string("O"),
        ]));
        assert_eq!(text, "INTRO ");
    }

    #[test]
    fn a_justified_line_gets_one_space_per_gap_and_no_double() {
        let text = collect(&tj(vec![
            string("the"),
            Object::Real(-416.003),
            string("low"),
            Object::Real(-242.003),
            string("processing"),
            Object::Real(-241.995),
            string("capacity"),
        ]));
        assert_eq!(text, "the low processing capacity ");
        // A string that carries its own space glyph is not given a second one.
        let text = collect(&tj(vec![
            string("Eugene "),
            Object::Real(-111.75),
            string("Fama"),
        ]));
        assert_eq!(text, "Eugene Fama ");
    }

    #[test]
    fn an_integer_gap_keeps_lopdfs_behaviour() {
        let text = collect(&tj(vec![
            string("the"),
            Object::Integer(-242),
            string("low"),
        ]));
        assert_eq!(text, "the low ");
        // lopdf pushes the space even when the string already ended in one.
        let text = collect(&tj(vec![
            string("Eugene "),
            Object::Integer(-111),
            string("Fama"),
        ]));
        assert_eq!(text, "Eugene  Fama ");
    }

    #[test]
    fn a_cjk_run_gets_no_space_between_its_characters() {
        let encoding = Encoding::SimpleEncoding(b"UniGB-UCS2-H");
        let operands = tj(vec![
            utf16_string("\u{4e2d}"),
            Object::Real(-250.0),
            utf16_string("\u{6587}\u{5b57}"),
        ]);
        let mut text = String::new();
        let mut gaps = Vec::new();
        collect_text(&mut text, &mut gaps, &encoding, &operands).expect("decoding UTF-16BE");
        drop_cjk_gap_spaces(&mut text, &gaps);
        assert_eq!(text, "\u{4e2d}\u{6587}\u{5b57} ");
    }

    #[test]
    fn a_cjk_punctuation_gap_is_not_a_space_either() {
        let encoding = Encoding::SimpleEncoding(b"UniGB-UCS2-H");
        let operands = tj(vec![
            utf16_string("\u{4e2d}\u{6587}\u{3002}"),
            Object::Real(-250.0),
            utf16_string("\u{5b57}"),
        ]);
        let mut text = String::new();
        let mut gaps = Vec::new();
        collect_text(&mut text, &mut gaps, &encoding, &operands).expect("decoding UTF-16BE");
        drop_cjk_gap_spaces(&mut text, &gaps);
        assert_eq!(text, "\u{4e2d}\u{6587}\u{3002}\u{5b57} ");
    }

    #[test]
    fn a_mixed_cjk_latin_line_keeps_the_space_next_to_the_latin_run() {
        let encoding = Encoding::SimpleEncoding(b"UniGB-UCS2-H");
        let operands = tj(vec![
            utf16_string("\u{4e2d}\u{6587}"),
            Object::Real(-250.0),
            utf16_string("pain"),
        ]);
        let mut text = String::new();
        let mut gaps = Vec::new();
        collect_text(&mut text, &mut gaps, &encoding, &operands).expect("decoding UTF-16BE");
        drop_cjk_gap_spaces(&mut text, &gaps);
        assert_eq!(text, "\u{4e2d}\u{6587} pain ");
    }

    /// The w24458 construct end to end: one page whose `TJ` array separates its words with
    /// `Real` adjustments, which lopdf's own walker publishes glued.
    #[test]
    fn a_page_with_real_tj_gaps_extracts_its_words_apart() {
        let mut document = LopdfDocument::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new(
                    "TJ",
                    vec![Object::Array(vec![
                        Object::string_literal("China's"),
                        Object::Real(-416.003),
                        Object::string_literal("market"),
                        Object::Real(-415.995),
                        Object::string_literal("and"),
                        Object::Real(-27.0),
                        Object::string_literal("investors"),
                    ])],
                ),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            content.encode().expect("fixture content should encode"),
        ));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => 1,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);

        let chunks = extract_text_chunks_with_spacing(&document, &[1], 16 * 1024 * 1024);
        let text = chunks
            .into_iter()
            .map(|chunk| chunk.expect("the fixture page decodes"))
            .collect::<String>();
        assert_eq!(text.trim(), "China's market andinvestors");
    }
}
