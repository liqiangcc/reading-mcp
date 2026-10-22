//! Pre-pass that makes lopdf's own text extractor see text that its 0.44
//! content walker cannot reach (PDF-1c).
//!
//! Two independent repairs, both applied to a working copy of the document
//! used only for text extraction:
//!
//! * Form XObjects that draw text are inlined into the page content stream,
//!   with their font resources copied to fresh names in the page resources.
//! * Simple fonts whose `/Encoding` `/Differences` use `uniXXXX` / `uXXXX`
//!   glyph names are decoded by lopdf again: the shadowing `/Encoding` is
//!   removed and, when the font has no `/ToUnicode`, a synthesised CMap is
//!   attached.
//!
//! Every repair is best effort.  Any error leaves the affected page or font
//! exactly as lopdf saw it before, so the old result is never made worse.

use std::collections::{BTreeMap, HashMap, HashSet};

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document as LopdfDocument, Encoding, Object, ObjectId, Stream};

const MAX_PAGE_DECOMPRESSED_BYTES: usize = 16 * 1024 * 1024;
const MAX_XOBJECT_DEPTH: usize = 8;

/// Return a document copy with the text repairs applied, or `None` when the
/// document has neither a text-bearing Form XObject nor a `uniXXXX` font.
pub(super) fn prepare_text_extraction(pdf: &LopdfDocument) -> Option<LopdfDocument> {
    let has_uni_font = pdf.objects.values().any(|object| {
        object
            .as_dict()
            .is_ok_and(|font| font_has_uni_differences(pdf, font))
    });
    let has_text_form = pdf.objects.values().any(|object| {
        object
            .as_stream()
            .is_ok_and(|stream| form_stream_has_cjk(pdf, stream))
    });
    if !has_uni_font && !has_text_form {
        return None;
    }

    let mut working = pdf.clone();
    if has_uni_font {
        rewrite_uni_fonts(&mut working);
    }
    if has_text_form {
        inline_page_text_xobjects(&mut working);
    }
    Some(working)
}

// ---------------------------------------------------------------------------
// A2: `/Encoding` with `/Differences` named `uniXXXX`
// ---------------------------------------------------------------------------

fn font_has_uni_differences(doc: &LopdfDocument, font: &Dictionary) -> bool {
    if !font.has_type(b"Font") {
        return false;
    }
    let subtype = font.get(b"Subtype").and_then(Object::as_name).ok();
    if subtype != Some(&b"Type1"[..]) && subtype != Some(&b"TrueType"[..]) {
        return false;
    }
    encoding_differences(doc, font)
        .map(|differences| uni_differences(differences).is_some())
        .unwrap_or(false)
}

fn encoding_differences<'a>(
    doc: &'a LopdfDocument,
    font: &'a Dictionary,
) -> Option<&'a Vec<Object>> {
    let encoding = font.get(b"Encoding").ok()?;
    let (_, encoding) = doc.dereference(encoding).ok()?;
    let encoding = encoding.as_dict().ok()?;
    encoding.get(b"Differences").ok()?.as_array().ok()
}

fn uni_differences(differences: &[Object]) -> Option<Vec<(u8, char)>> {
    let mut code = 0_u16;
    let mut entries = Vec::new();
    for object in differences {
        match object {
            Object::Integer(value) if (0_i64..=255).contains(value) => code = *value as u16,
            Object::Integer(_) => return None,
            Object::Name(name) => {
                if let Some(character) = unicode_from_glyph_name(name) {
                    entries.push((code as u8, character));
                }
                code = code.wrapping_add(1);
            }
            _ => {}
        }
    }
    (!entries.is_empty()).then_some(entries)
}

/// `uniXXXX` or `uXXXX`/`uXXXXXX` (hex) glyph names.
fn unicode_from_glyph_name(name: &[u8]) -> Option<char> {
    let hex = if let Some(hex) = name.strip_prefix(b"uni") {
        if hex.len() != 4 {
            return None;
        }
        hex
    } else {
        let hex = name.strip_prefix(b"u")?;
        if !(4..=6).contains(&hex.len()) {
            return None;
        }
        hex
    };
    if !hex.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let value = std::str::from_utf8(hex).ok()?;
    char::from_u32(u32::from_str_radix(value, 16).ok()?)
}

