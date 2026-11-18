//! PDF-6b: decode simple Type 1 fonts whose encoding lives in the font program
//! and which carry no `/ToUnicode` (PDF-1c's sibling repair).
//!
//! pdfTeX writes Computer Modern fonts as Type 1 programs with their own
//! encoding (`dup 11 /ff put`, ...) and no `/ToUnicode`. lopdf 0.44 decodes
//! such a font with StandardEncoding, so the OT1 ligature slots 0x0B-0x0F are
//! dropped (`eects` for `effects`), CMMI's `period` at 0x3A becomes `:`, and
//! CMSY's `minus` at 0x00 disappears - which turns `10^-12` into `10 12`.
//!
//! The repair reads the built-in encoding from the cleartext part of the
//! embedded program (the first `/Length1` bytes), applies the font
//! dictionary's `/Encoding` `/Differences` on top, maps the glyph names to
//! Unicode, and attaches a synthesised `/ToUnicode` CMap to the working copy
//! that is used for text extraction only.  Codes it cannot map keep exactly
//! the text lopdf produced before, and a font whose entries would not change
//! is left alone, so the repair is never worse than the old extraction.

use std::collections::BTreeMap;

use lopdf::{Dictionary, Document as LopdfDocument, Object, ObjectId, Stream};

use super::pdf_text_repair::{
    build_to_unicode_cmap, encoding_differences, unicode_from_glyph_name,
};
use super::pdf_type1_glyphs::glyph_name_unicode;

/// The cleartext part of a Type 1 program is a few kilobytes (`/Length1` of
/// the corpus fonts is under 12 KiB); 64 KiB bounds a crafted one.
const MAX_PROGRAM_BYTES: usize = 64 * 1024;
/// A Type 1 encoding has at most 256 codes; the cap bounds a crafted program.
const MAX_ENCODING_ENTRIES: usize = 256;
const MAX_NAME_BYTES: usize = 64;

/// The built-in encoding a Type 1 program declares in its cleartext part.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum BuiltinEncoding {
    /// `/Encoding StandardEncoding def`, or no `/Encoding` at all (the Type 1 default).
    Standard,
    /// `dup <code> /<glyphname> put` entries.
    Codes(BTreeMap<u8, Vec<u8>>),
}

/// Whether the repair would change any font of this document.
///
/// `prepare_text_extraction` copies the whole document before repairing it, so
/// this asks the cheap question - would a font actually be rewritten? - on the
/// original: a Type 1 font whose program declares an empty encoding (a
/// Ghostscript conversion) or a code set identical to today's decoding is not
/// worth a copy of a 490-page file.
pub(super) fn has_rewritable_font(pdf: &LopdfDocument) -> bool {
    pdf.objects.iter().any(|(id, object)| {
        object
            .as_dict()
            .is_ok_and(|font| is_candidate(pdf, font) && font_entries(pdf, *id).is_some())
    })
}

fn is_candidate(doc: &LopdfDocument, font: &Dictionary) -> bool {
    if !font.has_type(b"Font") {
        return false;
    }
    if font.get(b"Subtype").and_then(Object::as_name).ok() != Some(&b"Type1"[..]) {
        return false;
    }
    if font.get_deref(b"ToUnicode", doc).is_ok() {
        return false;
    }
    font_descriptor(doc, font).is_some_and(|descriptor| descriptor.get(b"FontFile").is_ok())
}

fn font_descriptor<'a>(doc: &'a LopdfDocument, font: &'a Dictionary) -> Option<&'a Dictionary> {
    font.get_deref(b"FontDescriptor", doc).ok()?.as_dict().ok()
}

/// Which base encoding applies to the font, or `None` when the font dictionary
/// names a base the built-in program encoding does not describe.
enum Base {
    /// The program's own encoding.
    Builtin,
    /// `/Encoding /StandardEncoding`, or a `/BaseEncoding /StandardEncoding`.
    Standard,
}

fn base_encoding(doc: &LopdfDocument, font: &Dictionary) -> Option<Base> {
    let Ok(encoding) = font.get(b"Encoding") else {
        return Some(Base::Builtin);
    };
    let (_, encoding) = doc.dereference(encoding).ok()?;
    match encoding {
        Object::Name(name) if name.as_slice() == b"StandardEncoding" => Some(Base::Standard),
        Object::Name(_) => None,
        Object::Dictionary(dictionary) => match dictionary.get(b"BaseEncoding") {
            Err(_) => Some(Base::Builtin),
            Ok(base) => {
                let (_, base) = doc.dereference(base).ok()?;
                (base.as_name().ok() == Some(&b"StandardEncoding"[..])).then_some(Base::Standard)
            }
        },
        _ => None,
    }
}

