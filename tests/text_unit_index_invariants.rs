use std::collections::BTreeMap;

use reading_mcp::application::ports::{ApplicationError, TextUnitIndex};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::SqliteTextUnitIndex;
use tempfile::tempdir;

#[tokio::test]
async fn sqlite_text_unit_index_rejects_derived_rows_that_break_exact_range_invariants() {
    let directory = tempdir().expect("temporary directory should be created");
    let index = SqliteTextUnitIndex::open(directory.path().join("state.sqlite"))
        .expect("text unit index should open");
    let document = document();
    let mut unit = document
        .paragraph_text_units()
        .units
        .into_iter()
        .next()
        .expect("fixture should produce one paragraph");

    unit.text.push('x');
    let error = index
        .replace_document(&document.id, &[unit])
        .await
        .expect_err("range/text length mismatch must be rejected");

    assert!(matches!(error, ApplicationError::TextUnitIndexFailed(_)));
}

#[tokio::test]
async fn sqlite_text_unit_index_rejects_zero_paragraph_ordinal() {
    let directory = tempdir().expect("temporary directory should be created");
    let index = SqliteTextUnitIndex::open(directory.path().join("state.sqlite"))
        .expect("text unit index should open");
    let document = document();
    let mut unit = document
        .paragraph_text_units()
        .units
        .into_iter()
        .next()
        .expect("fixture should produce one paragraph");

    unit.paragraph_index = 0;
    let error = index
        .replace_document(&document.id, &[unit])
        .await
        .expect_err("zero human-facing paragraph ordinal must be rejected");

    assert!(matches!(error, ApplicationError::TextUnitIndexFailed(_)));
}

fn document() -> Document {
    Document {
        id: DocumentId("doc:invariant".into()),
        source: DocumentSource("memory:invariant".into()),
        title: "Invariant".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:raw".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: "Exact paragraph".into(),
            location: Location::default(),
            children: vec![],
        }],
    }
}
