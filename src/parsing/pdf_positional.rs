//! Positional text extraction for the default PDF backend (PDF-7b-1, stage 1).
//!
//! lopdf's own extractor walks the text-showing operators but never tracks a
//! text matrix, a pen advance or a graphics state, so a page comes out as one
//! concatenated run of font chunks with no line structure. This module is the
//! stage-1 positional layer of `docs/design/pdf7b-a-positional-text.md`: it
//! walks the same content stream but keeps the text matrix, the CTM and the
//! `cm`/`q`/`Q` stack, the `Tc`/`Tw`/`Tz`/`Ts` text state and the per-glyph
//! pen advance from the font's `/Widths` (simple fonts) or `/W`+`/DW` (CID
//! fonts), and it follows `Do` into Form XObjects with their `/Matrix`.
//!
//! From the glyph positions the page is grouped into **lines** (same rotation
//! bucket, baselines within `0.35 * size`, x-adjacency within `2.0 * size`)
//! and lines into **blocks**. Stage 1 starts a new block only at a large
//! vertical gap - a baseline gap above `1.8x` the page's modal leading, or a
//! line in the top or bottom margin band that the body does not share - so a
//! wrapped sentence is never cut in two and running headers, footers and page
//! numbers fall out of the body paragraphs. The finer paragraph rules stay
//! stage 3's job; space insertion is stage 2's; header removal is stage 4's.
//!
//! A page the layer cannot position exactly bails out to
//! `extract_text_chunks_with_limit`, the path the page used before: a glyph
//! count over [`MAX_PAGE_GLYPHS`], a vertical-writing font, or a font whose
//! width table cannot be read at all.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document as LopdfDocument, Encoding, Object, ObjectId, Stream};

use super::pdf_layout::FontEncodingMemo;

/// Stage-1 per-page glyph cap (design §8 item 7 proposes 200,000; the probe
/// measures the real maximum on the corpus).
pub(super) const MAX_PAGE_GLYPHS: usize = 200_000;
/// lopdf inserts a space for a `TJ` adjustment below -100 thousandths of an
/// em. Stage 1 keeps exactly that behaviour for the text it concatenates;
/// the measured-gap spacing rules are stage 2.
const TJ_SPACE_THRESHOLD: f64 = -100.0;
/// Nested `Do` recursion depth (same bound as the repair pass).
const MAX_FORM_DEPTH: usize = 8;
/// Two fragments share a baseline when their device y differs by at most this
/// fraction of the larger size. `0.5` is PyMuPDF's own threshold
/// (`same_line` compares against half the line height), which keeps
/// sub- and superscripts on the parent's line.
const BASELINE_TOLERANCE: f32 = 0.5;
/// A same-baseline fragment joins the open line while its x extent reaches
/// within `2.0 * size` of the line's extent; farther apart it is its own
/// span (a different column or table cell).
const LINE_X_GAP: f32 = 2.0;
/// A baseline gap above this multiple of the page's modal leading starts a
/// new block.
const BLOCK_GAP_RATIO: f32 = 1.8;
/// A baseline gap above this multiple of the modal leading also starts a new
/// block when either neighbour is in the page's margin band.
const MARGIN_GAP_RATIO: f32 = 1.0;
/// The body band is the [2nd, 98th] character-weighted baseline percentile
/// extended by half a modal leading on each side.
const MARGIN_BODY_BAND_LEADING: f32 = 0.5;

/// One shown-string run at a measured position, in device space.
#[derive(Clone, Debug)]
struct PositionedFragment {
    /// Decoded text of the run (never all-whitespace).
    text: String,
    /// Device-space x where the run starts.
    x0: f32,
    /// Device-space x where the pen is after the run.
    x1: f32,
    /// Device y of the run's baseline.
    baseline: f32,
    /// Device-space glyph size (`Tfs` through the text and current matrices).
    size: f32,
    /// Nearest multiple of 90 degrees of the run's direction.
    rotation: i32,
}

/// The lines and blocks of one page, ready to become `PageText`.
#[derive(Clone, Debug, Default)]
pub(super) struct PositionalPage {
    /// One item per block; `normalized_page_text` joins them into the
    /// page text, so no separate joined string is kept.
    pub chunks: Vec<String>,
    /// Why the page could not be positioned, when it could not.
    pub bail: Option<&'static str>,
}

/// Why pages fell back to the chunk path, counted over the document.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct BailCounts {
    pub glyph_cap: usize,
    pub vertical_writing: usize,
    pub unsupported_font: usize,
    pub undecodable: usize,
}

impl BailCounts {
    pub fn add(&mut self, reason: &str) {
        match reason {
            "glyph_cap" => self.glyph_cap += 1,
            "vertical_writing" => self.vertical_writing += 1,
            "unsupported_font" => self.unsupported_font += 1,
            _ => self.undecodable += 1,
        }
    }

    pub fn total(&self) -> usize {
        self.glyph_cap + self.vertical_writing + self.unsupported_font + self.undecodable
    }
}

// ---------------------------------------------------------------------------
// Matrices: PDF [a b c d e f], x' = a*x + c*y + e, y' = b*x + d*y + f
// ---------------------------------------------------------------------------

type Matrix = [f64; 6];

const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `first` then `second`, matching the PDF `cm` operator.
fn multiply(first: Matrix, second: Matrix) -> Matrix {
    [
        first[0] * second[0] + first[1] * second[2],
        first[0] * second[1] + first[1] * second[3],
        first[2] * second[0] + first[3] * second[2],
        first[2] * second[1] + first[3] * second[3],
        first[4] * second[0] + first[5] * second[2] + second[4],
        first[4] * second[1] + first[5] * second[3] + second[5],
    ]
}

fn apply(m: Matrix, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(f64::from(*value)),
        _ => None,
    }
}

fn matrix_of(operands: &[Object]) -> Option<Matrix> {
    if operands.len() < 6 {
        return None;
    }
    Some([
        number(&operands[0])?,
        number(&operands[1])?,
        number(&operands[2])?,
        number(&operands[3])?,
        number(&operands[4])?,
        number(&operands[5])?,
    ])
}

fn stream_matrix(stream: &Stream) -> Matrix {
    stream
        .dict
        .get(b"Matrix")
        .ok()
        .and_then(|object| object.as_array().ok())
        .and_then(|values| matrix_of(values))
        .unwrap_or(IDENTITY)
}

// ---------------------------------------------------------------------------
// Font metrics
// ---------------------------------------------------------------------------

/// Width table of one font, in thousandths of an em (`w` of the spec).
enum WidthTable {
    /// Simple font: `/Widths[code - /FirstChar]`, already scaled by
    /// `FontMatrix[0] * 1000` so a non-standard Type 3 matrix is exact.
    Simple { first_char: u32, widths: Vec<f32> },
    /// CID font: `(first, last)` ranges from `/W`, plus the `/DW` default.
    Cid {
        ranges: Vec<(u32, u32, f32)>,
        default: f32,
    },
}

impl WidthTable {
    fn width(&self, code: u64) -> Option<f32> {
        match self {
            WidthTable::Simple { first_char, widths } => {
                let code = u32::try_from(code).ok()?;
                if code < *first_char {
                    return None;
                }
                widths.get((code - *first_char) as usize).copied()
            }
            WidthTable::Cid { ranges, default } => Some(
                ranges
                    .iter()
                    .find_map(|(first, last, width)| {
                        (code >= u64::from(*first) && code <= u64::from(*last)).then_some(*width)
                    })
                    .unwrap_or(*default),
            ),
        }
    }
}

