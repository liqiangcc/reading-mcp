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
#[serde(untagged, deny_unknown_fields)]
pub enum OcrVisualTensor {
    Matrix(Vec<Vec<f64>>),
    Vector(Vec<f64>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrVisualRawBox {
    pub cls_id: u32,
    pub label: String,
    pub score: f64,
    pub coordinate: [f64; 4],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrVisualRawAttempt {
    pub res: OcrVisualRawResult,
    pub raw_outputs: Vec<OcrVisualTensor>,
    pub postprocess: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrVisualRawResult {
    pub boxes: Vec<OcrVisualRawBox>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrVisualRegion {
    pub prediction_index: usize,
    pub label: String,
    pub model_score: f64,
    pub pixel_bbox: [f64; 4],
    pub bbox: [f64; 4],
    pub box_sources: Vec<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrVisualBinding {
    pub source_box: usize,
    pub prediction_index: usize,
    pub source_bbox: [f64; 4],
    pub projected_bbox: [f64; 4],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrVisualFailure {
    #[serde(default)]
    pub source_box: Option<usize>,
    #[serde(default)]
    pub source_boxes: Vec<usize>,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrVisualParagraphMerge {
    pub source_boxes: Vec<usize>,
    pub prediction_index: Option<usize>,
    pub bbox: [f64; 4],
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrVisualMove {
    pub source_boxes: Vec<usize>,
    pub from_group: usize,
    pub to_group: usize,
    pub prose_source_boxes: Vec<usize>,
    pub prose_bottom: f64,
    pub visual_top: f64,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrVisualTerminalOrder {
    pub schema: String,
    pub input_source_groups: Vec<Vec<usize>>,
    pub output_group_indices: Vec<usize>,
    pub moves: Vec<OcrVisualMove>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrVisualProjection {
    pub schema: String,
    pub page: u32,
    pub page_bounds: [f64; 4],
    pub raster_size: [u32; 2],
    pub regions: Vec<OcrVisualRegion>,
    pub bindings: Vec<OcrVisualBinding>,
    pub failures: Vec<OcrVisualFailure>,
    pub text_regions: Vec<OcrVisualRegion>,
    pub paragraph_merges: Vec<OcrVisualParagraphMerge>,
    pub projected_source_groups: Vec<Vec<usize>>,
    pub unanchored_visual_regions: Vec<usize>,
    pub terminal_visual_order: OcrVisualTerminalOrder,
    pub complete: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrVisualAttempt {
    pub schema: String,
    pub page: u32,
    pub raster_size: [u32; 2],
    pub raster_sha256: String,
    pub dependencies: Vec<super::DependencyFingerprint>,
    pub attempts: Vec<OcrVisualRawAttempt>,
    pub projection: OcrVisualProjection,
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
pub struct BlankRasterEvidence {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub white_samples: u64,
    pub glyph_spans: u32,
    pub samples_sha256: String,
}

impl BlankRasterEvidence {
    fn validate(&self) -> Result<(), String> {
        let pixels = u64::from(self.width) * u64::from(self.height);
        if pixels == 0
            || pixels > 16_000_000
            || self.channels != 3
            || self.white_samples != pixels * 3
            || self.glyph_spans != 0
        {
            return Err("invalid blank raster observation".into());
        }
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        let bytes = [255_u8; 4096];
        let mut remaining = self.white_samples;
        while remaining > 0 {
            let count = remaining.min(bytes.len() as u64) as usize;
            digest.update(&bytes[..count]);
            remaining -= count as u64;
        }
        if format!("{:x}", digest.finalize()) != self.samples_sha256 {
            return Err("blank raster pixel digest mismatch".into());
        }
        Ok(())
    }
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
    #[serde(default)]
    pub blank_raster: Option<BlankRasterEvidence>,
    #[serde(default)]
    pub native_coverage: Option<NativeRasterCoverage>,
    #[serde(default)]
    pub mixed_order: Option<OcrMixedOrder>,
    #[serde(default)]
    pub native_regions: Vec<NativeTextRegion>,
    #[serde(default)]
    pub excluded_sources: Vec<OcrObservationReference>,
    #[serde(default)]
    pub projection_failure: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "origin", rename_all = "snake_case", deny_unknown_fields)]
pub enum MixedOrderEntry {
    Native {
        source_box: usize,
        projected_box: usize,
    },
    LocalOcr {
        source: OcrObservationReference,
        projected_box: usize,
    },
}

impl MixedOrderEntry {
    pub fn projected_box(&self) -> usize {
        match self {
            Self::Native { projected_box, .. } | Self::LocalOcr { projected_box, .. } => {
                *projected_box
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrMixedOrder {
    pub schema: String,
    pub original_box_count: usize,
    pub entries: Vec<MixedOrderEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCoverageMask {
    pub source_box: usize,
    pub text: String,
    pub bbox: [f64; 4],
    pub pixel_bbox: [u32; 4],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRasterCoverage {
    pub schema: String,
    pub width: u32,
    pub height: u32,
    pub padding_pixels: u32,
    pub source_samples_sha256: String,
    pub masked_samples_sha256: String,
    pub uncovered_samples: u64,
    pub masks: Vec<NativeCoverageMask>,
}

impl NativeRasterCoverage {
    fn validate(&self, page: &OcrPageObservations) -> Result<(), String> {
        use unicode_normalization::UnicodeNormalization;
        let compact = |text: &str| {
            text.nfkc()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        };
        let samples = u64::from(self.width) * u64::from(self.height) * 3;
        if self.schema != "ocr-native-raster-coverage/v1"
            || self.padding_pixels != 2
            || samples == 0
            || samples > 48_000_000
            || self.uncovered_samples > samples
            || f64::from(self.width) != (page.page_bounds[2] * 300.0 / 72.0).ceil()
            || f64::from(self.height) != (page.page_bounds[3] * 300.0 / 72.0).ceil()
            || [&self.source_samples_sha256, &self.masked_samples_sha256]
                .iter()
                .any(|hash| {
                    hash.len() != 64
                        || !hash
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
        {
            return Err("invalid native raster coverage".into());
        }
        for mask in &self.masks {
            let native = page
                .native_regions
                .iter()
                .find(|region| region.source_box == mask.source_box)
                .ok_or("coverage mask has no native source")?;
            let text = compact(&mask.text);
            let x = (mask.bbox[0] + mask.bbox[2]) / 2.0;
            let y = (mask.bbox[1] + mask.bbox[3]) / 2.0;
            if !within(mask.bbox, page.page_bounds)
                || text.is_empty()
                || mask.text.contains(['\0', '\u{fffd}'])
                || !compact(&native.text).contains(&text)
                || x < native.bbox[0]
                || x > native.bbox[2]
                || y < native.bbox[1]
                || y > native.bbox[3]
            {
                return Err("unbound native coverage mask".into());
            }
            let scale = 300.0 / 72.0;
            let expected = [
                (mask.bbox[0] * scale).floor().max(2.0) as u32 - 2,
                (mask.bbox[1] * scale).floor().max(2.0) as u32 - 2,
                ((mask.bbox[2] * scale).ceil() as u32 + 2).min(self.width),
                ((mask.bbox[3] * scale).ceil() as u32 + 2).min(self.height),
            ];
            if mask.pixel_bbox != expected {
                return Err("native coverage raster transform mismatch".into());
            }
        }
        if self.uncovered_samples == 0 {
            if self.masks.is_empty() {
                return Err("native reuse requires source masks".into());
            }
            // Recompute the exact all-white masked raster digest, not just its shape.
            BlankRasterEvidence {
                width: self.width,
                height: self.height,
                channels: 3,
                white_samples: samples,
                glyph_spans: 0,
                samples_sha256: self.masked_samples_sha256.clone(),
            }
            .validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeTextRegion {
    pub source_box: usize,
    pub source_class: String,
    pub bbox: [f64; 4],
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrEvidenceBlob {
    pub schema: String,
    pub original_sha256: String,
    pub runtime_identity: OcrRuntimeIdentity,
    pub pages: Vec<OcrPageObservations>,
    pub selected_words: Vec<OcrEvidenceRecord>,
    #[serde(default)]
    pub visual_attempts: Vec<OcrVisualAttempt>,
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
    fn validate_mixed_order(&self, sources: &[OcrObservationReference]) -> Result<(), String> {
        if self.native_regions.is_empty() || self.selection.is_empty() {
            return if self.mixed_order.is_none() {
                Ok(())
            } else {
                Err("unexpected mixed order".into())
            };
        }
        let order = self
            .mixed_order
            .as_ref()
            .ok_or("missing mixed source order")?;
        if order.schema != "ocr-native-anchor-order/v1"
            || self
                .native_regions
                .iter()
                .any(|n| n.source_box >= order.original_box_count)
        {
            return Err("invalid mixed source order".into());
        }
        let auxiliary = |class: &str| {
            matches!(
                class,
                "page-header" | "page-footer" | "footnote" | "caption"
            )
        };
        let mut expected = Vec::new();
        let mut seen = BTreeSet::new();
        let mut body = Vec::new();
        for source in sources {
            if self.excluded_sources.contains(source) {
                let words: Vec<_> = self
                    .box_at(source)?
                    .textlines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .collect();
                let candidates: Vec<_> = self
                    .native_regions
                    .iter()
                    .filter(|region| {
                        !words.is_empty()
                            && words.iter().all(|word| {
                                let x = (word.bbox[0] + word.bbox[2]) / 2.0;
                                let y = (word.bbox[1] + word.bbox[3]) / 2.0;
                                region.bbox[0] <= x
                                    && x <= region.bbox[2]
                                    && region.bbox[1] <= y
                                    && y <= region.bbox[3]
                            })
                    })
                    .collect();
                if candidates.len() != 1 || !seen.insert(candidates[0].source_box) {
                    return Err("nonunique native order anchor".into());
                }
                let native = candidates[0];
                if !auxiliary(&native.source_class) {
                    body.push(native.source_box);
                }
                expected.push(MixedOrderEntry::Native {
                    source_box: native.source_box,
                    projected_box: native.source_box,
                });
            } else {
                let index = self
                    .selection
                    .iter()
                    .position(|entry| &entry.source == source)
                    .ok_or("missing mixed OCR source")?;
                expected.push(MixedOrderEntry::LocalOcr {
                    source: source.clone(),
                    projected_box: order
                        .original_box_count
                        .checked_add(index)
                        .ok_or("mixed box overflow")?,
                });
            }
        }
        let mut required: Vec<_> = self
            .native_regions
            .iter()
            .filter(|n| !auxiliary(&n.source_class))
            .map(|n| n.source_box)
            .collect();
        required.sort_unstable();
        if body != required || expected != order.entries {
            return Err("mixed order missing or inconsistent with original anchors".into());
        }
        Ok(())
    }
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
        if self.schema != "ocr-regional-observations/v3"
            || self.page == 0
            || self.page > max_page
            || !self.complete
            || self.projection_failure.is_some()
            || self.page_bounds[0] != 0.0
            || self.page_bounds[1] != 0.0
            || !within(self.page_bounds, self.page_bounds)
        {
            return Err("invalid/incomplete OCR page observations".into());
        }
        let mut native_ids = BTreeSet::new();
        for native in &self.native_regions {
            if !within(native.bbox, self.page_bounds)
                || native.text.trim().is_empty()
                || native.source_class.is_empty()
                || !native_ids.insert(native.source_box)
            {
                return Err("invalid native exclusion region".into());
            }
        }
        if let Some(coverage) = &self.native_coverage {
            coverage.validate(self)?;
            if self.blank_raster.is_some() {
                return Err("native reuse cannot claim a blank source raster".into());
            }
            if coverage.uncovered_samples == 0 {
                if !self.attempts.is_empty()
                    || !self.selection.is_empty()
                    || !self.components.is_empty()
                    || !self.excluded_sources.is_empty()
                    || self.mixed_order.is_some()
                {
                    return Err("covered native layer must not claim OCR attempts".into());
                }
                return Ok(vec![]);
            }
        }
        if let Some(blank) = &self.blank_raster {
            blank.validate()?;
            if f64::from(blank.width) != (self.page_bounds[2] * 300.0 / 72.0).ceil()
                || f64::from(blank.height) != (self.page_bounds[3] * 300.0 / 72.0).ceil()
            {
                return Err("blank raster dimensions do not match original page at 300 DPI".into());
            }
            if !self.attempts.is_empty()
                || !self.selection.is_empty()
                || !self.components.is_empty()
                || !self.native_regions.is_empty()
                || !self.excluded_sources.is_empty()
                || self.mixed_order.is_some()
            {
                return Err("blank raster must not claim OCR attempts or selected text".into());
            }
            return Ok(vec![]);
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
        let mut retained = Vec::new();
        let mut excluded = Vec::new();
        let order_sources = expected_sources.clone();
        for source in expected_sources {
            let words: Vec<_> = self
                .box_at(&source)?
                .textlines
                .iter()
                .flat_map(|l| &l.spans)
                .collect();
            let covered = words
                .iter()
                .filter(|w| {
                    let x = (w.bbox[0] + w.bbox[2]) / 2.0;
                    let y = (w.bbox[1] + w.bbox[3]) / 2.0;
                    self.native_regions.iter().any(|r| {
                        r.bbox[0] <= x && x <= r.bbox[2] && r.bbox[1] <= y && y <= r.bbox[3]
                    })
                })
                .count();
            if covered == words.len() {
                excluded.push(source);
            } else if covered == 0 {
                retained.push(source);
            } else {
                return Err("unresolved partial native overlap".into());
            }
        }
        if excluded != self.excluded_sources {
            return Err("native exclusion references mismatch".into());
        }
        if self.selection.len() != retained.len() {
            return Err("incomplete OCR selection".into());
        }
        self.validate_mixed_order(&order_sources)?;
        let mut selected_words = Vec::new();
        for (index, (selection, expected)) in self.selection.iter().zip(retained).enumerate() {
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
        if self.schema != "ocr-evidence/v3" || self.pages.is_empty() {
            return Err("missing/version-mismatched OCR observations".into());
        }
        if !super::ocr::valid_sha256(&self.original_sha256)
            || OcrRuntimeIdentity::build_with_package(
                self.runtime_identity.config.clone(),
                self.runtime_identity.dependencies.clone(),
                self.runtime_identity.runtime_package.clone(),
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
        let mut visual_pages = BTreeSet::new();
        for visual in &self.visual_attempts {
            if visual.schema != "ocr-visual-model-attempt/v1"
                || visual.page == 0
                || visual.page > max_page
                || !visual_pages.insert(visual.page)
                || visual.raster_size.iter().any(|value| *value == 0)
                || !super::ocr::valid_sha256(&visual.raster_sha256)
                || visual.attempts.len() != 1
                || visual.projection.schema != "ocr-visual-projection/v1"
                || visual.projection.page != visual.page
                || visual.projection.raster_size != visual.raster_size
                || !visual.projection.complete
            {
                return Err("invalid OCR visual model evidence".into());
            }
            let mut dependency_names = BTreeSet::new();
            for dependency in &visual.dependencies {
                if !dependency_names.insert(&dependency.name)
                    || dependency.name.is_empty()
                    || !super::ocr::valid_sha256(&dependency.sha256)
                {
                    return Err("invalid OCR visual dependency evidence".into());
                }
            }
            for attempt in &visual.attempts {
                if attempt.postprocess
                    != "pinned draw_threshold only; no layout NMS or gold selection"
                    || attempt.raw_outputs.is_empty()
                {
                    return Err("invalid OCR visual raw attempt".into());
                }
                for value in &attempt.res.boxes {
                    if !value.score.is_finite()
                        || !(0.0..=1.0).contains(&value.score)
                        || value.label.is_empty()
                        || !within(
                            value.coordinate,
                            [
                                0.0,
                                0.0,
                                f64::from(visual.raster_size[0]),
                                f64::from(visual.raster_size[1]),
                            ],
                        )
                    {
                        return Err("invalid OCR visual model box".into());
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn mixed_order_fixture() -> OcrPageObservations {
    let source = |index| serde_json::json!({"page":1,"attempt":"primary","box":index});
    let bbox = |y: u32| [10, y, 20, y + 10];
    let observed = |index: u32, y: u32, text: &str| {
        serde_json::json!({
            "boxclass":"text","bbox":bbox(y),"x0":10,"y0":y,"x1":20,"y1":y+10,
            "ocr_block":index+1,"ocr_paragraph":1,
            "textlines":[{"text":text,"bbox":bbox(y),"spans":[{"text":text,"bbox":bbox(y),
                "flags":0,"ocr_block":index+1,"ocr_paragraph":1,"ocr_line":1,"confidence":90}]}]
        })
    };
    serde_json::from_value(serde_json::json!({
        "schema":"ocr-regional-observations/v3","page":1,"page_bounds":[0,0,100,100],"complete":true,
        "attempts":[{"id":"primary","psm":3,"boxes":[observed(0,10,"A"),observed(1,30,"B")]}],
        "components":[],"native_regions":[{"source_box":0,"source_class":"text","bbox":bbox(30),"text":"Native B"}],
        "excluded_sources":[source(1)],
        "selection":[{"selected_box":0,"source":source(0),"words":[{"page":1,"attempt":"primary","box":0,"line":0,"word":0}]}],
        "mixed_order":{"schema":"ocr-native-anchor-order/v1","original_box_count":1,"entries":[
            {"origin":"local_ocr","source":source(0),"projected_box":1},
            {"origin":"native","source_box":0,"projected_box":0}]}
    })).unwrap()
}

#[cfg(test)]
mod blank_tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn mixed_order_replays_raw_observations_and_rejects_changed_or_missing_anchors() {
        let page = mixed_order_fixture();
        assert_eq!(page.validate(1).unwrap().len(), 1);
        let mut changed = page.clone();
        changed.mixed_order.as_mut().unwrap().entries.swap(0, 1);
        assert!(changed.validate(1).is_err());
        changed = page.clone();
        changed.mixed_order = None;
        assert!(changed.validate(1).is_err());
        changed = page.clone();
        changed.native_regions[0].bbox = [30., 30., 40., 40.];
        assert!(changed.validate(1).is_err());
        changed = page;
        changed.mixed_order.as_mut().unwrap().original_box_count = usize::MAX;
        assert!(changed.validate(1).is_err());
    }

    #[test]
    fn native_coverage_binds_masks_transform_and_exact_white_digest_before_reuse() {
        let value = serde_json::json!({
            "schema":"ocr-regional-observations/v3", "page":1,"page_bounds":[0,0,0.96,0.96],
            "complete":true,"attempts":[],"selection":[],"components":[],
            "native_regions":[{"source_box":7,"source_class":"text","bbox":[0,0,0.96,0.96],"text":"X"}],
            "native_coverage":{"schema":"ocr-native-raster-coverage/v1","width":4,"height":4,
                "padding_pixels":2,"source_samples_sha256":format!("{:x}",Sha256::digest([0_u8;48])),
                "masked_samples_sha256":format!("{:x}",Sha256::digest([255_u8;48])),"uncovered_samples":0,
                "masks":[{"source_box":7,"text":"X","bbox":[0.24,0.24,0.48,0.48],"pixel_bbox":[0,0,4,4]}]}
        });
        let decode = |value| serde_json::from_value::<OcrPageObservations>(value).unwrap();
        assert!(decode(value.clone()).validate(1).unwrap().is_empty());
        for (pointer, replacement) in [
            (
                "/native_coverage/masked_samples_sha256",
                serde_json::json!("a".repeat(64)),
            ),
            (
                "/native_coverage/source_samples_sha256",
                serde_json::json!("unknown"),
            ),
            ("/native_coverage/padding_pixels", serde_json::json!(3)),
            ("/native_coverage/uncovered_samples", serde_json::json!(1)),
            ("/native_coverage/masks/0/source_box", serde_json::json!(8)),
            ("/native_coverage/masks/0/text", serde_json::json!("Y")),
            (
                "/native_coverage/masks/0/pixel_bbox",
                serde_json::json!([1, 0, 4, 4]),
            ),
        ] {
            let mut altered = value.clone();
            *altered.pointer_mut(pointer).unwrap() = replacement;
            assert!(decode(altered).validate(1).is_err(), "{pointer}");
        }
        let mut invalid = value;
        invalid["native_coverage"]["uncovered_samples"] = serde_json::json!(-1);
        assert!(serde_json::from_value::<OcrPageObservations>(invalid).is_err());
    }

    #[test]
    fn blank_pixels_require_exact_white_digest_and_no_claimed_recognition() {
        let mut observation: OcrPageObservations = serde_json::from_value(serde_json::json!({
            "schema":"ocr-regional-observations/v3", "page":1, "page_bounds":[0,0,0.24,0.24],
            "complete":true,"attempts":[],"selection":[],"components":[],
            "blank_raster":{"width":1,"height":1,"channels":3,"white_samples":3,"glyph_spans":0,
                "samples_sha256":format!("{:x}", Sha256::digest([255_u8;3]))}
        }))
        .unwrap();
        assert!(observation.validate(1).unwrap().is_empty());
        observation.blank_raster.as_mut().unwrap().samples_sha256 =
            format!("{:x}", Sha256::digest([255_u8, 254, 255]));
        assert!(observation.validate(1).unwrap_err().contains("digest"));
        observation.blank_raster.as_mut().unwrap().samples_sha256 =
            format!("{:x}", Sha256::digest([255_u8; 3]));
        observation.attempts.push(OcrRawAttempt {
            id: OcrAttemptId::Primary,
            psm: 3,
            boxes: vec![],
        });
        assert!(
            observation
                .validate(1)
                .unwrap_err()
                .contains("must not claim")
        );
    }
}
