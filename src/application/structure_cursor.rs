use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::application::ports::ApplicationError;

pub(crate) const STRUCTURE_CURSOR_SCHEMA_VERSION: &str = "structure-cursor/v1";
pub(crate) const STRUCTURE_TRAVERSAL_VERSION: &str = "structure-preorder/v1";
const STRUCTURE_CURSOR_PREFIX: &str = "sc1.";
const STRUCTURE_CURSOR_CHECKSUM_DOMAIN: &[u8] = b"reading-mcp/structure-cursor-checksum/v1\0";
const MAX_STRUCTURE_CURSOR_CHARS: usize = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct StructureCursorClaims {
    pub schema_version: String,
    pub traversal_version: String,
    pub document_id: String,
    pub content_hash: String,
    pub normalized_document_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_section_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_max_depth: Option<u8>,
    pub next_index: usize,
    pub total_nodes: usize,
}

impl StructureCursorClaims {
    pub(crate) fn new(
        document_id: String,
        content_hash: String,
        normalized_document_hash: String,
        root_section_id: Option<String>,
        effective_max_depth: Option<u8>,
        next_index: usize,
        total_nodes: usize,
    ) -> Self {
        Self {
            schema_version: STRUCTURE_CURSOR_SCHEMA_VERSION.into(),
            traversal_version: STRUCTURE_TRAVERSAL_VERSION.into(),
            document_id,
            content_hash,
            normalized_document_hash,
            root_section_id,
            effective_max_depth,
            next_index,
            total_nodes,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StructureCursorEnvelope {
    claims: StructureCursorClaims,
    checksum: String,
}

pub(crate) fn encode_structure_cursor(
    claims: StructureCursorClaims,
) -> Result<String, ApplicationError> {
    validate_cursor_claims(&claims).map_err(|message| {
        ApplicationError::CursorEncodingFailed(format!(
            "structure cursor claims are impossible: {message}"
        ))
    })?;
    let claims_bytes = serde_json::to_vec(&claims).map_err(|error| {
        ApplicationError::CursorEncodingFailed(format!(
            "failed to serialize structure cursor claims: {error}"
        ))
    })?;
    let envelope = StructureCursorEnvelope {
        checksum: cursor_checksum(&claims_bytes),
        claims,
    };
    let envelope_bytes = serde_json::to_vec(&envelope).map_err(|error| {
        ApplicationError::CursorEncodingFailed(format!(
            "failed to serialize structure cursor envelope: {error}"
        ))
    })?;
    let encoded = format!("{STRUCTURE_CURSOR_PREFIX}{}", encode_hex(&envelope_bytes));
    if encoded.len() > MAX_STRUCTURE_CURSOR_CHARS {
        return Err(ApplicationError::CursorEncodingFailed(
            "structure cursor exceeds the maximum encoded size".into(),
        ));
    }
    Ok(encoded)
}

pub(crate) fn decode_structure_cursor(
    cursor: &str,
) -> Result<StructureCursorClaims, ApplicationError> {
    if cursor.len() > MAX_STRUCTURE_CURSOR_CHARS {
        return Err(ApplicationError::InvalidCursor(
            "structure cursor exceeds the maximum encoded size".into(),
        ));
    }
    let encoded = cursor
        .strip_prefix(STRUCTURE_CURSOR_PREFIX)
        .ok_or_else(|| {
            ApplicationError::InvalidCursor("structure cursor prefix is invalid".into())
        })?;
    let envelope_bytes = decode_hex(encoded)?;
    let envelope: StructureCursorEnvelope =
        serde_json::from_slice(&envelope_bytes).map_err(|error| {
            ApplicationError::InvalidCursor(format!("structure cursor payload is invalid: {error}"))
        })?;
    let claims_bytes = serde_json::to_vec(&envelope.claims).map_err(|error| {
        ApplicationError::InvalidCursor(format!(
            "structure cursor claims cannot be validated: {error}"
        ))
    })?;
    if envelope.checksum != cursor_checksum(&claims_bytes) {
        return Err(ApplicationError::InvalidCursor(
            "structure cursor checksum does not match its claims".into(),
        ));
    }
    if envelope.claims.schema_version != STRUCTURE_CURSOR_SCHEMA_VERSION {
        return Err(ApplicationError::StaleCursor(format!(
            "unsupported structure cursor schema {}; expected {STRUCTURE_CURSOR_SCHEMA_VERSION}",
            envelope.claims.schema_version
        )));
    }
    if envelope.claims.traversal_version != STRUCTURE_TRAVERSAL_VERSION {
        return Err(ApplicationError::StaleCursor(format!(
            "structure traversal version {} is incompatible with {STRUCTURE_TRAVERSAL_VERSION}",
            envelope.claims.traversal_version
        )));
    }
    validate_cursor_claims(&envelope.claims)
        .map_err(|message| ApplicationError::InvalidCursor(message.to_string()))?;
    Ok(envelope.claims)
}

fn validate_cursor_claims(claims: &StructureCursorClaims) -> Result<(), &'static str> {
    if claims.total_nodes == 0 {
        return Err("structure cursor cannot target an empty stream");
    }
    if claims.next_index >= claims.total_nodes {
        return Err("structure cursor position must be before the end of the stream");
    }
    Ok(())
}

fn cursor_checksum(payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(STRUCTURE_CURSOR_CHECKSUM_DOMAIN);
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
            "structure cursor hex payload has an odd length".into(),
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
            "structure cursor contains a non-hex character".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{StructureCursorClaims, decode_structure_cursor, encode_structure_cursor};
    use crate::application::ports::ApplicationError;

    fn claims() -> StructureCursorClaims {
        StructureCursorClaims::new(
            "doc:1".into(),
            "sha256:raw".into(),
            "sha256:normalized".into(),
            Some("section://root".into()),
            Some(3),
            4,
            10,
        )
    }

    #[test]
    fn cursor_round_trip_preserves_structure_scope() {
        let expected = claims();
        let encoded = encode_structure_cursor(expected.clone()).expect("cursor should encode");
        assert_eq!(
            decode_structure_cursor(&encoded).expect("cursor should decode"),
            expected
        );
    }

    #[test]
    fn cursor_tampering_is_rejected() {
        let mut encoded = encode_structure_cursor(claims()).expect("cursor should encode");
        let last = encoded.pop().expect("cursor should not be empty");
        encoded.push(if last == '0' { '1' } else { '0' });
        let error = decode_structure_cursor(&encoded).expect_err("tampering must fail");
        assert!(matches!(error, ApplicationError::InvalidCursor(_)));
    }

    #[test]
    fn impossible_terminal_cursor_is_rejected() {
        let mut impossible = claims();
        impossible.next_index = impossible.total_nodes;
        let error =
            encode_structure_cursor(impossible).expect_err("terminal cursor must not be encoded");
        assert!(matches!(error, ApplicationError::CursorEncodingFailed(_)));
    }
}
