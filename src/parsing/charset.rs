use std::borrow::Cow;
use std::collections::BTreeMap;

use encoding_rs::{Encoding, REPLACEMENT, UTF_8};

use crate::application::ports::ApplicationError;

/// A decoding encoding and where its label came from: `bom` or `http`, and for HTML also `meta` or `fallback`.
pub(super) type DetectedCharset = (&'static Encoding, &'static str);

/// Records which declared encoding decoded the input, as `text_charset` / `text_charset_source`.
pub(super) fn record_charset(
    metadata: &mut BTreeMap<String, String>,
    (encoding, origin): DetectedCharset,
) {
    metadata.insert("text_charset".into(), encoding.name().into());
    metadata.insert("text_charset_source".into(), origin.into());
}

/// Turns owned text bytes into a `String`: valid UTF-8 is moved without copying; otherwise a declared charset
/// decodes it and is recorded in `metadata`. `invalid_utf8` builds the error when nothing declares an encoding.
pub(super) fn decode_text(
    bytes: Vec<u8>,
    content_type: &str,
    metadata: &mut BTreeMap<String, String>,
    invalid_utf8: impl FnOnce(std::str::Utf8Error) -> ApplicationError,
) -> Result<String, ApplicationError> {
    let error = match String::from_utf8(bytes) {
        Ok(text) => return Ok(text),
        Err(error) => error,
    };
    let utf8_error = error.utf8_error();
    let bytes = error.into_bytes();
    let Some((text, detected)) = decode_declared_charset(&bytes, content_type)? else {
        return Err(invalid_utf8(utf8_error));
    };
    record_charset(metadata, detected);
    Ok(text.into_owned())
}

/// Decodes bytes that are not valid UTF-8 by their byte-order mark or, failing that, the `charset` parameter of
/// `content_type`. `Ok(None)` means neither names a non-UTF-8 encoding: no encoding is guessed, and the caller
/// keeps its own UTF-8 error.
pub(super) fn decode_declared_charset<'a>(
    bytes: &'a [u8],
    content_type: &str,
) -> Result<Option<(Cow<'a, str>, DetectedCharset)>, ApplicationError> {
    let (encoding, origin, body) = if let Some((encoding, bom_len)) = Encoding::for_bom(bytes) {
        (encoding, "bom", &bytes[bom_len..])
    } else if let Some(label) = content_type_charset(content_type) {
        let encoding = Encoding::for_label(label.as_bytes())
            .filter(|encoding| *encoding != REPLACEMENT)
            .ok_or_else(|| {
                ApplicationError::UnsupportedTextEncoding(format!(
                    "declared charset {label:?} (charset source: http) is not supported"
                ))
            })?;
        (encoding, "http", bytes)
    } else {
        return Ok(None);
    };
    if encoding == UTF_8 {
        return Ok(None);
    }
    let text = encoding
        .decode_without_bom_handling_and_without_replacement(body)
        .ok_or_else(|| {
            ApplicationError::UnsupportedTextEncoding(format!(
                "bytes are not valid {} (charset source: {origin})",
                encoding.name()
            ))
        })?;
    Ok(Some((text, (encoding, origin))))
}

pub(super) fn content_type_charset(content_type: &str) -> Option<&str> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        let value = value.trim().trim_matches(['"', '\'']);
        (name.trim().eq_ignore_ascii_case("charset") && !value.is_empty()).then_some(value)
    })
}
