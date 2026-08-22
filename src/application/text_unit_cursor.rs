use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::application::ports::ApplicationError;

pub(crate) const TEXT_UNIT_CURSOR_SCHEMA_VERSION: &str = "text-unit-cursor/v1";
const TEXT_UNIT_CURSOR_PREFIX: &str = "tuc1.";
const TEXT_UNIT_CURSOR_CHECKSUM_DOMAIN: &[u8] = b"reading-mcp/text-unit-cursor-checksum/v1\0";
const MAX_TEXT_UNIT_CURSOR_CHARS: usize = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct TextUnitCursorClaims {
    pub schema_version: String,
    pub document_id: String,
    pub content_hash: String,
    pub normalized_document_hash: String,
    pub section_id: String,
    pub segmentation_version: String,
    pub requested_kind: String,
    pub direction: String,
    pub coverage_policy: String,
    pub next_index: usize,
    pub total_items: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_anchor_index: Option<usize>,
}

impl TextUnitCursorClaims {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        document_id: String,
        content_hash: String,
        normalized_document_hash: String,
        section_id: String,
        segmentation_version: &str,
        requested_kind: &str,
        direction: &str,
        coverage_policy: &str,
        next_index: usize,
        total_items: usize,
    ) -> Self {
        Self {
            schema_version: TEXT_UNIT_CURSOR_SCHEMA_VERSION.into(),
            document_id,
            content_hash,
            normalized_document_hash,
            section_id,
            segmentation_version: segmentation_version.into(),
            requested_kind: requested_kind.into(),
            direction: direction.into(),
            coverage_policy: coverage_policy.into(),
            next_index,
            total_items,
            origin_anchor_index: None,
        }
    }

    pub(crate) fn with_origin_anchor_index(mut self, origin_anchor_index: Option<usize>) -> Self {
        self.origin_anchor_index = origin_anchor_index;
        self
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct TextUnitCursorEnvelope {
    claims: TextUnitCursorClaims,
    checksum: String,
}

pub(crate) fn encode_text_unit_cursor(
    claims: TextUnitCursorClaims,
) -> Result<String, ApplicationError> {
    let claims_bytes = serde_json::to_vec(&claims).map_err(|error| {
        ApplicationError::CursorEncodingFailed(format!(
            "failed to serialize text-unit cursor claims: {error}"
        ))
    })?;
    let envelope = TextUnitCursorEnvelope {
        checksum: cursor_checksum(&claims_bytes),
        claims,
    };
    let envelope_bytes = serde_json::to_vec(&envelope).map_err(|error| {
        ApplicationError::CursorEncodingFailed(format!(
            "failed to serialize text-unit cursor envelope: {error}"
        ))
    })?;
    let encoded = format!("{TEXT_UNIT_CURSOR_PREFIX}{}", encode_hex(&envelope_bytes));
    if encoded.len() > MAX_TEXT_UNIT_CURSOR_CHARS {
        return Err(ApplicationError::CursorEncodingFailed(
            "text-unit cursor exceeds the maximum encoded size".into(),
        ));
    }
    Ok(encoded)
}

pub(crate) fn decode_text_unit_cursor(
    cursor: &str,
) -> Result<TextUnitCursorClaims, ApplicationError> {
    if cursor.len() > MAX_TEXT_UNIT_CURSOR_CHARS {
        return Err(ApplicationError::InvalidCursor(
            "text-unit cursor exceeds the maximum encoded size".into(),
        ));
    }
    let encoded = cursor
        .strip_prefix(TEXT_UNIT_CURSOR_PREFIX)
        .ok_or_else(|| {
            ApplicationError::InvalidCursor("text-unit cursor prefix is invalid".into())
        })?;
    let envelope_bytes = decode_hex(encoded)?;
    let envelope: TextUnitCursorEnvelope =
        serde_json::from_slice(&envelope_bytes).map_err(|error| {
            ApplicationError::InvalidCursor(format!("text-unit cursor payload is invalid: {error}"))
        })?;
    let claims_bytes = serde_json::to_vec(&envelope.claims).map_err(|error| {
        ApplicationError::InvalidCursor(format!(
            "text-unit cursor claims cannot be validated: {error}"
        ))
    })?;
    if envelope.checksum != cursor_checksum(&claims_bytes) {
        return Err(ApplicationError::InvalidCursor(
            "text-unit cursor checksum does not match its claims".into(),
        ));
    }
    if envelope.claims.schema_version != TEXT_UNIT_CURSOR_SCHEMA_VERSION {
        return Err(ApplicationError::StaleCursor(format!(
            "unsupported text-unit cursor schema {}; expected {TEXT_UNIT_CURSOR_SCHEMA_VERSION}",
            envelope.claims.schema_version
        )));
    }
    Ok(envelope.claims)
}

fn cursor_checksum(payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(TEXT_UNIT_CURSOR_CHECKSUM_DOMAIN);
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
            "text-unit cursor hex payload has an odd length".into(),
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
            "text-unit cursor contains a non-hex character".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{TextUnitCursorClaims, decode_text_unit_cursor, encode_text_unit_cursor};
    use crate::application::ports::ApplicationError;

    fn claims() -> TextUnitCursorClaims {
        TextUnitCursorClaims::new(
            "doc:1".into(),
            "sha256:raw".into(),
            "sha256:normalized".into(),
            "section://root".into(),
            "text-segmentation/v1",
            "sentence",
            "forward",
            "preserve_source",
            3,
            10,
        )
    }

    #[test]
    fn cursor_round_trip_preserves_stream_bindings() {
        let expected = claims();
        let encoded = encode_text_unit_cursor(expected.clone()).expect("cursor should encode");
        assert_eq!(
            decode_text_unit_cursor(&encoded).expect("cursor should decode"),
            expected
        );
    }

    #[test]
    fn anchored_cursor_round_trip_preserves_origin_without_bumping_schema() {
        let expected = claims().with_origin_anchor_index(Some(4));
        let encoded = encode_text_unit_cursor(expected.clone()).expect("cursor should encode");
        assert_eq!(
            decode_text_unit_cursor(&encoded).expect("cursor should decode"),
            expected
        );
    }

    #[test]
    fn cursor_tampering_is_rejected() {
        let mut encoded = encode_text_unit_cursor(claims()).expect("cursor should encode");
        let last = encoded.pop().expect("cursor should not be empty");
        encoded.push(if last == '0' { '1' } else { '0' });
        let error = decode_text_unit_cursor(&encoded).expect_err("tampering must fail");
        assert!(matches!(error, ApplicationError::InvalidCursor(_)));
    }
}