struct FontMetrics {
    widths: WidthTable,
    /// Vertical writing (`WMode 1`, e.g. `Identity-V`): the layer does not
    /// place these glyphs.
    vertical: bool,
}

fn font_metrics<'a>(pdf: &'a LopdfDocument, font: &'a Dictionary) -> Option<FontMetrics> {
    let subtype = font
        .get(b"Subtype")
        .and_then(Object::as_name)
        .unwrap_or(b"");
    if subtype == b"Type0" {
        let encoding_name = font
            .get(b"Encoding")
            .ok()
            .and_then(|object| pdf.dereference(object).ok())
            .and_then(|(_, object)| object.as_name().ok());
        let vertical = encoding_name == Some(&b"Identity-V"[..]);
        let descendant = font
            .get(b"DescendantFonts")
            .ok()
            .and_then(|object| pdf.dereference(object).ok())
            .and_then(|(_, object)| object.as_array().ok())
            .and_then(|fonts| fonts.first())
            .and_then(|object| pdf.dereference(object).ok())
            .and_then(|(_, object)| object.as_dict().ok())
            .or(Some(font));
        let descendant = descendant?;
        let ranges = descendant
            .get_deref(b"W", pdf)
            .ok()
            .and_then(|object| object.as_array().ok())
            .map(|values| cid_width_ranges(values.as_slice()))
            .unwrap_or_default();
        let default = descendant
            .get_deref(b"DW", pdf)
            .ok()
            .and_then(number)
            .map(|value| value as f32);
        // A CID font with neither /W nor /DW gives every glyph the same
        // width: positions would be a guess, so the page keeps the old
        // path.
        if ranges.is_empty() && default.is_none() {
            return None;
        }
        return Some(FontMetrics {
            widths: WidthTable::Cid {
                ranges,
                default: default.unwrap_or(1000.0),
            },
            vertical,
        });
    }

    // Simple fonts: /Widths[/FirstChar..]. A Type 3 font's widths are in its
    // /FontMatrix glyph space, so they are rescaled to thousandths of an em.
    let scale = font
        .get(b"FontMatrix")
        .ok()
        .and_then(|object| object.as_array().ok())
        .and_then(|values| values.first().and_then(number))
        .unwrap_or(0.001);
    let first_char = font
        .get_deref(b"FirstChar", pdf)
        .ok()
        .and_then(number)
        .unwrap_or(0.0) as u32;
    let widths = font
        .get_deref(b"Widths", pdf)
        .ok()
        .and_then(|object| object.as_array().ok())
        .map(|values| {
            values
                .iter()
                .map(|value| number(value).unwrap_or(0.0) as f32)
                .collect::<Vec<_>>()
        });
    let missing = font
        .get_deref(b"FontDescriptor", pdf)
        .ok()
        .and_then(|object| object.as_dict().ok())
        .and_then(|descriptor| descriptor.get_deref(b"MissingWidth", pdf).ok())
        .and_then(number)
        .map(|value| value as f32);
    let mut widths = widths?;
    if let Some(missing) = missing {
        for width in &mut widths {
            if *width == 0.0 {
                *width = missing;
            }
        }
    }
    let factor = (scale * 1000.0) as f32;
    if (factor - 1.0).abs() > 1e-6 {
        for width in &mut widths {
            *width *= factor;
        }
    }
    Some(FontMetrics {
        widths: WidthTable::Simple { first_char, widths },
        vertical: false,
    })
}

/// `/W` supports two run shapes: `c [w1 w2 ...]` and `cfirst clast w`.
fn cid_width_ranges(values: &[Object]) -> Vec<(u32, u32, f32)> {
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < values.len() {
        let Some(first) = number(&values[index]).map(|v| v as u32) else {
            break;
        };
        match values.get(index + 1) {
            Some(Object::Array(list)) => {
                for (offset, value) in list.iter().enumerate() {
                    if let Some(width) = number(value) {
                        let code = first.saturating_add(offset as u32);
                        ranges.push((code, code, width as f32));
                    }
                }
                index += 2;
            }
            Some(second) => {
                let (Some(last), Some(width)) =
                    (number(second), values.get(index + 2).and_then(number))
                else {
                    break;
                };
                ranges.push((first, last as u32, width as f32));
                index += 3;
            }
            None => break,
        }
    }
    ranges
}

/// Per-document cache of each font's metrics, keyed like `FontEncodingMemo`:
/// the address of the `&Dictionary` `get_page_fonts` returns is stable for
/// the whole walk, so a font shared by every page is measured once.
pub(super) struct FontMetricsMemo<'a> {
    pdf: &'a LopdfDocument,
    resolved: HashMap<usize, Option<FontMetrics>>,
}

impl<'a> FontMetricsMemo<'a> {
    pub fn new(pdf: &'a LopdfDocument) -> Self {
        Self {
            pdf,
            resolved: HashMap::new(),
        }
    }

    fn resolve(&mut self, font: &'a Dictionary) -> Option<&FontMetrics> {
        let key = font as *const Dictionary as usize;
        match self.resolved.entry(key) {
            Entry::Occupied(entry) => entry.into_mut().as_ref(),
            Entry::Vacant(entry) => entry.insert(font_metrics(self.pdf, font)).as_ref(),
        }
    }
}

// ---------------------------------------------------------------------------
// The walker
// ---------------------------------------------------------------------------

struct TextSpace {
    /// The text matrix Tm, the matrix the pen lives in.
    tm: Matrix,
    /// The text line matrix Tlm: the matrix at the start of the current
    /// line. `Td`/`TD`/`T*` move Tlm; `Tm` follows it. Conflating the two
    /// made every `Td` move the pen instead of the line start, so a
    /// line-broken word landed one pen-width to the right.
    tlm: Matrix,
    font_resource: Option<Vec<u8>>,
    font_size: f64,
    char_spacing: f64,
    word_spacing: f64,
    horizontal_scaling: f64,
    leading: f64,
    rise: f64,
}

impl TextSpace {
    fn new() -> Self {
        Self {
            tm: IDENTITY,
            tlm: IDENTITY,
            font_resource: None,
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scaling: 100.0,
            leading: 0.0,
            rise: 0.0,
        }
    }
}

/// `'d` is the document borrow the encodings and font dictionaries live in;
/// `'w` is the shorter borrow of the memo objects themselves, so a caller can
/// lend them out for one page at a time inside a loop.
struct PageWalker<'d, 'w>
where
    'd: 'w,
{
    pdf: &'d LopdfDocument,
    max_decompressed: usize,
    memo: &'w mut FontEncodingMemo<'d>,
    metrics: &'w mut FontMetricsMemo<'d>,
    ctm: Matrix,
    ctm_stack: Vec<Matrix>,
    text: TextSpace,
    fragments: Vec<PositionedFragment>,
    glyphs: usize,
    bail: Option<&'static str>,
    /// Form object ids on the recursion stack (cycle guard).
    forms: HashSet<ObjectId>,
    /// Total decompressed bytes the Do recursion may still read.
    form_budget: usize,
}