/// Attach a synthesised `/ToUnicode` to every targeted font of the working copy.
pub(super) fn rewrite_type1_builtin_fonts(doc: &mut LopdfDocument) {
    let font_ids = doc
        .objects
        .iter()
        .filter_map(|(id, object)| {
            let font = object.as_dict().ok()?;
            is_candidate(doc, font).then_some(*id)
        })
        .collect::<Vec<_>>();

    for font_id in font_ids {
        let Some(entries) = font_entries(doc, font_id) else {
            continue;
        };
        let to_unicode = doc.add_object(Stream::new(
            Dictionary::new(),
            build_to_unicode_cmap(&entries),
        ));
        let Some(font) = doc
            .get_object_mut(font_id)
            .and_then(Object::as_dict_mut)
            .ok()
        else {
            continue;
        };
        // lopdf prefers `/Encoding` over `/ToUnicode`, and the CMap already
        // holds every code the old encoding produced.
        let _ = font.remove(b"Encoding");
        font.set("ToUnicode", Object::Reference(to_unicode));
    }
}

/// The CMap entries for one font, or `None` when the repair would change nothing.
fn font_entries(doc: &LopdfDocument, font_id: ObjectId) -> Option<BTreeMap<u8, Vec<u16>>> {
    let font = doc.get_dictionary(font_id).ok()?;
    let builtin = match base_encoding(doc, font)? {
        Base::Standard => BuiltinEncoding::Standard,
        Base::Builtin => {
            let clear = program_cleartext(doc, font)?;
            parse_builtin_encoding(&clear)?
        }
    };
    let differences = encoding_differences(doc, font);
    let old = current_decoding(doc, font)?;
    let entries = synthesised_entries(&old, &builtin, differences.map(Vec::as_slice))?;
    changes_any_code(&old, &entries).then_some(entries)
}

/// The cleartext part of the embedded Type 1 program, bounded by
/// `MAX_PROGRAM_BYTES` and by the program's own `/Length1`.
fn program_cleartext(doc: &LopdfDocument, font: &Dictionary) -> Option<Vec<u8>> {
    let descriptor = font_descriptor(doc, font)?;
    let program = descriptor
        .get(b"FontFile")
        .ok()
        .and_then(|object| doc.dereference(object).ok())
        .and_then(|(_, object)| object.as_stream().ok())?;
    let length1 = program
        .dict
        .get(b"Length1")
        .ok()
        .and_then(|object| object.as_i64().ok())
        .filter(|length| *length > 0)
        .map(|length| (length as usize).min(MAX_PROGRAM_BYTES));
    let data = program
        .decompressed_content_with_limit(MAX_PROGRAM_BYTES)
        .ok()?;
    let end = length1.unwrap_or(data.len()).min(data.len());
    (end > 0).then(|| data[..end].to_vec())
}

/// Decode every byte code the way lopdf decodes this font today.
fn current_decoding(doc: &LopdfDocument, font: &Dictionary) -> Option<Vec<Option<String>>> {
    let encoding = font.get_font_encoding(doc).ok()?;
    let mut map = Vec::with_capacity(256);
    for code in 0_u16..=255 {
        let mut text = String::new();
        if encoding.write_to_string(&[code as u8], &mut text).is_err() {
            text.clear();
        }
        map.push((!text.is_empty()).then_some(text));
    }
    Some(map)
}

fn synthesised_entries(
    old: &[Option<String>],
    builtin: &BuiltinEncoding,
    differences: Option<&[Object]>,
) -> Option<BTreeMap<u8, Vec<u16>>> {
    let mut names = match builtin {
        BuiltinEncoding::Standard => BTreeMap::new(),
        BuiltinEncoding::Codes(codes) => codes.clone(),
    };
    if let Some(differences) = differences {
        let mut code = 0_i64;
        for object in differences {
            match object {
                Object::Integer(value) if (0_i64..=255).contains(value) => code = *value,
                Object::Integer(_) => return None,
                Object::Name(name) => {
                    names.insert(code as u8, name.clone());
                    code = code.wrapping_add(1);
                }
                _ => {}
            }
        }
    }

    let mut entries = BTreeMap::new();
    for (code, text) in old.iter().enumerate() {
        if let Some(text) = text {
            entries.insert(code as u8, text.encode_utf16().collect::<Vec<u16>>());
        }
    }
    for (code, name) in names {
        if let Some(units) = glyph_name_units(&name) {
            entries.insert(code, units);
        }
    }
    Some(entries)
}

