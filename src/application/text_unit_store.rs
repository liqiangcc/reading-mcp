use std::sync::Arc;

use crate::application::ports::{ApplicationError, TextUnitIndex};
use crate::domain::Document;

/// Segments a document and stores the boundaries, as `open_document` does.
pub async fn store_text_units(
    index: &dyn TextUnitIndex,
    document: &Document,
) -> Result<(), ApplicationError> {
    let segmentation = document.try_segmentation().map_err(|error| {
        ApplicationError::TextUnitIndexFailed(format!(
            "cannot build current Paragraph coverage from persisted block evidence: {error}"
        ))
    })?;
    index
        .replace_document_segmentation(&Arc::new(document.clone()), Arc::new(segmentation))
        .await
}
