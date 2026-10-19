use std::borrow::Cow;

use encoding_rs::{Encoding, REPLACEMENT, UTF_8};

use crate::application::ports::ApplicationError;

/// The encoding that decoded non-UTF-8 HTML and where its label came from (`bom`, `http`, `meta`, `fallback`).
pub(super) type DetectedCharset = (&'static Encoding, &'static str);

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