fn changes_any_code(old: &[Option<String>], entries: &BTreeMap<u8, Vec<u16>>) -> bool {
    let mut previous = 0_usize;
    for (code, text) in old.iter().enumerate() {
        let Some(text) = text else { continue };
        previous += 1;
        if entries.get(&(code as u8)) != Some(&text.encode_utf16().collect::<Vec<u16>>()) {
            return true;
        }
    }
    entries.len() != previous
}

// ---------------------------------------------------------------------------
// The cleartext parser
// ---------------------------------------------------------------------------

/// Read the built-in encoding from a Type 1 program's cleartext part.
///
/// Only the two forms a pdfTeX/Dvips program uses are recognised: an
/// `/Encoding StandardEncoding def`, and an `/Encoding <n> array` followed by
/// `dup <code> /<name> put` lines.  Anything else, and any malformed entry,
/// returns `None` so the font is left exactly as lopdf saw it.
pub(super) fn parse_builtin_encoding(clear: &[u8]) -> Option<BuiltinEncoding> {
    let Some(keyword) = find(clear, b"/Encoding") else {
        // A Type 1 program with no `/Encoding` uses StandardEncoding.
        return Some(BuiltinEncoding::Standard);
    };
    if !is_token_start(clear, keyword) {
        return None;
    }
    let cursor = skip_space(clear, keyword + b"/Encoding".len());
    if starts_with(clear, cursor, b"StandardEncoding") {
        return Some(BuiltinEncoding::Standard);
    }
    let (size, next) = parse_int(clear, cursor)?;
    let cursor = skip_space(clear, next);
    if !starts_with(clear, cursor, b"array") || size != 256 {
        return None;
    }

    let mut codes = BTreeMap::new();
    let mut index = cursor + b"array".len();
    let mut parsed = 0_usize;
    while let Some(offset) = find(&clear[index..], b"dup") {
        let at = index + offset;
        if !is_token_start(clear, at) {
            index = at + b"dup".len();
            continue;
        }
        let (code, next) = parse_int(clear, skip_space(clear, at + b"dup".len()))?;
        let next = skip_space(clear, next);
        let (name, next) = parse_name(clear, next)?;
        let next = skip_space(clear, next);
        if !starts_with(clear, next, b"put") {
            return None;
        }
        if (0_i64..=255).contains(&code) && name != b".notdef" {
            codes.insert(code as u8, name.to_vec());
        }
        parsed += 1;
        if parsed >= MAX_ENCODING_ENTRIES {
            break;
        }
        index = next + b"put".len();
    }
    if codes.is_empty() {
        return None;
    }
    Some(BuiltinEncoding::Codes(codes))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn starts_with(haystack: &[u8], at: usize, needle: &[u8]) -> bool {
    haystack.len() >= at + needle.len() && &haystack[at..at + needle.len()] == needle
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | b'\x00')
}

