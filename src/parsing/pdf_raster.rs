//! Raster-image coverage of PDF pages.
//!
//! The default backend needs to tell an image-only scan from a text PDF that
//! merely contains figures. Text length alone cannot do that: a watermark, a
//! cover page or a figure appendix can leave a page with almost no text. The
//! second condition is how much of each page is actually painted by raster
//! images.
//!
//! This walk is deliberately small and self-contained: it reads the page's own
//! content stream, follows `Do` into image and form XObjects, and tracks the
//! graphics-state transformation matrix. It never decodes image data, never
//! renders a page and never touches the text extractor's output. Callers only
//! run it once the text is already known to be nearly empty, so a PDF with a
//! text layer never pays for it.
//!
//! Inline images (`BI ... ID ... EI`) are not counted. A page that paints its
//! scan inline therefore reports zero coverage and is not flagged, which is the
//! conservative direction: the rule never guesses.

use std::collections::HashSet;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document as LopdfDocument, Object, ObjectId, Stream};

const MAX_DECOMPRESSED_CONTENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_FORM_DEPTH: usize = 8;
const MAX_MEDIA_BOX_PARENTS: usize = 32;
const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// Mean fraction of each page's area painted by raster images, in `[0, 1]`.
pub(super) fn document_raster_coverage(pdf: &LopdfDocument, pages: &[(u32, ObjectId)]) -> f64 {
    if pages.is_empty() {
        return 0.0;
    }
    let total = pages
        .iter()
        .map(|(_, page_id)| page_raster_coverage(pdf, *page_id))
        .sum::<f64>();
    total / pages.len() as f64
}

/// Fraction of one page's area painted by raster images, in `[0, 1]`.
fn page_raster_coverage(pdf: &LopdfDocument, page_id: ObjectId) -> f64 {
    let Some(page_area) = page_area(pdf, page_id) else {
        return 0.0;
    };
    let Ok(bytes) = pdf.get_page_content_with_limit(page_id, MAX_DECOMPRESSED_CONTENT_BYTES) else {
        return 0.0;
    };
    let Ok(content) = Content::decode(&bytes) else {
        return 0.0;
    };
    let resources = page_resources(pdf, page_id);
    let mut walk = CoverageWalk::default();
    walk.operations(pdf, &content.operations, &resources, IDENTITY, 0);
    (walk.covered / page_area).clamp(0.0, 1.0)
}

#[derive(Default)]
struct CoverageWalk {
    covered: f64,
    visited_forms: HashSet<ObjectId>,
    saved_matrices: Vec<[f64; 6]>,
}

impl CoverageWalk {
    fn operations(
        &mut self,
        pdf: &LopdfDocument,
        operations: &[Operation],
        resources: &[Dictionary],
        mut ctm: [f64; 6],
        depth: usize,
    ) {
        for operation in operations {
            match operation.operator.as_str() {
                "q" => self.saved_matrices.push(ctm),
                "Q" => {
                    if let Some(saved) = self.saved_matrices.pop() {
                        ctm = saved;
                    }
                }
                "cm" => {
                    if let Some(matrix) = matrix_from(&operation.operands) {
                        ctm = multiply(ctm, matrix);
                    }
                }
                "Do" => {
                    let Some(name) = operation
                        .operands
                        .first()
                        .and_then(|operand| operand.as_name().ok())
                    else {
                        continue;
                    };
                    let Some((id, stream)) = lookup_xobject(pdf, resources, name) else {
                        continue;
                    };
                    if is_image(stream) {
                        self.covered += (ctm[0] * ctm[3] - ctm[1] * ctm[2]).abs();
                    } else if is_form(stream) && depth < MAX_FORM_DEPTH {
                        if let Some(id) = id
                            && !self.visited_forms.insert(id)
                        {
                            continue;
                        }
                        let form_resources = form_resources(pdf, stream, resources);
                        let form_matrix = stream_matrix(stream);
                        let Ok(bytes) =
                            stream.decompressed_content_with_limit(MAX_DECOMPRESSED_CONTENT_BYTES)
                        else {
                            continue;
                        };
                        let Ok(content) = Content::decode(&bytes) else {
                            continue;
                        };
                        self.operations(
                            pdf,
                            &content.operations,
                            &form_resources,
                            multiply(ctm, form_matrix),
                            depth + 1,
                        );
                    }
                }
                _ => {}
            }
        }
    }
}

