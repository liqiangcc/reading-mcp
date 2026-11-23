#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestedTextUnitKind {
    Paragraph,
    Sentence,
}

impl RequestedTextUnitKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
            Self::Sentence => "sentence",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextUnitDirection {
    Forward,
    Backward,
}

impl TextUnitDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Backward => "backward",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextUnitCoveragePolicy {
    PreserveSource,
    EligibleOnly,
}

impl TextUnitCoveragePolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreserveSource => "preserve_source",
            Self::EligibleOnly => "eligible_only",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectiveTextUnitKind {
    Paragraph,
    Sentence,
}

impl EffectiveTextUnitKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
            Self::Sentence => "sentence",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextUnitContentClass {
    Unknown,
    NonProse,
}

impl TextUnitContentClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::NonProse => "non_prose",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextUnitEnumerationCoverage {
    pub owner_chars: usize,
    pub section_separator_chars: usize,
    pub sentence_separator_chars: usize,
    pub paragraph_count: usize,
    pub sentence_eligible_paragraphs: usize,
    pub non_prose_paragraphs: usize,
    pub coarse_structural_paragraphs: usize,
    pub represented_paragraphs: usize,
    pub represented_sentences: usize,
    pub coarse_non_prose_items: usize,
    pub coarse_structural_items: usize,
    pub intentionally_skipped: usize,
    pub unsupported_gaps: usize,
    pub source_complete: bool,
}
