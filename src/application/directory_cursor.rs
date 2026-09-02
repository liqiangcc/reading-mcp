use serde::{Deserialize, Serialize};

use crate::application::ports::ApplicationError;

pub(crate) const DIRECTORY_CURSOR_SCHEMA_VERSION: &str = "directory-cursor/v1";
pub(crate) const DIRECTORY_ORDERING_VERSION: &str = "directory-path/v1";
const DIRECTORY_CURSOR_PREFIX: &str = "dir1.";
const DIRECTORY_CURSOR_CHECKSUM_DOMAIN: &[u8] = b"reading-mcp/directory-cursor-checksum/v1\0";
const MAX_DIRECTORY_CURSOR_CHARS: usize = 16 * 1024;
const DIRECTORY_MANIFEST_DOMAIN: &[u8] = b"reading-mcp/directory-manifest/v1\0";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DirectoryCursorClaims {
    pub schema_version: String,
    pub ordering_version: String,
    pub allowed_roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_path: Option<String>,
    pub entry_manifest_hash: String,
    pub total_entries: usize,
    pub next_index: usize,
}

impl DirectoryCursorClaims {
    pub(crate) fn new(
        allowed_roots: Vec<String>,
        requested_path: Option<String>,
        entry_manifest_hash: String,
        total_entries: usize,
        next_index: usize,
    ) -> Self {
        Self {
            schema_version: DIRECTORY_CURSOR_SCHEMA_VERSION.into(),
            ordering_version: DIRECTORY_ORDERING_VERSION.into(),
            allowed_roots,
            requested_path,
            entry_manifest_hash,
            total_entries,
            next_index,
        }
    }
}

pub(crate) fn encode_directory_cursor(
    claims: DirectoryCursorClaims,
) -> Result<String, ApplicationError> {
    validate_claims(&claims).map_err(|message| {
        ApplicationError::CursorEncodingFailed(format!(
            "directory cursor claims are impossible: {message}"
        ))
    })?;
    let claims_bytes = serde_json::to_vec(&claims).map_err(|error| {
        ApplicationError::CursorEncodingFailed(format!(
            "failed to serialize directory cursor claims: {error}"
        ))
    })?;
    let envelope = CursorEnvelope {
        checksum: checksum(&claims_bytes),
        claims,
    };
    let envelope_bytes = serde_json::to_vec(&envelope).map_err(|error| {
        ApplicationError::CursorEncodingFailed(format!(
            "failed to serialize directory cursor envelope: {error}"
        ))
    })?;
    let encoded = format!("{DIRECTORY_CURSOR_PREFIX}{}", encode_hex(&envelope_bytes));
    if encoded.len() > MAX_DIRECTORY_CURSOR_CHARS {
        return Err(ApplicationError::CursorEncodingFailed(
            "directory cursor exceeds the maximum encoded size".into(),
        ));
    }
    Ok(encoded)
}

pub(crate) fn decode_directory_cursor(
    cursor: &str,
) -> Result<DirectoryCursorClaims, ApplicationError> {
    if cursor.len() > MAX_DIRECTORY_CURSOR_CHARS {
        return Err(ApplicationError::InvalidCursor(
            "directory cursor exceeds the maximum encoded size".into(),
        ));
    }
    let encoded = cursor
        .strip_prefix(DIRECTORY_CURSOR_PREFIX)
        .ok_or_else(|| {
            ApplicationError::InvalidCursor("directory cursor prefix is invalid".into())
        })?;
    let envelope_bytes = decode_hex(encoded)?;
    let envelope: CursorEnvelope = serde_json::from_slice(&envelope_bytes).map_err(|error| {
        ApplicationError::InvalidCursor(format!("directory cursor payload is invalid: {error}"))
    })?;
    let claims_bytes = serde_json::to_vec(&envelope.claims).map_err(|error| {
        ApplicationError::InvalidCursor(format!(
            "directory cursor claims cannot be validated: {error}"
        ))
    })?;
    if envelope.checksum != checksum(&claims_bytes) {
        return Err(ApplicationError::InvalidCursor(
            "directory cursor checksum does not match its claims".into(),
        ));
    }
    if envelope.claims.schema_version != DIRECTORY_CURSOR_SCHEMA_VERSION {
        return Err(ApplicationError::StaleCursor(format!(
            "unsupported directory cursor schema {}; expected {DIRECTORY_CURSOR_SCHEMA_VERSION}",
            envelope.claims.schema_version
        )));
    }
    if envelope.claims.ordering_version != DIRECTORY_ORDERING_VERSION {
        return Err(ApplicationError::StaleCursor(format!(
            "directory ordering version {} is incompatible with {DIRECTORY_ORDERING_VERSION}",
            envelope.claims.ordering_version
        )));
    }
    validate_claims(&envelope.claims)
        .map_err(|message| ApplicationError::InvalidCursor(message.to_string()))?;
    Ok(envelope.claims)
}

fn validate_claims(claims: &DirectoryCursorClaims) -> Result<(), &'static str> {
    if claims.total_entries == 0 {
        return Err("directory cursor cannot target an empty stream");
    }
    if claims.next_index == 0 || claims.next_index >= claims.total_entries {
        return Err("directory cursor position must be between the first item and stream end");
    }
    Ok(())
}

fn checksum(payload: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(DIRECTORY_CURSOR_CHECKSUM_DOMAIN);
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
            "directory cursor hex payload has an odd length".into(),
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
            "directory cursor contains a non-hex character".into(),
        )),
    }
}

pub(crate) fn directory_manifest_hash<T: Serialize>(
    entries: &T,
) -> Result<String, ApplicationError> {
    use sha2::{Digest, Sha256};

    let bytes = serde_json::to_vec(entries).map_err(|error| {
        ApplicationError::CursorEncodingFailed(format!(
            "failed to serialize directory entry manifest: {error}"
        ))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(DIRECTORY_MANIFEST_DOMAIN);
    hasher.update(bytes);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

#[derive(Debug, Serialize, Deserialize)]
struct CursorEnvelope {
    claims: DirectoryCursorClaims,
    checksum: String,
}
