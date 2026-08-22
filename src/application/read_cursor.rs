use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::application::ports::ApplicationError;

pub(crate) const READ_CURSOR_SCHEMA_VERSION: &str = "read-cursor/v2";
// The prefix versions the envelope encoding. The v2 schema keeps the same
// envelope representation and remains backward-compatible for SectionTree
// cursors. Exact-target cursors add mode-specific optional bindings.
const READ_CURSOR_PREFIX: &str = "rc1.";
const READ_CURSOR_CHECKSUM_DOMAIN: &[u8] = b"reading-mcp/read-cursor-checksum/v1\0";
const MAX_READ_CURSOR_CHARS: usize = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ReadCursorClaims {
    pub schema_version: String,
    pub document_id: String,
    pub content_hash: String,
    pub normalized_document_hash: String,
    pub section_id: String,
    pub read_mode: String,
    pub rendering_version: String,
    pub next_char: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_paragraph_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_sentence_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_range_start: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_range_end: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_segmentation_version: Option<String>,
}

impl ReadCursorClaims {
    pub(crate) fn new(
        document_id: String,
        content_hash: String,
        normalized_document_hash: String,
        section_id: String,
        read_mode: &str,
        rendering_version: &str,
        next_char: usize,
    ) -> Self {
        Self {
            schema_version: READ_CURSOR_SCHEMA_VERSION.into(),
            document_id,
            content_hash,
            normalized_document_hash,
            section_id,
            read_mode: read_mode.into(),
            rendering_version: rendering_version.into(),
            next_char,
            target_kind: None,
            target_paragraph_index: None,
            target_sentence_index: None,
            target_range_start: None,
            target_range_end: None,
            target_segmentation_version: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_exact(
        document_id: String,
        content_hash: String,
        normalized_document_hash: String,
        section_id: String,
        read_mode: &str,
        rendering_version: &str,
        next_char: usize,
        target_kind: &str,
        target_paragraph_index: Option<usize>,
        target_sentence_index: Option<usize>,
        target_range_start: Option<usize>,
        target_range_end: Option<usize>,
        target_segmentation_version: Option<String>,
    ) -> Self {
        Self {
            schema_version: READ_CURSOR_SCHEMA_VERSION.into(),
            document_id,
            content_hash,
            normalized_document_hash,
            section_id,
            read_mode: read_mode.into(),
            rendering_version: rendering_version.into(),
            next_char,
            target_kind: Some(target_kind.into()),
            target_paragraph_index,
            target_sentence_index,
            target_range_start,
            target_range_end,
            target_segmentation_version,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ReadCursorEnvelope {
    claims: ReadCursorClaims,
    checksum: String,
}

pub(crate) fn encode_read_cursor(claims: ReadCursorClaims) -> Result<String, ApplicationError> {
    let claims_bytes = serde_json::to_vec(&claims).map_err(|error| {
        ApplicationError::CursorEncodingFailed(format!(
            "failed to serialize read cursor claims: {error}"
        ))
    })?;
    let envelope = ReadCursorEnvelope {
        checksum: cursor_checksum(&claims_bytes),
        claims,
    };
    let envelope_bytes = serde_json::to_vec(&envelope).map_err(|error| {
        ApplicationError::CursorEncodingFailed(format!(
            "failed to serialize read cursor envelope: {error}"
        ))
    })?;

    let encoded = format!("{READ_CURSOR_PREFIX}{}", encode_hex(&envelope_bytes));
    if encoded.len() > MAX_READ_CURSOR_CHARS {
        return Err(ApplicationError::CursorEncodingFailed(
            "read cursor exceeds the maximum encoded size".into(),
        ));
    }
    Ok(encoded)
}

pub(crate) fn decode_read_cursor(cursor: &str) -> Result<ReadCursorClaims, ApplicationError> {
    if cursor.len() > MAX_READ_CURSOR_CHARS {
        return Err(ApplicationError::InvalidCursor(
            "read cursor exceeds the maximum encoded size".into(),
        ));
    }

    let encoded = cursor
        .strip_prefix(READ_CURSOR_PREFIX)
        .ok_or_else(|| ApplicationError::InvalidCursor("read cursor prefix is invalid".into()))?;
    let envelope_bytes = decode_hex(encoded)?;
    let envelope: ReadCursorEnvelope =
        serde_json::from_slice(&envelope_bytes).map_err(|error| {
            ApplicationError::InvalidCursor(format!("read cursor payload is invalid: {error}"))
        })?;
    let claims_bytes = serde_json::to_vec(&envelope.claims).map_err(|error| {
        ApplicationError::InvalidCursor(format!("read cursor claims cannot be validated: {error}"))
    })?;

    if envelope.checksum != cursor_checksum(&claims_bytes) {
        return Err(ApplicationError::InvalidCursor(
            "read cursor checksum does not match its claims".into(),
        ));
    }
    if envelope.claims.schema_version != READ_CURSOR_SCHEMA_VERSION {
        return Err(ApplicationError::StaleCursor(format!(
            "unsupported cursor schema {}; expected {READ_CURSOR_SCHEMA_VERSION}",
            envelope.claims.schema_version
        )));
    }

    Ok(envelope.claims)
}

fn cursor_checksum(payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(READ_CURSOR_CHECKSUM_DOMAIN);
    hasher.update(payload);
    format!("sha256:{:x}", hasher.finalize())
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn decode_hex(value: &str) -> Result<Vec<u8>, ApplicationError> {
    if !value.len().is_multiple_of(2) {
        return Err(ApplicationError::InvalidCursor(
            "read cursor hex payload has an odd length".into(),
        ));
    }

    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = decode_hex_nibble(pair[0])?;
            let low = decode_hex_nibble(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn decode_hex_nibble(value: u8) -> Result<u8, ApplicationError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(ApplicationError::InvalidCursor(
            "read cursor contains a non-hex character".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{ReadCursorClaims, decode_read_cursor, encode_read_cursor};
    use crate::application::ports::ApplicationError;

    fn claims() -> ReadCursorClaims {
        ReadCursorClaims::new(
            "doc:1".into(),
            "sha256:raw".into(),
            "sha256:normalized".into(),
            "section://root".into(),
            "section_tree",
            "section-tree-markdown/v1",
            42,
        )
    }

    #[test]
    fn cursor_round_trip_preserves_all_bindings() {
        let expected = claims();
        let encoded = encode_read_cursor(expected.clone()).expect("cursor should encode");
        let decoded = decode_read_cursor(&encoded).expect("cursor should decode");
        assert_eq!(decoded, expected);
    }

    #[test]
    fn pre_exact_v2_section_cursor_fixture_remains_valid() {
        // Captured from the pre-exact-target v2 claim shape. Optional exact-target
        // fields must deserialize as None and, because they are omitted during
        // serialization, must not invalidate the historical checksum.
        const PRE_EXACT_V2_CURSOR: &str = "rc1.7b22636c61696d73223a7b22736368656d615f76657273696f6e223a22726561642d637572736f722f7632222c22646f63756d656e745f6964223a22646f633a31222c22636f6e74656e745f68617368223a227368613235363a726177222c226e6f726d616c697a65645f646f63756d656e745f68617368223a227368613235363a6e6f726d616c697a6564222c2273656374696f6e5f6964223a2273656374696f6e3a2f2f726f6f74222c22726561645f6d6f6465223a2273656374696f6e5f74726565222c2272656e646572696e675f76657273696f6e223a2273656374696f6e2d747265652d6d61726b646f776e2f7631222c226e6578745f63686172223a34327d2c22636865636b73756d223a227368613235363a33643266633263646235313961356665376633323038663739353131363432633033333533633635306338353736333736353131393439343264303734353466227d";

        assert_eq!(
            decode_read_cursor(PRE_EXACT_V2_CURSOR).expect("pre-exact v2 cursor should decode"),
            claims()
        );
    }

    #[test]
    fn exact_cursor_round_trip_preserves_target_bindings() {
        let expected = ReadCursorClaims::new_exact(
            "doc:1".into(),
            "sha256:raw".into(),
            "sha256:normalized".into(),
            "section://root".into(),
            "exact_target",
            "exact-normalized-source/v1",
            8,
            "sentence",
            Some(2),
            Some(1),
            Some(10),
            Some(30),
            Some("text-segmentation/v1".into()),
        );
        let encoded = encode_read_cursor(expected.clone()).expect("cursor should encode");
        assert_eq!(
            decode_read_cursor(&encoded).expect("cursor should decode"),
            expected
        );
    }

    #[test]
    fn previous_normalized_hash_cursor_schema_is_explicitly_stale() {
        let mut legacy = claims();
        legacy.schema_version = "read-cursor/v1".into();
        let encoded = encode_read_cursor(legacy).expect("legacy envelope should encode");

        let error = decode_read_cursor(&encoded).expect_err("old cursor schema must be stale");
        assert!(matches!(error, ApplicationError::StaleCursor(_)));
    }

    #[test]
    fn tampered_cursor_is_rejected() {
        let mut encoded = encode_read_cursor(claims()).expect("cursor should encode");
        let last = encoded.pop().expect("cursor should be non-empty");
        encoded.push(if last == '0' { '1' } else { '0' });

        let error = decode_read_cursor(&encoded).expect_err("tampering must fail");
        assert!(matches!(error, ApplicationError::InvalidCursor(_)));
    }

    #[test]
    fn oversized_cursor_is_not_issued() {
        let mut claims = claims();
        claims.section_id = "section://".to_owned() + &"x".repeat(16 * 1024);

        let error = encode_read_cursor(claims).expect_err("oversized cursor must fail");
        assert!(matches!(error, ApplicationError::CursorEncodingFailed(_)));
    }
}