fn rewrite_uni_fonts(doc: &mut LopdfDocument) {
    let font_ids = doc
        .objects
        .iter()
        .filter_map(|(id, object)| {
            let font = object.as_dict().ok()?;
            font_has_uni_differences(doc, font).then_some(*id)
        })
        .collect::<Vec<_>>();

    for font_id in font_ids {
        let Some((uni_entries, existing_to_unicode)) = (|| {
            let font = doc.get_dictionary(font_id).ok()?;
            let differences = encoding_differences(doc, font)?;
            let uni_entries = uni_differences(differences)?;
            let existing_to_unicode = font
                .get_deref(b"ToUnicode", doc)
                .and_then(Object::as_stream)
                .is_ok();
            Some((uni_entries, existing_to_unicode))
        })() else {
            continue;
        };

        let to_unicode = if existing_to_unicode {
            None
        } else {
            let font = match doc.get_dictionary(font_id) {
                Ok(font) => font,
                Err(_) => continue,
            };
            let differences = match encoding_differences(doc, font) {
                Some(differences) => differences,
                None => continue,
            };
            let entries = match synthesised_entries(doc, font, differences, &uni_entries) {
                Some(entries) => entries,
                None => continue,
            };
            Some(Stream::new(
                Dictionary::new(),
                build_to_unicode_cmap(&entries),
            ))
        };

        let to_unicode_id = to_unicode.map(|stream| doc.add_object(stream));
        let Some(font) = doc
            .get_object_mut(font_id)
            .and_then(Object::as_dict_mut)
            .ok()
        else {
            continue;
        };
        let _ = font.remove(b"Encoding");
        if let Some(id) = to_unicode_id {
            font.set("ToUnicode", Object::Reference(id));
        }
    }
}

fn synthesised_entries(
    doc: &LopdfDocument,
    font: &Dictionary,
    differences: &[Object],
    uni_entries: &[(u8, char)],
) -> Option<BTreeMap<u8, Vec<u16>>> {
    let mut entries = BTreeMap::new();
    if let Some(old) = old_decoding_map(doc, font, differences) {
        for (code, text) in old.into_iter().enumerate() {
            if let Some(text) = text {
                entries.insert(code as u8, text.encode_utf16().collect());
            }
        }
    }
    for (code, character) in uni_entries {
        let mut units = [0_u16; 2];
        let units = character.encode_utf16(&mut units);
        entries.insert(*code, units.to_vec());
    }
    Some(entries)
}

