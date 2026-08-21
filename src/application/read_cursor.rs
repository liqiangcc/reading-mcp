use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::application::ports::ApplicationError;

pub(crate) const READ_CURSOR_SCHEMA_VERSION: &str = "read-cursor/v1";
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

    Ok(format!(
        "{READ_CURSOR_PREFIX}{}",
        encode_hex(&envelope_bytes)
    ))
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
        .chunks_exact(2)
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
    fn cursor_tampering_is_rejected() {
        let mut encoded = encode_read_cursor(claims()).expect("cursor should encode");
        let last = encoded.pop().expect("cursor should not be empty");
        encoded.push(if last == '0' { '1' } else { '0' });

        let error = decode_read_cursor(&encoded).expect_err("tampering must fail");
        assert!(matches!(error, ApplicationError::InvalidCursor(_)));
    }
}