fn page_area(pdf: &LopdfDocument, page_id: ObjectId) -> Option<f64> {
    let (x0, y0, x1, y1) = page_media_box(pdf, page_id)?;
    let area = (x1 - x0).abs() * (y1 - y0).abs();
    (area > 0.0 && area.is_finite()).then_some(area)
}

fn page_media_box(pdf: &LopdfDocument, page_id: ObjectId) -> Option<(f64, f64, f64, f64)> {
    let mut current = page_id;
    for _ in 0..MAX_MEDIA_BOX_PARENTS {
        let dictionary = pdf.get_dictionary(current).ok()?;
        if let Ok(value) = dictionary.get_deref(b"MediaBox", pdf)
            && let Some(bounds) = rectangle(value)
        {
            return Some(bounds);
        }
        current = dictionary.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

fn rectangle(object: &Object) -> Option<(f64, f64, f64, f64)> {
    let values = object.as_array().ok()?;
    if values.len() < 4 {
        return None;
    }
    Some((
        number(&values[0])?,
        number(&values[1])?,
        number(&values[2])?,
        number(&values[3])?,
    ))
}

fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(f64::from(*value)),
        _ => None,
    }
}

fn matrix_from(operands: &[Object]) -> Option<[f64; 6]> {
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

fn stream_matrix(stream: &Stream) -> [f64; 6] {
    stream
        .dict
        .get(b"Matrix")
        .ok()
        .and_then(|object| object.as_array().ok())
        .and_then(|values| {
            if values.len() < 6 {
                return None;
            }
            Some([
                number(&values[0])?,
                number(&values[1])?,
                number(&values[2])?,
                number(&values[3])?,
                number(&values[4])?,
                number(&values[5])?,
            ])
        })
        .unwrap_or(IDENTITY)
}

/// `first` then `second`, matching the PDF `cm` operator.
fn multiply(first: [f64; 6], second: [f64; 6]) -> [f64; 6] {
    [
        first[0] * second[0] + first[1] * second[2],
        first[0] * second[1] + first[1] * second[3],
        first[2] * second[0] + first[3] * second[2],
        first[2] * second[1] + first[3] * second[3],
        first[4] * second[0] + first[5] * second[2] + second[4],
        first[4] * second[1] + first[5] * second[3] + second[5],
    ]
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

fn form_resources(
    pdf: &LopdfDocument,
    stream: &Stream,
    inherited: &[Dictionary],
) -> Vec<Dictionary> {
    let own = stream
        .dict
        .get(b"Resources")
        .ok()
        .and_then(|object| pdf.dereference(object).ok())
        .and_then(|(_, object)| object.as_dict().ok());
    match own {
        Some(dictionary) => vec![dictionary.clone()],
        None => inherited.to_vec(),
    }
}

fn lookup_xobject<'a>(
    pdf: &'a LopdfDocument,
    resources: &'a [Dictionary],
    name: &[u8],
) -> Option<(Option<ObjectId>, &'a Stream)> {
    for dictionary in resources {
        let Ok(xobjects) = dictionary.get(b"XObject") else {
            continue;
        };
        let Ok((_, xobjects)) = pdf.dereference(xobjects) else {
            continue;
        };
        let Ok(xobjects) = xobjects.as_dict() else {
            continue;
        };
        let Ok(entry) = xobjects.get(name) else {
            continue;
        };
        let Ok((id, object)) = pdf.dereference(entry) else {
            continue;
        };
        let Ok(stream) = object.as_stream() else {
            continue;
        };
        return Some((id, stream));
    }
    None
}

fn is_image(stream: &Stream) -> bool {
    stream.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(&b"Image"[..])
}

fn is_form(stream: &Stream) -> bool {
    stream.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(&b"Form"[..])
}
