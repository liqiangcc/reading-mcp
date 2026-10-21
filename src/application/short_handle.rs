//! The v2 short reading handles: `d3~9f2c.s12.p4.2` and the backward `b:` cursor.
//!
//! A handle names one document, section, paragraph or sentence in a handful of bytes. The
//! document part carries the `text_unit_documents_v2.doc_key` integer and a four hex digit
//! checksum of the document identity, so a handle that outlived its document fails as
//! `stale_handle` instead of silently reading a different book.
//!
//! Parsing is strict: anything that does not match the grammar exactly is rejected.

use sha2::{Digest, Sha256};

use crate::domain::{DocumentId, NormalizedDocumentHash, TEXT_SEGMENTATION_VERSION};

/// The number of lowercase hex digits of the document identity checksum.
pub const HANDLE_CHECKSUM_DIGITS: usize = 4;

/// The document prefix of every handle: `d<doc_key>~<checksum>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentHandle {
    pub doc_key: i64,
    pub checksum: String,
}

/// A section handle: `<document>.s<section_key>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionHandle {
    pub document: DocumentHandle,
    pub section_key: i64,
}

/// One parsed reading handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadingHandle {
    Document(DocumentHandle),
    Section(SectionHandle),
    Paragraph {
        section: SectionHandle,
        paragraph_index: usize,
    },
    Sentence {
        section: SectionHandle,
        paragraph_index: usize,
        sentence_index: usize,
    },
}

impl ReadingHandle {
    pub fn document(&self) -> &DocumentHandle {
        match self {
            Self::Document(document) => document,
            Self::Section(section)
            | Self::Paragraph { section, .. }
            | Self::Sentence { section, .. } => &section.document,
        }
    }
}

/// The unit a cursor walks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorUnit {
    Sentence,
    Paragraph,
}

impl CursorUnit {
    pub const fn letter(self) -> char {
        match self {
            Self::Sentence => 'S',
            Self::Paragraph => 'P',
        }
    }
}

/// A backward continuation cursor: `b:<section handle>.<unit>.<position>`.
///
/// The direction travels with the cursor, so a backward walk needs nothing else. Forward reading
/// needs no cursor at all: `next` is the next item's handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadingCursor {
    pub section: SectionHandle,
    pub unit: CursorUnit,
    /// The exclusive end of the stream range the next backward step reads.
    pub position: usize,
}

/// What `from` named: a handle or a cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadingFrom {
    Handle(ReadingHandle),
    Cursor(ReadingCursor),
}

/// Why a handle or cursor could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandleParseError {
    pub message: String,
}

impl HandleParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// The four lowercase hex digits that bind a handle to one document identity and segmentation.
pub fn handle_checksum(
    document_id: &DocumentId,
    normalized_document_hash: &NormalizedDocumentHash,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(document_id.0.as_bytes());
    hasher.update(b"|");
    hasher.update(normalized_document_hash.0.as_bytes());
    hasher.update(b"|");
    hasher.update(TEXT_SEGMENTATION_VERSION.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    hex[..HANDLE_CHECKSUM_DIGITS].to_owned()
}

impl std::fmt::Display for ReadingHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Document(document) => std::fmt::Display::fmt(document, formatter),
            Self::Section(section) => std::fmt::Display::fmt(section, formatter),
            Self::Paragraph {
                section,
                paragraph_index,
            } => formatter.write_str(&paragraph_handle(section, *paragraph_index)),
            Self::Sentence {
                section,
                paragraph_index,
                sentence_index,
            } => formatter.write_str(&sentence_handle(section, *paragraph_index, *sentence_index)),
        }
    }
}

impl std::fmt::Display for DocumentHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "d{}~{}", self.doc_key, self.checksum)
    }
}

impl std::fmt::Display for SectionHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.s{}", self.document, self.section_key)
    }
}

/// The paragraph handle of one paragraph of `section`.
pub fn paragraph_handle(section: &SectionHandle, paragraph_index: usize) -> String {
    format!("{section}.p{paragraph_index}")
}

/// The sentence handle of one sentence of `section`.
pub fn sentence_handle(
    section: &SectionHandle,
    paragraph_index: usize,
    sentence_index: usize,
) -> String {
    format!("{section}.p{paragraph_index}.{sentence_index}")
}

impl std::fmt::Display for ReadingCursor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "b:{}.{}.{}",
            self.section,
            self.unit.letter(),
            self.position
        )
    }
}

