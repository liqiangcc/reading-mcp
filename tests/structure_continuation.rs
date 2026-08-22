use std::sync::Arc;

use reading_mcp::application::get_document_structure::{
    GetDocumentStructureCommand, GetDocumentStructureUseCase, SectionOutline,
};
use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::InMemoryDocumentRepository;
use reading_mcp::infrastructure::SqliteDocumentRepository;

#[tokio::test]
async fn paginates_more_than_one_thousand_roots_without_gap_or_overlap() {
    let repository = repository_with(document_with_many_roots(1_500)).await;
    let use_case = GetDocumentStructureUseCase::new(repository);

    let mut cursor = None;
    let mut all_ids = Vec::new();
    let mut page = 0usize;
    loop {
        let max_nodes = match page {
            0 => Some(400),
            1 => Some(127),
            _ => Some(600),
        };
        let result = use_case
            .execute_command(GetDocumentStructureCommand {
                document_id: DocumentId("doc:structure".into()),
                root_section_id: None,
                max_depth: None,
                max_nodes,
                cursor,
            })
            .await
            .expect("structure page should succeed");

        let ids = flatten_outline_ids(&result.sections);
        assert_eq!(
            ids.len(),
            result.stream.end_index - result.stream.start_index
        );
        all_ids.extend(ids);
        page += 1;

        if result.complete {
            assert!(!result.truncated);
            assert!(result.next_cursor.is_none());
            assert_eq!(result.stream.end_index, result.stream.total_nodes);
            break;
        }
        assert!(result.truncated);
        cursor = result.next_cursor;
        assert!(cursor.is_some());
    }

    assert_eq!(all_ids.len(), 1_500);
    let expected = (0..1_500)
        .map(|index| format!("section://{index}"))
        .collect::<Vec<_>>();
    assert_eq!(all_ids, expected);
}

#[tokio::test]
async fn continuation_page_is_a_page_forest_without_repeated_ancestors() {
    let repository = repository_with(deep_document()).await;
    let use_case = GetDocumentStructureUseCase::new(repository);

    let first = use_case
        .execute_command(GetDocumentStructureCommand {
            document_id: DocumentId("doc:structure".into()),
            root_section_id: None,
            max_depth: None,
            max_nodes: Some(2),
            cursor: None,
        })
        .await
        .expect("first page should succeed");

    assert_eq!(
        flatten_outline_ids(&first.sections),
        vec!["section://a", "section://a/a1"]
    );
    assert_eq!(first.sections.len(), 1);
    assert!(!first.sections[0].children_complete);
    assert_eq!(first.sections[0].children.len(), 1);
    assert!(!first.sections[0].children[0].children_complete);

    let second = use_case
        .execute_command(GetDocumentStructureCommand {
            document_id: DocumentId("doc:structure".into()),
            root_section_id: None,
            max_depth: None,
            max_nodes: Some(2),
            cursor: first.next_cursor,
        })
        .await
        .expect("second page should succeed");

    assert_eq!(
        flatten_outline_ids(&second.sections),
        vec!["section://a/a1/a1a", "section://a/a2"]
    );
    assert_eq!(second.sections.len(), 2);
    assert_eq!(
        second.sections[0]
            .parent_id
            .as_ref()
            .map(|id| id.0.as_str()),
        Some("section://a/a1")
    );
    assert_eq!(
        second.sections[1]
            .parent_id
            .as_ref()
            .map(|id| id.0.as_str()),
        Some("section://a")
    );
    assert!(second.sections.iter().all(|node| node.children_complete));
    assert_eq!(first.stream.end_index, second.stream.start_index);
}

