//! PDF text extraction with the spacing semantics of the PDF number type.
//!
//! This is lopdf 0.44's content walk, kept in process and byte-for-byte compatible except for
//! one correction: numeric entries in a `TJ` array are spacing adjustments whether the lexer
//! spelled them as integers (`-300`) or reals (`-300.004`). pdfTeX writes justified text both
//! ways. Treating only the integer spelling as a possible word gap joins the words around every
//! real-valued adjustment.
//!
//! A gap between two CJK characters is never a word boundary, so no space is inserted there:
//! CJK fonts encode justification and compressed punctuation as large negative adjustments.
//!
//! The walk keeps the font encodings of the whole document (`FontEncodings`) instead of
//! resolving one per page, and decodes a one-byte `/ToUnicode` CMap through a table
//! (`CodeTable`) instead of lopdf's generic four-byte walk.  Both produce the same text; the
//! second falls back to lopdf for a run the table cannot answer.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use lopdf::content::Content;
use lopdf::{Dictionary, Document as LopdfDocument, Encoding, Error, Object, ObjectId, Result};

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
    let mut encodings = FontEncodings::default();
    page_numbers
        .iter()
        .flat_map(|page_number| {
            match extract_text_chunks_from_page(
                pdf,
                &pages,
                *page_number,
                max_decompressed_size,
                &mut encodings,
            ) {
                Ok(chunks) => chunks,
                Err(error) => vec![Err(error)],
            }
        })
        .collect()
}

/// The encodings of the fonts one document puts on its pages, resolved once per
/// font instead of once per page.
///
/// A font dictionary is shared by every page that sets it, so the page walk used
/// to rebuild its encoding on every page that draws text with it, and a CMap is
/// expensive to parse: 0.2 to 0.8 ms for the 256-code CMaps of a pdfTeX
/// document, against microseconds for a `/Differences` map.  The key is the
/// dictionary's address, which is stable for as long as the document is not
/// mutated, the same key `pdf_layout::FontEncodingMemo` uses.
///
/// A simple font whose only encoding source is its `/ToUnicode` decodes the same
/// as every other font that references that stream, so those share one parse.
#[derive(Default)]
struct FontEncodings<'a> {
    resolved: HashMap<usize, Rc<ResolvedEncoding<'a>>>,
    shared: HashMap<ObjectId, Rc<ResolvedEncoding<'a>>>,
}

/// One font's encoding and, when it decodes through a `/ToUnicode` CMap, the
/// table that answers a one-byte text run.
struct ResolvedEncoding<'a> {
    encoding: Encoding<'a>,
    table: Option<CodeTable>,
}

impl<'a> ResolvedEncoding<'a> {
    /// An encoding with no CMap table, decoded by lopdf.
    #[cfg(test)]
    fn plain(encoding: Encoding<'a>) -> Self {
        Self {
            encoding,
            table: None,
        }
    }

    /// Decode one string operand, through the CMap table when it answers the run.
    fn write_text(&self, bytes: &[u8], text: &mut String) -> Result<()> {
        if let Some(table) = &self.table
            && table.decode_into(bytes, text)
        {
            return Ok(());
        }
        self.encoding.write_to_string(bytes, text)
    }
}

impl<'a> FontEncodings<'a> {
    /// The font's encoding, or the error lopdf raised for it.  An error is not
    /// kept: it is reported on every page that sets the font, as it was before.
    fn resolve(
        &mut self,
        pdf: &'a LopdfDocument,
        font: &'a Dictionary,
        max_decompressed_bytes: usize,
    ) -> Result<Rc<ResolvedEncoding<'a>>> {
        let key = font as *const Dictionary as usize;
        if let Some(resolved) = self.resolved.get(&key) {
            return Ok(Rc::clone(resolved));
        }
        let shared_key = to_unicode_only(font);
        if let Some(shared) = shared_key.as_ref().and_then(|id| self.shared.get(id)) {
            let shared = Rc::clone(shared);
            self.resolved.insert(key, Rc::clone(&shared));
            return Ok(shared);
        }
        let encoding = font.get_font_encoding_with_limit(pdf, max_decompressed_bytes)?;
        let resolved = Rc::new(ResolvedEncoding {
            table: CodeTable::build(&encoding),
            encoding,
        });
        if let Some(id) = shared_key {
            self.shared.insert(id, Rc::clone(&resolved));
        }
        self.resolved.insert(key, Rc::clone(&resolved));
        Ok(resolved)
    }
}

