use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::application::ports::ApplicationError;

pub(crate) const DISCOVERY_CURSOR_SCHEMA_VERSION: &str = "discovery-cursor/v1";
pub(crate) const DISCOVERY_ORDERING_VERSION: &str = "discovery-path/v1";
const DISCOVERY_CURSOR_PREFIX: &str = "dc1.";
const DISCOVERY_CURSOR_CHECKSUM_DOMAIN: &[u8] = b"reading-mcp/discovery-cursor-checksum/v1\0";
const MAX_DISCOVERY_CURSOR_CHARS: usize = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DiscoveryCursorClaims {
    pub schema_version: String,
    pub ordering_version: String,
    pub allowed_roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_path: Option<String>,
    pub recursive: bool,
    pub candidate_manifest_hash: String,
    pub total_candidates: usize,
    pub next_index: usize,
}

impl DiscoveryCursorClaims {
    pub(crate) fn new(
        allowed_roots: Vec<String>,
        requested_path: Option<String>,
        recursive: bool,
        candidate_manifest_hash: String,
        total_candidates: usize,
        next_index: usize,
    ) -> Self {
        Self {
            schema_version: DISCOVERY_CURSOR_SCHEMA_VERSION.into(),
            ordering_version: DISCOVERY_ORDERING_VERSION.into(),
            allowed_roots,
            requested_path,
            recursive,
            candidate_manifest_hash,
            total_candidates,
            next_index,
        }
    }
}

pub(crate) fn encode_discovery_cursor(
    claims: DiscoveryCursorClaims,
) -> Result<String, ApplicationError> {
    validate_claims(&claims).map_err(|message| {
        ApplicationError::CursorEncodingFailed(format!(
            "discovery cursor claims are impossible: {message}"
        ))
    })?;
    let claims_bytes = serde_json::to_vec(&claims).map_err(|error| {
        ApplicationError::CursorEncodingFailed(format!(
            "failed to serialize discovery cursor claims: {error}"
        ))
    })?;
    let envelope = CursorEnvelope {
        checksum: checksum(&claims_bytes),
        claims,
    };
    let envelope_bytes = serde_json::to_vec(&envelope).map_err(|error| {
        ApplicationError::CursorEncodingFailed(format!(
            "failed to serialize discovery cursor envelope: {error}"
        ))
    })?;
    let encoded = format!("{DISCOVERY_CURSOR_PREFIX}{}", encode_hex(&envelope_bytes));
    if encoded.len() > MAX_DISCOVERY_CURSOR_CHARS {
        return Err(ApplicationError::CursorEncodingFailed(
            "discovery cursor exceeds the maximum encoded size".into(),
        ));
    }
    Ok(encoded)
}

pub(crate) fn decode_discovery_cursor(
    cursor: &str,
) -> Result<DiscoveryCursorClaims, ApplicationError> {
    if cursor.len() > MAX_DISCOVERY_CURSOR_CHARS {
        return Err(ApplicationError::InvalidCursor(
            "discovery cursor exceeds the maximum encoded size".into(),
        ));
    }
    let encoded = cursor
        .strip_prefix(DISCOVERY_CURSOR_PREFIX)
        .ok_or_else(|| {
            ApplicationError::InvalidCursor("discovery cursor prefix is invalid".into())
        })?;
    let envelope_bytes = decode_hex(encoded)?;
    let envelope: CursorEnvelope = serde_json::from_slice(&envelope_bytes).map_err(|error| {
        ApplicationError::InvalidCursor(format!("discovery cursor payload is invalid: {error}"))
    })?;
    let claims_bytes = serde_json::to_vec(&envelope.claims).map_err(|error| {
        ApplicationError::InvalidCursor(format!(
            "discovery cursor claims cannot be validated: {error}"
        ))
    })?;
    if envelope.checksum != checksum(&claims_bytes) {
        return Err(ApplicationError::InvalidCursor(
            "discovery cursor checksum does not match its claims".into(),
        ));
    }
    if envelope.claims.schema_version != DISCOVERY_CURSOR_SCHEMA_VERSION {
        return Err(ApplicationError::StaleCursor(format!(
            "unsupported discovery cursor schema {}; expected {DISCOVERY_CURSOR_SCHEMA_VERSION}",
            envelope.claims.schema_version
        )));
    }
    if envelope.claims.ordering_version != DISCOVERY_ORDERING_VERSION {
        return Err(ApplicationError::StaleCursor(format!(
            "discovery ordering version {} is incompatible with {DISCOVERY_ORDERING_VERSION}",
            envelope.claims.ordering_version
        )));
    }
    validate_claims(&envelope.claims)
        .map_err(|message| ApplicationError::InvalidCursor(message.to_string()))?;
    Ok(envelope.claims)
}

#[derive(Debug, Serialize, Deserialize)]
struct CursorEnvelope {
    claims: DiscoveryCursorClaims,
    checksum: String,
}

fn validate_claims(claims: &DiscoveryCursorClaims) -> Result<(), &'static str> {
    if claims.total_candidates == 0 {
        return Err("discovery cursor cannot target an empty stream");
    }
    if claims.next_index == 0 || claims.next_index >= claims.total_candidates {
        return Err("discovery cursor position must be between the first item and stream end");
    }
    Ok(())
}

pub(crate) fn manifest_hash<T: Serialize>(candidates: &T) -> Result<String, ApplicationError> {
    let bytes = serde_json::to_vec(candidates).map_err(|error| {
        ApplicationError::CursorEncodingFailed(format!(
            "failed to serialize discovery candidate manifest: {error}"
        ))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(b"reading-mcp/discovery-manifest/v1\0");
    hasher.update(bytes);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn checksum(payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(DISCOVERY_CURSOR_CHECKSUM_DOMAIN);
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
            "discovery cursor hex payload has an odd length".into(),
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
            "discovery cursor contains a non-hex character".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{DiscoveryCursorClaims, decode_discovery_cursor, encode_discovery_cursor};
    use crate::application::ports::ApplicationError;

    fn claims() -> DiscoveryCursorClaims {
        DiscoveryCursorClaims::new(
            vec!["/docs".into()],
            Some("/docs/book".into()),
            true,
            "sha256:manifest".into(),
            10,
            4,
        )
    }

    #[test]
    fn cursor_round_trip_preserves_scope_and_position() {
        let expected = claims();
        let encoded = encode_discovery_cursor(expected.clone()).expect("cursor should encode");
        assert_eq!(
            decode_discovery_cursor(&encoded).expect("cursor should decode"),
            expected
        );
    }

    #[test]
    fn cursor_tampering_is_rejected() {
        let mut encoded = encode_discovery_cursor(claims()).expect("cursor should encode");
        let last = encoded.pop().expect("cursor should not be empty");
        encoded.push(if last == '0' { '1' } else { '0' });
        let error = decode_discovery_cursor(&encoded).expect_err("tampering must fail");
        assert!(matches!(error, ApplicationError::InvalidCursor(_)));
    }

    #[test]
    fn terminal_cursor_is_rejected() {
        let mut impossible = claims();
        impossible.next_index = impossible.total_candidates;
        let error =
            encode_discovery_cursor(impossible).expect_err("terminal cursor must not be encoded");
        assert!(matches!(error, ApplicationError::CursorEncodingFailed(_)));
    }
}