#[tokio::test]
async fn subtree_scope_and_relative_max_depth_are_deterministic() {
    let repository = repository_with(deep_document()).await;
    let use_case = GetDocumentStructureUseCase::new(repository);

    let result = use_case
        .execute_command(GetDocumentStructureCommand {
            document_id: DocumentId("doc:structure".into()),
            root_section_id: Some(SectionId("section://a/a1".into())),
            max_depth: Some(1),
            max_nodes: Some(10),
            cursor: None,
        })
        .await
        .expect("subtree structure should succeed");

    assert_eq!(
        flatten_outline_ids(&result.sections),
        vec!["section://a/a1"]
    );
    assert_eq!(
        result.sections[0]
            .parent_id
            .as_ref()
            .map(|id| id.0.as_str()),
        Some("section://a")
    );
    assert!(result.sections[0].children_complete);
    assert!(result.complete);
    assert_eq!(
        result.stream.root_section_id,
        Some(SectionId("section://a/a1".into()))
    );
    assert_eq!(result.stream.max_depth, Some(1));

    let legacy_zero = use_case
        .execute_command(GetDocumentStructureCommand {
            document_id: DocumentId("doc:structure".into()),
            root_section_id: Some(SectionId("section://a/a1".into())),
            max_depth: Some(0),
            max_nodes: Some(10),
            cursor: None,
        })
        .await
        .expect("max_depth=0 should retain legacy root-only behavior");
    assert_eq!(legacy_zero.stream.max_depth, Some(1));
    assert_eq!(
        flatten_outline_ids(&legacy_zero.sections),
        vec!["section://a/a1"]
    );
}

#[tokio::test]
async fn cursor_scope_mismatch_fails_closed() {
    let repository = repository_with(deep_document()).await;
    let use_case = GetDocumentStructureUseCase::new(repository);

    let first = use_case
        .execute_command(GetDocumentStructureCommand {
            document_id: DocumentId("doc:structure".into()),
            root_section_id: Some(SectionId("section://a".into())),
            max_depth: None,
            max_nodes: Some(1),
            cursor: None,
        })
        .await
        .expect("first page should succeed");
    let cursor = first.next_cursor.expect("first page should continue");

    let error = use_case
        .execute_command(GetDocumentStructureCommand {
            document_id: DocumentId("doc:structure".into()),
            root_section_id: Some(SectionId("section://b".into())),
            max_depth: None,
            max_nodes: Some(1),
            cursor: Some(cursor),
        })
        .await
        .expect_err("cursor root mismatch must fail");
    assert!(matches!(error, ApplicationError::CursorTargetMismatch(_)));
}

#[tokio::test]
async fn normalized_document_change_makes_cursor_stale() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository
        .save(deep_document())
        .await
        .expect("fixture document should save");
    let use_case = GetDocumentStructureUseCase::new(repository.clone());

    let first = use_case
        .execute_command(GetDocumentStructureCommand {
            document_id: DocumentId("doc:structure".into()),
            root_section_id: None,
            max_depth: None,
            max_nodes: Some(1),
            cursor: None,
        })
        .await
        .expect("first page should succeed");
    let cursor = first.next_cursor.expect("first page should continue");

    let mut changed = deep_document();
    changed.root_sections[0].title = "Changed A".into();
    repository
        .save(changed)
        .await
        .expect("changed canonical document should replace fixture");

    let error = use_case
        .execute_command(GetDocumentStructureCommand {
            document_id: DocumentId("doc:structure".into()),
            root_section_id: None,
            max_depth: None,
            max_nodes: Some(1),
            cursor: Some(cursor),
        })
        .await
        .expect_err("normalized identity change must stale the cursor");
    assert!(matches!(error, ApplicationError::StaleCursor(_)));
}

#[tokio::test]
async fn raw_document_change_makes_cursor_stale_even_when_normalized_text_matches() {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository
        .save(deep_document())
        .await
        .expect("fixture document should save");
    let use_case = GetDocumentStructureUseCase::new(repository.clone());

    let first = use_case
        .execute_command(GetDocumentStructureCommand {
            document_id: DocumentId("doc:structure".into()),
            root_section_id: None,
            max_depth: None,
            max_nodes: Some(1),
            cursor: None,
        })
        .await
        .expect("first page should succeed");
    let cursor = first.next_cursor.expect("first page should continue");

    let mut changed = deep_document();
    changed.content_hash = ContentHash("sha256:raw-changed".into());
    repository
        .save(changed)
        .await
        .expect("changed raw document should replace fixture");

    let error = use_case
        .execute_command(GetDocumentStructureCommand {
            document_id: DocumentId("doc:structure".into()),
            root_section_id: None,
            max_depth: None,
            max_nodes: Some(1),
            cursor: Some(cursor),
        })
        .await
        .expect_err("raw identity change must stale the cursor");
    assert!(matches!(error, ApplicationError::StaleCursor(_)));
}