/// Parses `from`: a `b:` cursor, or a handle.
pub fn parse_from(value: &str) -> Result<ReadingFrom, HandleParseError> {
    if let Some(rest) = value.strip_prefix("b:") {
        return parse_cursor(rest).map(ReadingFrom::Cursor);
    }
    parse_handle(value).map(ReadingFrom::Handle)
}

fn parse_cursor(value: &str) -> Result<ReadingCursor, HandleParseError> {
    // `<section handle>.<S|P>.<position>`; the position is the only unbounded numeric tail, so split
    // it off first and parse the rest as a section handle.
    let (head, position) = value
        .rsplit_once('.')
        .ok_or_else(|| HandleParseError::new("cursor is missing its stream position"))?;
    let position = parse_number(position, "cursor position")?;
    let (section, unit) = head
        .rsplit_once('.')
        .ok_or_else(|| HandleParseError::new("cursor is missing its unit"))?;
    let unit = match unit {
        "S" => CursorUnit::Sentence,
        "P" => CursorUnit::Paragraph,
        other => {
            return Err(HandleParseError::new(format!(
                "cursor unit must be S or P, got {other:?}"
            )));
        }
    };
    let handle = parse_handle(section)?;
    let section = match handle {
        ReadingHandle::Section(section) => section,
        _ => {
            return Err(HandleParseError::new(
                "cursor must name a section handle, not a document, paragraph or sentence",
            ));
        }
    };
    Ok(ReadingCursor {
        section,
        unit,
        position,
    })
}

/// Parses a handle, strictly: `d<key>~<4 hex>[.s<key>[.p<index>[.<index>]]]`.
pub fn parse_handle(value: &str) -> Result<ReadingHandle, HandleParseError> {
    let mut parts = value.split('.');
    let document_part = parts
        .next()
        .ok_or_else(|| HandleParseError::new("handle is empty"))?;
    let document = parse_document(document_part)?;
    let mut handle = ReadingHandle::Document(document);

    let Some(section_part) = parts.next() else {
        return Ok(handle);
    };
    let section_key = section_part
        .strip_prefix('s')
        .ok_or_else(|| HandleParseError::new("section part must start with 's'"))?;
    let section_key = parse_number(section_key, "section key")?;
    let section = SectionHandle {
        document: handle.document().clone(),
        section_key: section_key as i64,
    };
    handle = ReadingHandle::Section(section.clone());

    let Some(paragraph_part) = parts.next() else {
        return Ok(handle);
    };
    let paragraph_index = paragraph_part
        .strip_prefix('p')
        .ok_or_else(|| HandleParseError::new("paragraph part must start with 'p'"))?;
    let paragraph_index = parse_number(paragraph_index, "paragraph index")?;
    if paragraph_index == 0 {
        return Err(HandleParseError::new("paragraph index must be at least 1"));
    }
    handle = ReadingHandle::Paragraph {
        section: section.clone(),
        paragraph_index,
    };

    let Some(sentence_part) = parts.next() else {
        return Ok(handle);
    };
    let sentence_index = parse_number(sentence_part, "sentence index")?;
    if sentence_index == 0 {
        return Err(HandleParseError::new("sentence index must be at least 1"));
    }
    if parts.next().is_some() {
        return Err(HandleParseError::new(
            "handle has more parts than a sentence handle allows",
        ));
    }
    Ok(ReadingHandle::Sentence {
        section,
        paragraph_index,
        sentence_index,
    })
}

fn parse_document(value: &str) -> Result<DocumentHandle, HandleParseError> {
    let rest = value
        .strip_prefix('d')
        .ok_or_else(|| HandleParseError::new("handle must start with 'd'"))?;
    let (doc_key, checksum) = rest
        .split_once('~')
        .ok_or_else(|| HandleParseError::new("document handle is missing its '~' checksum"))?;
    let doc_key = parse_number(doc_key, "document key")?;
    if doc_key == 0 {
        return Err(HandleParseError::new("document key must be at least 1"));
    }
    if checksum.len() != HANDLE_CHECKSUM_DIGITS
        || !checksum
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(HandleParseError::new(format!(
            "checksum must be {HANDLE_CHECKSUM_DIGITS} lowercase hex digits"
        )));
    }
    Ok(DocumentHandle {
        doc_key: doc_key as i64,
        checksum: checksum.to_owned(),
    })
}

fn parse_number(value: &str, what: &str) -> Result<usize, HandleParseError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(HandleParseError::new(format!(
            "{what} must be a decimal integer, got {value:?}"
        )));
    }
    value
        .parse::<usize>()
        .map_err(|_| HandleParseError::new(format!("{what} {value} is out of range")))
}

