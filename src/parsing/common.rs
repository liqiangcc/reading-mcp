use crate::domain::{ContentHash, DocumentId, DocumentSource};

pub fn content_hash(bytes: &[u8]) -> ContentHash {
    ContentHash::of_bytes(bytes)
}

pub fn document_id(source: &DocumentSource, content_hash: &ContentHash) -> DocumentId {
    DocumentId::derive(source, content_hash)
}

pub fn title_from_metadata(
    metadata: &std::collections::BTreeMap<String, String>,
    fallback: &DocumentSource,
) -> String {
    metadata
        .get("file_stem")
        .or_else(|| metadata.get("file_name"))
        .cloned()
        .unwrap_or_else(|| fallback.0.clone())
}

pub fn slugify(value: &str) -> String {
    let mut output = String::new();
    let mut pending_dash = false;

    for ch in value.trim().chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            if pending_dash && !output.is_empty() {
                output.push('-');
            }
            output.push(ch);
            pending_dash = false;
        } else {
            pending_dash = true;
        }
    }

    if output.is_empty() {
        "section".into()
    } else {
        output
    }
}