#[tokio::test]
async fn structure_cursor_survives_sqlite_repository_reopen() {
    let directory = tempfile::tempdir().expect("temporary state directory should be created");
    let database = directory.path().join("reading-mcp.sqlite");
    let repository =
        Arc::new(SqliteDocumentRepository::open(&database).expect("SQLite repository should open"));
    repository
        .save(deep_document())
        .await
        .expect("fixture document should save");
    let first = GetDocumentStructureUseCase::new(repository)
        .execute_command(GetDocumentStructureCommand {
            document_id: DocumentId("doc:structure".into()),
            root_section_id: None,
            max_depth: None,
            max_nodes: Some(1),
            cursor: None,
        })
        .await
        .expect("first page should succeed");
    let cursor = first.next_cursor.expect("first page should continue");

    let reopened = Arc::new(
        SqliteDocumentRepository::open(&database).expect("SQLite repository should reopen"),
    );
    let second = GetDocumentStructureUseCase::new(reopened)
        .execute_command(GetDocumentStructureCommand {
            document_id: DocumentId("doc:structure".into()),
            root_section_id: None,
            max_depth: None,
            max_nodes: Some(10),
            cursor: Some(cursor),
        })
        .await
        .expect("continuation should survive repository reopen");
    assert_eq!(
        flatten_outline_ids(&second.sections),
        [
            "section://a/a1",
            "section://a/a1/a1a",
            "section://a/a2",
            "section://b"
        ]
    );
    assert_eq!(second.stream.start_index, 1);
}

#[tokio::test]
async fn tampered_cursor_is_invalid() {
    let repository = repository_with(deep_document()).await;
    let use_case = GetDocumentStructureUseCase::new(repository);

    let first = use_case
        .execute_command(GetDocumentStructureCommand {
            document_id: DocumentId("doc:structure".into()),
            root_section_id: None,
            max_depth: None,
            max_nodes: Some(1),
            cursor: None,
        })
        .await
        .expect("first page should succeed");
    let mut cursor = first.next_cursor.expect("first page should continue");
    let last = cursor.pop().expect("cursor should not be empty");
    cursor.push(if last == '0' { '1' } else { '0' });

    let error = use_case
        .execute_command(GetDocumentStructureCommand {
            document_id: DocumentId("doc:structure".into()),
            root_section_id: None,
            max_depth: None,
            max_nodes: Some(1),
            cursor: Some(cursor),
        })
        .await
        .expect_err("tampered cursor must fail");
    assert!(matches!(error, ApplicationError::InvalidCursor(_)));
}

async fn repository_with(document: Document) -> Arc<dyn DocumentRepository> {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository
        .save(document)
        .await
        .expect("fixture document should save");
    repository
}

fn document_with_many_roots(count: usize) -> Document {
    Document {
        id: DocumentId("doc:structure".into()),
        source: DocumentSource("memory:structure.md".into()),
        title: "Structure".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:structure".into()),
        metadata: Default::default(),
        root_sections: (0..count)
            .map(|index| section(&format!("section://{index}"), None, Vec::new()))
            .collect(),
    }
}

fn deep_document() -> Document {
    let a1a = section("section://a/a1/a1a", Some("section://a/a1"), Vec::new());
    let a1 = section("section://a/a1", Some("section://a"), vec![a1a]);
    let a2 = section("section://a/a2", Some("section://a"), Vec::new());
    let a = section("section://a", None, vec![a1, a2]);
    let b = section("section://b", None, Vec::new());

    Document {
        id: DocumentId("doc:structure".into()),
        source: DocumentSource("memory:structure.md".into()),
        title: "Structure".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:structure".into()),
        metadata: Default::default(),
        root_sections: vec![a, b],
    }
}

fn section(id: &str, parent: Option<&str>, children: Vec<Section>) -> Section {
    Section {
        id: SectionId(id.into()),
        parent_id: parent.map(|value| SectionId(value.into())),
        title: id.rsplit('/').next().unwrap_or(id).into(),
        level: 1,
        content: format!("Content for {id}."),
        location: Location::default(),
        children,
    }
}

fn flatten_outline_ids(sections: &[SectionOutline]) -> Vec<String> {
    fn collect(section: &SectionOutline, output: &mut Vec<String>) {
        output.push(section.section_id.0.clone());
        for child in &section.children {
            collect(child, output);
        }
    }

    let mut output = Vec::new();
    for section in sections {
        collect(section, &mut output);
    }
    output
}