/// A `/ToUnicode` CMap as a table of one character per one-byte code.
///
/// lopdf's generic walk accepts source codes of one to four bytes and re-encodes
/// the run to UTF-16 on every call: 611 ns for a ten-byte run against 26 ns for a
/// `/Differences` map, paid once per `Tj`/`TJ` string of the page text.  A CMap
/// that maps every one-byte code to at most one character - the CMaps this build
/// synthesises, and the one-byte CMaps of the corpus - is answered by a table
/// lookup, and a run with a byte the table does not hold is handed back to lopdf,
/// so the decoded text is unchanged.
struct CodeTable {
    /// One character per code, `None` for a code the CMap does not hold or that
    /// decodes to more than one character.
    single: [Option<char>; 256],
}

impl CodeTable {
    fn build(encoding: &Encoding<'_>) -> Option<Self> {
        let Encoding::UnicodeMapEncoding(map) = encoding else {
            return None;
        };
        let mut single = [None; 256];
        for code in 0_u16..=255 {
            let Some(units) = map.get(code as u32, 1) else {
                continue;
            };
            let mut chars = char::decode_utf16(units.iter().copied());
            if let (Some(Ok(character)), None) = (chars.next(), chars.next()) {
                single[code as usize] = Some(character);
            }
        }
        Some(Self { single })
    }

    /// Append the run's characters, or `false` and nothing at all when a byte is
    /// not in the table.
    fn decode_into(&self, bytes: &[u8], out: &mut String) -> bool {
        let start = out.len();
        for byte in bytes {
            match self.single[*byte as usize] {
                Some(character) => out.push(character),
                None => {
                    out.truncate(start);
                    return false;
                }
            }
        }
        true
    }
}

/// The `/ToUnicode` stream of a simple font that has no `/Encoding` to decode
/// with instead, the case where that stream alone decides the encoding.
fn to_unicode_only(font: &Dictionary) -> Option<ObjectId> {
    let subtype = font.get(b"Subtype").and_then(Object::as_name).ok()?;
    if subtype != b"Type1" && subtype != b"TrueType" {
        return None;
    }
    if font.get(b"Encoding").is_ok() {
        return None;
    }
    match font.get(b"ToUnicode").ok()? {
        Object::Reference(id) => Some(*id),
        _ => None,
    }
}