#[cfg(test)]
mod tests {
    use super::{
        CursorUnit, DocumentHandle, ReadingCursor, ReadingFrom, ReadingHandle, SectionHandle,
        handle_checksum, paragraph_handle, parse_from, parse_handle, sentence_handle,
    };
    use crate::domain::{DocumentId, NormalizedDocumentHash};

    fn document() -> DocumentHandle {
        DocumentHandle {
            doc_key: 3,
            checksum: "9f2c".into(),
        }
    }

    fn section() -> SectionHandle {
        SectionHandle {
            document: document(),
            section_key: 12,
        }
    }

    #[test]
    fn handles_round_trip_at_every_depth() {
        let section = section();
        let cases = [
            (document().to_string(), ReadingHandle::Document(document())),
            (section.to_string(), ReadingHandle::Section(section.clone())),
            (
                paragraph_handle(&section, 4),
                ReadingHandle::Paragraph {
                    section: section.clone(),
                    paragraph_index: 4,
                },
            ),
            (
                sentence_handle(&section, 4, 2),
                ReadingHandle::Sentence {
                    section: section.clone(),
                    paragraph_index: 4,
                    sentence_index: 2,
                },
            ),
        ];
        for (text, expected) in cases {
            let parsed = parse_handle(&text).expect("the generated handle must parse");
            assert_eq!(parsed, expected, "round trip failed for {text}");
            assert_eq!(parsed.to_string(), text);
        }
    }

    #[test]
    fn backward_cursors_round_trip_in_both_units() {
        for unit in [CursorUnit::Sentence, CursorUnit::Paragraph] {
            for position in [0usize, 1, 17, 12345] {
                let cursor = ReadingCursor {
                    section: section(),
                    unit,
                    position,
                };
                let text = cursor.to_string();
                assert_eq!(
                    parse_from(&text).expect("the generated cursor must parse"),
                    ReadingFrom::Cursor(cursor.clone()),
                    "round trip failed for {text}"
                );
            }
        }
        assert_eq!(
            ReadingCursor {
                section: section(),
                unit: CursorUnit::Sentence,
                position: 1,
            }
            .to_string(),
            "b:d3~9f2c.s12.S.1"
        );
    }

    #[test]
    fn the_forward_cursor_is_gone() {
        for value in ["r:d3~9f2c.s12.S.1", "r:d3~9f2c.s12.P.0"] {
            assert!(
                parse_from(value).is_err(),
                "{value:?} must no longer parse: forward reading follows handles"
            );
        }
    }

    #[test]
    fn malformed_handles_are_rejected() {
        for value in [
            "",
            "d",
            "d3",
            "d3~",
            "d3~9f2",
            "d3~9f2cc",
            "d3~9F2C",
            "d3~9f2g",
            "d0~9f2c",
            "dx~9f2c",
            "d-1~9f2c",
            "x3~9f2c",
            "d3~9f2c.s",
            "d3~9f2c.x12",
            "d3~9f2c.s12.p0",
            "d3~9f2c.s12.p",
            "d3~9f2c.s12.p4.0",
            "d3~9f2c.s12.p4.2.1",
            "d3~9f2c.s12.p4.x",
            "d3~9f2c.s12.4",
            "b:d3~9f2c.s12.S",
            "b:d3~9f2c.s12.X.1",
            "b:d3~9f2c.S.1",
            "b:d3~9f2c.s12.p4.2.S.1",
            "b:d3~9f2c.s12.S.x",
            "b:d3~9f2c.s12.S.-1",
            "c:d3~9f2c.s12.S.1",
        ] {
            assert!(
                parse_from(value).is_err(),
                "{value:?} must not parse as a handle or cursor"
            );
        }
    }

    #[test]
    fn checksum_is_four_lowercase_hex_digits_of_the_document_identity() {
        let checksum = handle_checksum(
            &DocumentId("doc:alpha".into()),
            &NormalizedDocumentHash("sha256:aaaa".into()),
        );
        assert_eq!(checksum.len(), 4);
        assert!(
            checksum
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );

        let other_document = handle_checksum(
            &DocumentId("doc:beta".into()),
            &NormalizedDocumentHash("sha256:aaaa".into()),
        );
        let other_hash = handle_checksum(
            &DocumentId("doc:alpha".into()),
            &NormalizedDocumentHash("sha256:bbbb".into()),
        );
        assert_ne!(checksum, other_document);
        assert_ne!(checksum, other_hash);

        // A checksum is stable across calls: nothing time or address dependent may leak in.
        assert_eq!(
            checksum,
            handle_checksum(
                &DocumentId("doc:alpha".into()),
                &NormalizedDocumentHash("sha256:aaaa".into()),
            )
        );
    }
}
