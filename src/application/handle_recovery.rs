//! Lazy re-ingest behind the v2 handle readers.
//!
//! A parser deploy does not rewrite stored documents at startup. The first handle read after that
//! deploy is therefore the first moment at which the server can both prove the old layout against
//! the new one and answer with a remap. This object performs that re-ingest through the ordinary
//! `open_document` pipeline, serialized per server so concurrent stale reads do not parse the same
//! source twice.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use crate::application::ports::{
    DocumentKeyRow, DocumentRepository, RetrievalOptions, TextUnitIndex,
};
use crate::application::reading_error::ReadingError;

pub(crate) struct HandleRecovery {
    open_document: Arc<OpenDocumentUseCase>,
    repository: Arc<dyn DocumentRepository>,
    text_unit_index: Arc<dyn TextUnitIndex>,
    ingest_fingerprint: String,
    /// One lazy re-ingest at a time. Re-ingests are already serialized by SQLite, but this lock
    /// keeps duplicate parses and their peak memory off a concurrent burst of stale reads.
    reingest: Mutex<()>,
}

impl HandleRecovery {
    pub(crate) fn new(
        open_document: Arc<OpenDocumentUseCase>,
        repository: Arc<dyn DocumentRepository>,
        text_unit_index: Arc<dyn TextUnitIndex>,
        ingest_fingerprint: String,
    ) -> Self {
        Self {
            open_document,
            repository,
            text_unit_index,
            ingest_fingerprint,
            reingest: Mutex::new(()),
        }
    }

    pub(crate) fn accepts(&self, fingerprint: Option<&str>) -> bool {
        fingerprint == Some(self.ingest_fingerprint.as_str())
    }

    /// The current document row behind `doc_key`, re-ingesting the stored source first when the
    /// row was written by another ingest fingerprint.
    pub(crate) async fn current_row(
        &self,
        doc_key: i64,
    ) -> Result<Option<DocumentKeyRow>, ReadingError> {
        let Some(row) = self.text_unit_index.document_key_row(doc_key).await? else {
            return Ok(None);
        };
        if self.accepts(row.ingest_fingerprint.as_deref()) {
            return Ok(Some(row));
        }

        let _guard = self.reingest.lock().await;
        let Some(mut row) = self.text_unit_index.document_key_row(doc_key).await? else {
            return Ok(None);
        };
        if self.accepts(row.ingest_fingerprint.as_deref()) {
            return Ok(Some(row));
        }

        let Some(header) = self.repository.document_header(&row.document_id).await? else {
            return Ok(Some(row));
        };
        // An unreachable or undecodable source must not make today's stored rows less available
        // than they were before this feature. The checksum still binds the handle to those rows;
        // recovery is best-effort and simply absent when the source cannot be re-ingested.
        let Ok(opened) = self
            .open_document
            .execute(OpenDocumentCommand {
                source: header.source,
                options: RetrievalOptions {
                    auth_profile: None,
                    // A hit means the stored rows were already produced by this fingerprint; a miss
                    // re-ingests. Forcing a refresh here would parse every pre-feature document once
                    // merely to stamp its metadata.
                    force_refresh: false,
                },
            })
            .await
        else {
            return Ok(Some(row));
        };

        let Some(fresh) = self.text_unit_index.document_key_row(doc_key).await? else {
            return Ok(None);
        };
        if self.accepts(fresh.ingest_fingerprint.as_deref()) {
            return Ok(Some(fresh));
        }

        // The open-result cache can be current even when the row predates the metadata column (the
        // first deploy of this feature), because that cache already keyed results by the same ingest
        // fingerprint. In that case `open` is a hit and does not rewrite the row; stamp the metadata
        // once instead of parsing the document on every read.
        if opened.document_id == fresh.document_id {
            self.text_unit_index
                .mark_ingest_fingerprint(&fresh.document_id, &self.ingest_fingerprint)
                .await?;
            row = DocumentKeyRow {
                document_id: fresh.document_id,
                ingest_fingerprint: Some(self.ingest_fingerprint.clone()),
            };
            return Ok(Some(row));
        }
        Ok(Some(fresh))
    }
}