fn extract_text_chunks_from_page<'a>(
    pdf: &'a LopdfDocument,
    pages: &BTreeMap<u32, lopdf::ObjectId>,
    page_number: u32,
    max_decompressed_size: usize,
    encodings: &mut FontEncodings<'a>,
) -> Result<Vec<Result<String>>> {
    let mut chunks = Vec::new();
    let page_id = *pages
        .get(&page_number)
        .ok_or(Error::PageNumberNotFound(page_number))?;
    let fonts = pdf.get_page_fonts(page_id)?;
    let mut page_encodings: BTreeMap<Vec<u8>, Rc<ResolvedEncoding<'a>>> = BTreeMap::new();
    for (name, font) in fonts {
        match encodings.resolve(pdf, font, max_decompressed_size) {
            Ok(resolved) => {
                page_encodings.insert(name, resolved);
            }
            Err(error) => chunks.push(Err(error)),
        }
    }
    let content_data = pdf.get_page_content_with_limit(page_id, max_decompressed_size)?;
    let content = Content::decode(&content_data)?;

    let mut current_encoding = None;
    let mut current_text = String::new();
    let mut whitespace_tail = false;
    for operation in &content.operations {
        match operation.operator.as_ref() {
            "Tf" => {
                let current_font = operation
                    .operands
                    .first()
                    .ok_or_else(|| Error::Syntax("missing font operand".to_string()))?
                    .as_name();
                current_encoding = match current_font {
                    Ok(font) => page_encodings.get(font),
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
                    && let Err(error) = collect_text(
                        &mut current_text,
                        encoding,
                        &operation.operands,
                        &mut whitespace_tail,
                    )
                {
                    chunks.push(Err(error));
                }
            }
            "'" => {
                if let Some(encoding) = current_encoding {
                    if !current_text.ends_with('\n') {
                        current_text.push('\n');
                        whitespace_tail = true;
                    }
                    if let Err(error) = collect_text(
                        &mut current_text,
                        encoding,
                        &operation.operands,
                        &mut whitespace_tail,
                    ) {
                        chunks.push(Err(error));
                    }
                }
            }
            "\"" => {
                if let Some(encoding) = current_encoding {
                    if !current_text.ends_with('\n') {
                        current_text.push('\n');
                        whitespace_tail = true;
                    }
                    if let Some(string_operand) = operation.operands.get(2)
                        && let Err(error) = collect_text(
                            &mut current_text,
                            encoding,
                            std::slice::from_ref(string_operand),
                            &mut whitespace_tail,
                        )
                    {
                        chunks.push(Err(error));
                    }
                }
            }
            "T*" | "ET" if !current_text.ends_with('\n') => {
                current_text.push('\n');
                whitespace_tail = true;
            }
            _ => {}
        }
    }
    if !current_text.is_empty() {
        chunks.push(Ok(current_text));
    }
    Ok(chunks)
}

fn collect_text(
    text: &mut String,
    encoding: &ResolvedEncoding<'_>,
    operands: &[Object],
    whitespace_tail: &mut bool,
) -> Result<()> {
    for operand in operands {
        match operand {
            Object::String(bytes, _) => {
                encoding.write_text(bytes, text)?;
                refresh_whitespace_tail(text, whitespace_tail);
            }
            Object::Array(array) => {
                collect_array_text(text, encoding, array, whitespace_tail)?;
                text.push(' ');
                *whitespace_tail = true;
            }
            _ => {}
        }
    }
    Ok(())
}

fn collect_array_text(
    text: &mut String,
    encoding: &ResolvedEncoding<'_>,
    operands: &[Object],
    whitespace_tail: &mut bool,
) -> Result<()> {
    let mut pending_word_gap = false;
    for operand in operands {
        match operand {
            Object::String(bytes, _) => {
                let mut decoded = String::new();
                encoding.write_text(bytes, &mut decoded)?;
                if pending_word_gap && !decoded.is_empty() {
                    if !*whitespace_tail
                        && !starts_with_whitespace(&decoded)
                        && !joins_two_cjk(text, &decoded)
                    {
                        text.push(' ');
                    }
                    pending_word_gap = false;
                }
                text.push_str(&decoded);
                refresh_whitespace_tail(text, whitespace_tail);
            }
            Object::Array(array) => {
                collect_array_text(text, encoding, array, whitespace_tail)?;
                text.push(' ');
                *whitespace_tail = true;
                pending_word_gap = false;
            }
            Object::Integer(adjustment) if *adjustment < -TJ_WORD_GAP_THRESHOLD as i64 => {
                pending_word_gap = !*whitespace_tail;
            }
            Object::Real(adjustment) if *adjustment < -TJ_WORD_GAP_THRESHOLD => {
                pending_word_gap = !*whitespace_tail;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether the gap that just ended falls between two CJK characters, which is never a word
/// boundary. CJK fonts routinely encode justification and compressed punctuation as large
/// negative `TJ` adjustments (-250, -500), so `中文字` must not become `中 文 字`.
fn joins_two_cjk(text: &str, decoded: &str) -> bool {
    text.chars().next_back().is_some_and(is_cjk) && decoded.chars().next().is_some_and(is_cjk)
}

/// CJK script and CJK-set punctuation: an ideograph (including the extension blocks), a kana,
/// a hangul syllable or jamo, CJK symbols and punctuation, or a fullwidth form.
fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x1100..=0x11FF   // hangul jamo
        | 0x2E80..=0x2FDF // CJK radicals and kangxi radicals
        | 0x3000..=0x303F // CJK symbols and punctuation
        | 0x3040..=0x30FF // hiragana and katakana
        | 0x3130..=0x318F // hangul compatibility jamo
        | 0x31F0..=0x31FF // katakana phonetic extensions
        | 0x3400..=0x4DBF // CJK unified ideographs extension A
        | 0x4E00..=0x9FFF // CJK unified ideographs
        | 0xA960..=0xA97F // hangul jamo extended-A
        | 0xAC00..=0xD7AF // hangul syllables
        | 0xD7B0..=0xD7FF // hangul jamo extended-B
        | 0xF900..=0xFAFF // CJK compatibility ideographs
        | 0xFE30..=0xFE4F // CJK compatibility forms
        | 0xFF00..=0xFFEF // halfwidth and fullwidth forms
        | 0x20000..=0x2FA1F // extensions B-F and compatibility supplement
        | 0x30000..=0x323AF // extensions G-I
    )
}

fn refresh_whitespace_tail(text: &str, whitespace_tail: &mut bool) {
    if let Some(last) = text.chars().last() {
        *whitespace_tail = last.is_whitespace();
    }
}

fn starts_with_whitespace(text: &str) -> bool {
    text.chars().next().is_some_and(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn winansi_text(operands: Vec<Object>) -> String {
        let mut text = String::new();
        let mut whitespace_tail = false;
        collect_text(
            &mut text,
            &ResolvedEncoding::plain(Encoding::SimpleEncoding(b"WinAnsiEncoding")),
            &operands,
            &mut whitespace_tail,
        )
        .expect("WinAnsi text decodes");
        text
    }

    fn ucs2_text(operands: Vec<Object>) -> String {
        let mut text = String::new();
        let mut whitespace_tail = false;
        collect_text(
            &mut text,
            &ResolvedEncoding::plain(Encoding::SimpleEncoding(b"UniGB-UCS2-H")),
            &operands,
            &mut whitespace_tail,
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
    fn a_real_gap_after_a_space_glyph_does_not_add_a_second_space() {
        assert_eq!(
            winansi_text(tj(vec![
                literal_ascii("Log "),
                Object::Real(-314.006),
                literal_ascii("processing"),
            ])),
            "Log processing "
        );
    }

    #[test]
    fn a_real_gap_before_a_leading_space_glyph_does_not_add_a_second_space() {
        assert_eq!(
            winansi_text(tj(vec![
                literal_ascii("Log"),
                Object::Real(-314.006),
                literal_ascii(" processing"),
            ])),
            "Log processing "
        );
    }

    #[test]
    fn an_integer_gap_after_a_space_glyph_does_not_add_a_second_space() {
        assert_eq!(
            winansi_text(tj(vec![
                literal_ascii("Log "),
                Object::Integer(-314),
                literal_ascii("processing"),
            ])),
            "Log processing "
        );
    }

    /// The CMap table is only a shortcut: a run it answers decodes to exactly
    /// what lopdf's walk produces, and a run with a byte it does not hold is
    /// left to lopdf.
    #[test]
    fn the_cmap_table_decodes_exactly_as_lopdf_does() {
        let full = (0_u8..=255)
            .map(|code| (code, vec![0x20 + (code as u16 % 90)]))
            .collect::<BTreeMap<u8, Vec<u16>>>();
        with_cmap_encoding(&full, |encoding| {
            let table = CodeTable::build(encoding).expect("a one-byte CMap has a table");
            for bytes in [&b"abc"[..], &[0, 1, 2, 255][..], &b""[..], &[32][..]] {
                let mut lopdf_text = String::new();
                encoding
                    .write_to_string(bytes, &mut lopdf_text)
                    .expect("lopdf decodes the run");
                let mut table_text = String::new();
                assert!(
                    table.decode_into(bytes, &mut table_text),
                    "the table answers the run"
                );
                assert_eq!(table_text, lopdf_text);
            }
        });

        // A code the CMap leaves out makes the whole run lopdf's business, and
        // nothing of the run is left in the output.
        let partial = BTreeMap::from([(65_u8, vec![0x41_u16])]);
        with_cmap_encoding(&partial, |encoding| {
            let table = CodeTable::build(encoding).expect("a one-byte CMap has a table");
            let mut text = String::from("before ");
            assert!(!table.decode_into(&[65, 66], &mut text));
            assert_eq!(text, "before ");
            assert!(table.decode_into(&[65], &mut text));
            assert_eq!(text, "before A");
        });
    }

    /// Run `check` over the encoding of a font whose only encoding source is a
    /// `/ToUnicode` stream holding `entries`.
    fn with_cmap_encoding(entries: &BTreeMap<u8, Vec<u16>>, check: impl FnOnce(&Encoding<'_>)) {
        let mut document = LopdfDocument::with_version("1.5");
        let cmap = crate::parsing::pdf_text_repair::build_to_unicode_cmap(entries);
        let stream = document.add_object(lopdf::Stream::new(Dictionary::new(), cmap));
        let mut font = Dictionary::new();
        font.set("Type", Object::Name(b"Font".to_vec()));
        font.set("Subtype", Object::Name(b"Type1".to_vec()));
        font.set("ToUnicode", Object::Reference(stream));
        let encoding = font.get_font_encoding(&document).expect("the CMap parses");
        check(&encoding);
    }

    #[test]
    fn a_gap_after_a_fragment_boundary_sees_the_previous_whitespace_tail() {
        let encoding = ResolvedEncoding::plain(Encoding::SimpleEncoding(b"WinAnsiEncoding"));
        let mut whitespace_tail = false;
        let mut first = String::new();
        collect_text(
            &mut first,
            &encoding,
            &[literal_ascii("Log ")],
            &mut whitespace_tail,
        )
        .expect("first fragment decodes");
        let mut second = String::new();
        collect_text(
            &mut second,
            &encoding,
            &tj(vec![Object::Real(-314.006), literal_ascii("processing")]),
            &mut whitespace_tail,
        )
        .expect("second fragment decodes");
        assert_eq!(first + &second, "Log processing ");
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

    #[test]
    fn a_real_gap_between_two_cjk_characters_gets_no_space() {
        assert_eq!(
            ucs2_text(tj(vec![
                literal_ucs2("中文"),
                Object::Real(-250.0),
                literal_ucs2("字"),
            ])),
            "中文字 "
        );
    }

    #[test]
    fn an_integer_gap_between_two_cjk_characters_gets_no_space() {
        assert_eq!(
            ucs2_text(tj(vec![
                literal_ucs2("中文"),
                Object::Integer(-500),
                literal_ucs2("字"),
            ])),
            "中文字 "
        );
    }

    #[test]
    fn a_real_cjk_gap_before_latin_keeps_its_space() {
        assert_eq!(
            ucs2_text(tj(vec![
                literal_ucs2("中文"),
                Object::Real(-250.0),
                literal_ucs2("Latin"),
            ])),
            "中文 Latin "
        );
    }

    #[test]
    fn an_integer_cjk_gap_before_latin_keeps_its_space() {
        assert_eq!(
            ucs2_text(tj(vec![
                literal_ucs2("中文"),
                Object::Integer(-250),
                literal_ucs2("Latin"),
            ])),
            "中文 Latin "
        );
    }

    #[test]
    fn a_real_latin_gap_before_cjk_keeps_its_space() {
        assert_eq!(
            ucs2_text(tj(vec![
                literal_ucs2("Latin"),
                Object::Real(-250.0),
                literal_ucs2("中文"),
            ])),
            "Latin 中文 "
        );
    }

    #[test]
    fn an_integer_latin_gap_before_cjk_keeps_its_space() {
        assert_eq!(
            ucs2_text(tj(vec![
                literal_ucs2("Latin"),
                Object::Integer(-250),
                literal_ucs2("中文"),
            ])),
            "Latin 中文 "
        );
    }

    #[test]
    fn a_cjk_gap_before_a_digit_keeps_its_space() {
        assert_eq!(
            ucs2_text(tj(vec![
                literal_ucs2("中文"),
                Object::Integer(-250),
                literal_ucs2("1"),
            ])),
            "中文 1 "
        );
    }

    #[test]
    fn a_gap_between_kana_gets_no_space() {
        assert_eq!(
            ucs2_text(tj(vec![
                literal_ucs2("かな"),
                Object::Real(-250.0),
                literal_ucs2("カナ"),
            ])),
            "かなカナ "
        );
    }

    #[test]
    fn a_gap_between_hangul_syllables_gets_no_space() {
        assert_eq!(
            ucs2_text(tj(vec![
                literal_ucs2("한"),
                Object::Integer(-500),
                literal_ucs2("글"),
            ])),
            "한글 "
        );
    }

    #[test]
    fn a_gap_around_fullwidth_punctuation_gets_no_space() {
        assert_eq!(
            ucs2_text(tj(vec![
                literal_ucs2("中"),
                Object::Real(-250.0),
                literal_ucs2("，"),
                Object::Integer(-250),
                literal_ucs2("。"),
            ])),
            "中，。 "
        );
    }

    #[test]
    fn a_gap_between_extension_block_ideographs_gets_no_space() {
        assert_eq!(
            ucs2_text(tj(vec![
                literal_ucs2("\u{3400}"),
                Object::Integer(-250),
                literal_ucs2("\u{4E00}"),
                Object::Real(-250.0),
                literal_ucs2("\u{20000}"),
                Object::Integer(-500),
                literal_ucs2("\u{20001}"),
            ])),
            "\u{3400}\u{4E00}\u{20000}\u{20001} "
        );
    }
}