impl<'d, 'w> PageWalker<'d, 'w>
where
    'd: 'w,
{
    fn walk(
        &mut self,
        operations: &[Operation],
        fonts: &BTreeMap<Vec<u8>, &'d Dictionary>,
        resources: &[Dictionary],
        depth: usize,
    ) {
        for operation in operations {
            if self.bail.is_some() {
                return;
            }
            match operation.operator.as_str() {
                "q" => self.ctm_stack.push(self.ctm),
                "Q" => {
                    if let Some(ctm) = self.ctm_stack.pop() {
                        self.ctm = ctm;
                    }
                }
                "cm" => {
                    if let Some(matrix) = matrix_of(&operation.operands) {
                        self.ctm = multiply(matrix, self.ctm);
                    }
                }
                "BT" => self.text = TextSpace::new(),
                "Tf" => {
                    if let Some(name) = operation
                        .operands
                        .first()
                        .and_then(|value| value.as_name().ok())
                    {
                        self.text.font_resource = Some(name.to_vec());
                    }
                    if let Some(size) = operation.operands.get(1).and_then(number) {
                        self.text.font_size = size;
                    }
                }
                "Tm" => {
                    if let Some(matrix) = matrix_of(&operation.operands) {
                        // Tm sets the line start too: Tlm = Tm = M.
                        self.text.tm = matrix;
                        self.text.tlm = matrix;
                    }
                }
                "Td" | "TD" => {
                    let tx = operation.operands.first().and_then(number).unwrap_or(0.0);
                    let ty = operation.operands.get(1).and_then(number).unwrap_or(0.0);
                    if operation.operator == "TD" {
                        self.text.leading = -ty;
                    }
                    // Td moves the line start: Tlm = Tl x Tlm, Tm = Tlm.
                    self.text.tlm = multiply([1.0, 0.0, 0.0, 1.0, tx, ty], self.text.tlm);
                    self.text.tm = self.text.tlm;
                }
                "TL" => {
                    self.text.leading = operation.operands.first().and_then(number).unwrap_or(0.0);
                }
                "T*" => self.move_to_next_line(),
                "Tc" => {
                    self.text.char_spacing =
                        operation.operands.first().and_then(number).unwrap_or(0.0);
                }
                "Tw" => {
                    self.text.word_spacing =
                        operation.operands.first().and_then(number).unwrap_or(0.0);
                }
                "Tz" => {
                    self.text.horizontal_scaling =
                        operation.operands.first().and_then(number).unwrap_or(100.0);
                }
                "Ts" => {
                    self.text.rise = operation.operands.first().and_then(number).unwrap_or(0.0);
                }
                "Tj" => {
                    if let Some(bytes) = operation
                        .operands
                        .first()
                        .and_then(|value| value.as_str().ok())
                    {
                        let bytes = bytes.to_vec();
                        let font = self.current_font(fonts);
                        self.show_tj_pieces(&bytes, font, None);
                    }
                }
                "TJ" => {
                    if let Some(array) = operation
                        .operands
                        .first()
                        .and_then(|value| value.as_array().ok())
                    {
                        let font = self.current_font(fonts);
                        self.show_tj(array, font);
                    }
                }
                "'" => {
                    self.move_to_next_line();
                    if let Some(bytes) = operation
                        .operands
                        .first()
                        .and_then(|value| value.as_str().ok())
                    {
                        let bytes = bytes.to_vec();
                        let font = self.current_font(fonts);
                        self.show_tj_pieces(&bytes, font, None);
                    }
                }
                "\"" => {
                    if let (Some(aw), Some(ac)) = (
                        operation.operands.first().and_then(number),
                        operation.operands.get(1).and_then(number),
                    ) {
                        self.text.word_spacing = aw;
                        self.text.char_spacing = ac;
                    }
                    self.move_to_next_line();
                    if let Some(bytes) = operation
                        .operands
                        .get(2)
                        .and_then(|value| value.as_str().ok())
                    {
                        let bytes = bytes.to_vec();
                        let font = self.current_font(fonts);
                        self.show_tj_pieces(&bytes, font, None);
                    }
                }
                "Do" => self.walk_do(operation, fonts, resources, depth),
                _ => {}
            }
        }
    }

    fn move_to_next_line(&mut self) {
        // T* moves the line start down by the leading: Tlm = Tl x Tlm.
        self.text.tlm = multiply([1.0, 0.0, 0.0, 1.0, 0.0, -self.text.leading], self.text.tlm);
        self.text.tm = self.text.tlm;
    }

    fn current_font(&self, fonts: &BTreeMap<Vec<u8>, &'d Dictionary>) -> Option<&'d Dictionary> {
        fonts.get(self.text.font_resource.as_deref()?).copied()
    }

    /// `|det(Tm x CTM)|^.5 * Tfs`: the device-space glyph size.
    fn device_size(&self) -> f32 {
        let m = multiply(self.text.tm, self.ctm);
        let det = (m[0] * m[3] - m[1] * m[2]).abs().sqrt();
        (self.text.font_size * det.max(1e-6)) as f32
    }

    fn rotation_bucket(&self) -> i32 {
        let m = multiply(self.text.tm, self.ctm);
        let degrees = m[1].atan2(m[0]).to_degrees();
        ((degrees / 90.0).round() as i32 * 90).rem_euclid(360)
    }

    /// The device-space point of the text origin through `Tm x CTM`, lifted
    /// by the `Ts` rise (rise is a text-space offset along y).
    fn pen_device(&self) -> (f64, f64) {
        apply(multiply(self.text.tm, self.ctm), 0.0, self.text.rise)
    }

    /// Advances the pen by `tx` text units along the text matrix's own x
    /// axis, so a rotated line advances along itself.
    fn advance_pen(&mut self, tx: f64) {
        self.text.tm[4] += tx * self.text.tm[0];
        self.text.tm[5] += tx * self.text.tm[1];
    }

    /// Decoded text per code. The code length is one byte for a simple
    /// encoding; for a `ToUnicode` CMap it is the first length in 1..=4 the
    /// CMap accepts, which is exact for Identity and fixed-length CMaps.
    fn decode_codes(
        &mut self,
        font: Option<&'d Dictionary>,
        bytes: &[u8],
    ) -> Vec<(Vec<u8>, String)> {
        let mut codes = Vec::new();
        match font.and_then(|font| self.memo.resolve(self.pdf, font, self.max_decompressed)) {
            Some(encoding @ Encoding::UnicodeMapEncoding(_)) => {
                let mut index = 0;
                while index < bytes.len() {
                    let mut matched = false;
                    for len in 1..=4_usize {
                        let end = index + len;
                        if end > bytes.len() {
                            break;
                        }
                        let mut text = String::new();
                        if encoding
                            .write_to_string(&bytes[index..end], &mut text)
                            .is_ok()
                            && !text.is_empty()
                        {
                            codes.push((bytes[index..end].to_vec(), text));
                            index = end;
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        index += 1;
                    }
                }
            }
            Some(encoding) => {
                for byte in bytes {
                    let mut text = String::new();
                    if encoding.write_to_string(&[*byte], &mut text).is_ok() {
                        codes.push((vec![*byte], text));
                    }
                }
            }
            None => {
                // The old ASCII fall-through: only printable bytes show.
                for byte in bytes {
                    if byte.is_ascii_graphic() || byte.is_ascii_whitespace() {
                        codes.push((vec![*byte], (*byte as char).to_string()));
                    }
                }
            }
        }
        codes
    }

    /// Shows `bytes` and records one fragment from the pen at the run's start
    /// to the pen after the last code. When `collect` is given the decoded
    /// text is appended to it instead of emitting a fragment (the `TJ` array
    /// owns the fragment).
    fn show_tj_pieces(
        &mut self,
        bytes: &[u8],
        font: Option<&'d Dictionary>,
        mut collect: Option<&mut String>,
    ) {
        if let Some(font) = font {
            match self.metrics.resolve(font) {
                Some(metrics) if metrics.vertical => {
                    self.bail = Some("vertical_writing");
                    return;
                }
                None => {
                    self.bail = Some("unsupported_font");
                    return;
                }
                _ => {}
            }
        }
        let codes = self.decode_codes(font, bytes);
        if codes.is_empty() {
            return;
        }
        let start = self.pen_device();
        for (code_bytes, decoded) in &codes {
            if self.glyphs >= MAX_PAGE_GLYPHS {
                self.bail = Some("glyph_cap");
                return;
            }
            self.glyphs += 1;
            let code = code_bytes
                .iter()
                .fold(0_u64, |acc, byte| (acc << 8) | u64::from(*byte));
            let font = match font {
                Some(font) => font,
                None => {
                    self.bail = Some("unsupported_font");
                    return;
                }
            };
            let w = self
                .metrics
                .resolve(font)
                .and_then(|metrics| metrics.widths.width(code))
                .map(f64::from);
            let Some(w) = w else {
                // No width for this code: the pen cannot advance exactly,
                // so the page falls back rather than guessing positions.
                self.bail = Some("unsupported_font");
                return;
            };
            if let Some(piece) = collect.as_deref_mut() {
                piece.push_str(decoded);
            }
            let scaling = self.text.horizontal_scaling / 100.0;
            let tx = (w / 1000.0 * self.text.font_size
                + self.text.char_spacing
                + if code_bytes.len() == 1 && code_bytes[0] == 32 {
                    self.text.word_spacing
                } else {
                    0.0
                })
                * scaling;
            self.advance_pen(tx);
        }
        if collect.is_some() {
            return;
        }
        let text: String = codes.iter().map(|(_, decoded)| decoded.as_str()).collect();
        if text.trim().is_empty() {
            return;
        }
        if text.contains('\u{fffd}') {
            // Replacement characters mean the encoding could not name these
            // codes; PyMuPDF (and often lopdf's own path) still decode them.
            // Bailing beats showing garbage.
            self.bail = Some("undecodable");
            return;
        }
        let end = self.pen_device();
        if !end.0.is_finite() || !end.1.is_finite() || !start.0.is_finite() || !start.1.is_finite()
        {
            // Degenerate text matrix (huge Tz/Tm): drop the run rather than
            // poison every geometry comparison below.
            return;
        }
        self.fragments.push(PositionedFragment {
            text,
            x0: start.0 as f32,
            x1: end.0 as f32,
            baseline: start.1 as f32,
            size: self.device_size(),
            rotation: self.rotation_bucket(),
        });
    }

    /// One fragment per `TJ` array, matching lopdf's chunk boundary: the
    /// array's strings concatenate (with lopdf's -100/1000em space kept) and
    /// the numbers move the pen.
    fn show_tj(&mut self, array: &[Object], font: Option<&'d Dictionary>) {
        let mut piece = String::new();
        let start = self.pen_device();
        for value in array {
            match value {
                Object::String(bytes, _) => {
                    self.show_tj_pieces(bytes, font, Some(&mut piece));
                    if self.bail.is_some() {
                        return;
                    }
                }
                Object::Integer(adjust) => {
                    self.tj_adjust(*adjust as f64, &mut piece);
                }
                Object::Real(adjust) => {
                    self.tj_adjust(f64::from(*adjust), &mut piece);
                }
                _ => {}
            }
        }
        if piece.trim().is_empty() {
            return;
        }
        if piece.contains('\u{fffd}') {
            self.bail = Some("undecodable");
            return;
        }
        let end = self.pen_device();
        if !end.0.is_finite() || !end.1.is_finite() || !start.0.is_finite() || !start.1.is_finite()
        {
            return;
        }
        self.fragments.push(PositionedFragment {
            text: piece,
            x0: start.0 as f32,
            x1: end.0 as f32,
            baseline: start.1 as f32,
            size: self.device_size(),
            rotation: self.rotation_bucket(),
        });
    }

    fn tj_adjust(&mut self, adjust: f64, piece: &mut String) {
        if adjust < TJ_SPACE_THRESHOLD && !piece.is_empty() && !piece.ends_with(char::is_whitespace)
        {
            piece.push(' ');
        }
        let scaling = self.text.horizontal_scaling / 100.0;
        let tx = (-adjust / 1000.0) * self.text.font_size * scaling;
        self.advance_pen(tx);
    }

    fn walk_do(
        &mut self,
        operation: &Operation,
        fonts: &BTreeMap<Vec<u8>, &'d Dictionary>,
        resources: &[Dictionary],
        depth: usize,
    ) {
        if depth >= MAX_FORM_DEPTH {
            return;
        }
        let Some(name) = operation
            .operands
            .first()
            .and_then(|value| value.as_name().ok())
        else {
            return;
        };
        let Some((id, stream)) = lookup_xobject(self.pdf, resources, name) else {
            return;
        };
        if stream.dict.get(b"Subtype").and_then(Object::as_name).ok() != Some(&b"Form"[..]) {
            return;
        }
        if let Some(id) = id
            && !self.forms.insert(id)
        {
            return;
        }
        if let Ok(data) = stream.decompressed_content_with_limit(self.form_budget)
            && data.len() <= self.form_budget
            && let Ok(content) = Content::decode(&data)
        {
            self.form_budget -= data.len();
            let form_fonts = form_fonts(self.pdf, stream, fonts);
            let form_resources = form_resources(self.pdf, stream, resources);
            let saved_ctm = self.ctm;
            self.ctm = multiply(stream_matrix(stream), self.ctm);
            self.walk(&content.operations, &form_fonts, &form_resources, depth + 1);
            self.ctm = saved_ctm;
        }
        if let Some(id) = id {
            self.forms.remove(&id);
        }
    }
}

fn lookup_xobject<'a>(
    pdf: &'a LopdfDocument,
    resources: &[Dictionary],
    name: &[u8],
) -> Option<(Option<ObjectId>, &'a Stream)> {
    // The XObject entry must resolve to an object the document owns, so the
    // returned stream borrows from `pdf` (`'a`), not from the resource
    // dictionary clones the walker carries for the lookup. An inline XObject
    // inside a /Resources dictionary cannot be reborrowed that way and is
    // skipped; real XObject entries are references.
    for dictionary in resources {
        let Ok(xobjects) = dictionary.get(b"XObject") else {
            continue;
        };
        let xobjects = match xobjects {
            Object::Reference(id) => match pdf.get_dictionary(*id) {
                Ok(dictionary) => dictionary,
                Err(_) => continue,
            },
            Object::Dictionary(dictionary) => dictionary,
            _ => continue,
        };
        let Ok(entry) = xobjects.get(name) else {
            continue;
        };
        let stream = match entry {
            Object::Reference(id) => pdf
                .get_object(*id)
                .ok()
                .and_then(|object| object.as_stream().ok())
                .map(|stream| (Some(*id), stream)),
            _ => None,
        };
        if stream.is_some() {
            return stream;
        }
    }
    None
}

/// A `/Resources` or `/Font` entry dereferenced through the document: a
/// reference resolves to the object itself (`'a`), an inline dictionary
/// inside an owned stream is already `'a`.
fn deref_dict<'a>(pdf: &'a LopdfDocument, object: &'a Object) -> Option<&'a Dictionary> {
    match object {
        Object::Reference(id) => pdf.get_dictionary(*id).ok(),
        Object::Dictionary(dictionary) => Some(dictionary),
        _ => None,
    }
}

/// A form's own `/Resources/Font`, or the page fonts it inherits.
fn form_fonts<'a>(
    pdf: &'a LopdfDocument,
    stream: &'a Stream,
    inherited: &BTreeMap<Vec<u8>, &'a Dictionary>,
) -> BTreeMap<Vec<u8>, &'a Dictionary> {
    let Some(resources) = stream
        .dict
        .get(b"Resources")
        .ok()
        .and_then(|object| deref_dict(pdf, object))
    else {
        return inherited.clone();
    };
    let Some(fonts) = resources
        .get(b"Font")
        .ok()
        .and_then(|object| deref_dict(pdf, object))
    else {
        return inherited.clone();
    };
    let mut out = BTreeMap::new();
    for (name, value) in fonts.iter() {
        if let Some(dictionary) = deref_dict(pdf, value) {
            out.insert(name.clone(), dictionary);
        }
    }
    if out.is_empty() {
        inherited.clone()
    } else {
        out
    }
}