/// Decode every byte code with the encoding lopdf used to build from this
/// font's `/Differences`, minus the `uniXXXX` names it cannot parse.  This is
/// the old result for every code that is not named by a `uniXXXX` glyph.
fn old_decoding_map(
    doc: &LopdfDocument,
    font: &Dictionary,
    differences: &[Object],
) -> Option<Vec<Option<String>>> {
    let filtered = filtered_differences(differences)?;
    let mut encoding_dictionary = Dictionary::new();
    encoding_dictionary.set("Type", Object::Name(b"Encoding".to_vec()));
    if let Some(base) = font
        .get(b"Encoding")
        .ok()
        .and_then(|encoding| doc.dereference(encoding).ok())
        .and_then(|(_, encoding)| encoding.as_dict().ok())
        .and_then(|encoding| encoding.get(b"BaseEncoding").ok())
    {
        encoding_dictionary.set("BaseEncoding", base.clone());
    }
    encoding_dictionary.set("Differences", Object::Array(filtered));

    let mut temporary = font.clone();
    temporary.set("Encoding", Object::Dictionary(encoding_dictionary));
    let _ = temporary.remove(b"ToUnicode");

    let encoding = temporary.get_font_encoding(doc).ok()?;
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

/// Keep the differences lopdf can parse, but preserve the byte code of every
/// kept glyph when a `uniXXXX` entry is removed.
fn filtered_differences(differences: &[Object]) -> Option<Vec<Object>> {
    let mut filtered = Vec::new();
    let mut code = 0_i64;
    for object in differences {
        match object {
            Object::Integer(value) if (0_i64..=255).contains(value) => {
                code = *value;
                filtered.push(object.clone());
            }
            Object::Integer(_) => return None,
            Object::Name(name) => {
                if unicode_from_glyph_name(name).is_none() {
                    if !matches!(filtered.last(), Some(Object::Integer(value)) if *value == code) {
                        filtered.push(Object::Integer(code));
                    }
                    filtered.push(object.clone());
                }
                code = code.wrapping_add(1);
            }
            _ => return None,
        }
    }
    Some(filtered)
}

fn build_to_unicode_cmap(entries: &BTreeMap<u8, Vec<u16>>) -> Vec<u8> {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n\
         12 dict begin\n\
         begincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n\
         /CMapType 2 def\n\
         1 begincodespacerange\n\
         <00> <FF>\n\
         endcodespacerange\n",
    );
    if !entries.is_empty() {
        cmap.push_str(&format!("{} beginbfchar\n", entries.len()));
        for (code, units) in entries {
            cmap.push_str(&format!("<{code:02X}> <"));
            for unit in units {
                cmap.push_str(&format!("{unit:04X}"));
            }
            cmap.push_str(">\n");
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str(
        "endcmap\n\
         CMapName currentdict /CMap defineresource pop\n\
         end\n\
         end\n",
    );
    cmap.into_bytes()
}

// ---------------------------------------------------------------------------
// A1: text drawn inside Form XObjects
// ---------------------------------------------------------------------------

/// A document is only repaired for Form XObjects when at least one form uses a
/// font that can actually carry CJK text.  This is the product goal (PDF-1c is
/// about lost CJK text), and it keeps the pre-pass off ordinary English
/// papers, whose figure forms would otherwise add text that is not in the
/// reference and would cost a full parse of every vector figure.
fn form_stream_has_cjk(doc: &LopdfDocument, stream: &Stream) -> bool {
    if !is_form(stream) {
        return false;
    }
    let resources = stream
        .dict
        .get(b"Resources")
        .ok()
        .and_then(|object| dictionary_of(doc, object));
    let Some(resources) = resources else {
        return false;
    };
    resources_have_cjk_font(doc, resources)
}

fn resources_have_cjk_font(doc: &LopdfDocument, resources: &Dictionary) -> bool {
    let Some(fonts) = resources
        .get(b"Font")
        .ok()
        .and_then(|object| dictionary_of(doc, object))
    else {
        return false;
    };
    fonts.iter().any(|(_, value)| {
        doc.dereference(value)
            .ok()
            .and_then(|(_, object)| object.as_dict().ok())
            .is_some_and(|font| font_has_cjk_hint(doc, font))
    })
}

fn font_has_cjk_hint(doc: &LopdfDocument, font: &Dictionary) -> bool {
    if let Some(differences) = encoding_differences(doc, font)
        && uni_differences(differences)
            .is_some_and(|entries| entries.iter().any(|(_, character)| is_cjk(*character)))
    {
        return true;
    }
    let Some(to_unicode) = font
        .get_deref(b"ToUnicode", doc)
        .ok()
        .and_then(|object| object.as_stream().ok())
    else {
        return false;
    };
    to_unicode
        .decompressed_content_with_limit(2 * 1024 * 1024)
        .is_ok_and(|data| cmap_bytes_have_cjk(data.as_slice()))
}

fn cmap_bytes_have_cjk(data: &[u8]) -> bool {
    // Require a small run of CJK targets.  A single CJK-looking token can be
    // a source code or an incidental symbol mapping in an otherwise Latin
    // CMap; four is well below the CJK body-text CMaps of the corpus and
    // above the incidental noise seen on ordinary English papers.
    const MIN_CJK_TARGETS: usize = 4;
    let mut found = 0;
    let mut index = 0;
    while index < data.len() {
        if data[index] != b'<' {
            index += 1;
            continue;
        }
        let Some(end) = data[index + 1..].iter().position(|byte| *byte == b'>') else {
            break;
        };
        let end = index + 1 + end;
        let hex = &data[index + 1..end];
        if (4..=6).contains(&hex.len())
            && hex.iter().all(u8::is_ascii_hexdigit)
            && let Ok(text) = std::str::from_utf8(hex)
            && let Ok(value) = u32::from_str_radix(text, 16)
            && char::from_u32(value).is_some_and(is_cjk)
        {
            found += 1;
            if found >= MIN_CJK_TARGETS {
                return true;
            }
        }
        index = end + 1;
    }
    false
}

fn is_cjk(character: char) -> bool {
    let code = character as u32;
    (0x3400..=0x4DBF).contains(&code)
        || (0x4E00..=0x9FFF).contains(&code)
        || (0xF900..=0xFAFF).contains(&code)
        || (0x3040..=0x30FF).contains(&code)
        || (0x31F0..=0x31FF).contains(&code)
        || (0xAC00..=0xD7AF).contains(&code)
}

fn inline_page_text_xobjects(doc: &mut LopdfDocument) {
    let pages = doc.get_pages().values().copied().collect::<Vec<_>>();
    for page_id in pages {
        if !page_has_xobject(doc, page_id) {
            continue;
        }
        let Ok(content_data) =
            doc.get_page_content_with_limit(page_id, MAX_PAGE_DECOMPRESSED_BYTES)
        else {
            continue;
        };
        let Ok(content) = Content::decode(&content_data) else {
            continue;
        };

        let mut context = InlineContext::new(doc, page_id);
        let mut cache = HashMap::new();
        let mut active = HashSet::new();
        let mut budget = MAX_PAGE_DECOMPRESSED_BYTES;
        let Some(operations) = inline_page_operations(
            doc,
            page_id,
            &content.operations,
            &mut active,
            &mut cache,
            &mut context,
            &mut budget,
        ) else {
            continue;
        };
        let Ok(encoded) = (Content { operations }).encode() else {
            continue;
        };
        let stream_id = doc.add_object(Stream::new(Dictionary::new(), encoded));
        let Some(page) = doc.get_dictionary_mut(page_id).ok() else {
            continue;
        };
        page.set("Contents", Object::Reference(stream_id));
        context.commit(doc, page_id);
    }
}

#[allow(clippy::too_many_arguments)]
fn inline_page_operations(
    doc: &LopdfDocument,
    page_id: ObjectId,
    operations: &[Operation],
    active: &mut HashSet<ObjectId>,
    cache: &mut HashMap<ObjectId, Option<CachedForm>>,
    context: &mut InlineContext,
    budget: &mut usize,
) -> Option<Vec<Operation>> {
    let mut output = Vec::with_capacity(operations.len());
    let mut changed = false;
    for operation in operations {
        if operation.operator == "Do"
            && let Some(name) = operation.operands.first().and_then(|o| o.as_name().ok())
            && let Some((id, stream)) = lookup_page_xobject(doc, page_id, name)
            && let Some(inlined) = inline_form(doc, id, stream, 0, active, cache, context, budget)
        {
            push_wrapped_form(&mut output, stream, inlined);
            changed = true;
            continue;
        }
        output.push(operation.clone());
    }
    changed.then_some(output)
}

#[allow(clippy::too_many_arguments)]
fn inline_form(
    doc: &LopdfDocument,
    id: Option<ObjectId>,
    stream: &Stream,
    depth: usize,
    active: &mut HashSet<ObjectId>,
    cache: &mut HashMap<ObjectId, Option<CachedForm>>,
    context: &mut InlineContext,
    budget: &mut usize,
) -> Option<Vec<Operation>> {
    if depth >= MAX_XOBJECT_DEPTH {
        return None;
    }
    if let Some(id) = id
        && !active.insert(id)
    {
        return None;
    }

    let result = (|| {
        let resources = stream
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|object| dictionary_of(doc, object))?;
        let form = form_content(doc, id, stream, cache, budget)?;
        inline_form_operations(
            doc,
            resources,
            &form.operations,
            depth + 1,
            active,
            cache,
            context,
            budget,
        )
    })();

    if let Some(id) = id {
        active.remove(&id);
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn inline_form_operations(
    doc: &LopdfDocument,
    resources: &Dictionary,
    operations: &[Operation],
    depth: usize,
    active: &mut HashSet<ObjectId>,
    cache: &mut HashMap<ObjectId, Option<CachedForm>>,
    context: &mut InlineContext,
    budget: &mut usize,
) -> Option<Vec<Operation>> {
    let mut output = Vec::with_capacity(operations.len());
    let mut has_text = false;
    for operation in operations {
        match operation.operator.as_str() {
            "Tf" => {
                let name = operation.operands.first().and_then(|o| o.as_name().ok())?;
                let fresh = context.inject_font(doc, resources, name)?;
                let mut operation = operation.clone();
                operation.operands[0] = Object::Name(fresh);
                output.push(operation);
            }
            "Tj" | "TJ" | "'" | "\"" => {
                has_text = true;
                output.push(operation.clone());
            }
            "Do" => {
                let nested = operation
                    .operands
                    .first()
                    .and_then(|o| o.as_name().ok())
                    .and_then(|name| lookup_xobject(doc, resources, name));
                if let Some((id, stream)) = nested
                    && let Some(inlined) =
                        inline_form(doc, id, stream, depth, active, cache, context, budget)
                {
                    push_wrapped_form(&mut output, stream, inlined);
                    has_text = true;
                    continue;
                }
                output.push(operation.clone());
            }
            _ => output.push(operation.clone()),
        }
    }
    has_text.then_some(output)
}

fn push_wrapped_form(output: &mut Vec<Operation>, stream: &Stream, operations: Vec<Operation>) {
    output.push(Operation::new("q", vec![]));
    if let Some(matrix) = stream
        .dict
        .get(b"Matrix")
        .ok()
        .and_then(|object| object.as_array().ok())
        && matrix.len() == 6
    {
        output.push(Operation::new("cm", matrix.clone()));
    }
    output.extend(operations);
    output.push(Operation::new("Q", vec![]));
}

struct CachedForm {
    operations: Vec<Operation>,
    decoded_len: usize,
}

fn form_content(
    _doc: &LopdfDocument,
    id: Option<ObjectId>,
    stream: &Stream,
    cache: &mut HashMap<ObjectId, Option<CachedForm>>,
    budget: &mut usize,
) -> Option<CachedForm> {
    if let Some(id) = id
        && let Some(cached) = cache.get(&id)
    {
        let cached = cached.as_ref()?;
        if cached.decoded_len > *budget {
            return None;
        }
        *budget -= cached.decoded_len;
        return Some(CachedForm {
            operations: cached.operations.clone(),
            decoded_len: cached.decoded_len,
        });
    }

    let data = stream.decompressed_content_with_limit(*budget).ok()?;
    if data.len() > *budget {
        return None;
    }
    let operations = Content::decode(&data).ok()?.operations;
    *budget -= data.len();
    if let Some(id) = id {
        cache.insert(
            id,
            Some(CachedForm {
                operations: operations.clone(),
                decoded_len: data.len(),
            }),
        );
    }
    Some(CachedForm {
        operations,
        decoded_len: data.len(),
    })
}

struct InlineContext {
    fonts: Dictionary,
    used_names: HashSet<Vec<u8>>,
    fresh_names: HashMap<(usize, Vec<u8>), Vec<u8>>,
    counter: usize,
    added: bool,
}

impl InlineContext {
    fn new(doc: &LopdfDocument, page_id: ObjectId) -> Self {
        let mut used_names = HashSet::new();
        if let Ok(fonts) = doc.get_page_fonts(page_id) {
            used_names.extend(fonts.keys().cloned());
        }
        Self {
            fonts: page_font_dictionary(doc, page_id).unwrap_or_default(),
            used_names,
            fresh_names: HashMap::new(),
            counter: 0,
            added: false,
        }
    }

    fn inject_font(
        &mut self,
        doc: &LopdfDocument,
        resources: &Dictionary,
        name: &[u8],
    ) -> Option<Vec<u8>> {
        let key = (resources as *const Dictionary as usize, name.to_vec());
        if let Some(fresh) = self.fresh_names.get(&key) {
            return Some(fresh.clone());
        }
        let font_dictionary = resources
            .get(b"Font")
            .ok()
            .and_then(|object| dictionary_of(doc, object))?;
        let value = font_dictionary.get(name).ok()?.clone();
        let (_, font_object) = doc.dereference(&value).ok()?;
        let font = font_object.as_dict().ok()?;
        if !font_can_decode(doc, font) {
            return None;
        }
        let fresh = self.fresh_name();
        self.fonts.set(fresh.clone(), value);
        self.fresh_names.insert(key, fresh.clone());
        self.added = true;
        Some(fresh)
    }

    fn fresh_name(&mut self) -> Vec<u8> {
        loop {
            self.counter = self.counter.saturating_add(1);
            let name = format!("Pdf1cF{}", self.counter).into_bytes();
            if !self.used_names.contains(&name) && !self.fonts.has(&name) {
                self.used_names.insert(name.clone());
                return name;
            }
        }
    }

    fn commit(self, doc: &mut LopdfDocument, page_id: ObjectId) {
        if !self.added {
            return;
        }
        let mut resources = page_own_resources(doc, page_id).unwrap_or_default();
        resources.set("Font", Object::Dictionary(self.fonts));
        if let Ok(page) = doc.get_dictionary_mut(page_id) {
            page.set("Resources", Object::Dictionary(resources));
        }
    }
}

/// A font whose encoding lopdf can resolve without falling back to
/// StandardEncoding for a CID font, and without the unimplemented predefined
/// CMaps.  Inlining a form whose font would only produce a fallback encoding
/// would turn "no text" into garbage, so such a form is left alone.
fn font_can_decode(doc: &LopdfDocument, font: &Dictionary) -> bool {
    let subtype = font.get(b"Subtype").and_then(Object::as_name).ok();
    match font.get_font_encoding(doc) {
        Ok(Encoding::UnicodeMapEncoding(_)) | Ok(Encoding::Differences(_)) => true,
        Ok(Encoding::OneByteEncoding(_)) => subtype != Some(&b"Type0"[..]),
        Ok(Encoding::SimpleEncoding(_)) => false,
        Err(_) => false,
    }
}

fn page_font_dictionary(doc: &LopdfDocument, page_id: ObjectId) -> Option<Dictionary> {
    let resources = page_own_resources(doc, page_id)?;
    let fonts = resources.get(b"Font").ok()?;
    let (_, fonts) = doc.dereference(fonts).ok()?;
    fonts.as_dict().ok().cloned()
}

fn page_own_resources(doc: &LopdfDocument, page_id: ObjectId) -> Option<Dictionary> {
    let page = doc.get_dictionary(page_id).ok()?;
    let resources = page.get(b"Resources").ok()?;
    let (_, resources) = doc.dereference(resources).ok()?;
    resources.as_dict().ok().cloned()
}

fn page_has_xobject(doc: &LopdfDocument, page_id: ObjectId) -> bool {
    let Ok((page_resources, resource_ids)) = doc.get_page_resources(page_id) else {
        return false;
    };
    if page_resources.is_some_and(|resources| resources.has(b"XObject")) {
        return true;
    }
    resource_ids.iter().any(|id| {
        doc.get_dictionary(*id)
            .map(|resources| resources.has(b"XObject"))
            .unwrap_or(false)
    })
}

fn lookup_page_xobject<'a>(
    doc: &'a LopdfDocument,
    page_id: ObjectId,
    name: &[u8],
) -> Option<(Option<ObjectId>, &'a Stream)> {
    let (page_resources, resource_ids) = doc.get_page_resources(page_id).ok()?;
    if let Some(resources) = page_resources
        && let Some(xobject) = lookup_xobject(doc, resources, name)
    {
        return Some(xobject);
    }
    for id in resource_ids {
        if let Ok(resources) = doc.get_dictionary(id)
            && let Some(xobject) = lookup_xobject(doc, resources, name)
        {
            return Some(xobject);
        }
    }
    None
}

fn lookup_xobject<'a>(
    doc: &'a LopdfDocument,
    resources: &'a Dictionary,
    name: &[u8],
) -> Option<(Option<ObjectId>, &'a Stream)> {
    let xobjects = resources.get(b"XObject").ok()?;
    let xobjects = dictionary_of(doc, xobjects)?;
    let entry = xobjects.get(name).ok()?;
    let (id, object) = doc.dereference(entry).ok()?;
    let stream = object.as_stream().ok()?;
    is_form(stream).then_some((id, stream))
}

fn is_form(stream: &Stream) -> bool {
    stream.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(&b"Form"[..])
}

fn dictionary_of<'a>(doc: &'a LopdfDocument, object: &'a Object) -> Option<&'a Dictionary> {
    let (_, object) = doc.dereference(object).ok()?;
    object.as_dict().ok()
}
