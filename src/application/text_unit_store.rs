use std::sync::Arc;

use crate::application::ports::{ApplicationError, TextUnitIndex};
use crate::domain::Document;

/// Builds a document's paragraph and sentence units and stores them, as `open_document` does.
pub async fn store_text_units(
    index: &dyn TextUnitIndex,
    document: &Document,
) -> Result<(), ApplicationError> {
    let (paragraphs, sentences) = document.try_text_unit_sets().map_err(|error| {
        ApplicationError::TextUnitIndexFailed(format!(
            "cannot build current Paragraph coverage from persisted block evidence: {error}"
        ))
    })?;
    index
        .replace_document_sets(
            &Arc::new(document.clone()),
            Arc::new(paragraphs),
            Arc::new(sentences),
        )
        .await
}