fn form_resources(
    pdf: &LopdfDocument,
    stream: &Stream,
    inherited: &[Dictionary],
) -> Vec<Dictionary> {
    let own = stream
        .dict
        .get(b"Resources")
        .ok()
        .and_then(|object| deref_dict(pdf, object));
    match own {
        Some(dictionary) => vec![dictionary.clone()],
        None => inherited.to_vec(),
    }
}

fn page_resources(pdf: &LopdfDocument, page_id: ObjectId) -> Vec<Dictionary> {
    let Ok((own, inherited)) = pdf.get_page_resources(page_id) else {
        return Vec::new();
    };
    let mut resources = Vec::new();
    if let Some(dictionary) = own {
        resources.push(dictionary.clone());
    }
    for id in inherited {
        if let Ok(dictionary) = pdf.get_dictionary(id) {
            resources.push(dictionary.clone());
        }
    }
    resources
}

// ---------------------------------------------------------------------------
// Lines and blocks
// ---------------------------------------------------------------------------

struct Line {
    fragments: Vec<PositionedFragment>,
    baseline: f32,
    x0: f32,
    x1: f32,
    size: f32,
    rotation: i32,
    /// Character count: weights the modal leading and the body band.
    chars: usize,
    /// The block the line landed in.
    block: usize,
}

