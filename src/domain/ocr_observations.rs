use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::{OcrEvidenceRecord, OcrRuntimeIdentity};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OcrAttemptId {
    Primary,
    Retry,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct OcrObservationReference {
    pub page: u32,
    pub attempt: OcrAttemptId,
    pub r#box: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrObservedWord {
    pub text: String,
    pub bbox: [f64; 4],
    pub flags: u32,
    pub ocr_block: u32,
    pub ocr_paragraph: u32,
    pub ocr_line: u32,
    pub confidence: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrObservedLine {
    pub text: String,
    pub bbox: [f64; 4],
    pub spans: Vec<OcrObservedWord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrObservedBox {
    pub boxclass: String,
    pub bbox: [f64; 4],
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub textlines: Vec<OcrObservedLine>,
    pub ocr_block: u32,
    pub ocr_paragraph: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrRawAttempt {
    pub id: OcrAttemptId,
    pub psm: u8,
    pub boxes: Vec<OcrObservedBox>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrBoxSelection {
    pub selected_box: usize,
    pub source: OcrObservationReference,
    pub words: Vec<OcrObservationReference>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrRetryComponent {
    pub indices: Vec<usize>,
    pub roi: [f64; 4],
    pub effective_roi: [f64; 4],
    pub candidate_count: usize,
    pub resolved: bool,
    pub failure: Option<String>,
    pub primary_refs: Vec<OcrObservationReference>,
    pub candidate_refs: Vec<OcrObservationReference>,
    pub replaced_refs: Vec<OcrObservationReference>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrPageObservations {
    pub schema: String,
    pub page: u32,
    pub page_bounds: [f64; 4],
    pub complete: bool,
    pub attempts: Vec<OcrRawAttempt>,
    pub selection: Vec<OcrBoxSelection>,
    pub components: Vec<OcrRetryComponent>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrEvidenceBlob {
    pub schema: String,
    pub original_sha256: String,
    pub runtime_identity: OcrRuntimeIdentity,
    pub pages: Vec<OcrPageObservations>,
    pub selected_words: Vec<OcrEvidenceRecord>,
}

fn within(b: [f64; 4], outer: [f64; 4]) -> bool {
    b.iter().all(|x| x.is_finite())
        && b[0] < b[2]
        && b[1] < b[3]
        && b[0] >= outer[0]
        && b[1] >= outer[1]
        && b[2] <= outer[2]
        && b[3] <= outer[3]
}

impl OcrPageObservations {
    fn box_at(&self, reference: &OcrObservationReference) -> Result<&OcrObservedBox, String> {
        if reference.page != self.page || reference.line.is_some() || reference.word.is_some() {
            return Err("invalid OCR box reference".into());
        }
        self.attempts
            .iter()
            .find(|a| a.id == reference.attempt)
            .and_then(|a| a.boxes.get(reference.r#box))
            .ok_or_else(|| "missing OCR referenced box".into())
    }

    pub fn validate(&self, max_page: u32) -> Result<Vec<OcrEvidenceRecord>, String> {
        if self.schema != "ocr-regional-observations/v1"
            || self.page == 0
            || self.page > max_page
            || !self.complete
            || self.page_bounds[0] != 0.0
            || self.page_bounds[1] != 0.0
            || !within(self.page_bounds, self.page_bounds)
        {
            return Err("invalid/incomplete OCR page observations".into());
        }
        if self.attempts.is_empty()
            || self.attempts.len() > 2
            || self.attempts[0].id != OcrAttemptId::Primary
            || self.attempts[0].psm != 3
            || (self.attempts.len() == 2
                && (self.attempts[1].id != OcrAttemptId::Retry || self.attempts[1].psm != 6))
            || (self.components.is_empty() != (self.attempts.len() == 1))
        {
            return Err("invalid OCR attempt sequence".into());
        }
        for attempt in &self.attempts {
            for b in &attempt.boxes {
                if b.boxclass != "text"
                    || b.ocr_block == 0
                    || b.ocr_paragraph == 0
                    || b.bbox != [b.x0, b.y0, b.x1, b.y1]
                    || !within(b.bbox, self.page_bounds)
                    || b.textlines.is_empty()
                {
                    return Err("invalid OCR observed box".into());
                }
                for line in &b.textlines {
                    if !within(line.bbox, b.bbox) || line.spans.is_empty() {
                        return Err("invalid OCR observed line".into());
                    }
                    for word in &line.spans {
                        if !within(word.bbox, line.bbox)
                            || word.text.trim().is_empty()
                            || word.ocr_block != b.ocr_block
                            || word.ocr_paragraph != b.ocr_paragraph
                            || word.ocr_line == 0
                            || word
                                .confidence
                                .is_some_and(|c| !c.is_finite() || !(0.0..=100.0).contains(&c))
                        {
                            return Err("invalid OCR observed word".into());
                        }
                    }
                }
            }
        }
        let mut replaced = BTreeSet::new();
        let mut candidates = BTreeSet::new();
        for component in &self.components {
            if !component.resolved
                || component.failure.is_some()
                || component.indices.is_empty()
                || component.primary_refs != component.replaced_refs
                || component.candidate_count == 0
                || component.candidate_count != component.candidate_refs.len()
                || !within(component.roi, self.page_bounds)
                || !within(component.roi, component.effective_roi)
                || !within(component.effective_roi, self.page_bounds)
            {
                return Err("invalid OCR retry component".into());
            }
            let indices: Vec<_> = component.primary_refs.iter().map(|r| r.r#box).collect();
            if indices != component.indices || indices.windows(2).any(|w| w[0] >= w[1]) {
                return Err("invalid OCR component members".into());
            }
            for reference in &component.candidate_refs {
                if reference.attempt != OcrAttemptId::Retry
                    || !candidates.insert(reference.clone())
                    || !within(self.box_at(reference)?.bbox, component.effective_roi)
                {
                    return Err("invalid/reused OCR candidate".into());
                }
            }
            for reference in &component.primary_refs {
                if reference.attempt != OcrAttemptId::Primary || !replaced.insert(reference.clone())
                {
                    return Err("overlapping OCR component".into());
                }
                for line in &self.box_at(reference)?.textlines {
                    for word in &line.spans {
                        let x = (word.bbox[0] + word.bbox[2]) / 2.0;
                        let y = (word.bbox[1] + word.bbox[3]) / 2.0;
                        let mut covered = false;
                        for candidate in &component.candidate_refs {
                            covered |= self.box_at(candidate)?.textlines.iter().any(|l| {
                                l.bbox[0] <= x && x <= l.bbox[2] && l.bbox[1] <= y && y <= l.bbox[3]
                            });
                        }
                        if !covered {
                            return Err("OCR retry does not cover original word".into());
                        }
                    }
                }
            }
        }
        let mut expected_sources = Vec::new();
        for index in 0..self.attempts[0].boxes.len() {
            if let Some(component) = self.components.iter().find(|c| c.indices[0] == index) {
                expected_sources.extend(component.candidate_refs.clone());
            }
            let reference = OcrObservationReference {
                page: self.page,
                attempt: OcrAttemptId::Primary,
                r#box: index,
                line: None,
                word: None,
            };
            if !replaced.contains(&reference) {
                expected_sources.push(reference);
            }
        }
        if self.selection.len() != expected_sources.len() {
            return Err("incomplete OCR selection".into());
        }
        let mut selected_words = Vec::new();
        for (index, (selection, expected)) in
            self.selection.iter().zip(expected_sources).enumerate()
        {
            if selection.selected_box != index || selection.source != expected {
                return Err("OCR selection order mismatch".into());
            }
            let b = self.box_at(&selection.source)?;
            let mut expected_words = Vec::new();
            for (line_index, line) in b.textlines.iter().enumerate() {
                for (word_index, word) in line.spans.iter().enumerate() {
                    let mut reference = selection.source.clone();
                    reference.line = Some(line_index);
                    reference.word = Some(word_index);
                    expected_words.push(reference);
                    selected_words.push(OcrEvidenceRecord {
                        page: self.page,
                        block: word.ocr_block,
                        paragraph: word.ocr_paragraph,
                        line: word.ocr_line,
                        text: word.text.clone(),
                        bbox: word.bbox,
                        confidence: word.confidence,
                    });
                }
            }
            if selection.words != expected_words {
                return Err("OCR word references mismatch".into());
            }
        }
        Ok(selected_words)
    }
}

impl OcrEvidenceBlob {
    pub fn validate(&self, max_page: u32) -> Result<(), String> {
        if self.schema != "ocr-evidence/v2" || self.pages.is_empty() {
            return Err("missing/version-mismatched OCR observations".into());
        }
        if !super::ocr::valid_sha256(&self.original_sha256)
            || OcrRuntimeIdentity::build(
                self.runtime_identity.config.clone(),
                self.runtime_identity.dependencies.clone(),
            )? != self.runtime_identity
        {
            return Err("invalid OCR evidence source/runtime identity".into());
        }
        let mut seen = BTreeSet::new();
        let mut selected = Vec::new();
        for page in &self.pages {
            if !seen.insert(page.page) {
                return Err("duplicate OCR page".into());
            }
            selected.extend(page.validate(max_page)?);
        }
        if selected != self.selected_words {
            return Err("selected OCR evidence differs from raw attempts".into());
        }
        Ok(())
    }
}