fn is_delimiter(byte: u8) -> bool {
    is_space(byte)
        || matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

fn is_token_start(haystack: &[u8], at: usize) -> bool {
    at == 0 || is_delimiter(haystack[at - 1])
}

fn skip_space(haystack: &[u8], mut at: usize) -> usize {
    while at < haystack.len() && is_space(haystack[at]) {
        at += 1;
    }
    at
}

fn parse_int(haystack: &[u8], at: usize) -> Option<(i64, usize)> {
    let mut cursor = at;
    let mut value = 0_i64;
    let mut digits = 0;
    while cursor < haystack.len() && haystack[cursor].is_ascii_digit() {
        value = value
            .checked_mul(10)?
            .checked_add((haystack[cursor] - b'0') as i64)?;
        cursor += 1;
        digits += 1;
        if digits > 6 {
            return None;
        }
    }
    (digits > 0).then_some((value, cursor))
}

fn parse_name(haystack: &[u8], at: usize) -> Option<(&[u8], usize)> {
    if !starts_with(haystack, at, b"/") {
        return None;
    }
    let start = at + 1;
    let mut cursor = start;
    while cursor < haystack.len() && !is_delimiter(haystack[cursor]) {
        cursor += 1;
        if cursor - start > MAX_NAME_BYTES {
            return None;
        }
    }
    (cursor > start).then(|| (&haystack[start..cursor], cursor))
}

// ---------------------------------------------------------------------------
// Glyph names
// ---------------------------------------------------------------------------

/// The Unicode text for a glyph name: the TeX names the Adobe Glyph List lacks
/// or spells differently, then `uniXXXX` / `uXXXX`, then the glyph table.
fn glyph_name_units(name: &[u8]) -> Option<Vec<u16>> {
    if let Some(text) = tex_glyph_name(name) {
        return Some(text.encode_utf16().collect());
    }
    if let Some(character) = unicode_from_glyph_name(name) {
        let mut units = [0_u16; 2];
        return Some(character.encode_utf16(&mut units).to_vec());
    }
    glyph_name_unicode(name).map(|text| text.encode_utf16().collect())
}

/// TeX's names for the slots that matter to a reader: the OT1 ligatures as
/// plain letters (so search and quoting see `ffi`, not U+FB03), the symbols
/// whose Adobe Glyph List entry names a different glyph than TeX draws, and the
/// names the list does not have at all (TeX Gyre's math and text fonts).
fn tex_glyph_name(name: &[u8]) -> Option<&'static str> {
    Some(match name {
        b"ff" | b"f_f" => "ff",
        b"fi" | b"f_i" => "fi",
        b"fl" | b"f_l" => "fl",
        b"ffi" | b"f_f_i" => "ffi",
        b"ffl" | b"f_f_l" => "ffl",
        b"minus" => "\u{2212}",
        b"periodcentered" => "\u{00b7}",
        b"dotlessi" => "\u{0131}",
        b"dotlessj" => "\u{0237}",
        // TeX's triangles point the other way round from the list's small ones.
        b"triangleleft" => "\u{25c1}",
        b"triangleright" => "\u{25b7}",
        b"coproduct" => "\u{2210}",
        b"negationslash" => "\u{0338}",
        b"phi1" => "\u{03d5}",
        b"angbracketleft" => "\u{27e8}",
        b"angbracketright" => "\u{27e9}",
        b"arrowhookleft" => "\u{21a9}",
        b"arrowhookright" => "\u{21aa}",
        b"mapsto" => "\u{21a6}",
        // Oldstyle figures are still the digit a reader searches for; the list
        // puts them in the private use area.
        b"zerooldstyle" => "0",
        b"oneoldstyle" => "1",
        b"twooldstyle" => "2",
        b"threeoldstyle" => "3",
        b"fouroldstyle" => "4",
        b"fiveoldstyle" => "5",
        b"sixoldstyle" => "6",
        b"sevenoldstyle" => "7",
        b"eightoldstyle" => "8",
        b"nineoldstyle" => "9",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(encoding: BuiltinEncoding) -> BTreeMap<u8, Vec<u8>> {
        match encoding {
            BuiltinEncoding::Codes(codes) => codes,
            BuiltinEncoding::Standard => panic!("expected codes"),
        }
    }

    const HEADER: &[u8] = b"%!PS-AdobeFont-1.0: CMR10 003.002\n\
        /FontName /CMR10 def\n\
        /Encoding 256 array\n\
        0 1 255 {1 index exch /.notdef put} for\n\
        dup 11 /ff put\n\
        dup 12 /fi put\n\
        dup 14 /ffi put\n\
        dup 58 /colon put\n\
        dup 46 /period put\n\
        readonly def\n";

    #[test]
    fn a_program_encoding_is_read_from_its_dup_entries() {
        let parsed = codes(parse_builtin_encoding(HEADER).expect("the header parses"));
        assert_eq!(parsed.get(&11).map(Vec::as_slice), Some(&b"ff"[..]));
        assert_eq!(parsed.get(&12).map(Vec::as_slice), Some(&b"fi"[..]));
        assert_eq!(parsed.get(&14).map(Vec::as_slice), Some(&b"ffi"[..]));
        assert_eq!(parsed.get(&58).map(Vec::as_slice), Some(&b"colon"[..]));
        assert!(!parsed.contains_key(&0), ".notdef entries are skipped");
        assert_eq!(parsed.len(), 5);
    }

    #[test]
    fn standard_encoding_and_a_program_without_one_both_mean_standard() {
        assert_eq!(
            parse_builtin_encoding(b"/Encoding StandardEncoding def\n"),
            Some(BuiltinEncoding::Standard)
        );
        assert_eq!(
            parse_builtin_encoding(b"%!PS-AdobeFont-1.0: Foo\n"),
            Some(BuiltinEncoding::Standard)
        );
    }

    #[test]
    fn a_malformed_encoding_is_a_parse_error() {
        for clear in [
            &b"/Encoding 256 array\ndup 11 /ff\n"[..],
            &b"/Encoding 128 array\ndup 11 /ff put\n"[..],
            &b"/Encoding 256 array\ndup 300 /ff put\n"[..],
            &b"/Encoding 256 array\ndup 11 // put\n"[..],
            &b"/Encoding 256 array\ndup 11 /ff put\ndup /fi put\n"[..],
            &b"/Encoding\n"[..],
        ] {
            assert_eq!(
                parse_builtin_encoding(clear),
                None,
                "{} should not parse",
                String::from_utf8_lossy(clear)
            );
        }
    }

    #[test]
    fn an_encoding_without_usable_entries_is_not_used() {
        assert_eq!(
            parse_builtin_encoding(
                b"/Encoding 256 array\n0 1 255 {1 index exch /.notdef put} for\nreadonly def\n"
            ),
            None
        );
    }

    #[test]
    fn ligatures_map_to_plain_letters_and_minus_to_the_minus_sign() {
        assert_eq!(glyph_name_units(b"ff").as_deref(), Some(&[0x66, 0x66][..]));
        assert_eq!(glyph_name_units(b"fi").as_deref(), Some(&[0x66, 0x69][..]));
        assert_eq!(glyph_name_units(b"fl").as_deref(), Some(&[0x66, 0x6c][..]));
        assert_eq!(
            glyph_name_units(b"ffi").as_deref(),
            Some(&[0x66, 0x66, 0x69][..])
        );
        assert_eq!(
            glyph_name_units(b"ffl").as_deref(),
            Some(&[0x66, 0x66, 0x6c][..])
        );
        assert_eq!(glyph_name_units(b"minus").as_deref(), Some(&[0x2212][..]));
        assert_eq!(glyph_name_units(b"period").as_deref(), Some(&[0x2e][..]));
        assert_eq!(glyph_name_units(b"alpha").as_deref(), Some(&[0x3b1][..]));
        assert_eq!(glyph_name_units(b"Gamma").as_deref(), Some(&[0x393][..]));
        assert_eq!(glyph_name_units(b"uni4E2D").as_deref(), Some(&[0x4e2d][..]));
        // TeX Gyre's names: the list's small triangles point the other way, its
        // oldstyle figures sit in the private use area, and `coproduct` is a
        // different glyph from the one TeX draws.
        assert_eq!(
            glyph_name_units(b"triangleleft").as_deref(),
            Some(&[0x25c1][..])
        );
        assert_eq!(
            glyph_name_units(b"threeoldstyle").as_deref(),
            Some(&[0x33][..])
        );
        assert_eq!(
            glyph_name_units(b"coproduct").as_deref(),
            Some(&[0x2210][..])
        );
        assert_eq!(glyph_name_units(b"mapsto").as_deref(), Some(&[0x21a6][..]));
        assert_eq!(glyph_name_units(b"notaglyphname"), None);
    }

    #[test]
    fn differences_win_over_the_builtin_encoding() {
        let builtin = parse_builtin_encoding(HEADER).expect("the header parses");
        let old = vec![Some(":".to_string()); 256];
        let differences = vec![
            Object::Integer(58),
            Object::Name(b"period".to_vec()),
            Object::Name(b"minus".to_vec()),
        ];
        let entries =
            synthesised_entries(&old, &builtin, Some(&differences)).expect("entries build");
        assert_eq!(entries.get(&58).map(Vec::as_slice), Some(&[0x2e][..]));
        assert_eq!(entries.get(&59).map(Vec::as_slice), Some(&[0x2212][..]));
        assert_eq!(entries.get(&11).map(Vec::as_slice), Some(&[0x66, 0x66][..]));
    }

    #[test]
    fn a_differences_code_out_of_range_is_a_parse_error() {
        let builtin = BuiltinEncoding::Standard;
        let old = vec![None; 256];
        let differences = vec![Object::Integer(300), Object::Name(b"period".to_vec())];
        assert_eq!(
            synthesised_entries(&old, &builtin, Some(&differences)),
            None
        );
    }
}