impl Line {
    fn text(&self) -> String {
        let mut fragments = self.fragments.iter().collect::<Vec<_>>();
        fragments.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        let mut text = String::new();
        for fragment in fragments {
            // No gap-based space insertion: that is stage 2's calibrated
            // rule (orchestrator review, PDF-7b-1). Only the `TJ`
            // threshold lopdf already applies adds spaces in stage 1.
            text.push_str(fragment.text.trim_matches('\n'));
        }
        text.trim().to_string()
    }
}

/// Groups fragments into lines and lines into blocks (the `block` field).
fn layout_lines(fragments: Vec<PositionedFragment>) -> Vec<Line> {
    let mut lines = Vec::<Line>::new();
    for fragment in fragments {
        let joins = lines.last().is_some_and(|line: &Line| {
            // One line is a same-baseline run whose x extent touches or
            // nearly touches the line's: same-baseline fragments with a real
            // horizontal gap are separate spans (two-column text rows, table
            // cells), which keeps each line inside one column.
            line.rotation == fragment.rotation
                && (line.baseline - fragment.baseline).abs()
                    <= BASELINE_TOLERANCE * line.size.max(fragment.size)
                && fragment.x0 <= line.x1 + LINE_X_GAP * fragment.size
                && fragment.x1 >= line.x0 - LINE_X_GAP * fragment.size
        });
        if joins {
            let line = lines.last_mut().expect("line exists");
            line.x0 = line.x0.min(fragment.x0);
            line.x1 = line.x1.max(fragment.x1);
            line.chars += fragment.text.chars().count();
            line.size = line.size.max(fragment.size);
            line.fragments.push(fragment);
            continue;
        }
        lines.push(Line {
            chars: fragment.text.chars().count(),
            baseline: fragment.baseline,
            x0: fragment.x0,
            x1: fragment.x1,
            size: fragment.size,
            rotation: fragment.rotation,
            fragments: vec![fragment],
            block: 0,
        });
    }
    // Reading order within a rotation bucket: column bands left to right,
    // baselines top to bottom inside a band. Without the band pass an
    // interleaved two-column page alternates the columns line by line (the
    // Raft evidence run showed it) and headings land mid-paragraph; this is
    // the coarse version of stage 6's column work, needed now only so the
    // block boundaries are sane.
    lines.sort_by(|a, b| {
        a.rotation
            .cmp(&b.rotation)
            .then(b.baseline.total_cmp(&a.baseline))
            .then(a.x0.total_cmp(&b.x0))
    });
    let mut final_lines = Vec::with_capacity(lines.len());
    let mut buckets: Vec<Vec<Line>> = Vec::new();
    for line in lines {
        if buckets
            .last()
            .is_none_or(|bucket: &Vec<Line>| bucket[0].rotation != line.rotation)
        {
            buckets.push(Vec::new());
        }
        buckets.last_mut().expect("bucket").push(line);
    }
    for mut rest in buckets {
        let mut bands: Vec<Vec<Line>> = Vec::new();
        // Cluster the lines by their left edges: sorted by x0, split where
        // the next start is a full gap away. Column lines sit within an
        // indent of each other; a gutter or a centered line that begins
        // between the columns starts its own band, and a line that
        // STRADDLES the gutter but begins at a column edge only widens
        // its own band (membership never looks at x1, so it cannot chain
        // the columns together the way an x-extent union did).
        const BAND_X_GAP: f32 = 24.0;
        rest.sort_by(|a, b| a.x0.total_cmp(&b.x0));
        let mut band: Vec<Line> = Vec::new();
        let mut previous_x0 = f32::NEG_INFINITY;
        for line in rest {
            if !band.is_empty() && line.x0 - previous_x0 > BAND_X_GAP {
                bands.push(std::mem::take(&mut band));
            }
            previous_x0 = line.x0;
            band.push(line);
        }
        if !band.is_empty() {
            bands.push(band);
        }
        // Band order: highest first, then left to right. A full-width
        // title or a header row tops the page (and so leads its band's
        // block), and the left column still precedes the right.
        bands.sort_by(|a, b| {
            let top = |lines: &Vec<Line>| {
                lines
                    .iter()
                    .map(|line| line.baseline)
                    .fold(f32::NEG_INFINITY, f32::max)
            };
            top(b)
                .partial_cmp(&top(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    let left = |lines: &Vec<Line>| {
                        lines
                            .iter()
                            .map(|line| line.x0)
                            .fold(f32::INFINITY, f32::min)
                    };
                    left(a)
                        .partial_cmp(&left(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        for band in bands {
            final_lines.extend(band);
        }
    }
    let mut lines = final_lines;

    let leading = modal_leading(&lines);
    let (body_lo, body_hi) = body_band(&lines, leading);

    let mut block = 0_usize;
    let mut previous: Option<(i32, f32)> = None;
    for line in &mut lines {
        let starts_new = previous.is_some_and(|(rotation, baseline)| {
            if rotation != line.rotation {
                return true;
            }
            // A heading-shaped line breaks the block wherever it sits:
            // `infer_numbered_headings` only accepts a fragment that is
            // the heading alone, so a merged line loses the document's
            // whole inferred structure (the Raft evidence gate).
            if heading_shaped(&line.text()) {
                return true;
            }
            let gap = baseline - line.baseline;
            if gap <= 0.0 {
                // Overlapping or re-flowed line (an interleaved column
                // boundary): not a paragraph boundary in stage 1.
                return false;
            }
            if gap > BLOCK_GAP_RATIO * leading {
                return true;
            }
            let in_margin = |y: f32| y < body_lo || y > body_hi;
            gap > MARGIN_GAP_RATIO * leading && (in_margin(baseline) || in_margin(line.baseline))
        });
        if starts_new {
            block += 1;
        }
        line.block = block;
        previous = Some((line.rotation, line.baseline));
    }
    lines
}

/// The line looks like a numbered section heading: `N`, `N.N` or `N.N.N`
/// followed by a capitalized title, short enough to be a heading. This is
/// a lighter `parse_numbered_heading` (`pdf.rs`) - it only has to decide
/// whether the line starts a new block.
fn heading_shaped(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > 160 {
        return false;
    }
    let mut prefix_end = 0_usize;
    for (index, character) in text.char_indices() {
        if character.is_ascii_digit() || character == '.' {
            prefix_end = index + character.len_utf8();
        } else {
            break;
        }
    }
    if prefix_end == 0 {
        return false;
    }
    let number = text[..prefix_end].trim_end_matches('.');
    let title = text[prefix_end..]
        .trim_start_matches(|character: char| {
            character.is_whitespace()
                || matches!(character, '.' | ':' | '-' | '\u{2013}' | '\u{2014}')
        })
        .trim();
    !number.is_empty()
        && number
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.')
        && !title.is_empty()
        && title.split_whitespace().count() <= 24
        && title.chars().next().is_some_and(char::is_uppercase)
}

/// Renders the laid-out lines: `"\n"` inside a block, `"\n\n"` between
/// blocks.
fn render_lines(lines: &[Line]) -> (String, Vec<String>) {
    let mut text = String::new();
    let mut chunks = Vec::new();
    let mut current = None;
    let mut chunk = String::new();
    for line in lines {
        if current.is_some_and(|block| block != line.block) {
            let chunk = std::mem::take(&mut chunk);
            let chunk = chunk.trim().to_string();
            if !chunk.is_empty() {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(&chunk);
                chunks.push(chunk);
            }
        }
        current = Some(line.block);
        let line_text = line.text();
        if line_text.is_empty() {
            continue;
        }
        if !chunk.is_empty() {
            chunk.push('\n');
        }
        chunk.push_str(&line_text);
    }
    let chunk = chunk.trim().to_string();
    if !chunk.is_empty() {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&chunk);
        chunks.push(chunk);
    }
    (text.trim().to_string(), chunks)
}

/// One line of the layout, exposed to tests and the CI probe's JSON dump.
#[cfg(test)]
#[derive(Clone, Debug)]
pub(super) struct ProbeLine {
    pub text: String,
    pub x0: f32,
    pub x1: f32,
    pub baseline: f32,
    pub block: usize,
}

/// The page's modal leading: the most common baseline gap between consecutive
/// lines (whole points), weighted by the characters on the following line so a
/// header or footnote band cannot outvote the body.
fn modal_leading(lines: &[Line]) -> f32 {
    let mut votes: HashMap<u32, usize> = HashMap::new();
    for pair in lines.windows(2) {
        if pair[0].rotation != pair[1].rotation {
            continue;
        }
        let gap = pair[0].baseline - pair[1].baseline;
        if gap <= 0.5 || gap > 200.0 {
            continue;
        }
        *votes.entry(gap.round() as u32).or_default() += pair[1].chars;
    }
    let best = votes
        .into_iter()
        .max_by_key(|(_, weight)| *weight)
        .map(|(bucket, _)| bucket as f32);
    match best {
        Some(lead) => lead.max(1.0),
        None => {
            lines
                .iter()
                .map(|line| line.size)
                .max_by(|a, b| a.total_cmp(b))
                .unwrap_or(12.0)
                * 1.2
        }
    }
}

/// The [2nd, 98th] character-weighted baseline percentile, extended by half a
/// leading on each side. Lines outside the band are margin-band candidates;
/// the gap rule decides whether they break a block.
fn body_band(lines: &[Line], leading: f32) -> (f32, f32) {
    let total: usize = lines.iter().map(|line| line.chars).sum();
    if total == 0 || lines.len() < 6 {
        return (f32::MIN, f32::MAX);
    }
    let mut ordered = lines.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| a.baseline.total_cmp(&b.baseline));
    let percentile = |p: f32| {
        let target = (total as f32 * p) as usize;
        let mut seen = 0;
        for line in &ordered {
            seen += line.chars;
            if seen > target {
                return line.baseline;
            }
        }
        ordered.last().map(|line| line.baseline).unwrap_or(0.0)
    };
    (
        percentile(0.02) - MARGIN_BODY_BAND_LEADING * leading,
        percentile(0.98) + MARGIN_BODY_BAND_LEADING * leading,
    )
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// The shared walk: positioned lines, or a bail reason.
struct WalkedPage {
    lines: Vec<Line>,
    /// Shown codes walked; only the test-time probe reports it.
    #[allow(dead_code)]
    glyphs: usize,
    bail: Option<&'static str>,
}

fn walk_page<'d, 'w>(
    pdf: &'d LopdfDocument,
    page_id: ObjectId,
    max_decompressed: usize,
    memo: &'w mut FontEncodingMemo<'d>,
    metrics: &'w mut FontMetricsMemo<'d>,
) -> WalkedPage
where
    'd: 'w,
{
    let content_bytes = match pdf.get_page_content_with_limit(page_id, max_decompressed) {
        Ok(bytes) => bytes,
        Err(_) => {
            return WalkedPage {
                lines: Vec::new(),
                glyphs: 0,
                bail: Some("undecodable"),
            };
        }
    };
    let content = match Content::decode(&content_bytes) {
        Ok(content) => content,
        Err(_) => {
            return WalkedPage {
                lines: Vec::new(),
                glyphs: 0,
                bail: Some("undecodable"),
            };
        }
    };
    let fonts = pdf.get_page_fonts(page_id).unwrap_or_default();
    let resources = page_resources(pdf, page_id);
    let mut walker = PageWalker {
        pdf,
        max_decompressed,
        memo,
        metrics,
        ctm: IDENTITY,
        ctm_stack: Vec::new(),
        text: TextSpace::new(),
        fragments: Vec::new(),
        glyphs: 0,
        bail: None,
        forms: HashSet::new(),
        form_budget: max_decompressed,
    };
    walker.walk(&content.operations, &fonts, &resources, 0);
    if let Some(reason) = walker.bail {
        return WalkedPage {
            lines: Vec::new(),
            glyphs: walker.glyphs,
            bail: Some(reason),
        };
    }
    WalkedPage {
        lines: layout_lines(walker.fragments),
        glyphs: walker.glyphs,
        bail: None,
    }
}

/// Walks one page and returns its positional text, or a bailed-out page that
/// the caller sends through the old chunk path.
pub(super) fn extract_positional_page<'d, 'w>(
    pdf: &'d LopdfDocument,
    page_id: ObjectId,
    max_decompressed: usize,
    memo: &'w mut FontEncodingMemo<'d>,
    metrics: &'w mut FontMetricsMemo<'d>,
) -> PositionalPage
where
    'd: 'w,
{
    let walked = walk_page(pdf, page_id, max_decompressed, memo, metrics);
    if walked.bail.is_some() {
        return PositionalPage {
            bail: walked.bail,
            ..PositionalPage::default()
        };
    }
    let (_text, chunks) = render_lines(&walked.lines);
    PositionalPage { chunks, bail: None }
}

/// The same walk for the CI probe: the laid-out lines themselves, for the
/// line-by-line comparison against PyMuPDF.
#[cfg(test)]
pub(super) fn probe_positional_page<'d, 'w>(
    pdf: &'d LopdfDocument,
    page_id: ObjectId,
    max_decompressed: usize,
    memo: &'w mut FontEncodingMemo<'d>,
    metrics: &'w mut FontMetricsMemo<'d>,
) -> (Vec<ProbeLine>, usize, Option<&'static str>)
where
    'd: 'w,
{
    let walked = walk_page(pdf, page_id, max_decompressed, memo, metrics);
    let lines = walked
        .lines
        .iter()
        .map(|line| ProbeLine {
            text: line.text(),
            x0: line.x0,
            x1: line.x1,
            baseline: line.baseline,
            block: line.block,
        })
        .collect();
    (lines, walked.glyphs, walked.bail)
}

// ---------------------------------------------------------------------------
// Tests: hand-built content streams (PDF-7b-1 step 0, item 1) and the CI
// probe dump that feeds the PyMuPDF comparison and the glyph cap.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::{Document as LopdfDocument, Stream, dictionary};

    const LIMIT: usize = 16 * 1024 * 1024;

    /// A Type1 font with a 250-wide space and 500-wide everything else.
    fn wide_font() -> Dictionary {
        let mut widths = Vec::new();
        for code in 0_i64..=255 {
            widths.push(Object::Integer(if code == 32 { 250 } else { 500 }));
        }
        dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "FixtureSerif",
            "FirstChar" => 0,
            "LastChar" => 255,
            "Widths" => Object::Array(widths),
        }
    }

    fn document_with(
        pages: Vec<Vec<Operation>>,
        extra: impl FnOnce(&mut LopdfDocument, ObjectId) -> Dictionary,
    ) -> (LopdfDocument, Vec<ObjectId>) {
        let mut document = LopdfDocument::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(wide_font());
        let resources = extra(&mut document, font_id);
        let resources_id = document.add_object(resources);

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
        (document, page_ids)
    }

    fn font_resources() -> impl FnOnce(&mut LopdfDocument, ObjectId) -> Dictionary {
        |_document, font_id| {
            dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            }
        }
    }

    fn lines_of(document: &LopdfDocument, page_id: ObjectId) -> Vec<ProbeLine> {
        let mut memo = FontEncodingMemo::new();
        let mut metrics = FontMetricsMemo::new(document);
        let (lines, _glyphs, bail) =
            probe_positional_page(document, page_id, LIMIT, &mut memo, &mut metrics);
        assert!(bail.is_none(), "page bailed: {bail:?}");
        lines
    }

    fn op(operator: &str, operands: Vec<Object>) -> Operation {
        Operation::new(operator, operands)
    }

    fn text_at(x: i64, y: i64) -> Operation {
        op(
            "Tm",
            vec![1.into(), 0.into(), 0.into(), 1.into(), x.into(), y.into()],
        )
    }

    fn lit(text: &str) -> Object {
        Object::string_literal(text.as_bytes().to_vec())
    }

    /// w = 500/1000 * 12 = 6.0 per glyph at size 12; space is 3.0.
    #[test]
    fn tj_word_gap_advances_the_pen_and_keeps_the_space_rule() {
        let (document, page_ids) = document_with(
            vec![vec![
                op("BT", vec![]),
                op("Tf", vec!["F1".into(), 12.into()]),
                text_at(100, 700),
                op(
                    "TJ",
                    vec![Object::Array(vec![
                        lit("Hello"),
                        Object::Integer(-250),
                        lit("world"),
                    ])],
                ),
                op("ET", vec![]),
            ]],
            font_resources(),
        );
        let lines = lines_of(&document, page_ids[0]);
        assert_eq!(lines.len(), 1);
        // One fragment per TJ array: the -250/1000em adjustment became a
        // space and moved the pen by 3.0.
        assert_eq!(lines[0].text, "Hello world");
        assert!((lines[0].x0 - 100.0).abs() < 0.01);
        // 10 glyphs * 6.0 + 3.0 of the TJ gap = 63.0.
        assert!((lines[0].x1 - 163.0).abs() < 0.01);
    }

    #[test]
    fn td_positions_the_word_relative_to_the_pen() {
        let (document, page_ids) = document_with(
            vec![vec![
                op("BT", vec![]),
                op("Tf", vec!["F1".into(), 12.into()]),
                text_at(100, 700),
                op("Tj", vec![lit("AB")]),
                op("Td", vec![30.into(), 0.into()]),
                op("Tj", vec![lit("CD")]),
                op("ET", vec![]),
            ]],
            font_resources(),
        );
        let lines = lines_of(&document, page_ids[0]);
        // Td moves the line start (not just the pen): the 30-unit offset
        // lands CD at x=130, inside the 2*size join reach, so the baseline
        // shares one line.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "ABCD");
        assert!((lines[0].x0 - 100.0).abs() < 0.01);
        assert!((lines[0].x1 - 142.0).abs() < 0.01);
        assert!((lines[0].baseline - 700.0).abs() < 0.01);
    }

    #[test]
    fn ts_superscript_rides_above_the_baseline() {
        let (document, page_ids) = document_with(
            vec![vec![
                op("BT", vec![]),
                op("Tf", vec!["F1".into(), 12.into()]),
                text_at(100, 700),
                op("Tj", vec![lit("10")]),
                op("Ts", vec![4.into()]),
                op("Tj", vec![lit("3")]),
                op("ET", vec![]),
            ]],
            font_resources(),
        );
        let lines = lines_of(&document, page_ids[0]);
        // 4pt rise on a 12pt line is under 0.35*size, so the runs stay on
        // one line whose baseline is the first fragment's.
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "103");
        // The second fragment's baseline is +4: check via the fragment
        // extent (the line reports the run's own baseline).
        assert!((lines[0].x1 - (112.0 + 6.0)).abs() < 0.01);
    }

    #[test]
    fn cm_scaled_form_xobject_text_is_positioned_through_the_matrix() {
        let build = |document: &mut LopdfDocument, font_id: ObjectId| {
            // A form whose matrix doubles everything and shifts by (10, 20).
            let form_id = document.add_object(Stream::new(
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Form",
                    "Matrix" => vec![2.into(), 0.into(), 0.into(), 2.into(), 10.into(), 20.into()],
                    "Resources" => dictionary! {
                        "Font" => dictionary! { "F1" => font_id },
                    },
                    "BBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
                },
                Content {
                    operations: vec![
                        op("BT", vec![]),
                        op("Tf", vec!["F1".into(), 12.into()]),
                        text_at(50, 40),
                        op("Tj", vec![lit("XY")]),
                        op("ET", vec![]),
                    ],
                }
                .encode()
                .expect("form encodes"),
            ));
            dictionary! {
                "Font" => dictionary! { "F1" => font_id },
                "XObject" => dictionary! { "Fm1" => form_id },
            }
        };
        let (document, page_ids) = document_with(
            vec![vec![op("Do", vec![Object::Name(b"Fm1".to_vec())])]],
            build,
        );
        let lines = lines_of(&document, page_ids[0]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "XY");
        // Origin (50,40) through [2 0 0 2 10 20]: (110, 100).
        assert!((lines[0].x0 - 110.0).abs() < 0.01);
        assert!((lines[0].baseline - 100.0).abs() < 0.01);
        // Two 6pt glyphs scaled by 2: x1 = 110 + 24.
        assert!((lines[0].x1 - 134.0).abs() < 0.01);
    }

    #[test]
    fn rotated_line_lands_in_its_own_bucket() {
        let (document, page_ids) = document_with(
            vec![vec![
                op("BT", vec![]),
                op("Tf", vec!["F1".into(), 12.into()]),
                text_at(100, 700),
                op("Tj", vec![lit("body")]),
                op(
                    "Tm",
                    vec![
                        0.into(),
                        1.into(),
                        (-1).into(),
                        0.into(),
                        60.into(),
                        400.into(),
                    ],
                ),
                op("Tj", vec![lit("side")]),
                op("ET", vec![]),
            ]],
            font_resources(),
        );
        let lines = lines_of(&document, page_ids[0]);
        assert_eq!(lines.len(), 2);
        // The rotated line sorts before bucket 0 (rotation 90 < 0? no: the
        // sort puts rotation buckets in numeric order: 90 before 0 is
        // false; bucket order is 0 then 90). Find it by text.
        let side = lines
            .iter()
            .find(|line| line.text == "side")
            .expect("side line");
        // A 90-degree text matrix: the device baseline y is the Tm f
        // component 400, and x stays at Tm e = 60.
        assert!((side.x0 - 60.0).abs() < 0.01);
        assert!((side.baseline - 400.0).abs() < 0.01);
        assert!(lines.iter().any(|line| line.text == "body"));
    }

    #[test]
    fn tz_condensed_run_advances_half_as_far() {
        let (document, page_ids) = document_with(
            vec![vec![
                op("BT", vec![]),
                op("Tf", vec!["F1".into(), 12.into()]),
                text_at(100, 700),
                op("Tj", vec![lit("AB")]),
                op("Tz", vec![50.into()]),
                op("Tj", vec![lit("CD")]),
                op("ET", vec![]),
            ]],
            font_resources(),
        );
        let lines = lines_of(&document, page_ids[0]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "ABCD");
        // AB advances 12.0; CD advances 6.0 at Tz 50.
        assert!((lines[0].x1 - 118.0).abs() < 0.01);
    }

    /// CI probe (step 0, items 2 and 3): dumps each PDF under
    /// `PDF7B_PROBE_DIR` as one JSONL record per page to stdout - the
    /// workflow's Python half joins it with `page.get_text("dict")`. Run by
    /// `cargo test -p reading-mcp pdf7b1_positional_probe -- --ignored`.
    #[test]
    #[ignore = "CI probe: needs PDF7B_PROBE_DIR"]
    fn pdf7b1_positional_probe() {
        let dir = std::env::var("PDF7B_PROBE_DIR").expect("PDF7B_PROBE_DIR is not set");
        let mut entries = std::fs::read_dir(&dir)
            .expect("probe dir unreadable")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "pdf"))
            .collect::<Vec<_>>();
        entries.sort();
        let out_dir = std::env::var("PDF7B_PROBE_OUT").unwrap_or_else(|_| dir.clone());
        let mut out = String::new();
        for path in entries {
            let id = path.file_stem().expect("file name").to_string_lossy();
            let bytes = std::fs::read(&path).expect("pdf unreadable");
            let Ok(document) = LopdfDocument::load_mem(&bytes) else {
                println!("{{\"id\":\"{id}\",\"error\":\"load\"}}");
                continue;
            };
            let mut memo = FontEncodingMemo::new();
            let mut metrics = FontMetricsMemo::new(&document);
            let diag = std::env::var_os("PDF7B_PROBE_DIAG").is_some();
            eprintln!("probe starting {id}");
            for (number, page_id) in document.get_pages() {
                eprintln!("probe {id} page {number}");
                if diag {
                    let (lines, _glyphs, bail) =
                        probe_positional_page(&document, page_id, LIMIT, &mut memo, &mut metrics);
                    for line in &lines {
                        let marker = if line
                            .text
                            .trim_start()
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_ascii_digit())
                        {
                            " *"
                        } else {
                            ""
                        };
                        println!(
                            "DIAG id={id} page={number} b={} x={:.0}-{:.0} y={:.1}{marker}: {}",
                            line.block, line.x0, line.x1, line.baseline, line.text
                        );
                    }
                    println!("DIAG id={id} page={number} bail={bail:?}");
                    continue;
                }
                let (lines, glyphs, bail) =
                    probe_positional_page(&document, page_id, LIMIT, &mut memo, &mut metrics);
                let mut line_json = String::new();
                for line in &lines {
                    if !line_json.is_empty() {
                        line_json.push(',');
                    }
                    line_json.push_str(&format!(
                        "{{\"x0\":{:.2},\"x1\":{:.2},\"y\":{:.2},\"b\":{},\"t\":{}}}",
                        line.x0,
                        line.x1,
                        line.baseline,
                        line.block,
                        serde_json_escape(&line.text),
                    ));
                }
                let record = format!(
                    "{{\"id\":\"{id}\",\"page\":{number},\"glyphs\":{glyphs},\"bail\":{},\"lines\":[{}]}}",
                    bail.map_or("null".into(), |reason| format!("\"{reason}\"")),
                    line_json,
                );
                out.push_str(&record);
                out.push('\n');
                println!("{record}");
            }
        }
        let _ = std::fs::create_dir_all(&out_dir);
        let out_path = std::path::Path::new(&out_dir).join("positional-lines.jsonl");
        std::fs::write(&out_path, out).expect("probe output writable");
        eprintln!("probe wrote {}", out_path.display());
    }

    fn serde_json_escape(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        out.push('"');
        for character in text.chars() {
            match character {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
}
