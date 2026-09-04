use std::sync::Arc;

use reading_mcp::application::get_document_structure::{
    GetDocumentStructureUseCase, NAMED_SECTION_RESOLUTION_VERSION, NamedSectionMatchKind,
    NamedSectionResolutionStatus, ResolveNamedSectionCommand,
};
use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::InMemoryDocumentRepository;

#[tokio::test]
async fn resolves_exact_prefixed_and_title_only_with_executable_boundary() {
    let repository = repository_with(numbered_document()).await;
    let use_case = GetDocumentStructureUseCase::new(repository);
    let identity = numbered_document();
    let normalized = identity.normalized_document_hash().0;

    for (query, expected_kind) in [
        ("1 Introduction", NamedSectionMatchKind::ExactTitle),
        (
            "Section 1 — Introduction",
            NamedSectionMatchKind::SectionPrefixedTitle,
        ),
        ("Introduction", NamedSectionMatchKind::TitleOnly),
    ] {
        let result = use_case
            .resolve_named_section(ResolveNamedSectionCommand {
                document_id: identity.id.clone(),
                query: query.into(),
                expected_content_hash: identity.content_hash.0.clone(),
                expected_normalized_document_hash: normalized.clone(),
                expected_structure_resolution_version: Some(
                    NAMED_SECTION_RESOLUTION_VERSION.into(),
                ),
            })
            .await
            .expect("named section should resolve");

        assert_eq!(
            result.resolution.status,
            NamedSectionResolutionStatus::Resolved
        );
        assert_eq!(result.resolution.match_kind, Some(expected_kind));
        let matched = result
            .resolution
            .matched
            .as_ref()
            .expect("resolved section metadata should be present");
        assert_eq!(matched.section_id.0, "section://1-introduction");
        assert_eq!(matched.start_locator.owner_section_id, matched.section_id);
        assert!(matched.start_locator.normalized_range.is_none());

        let boundary = result
            .resolution
            .boundary
            .as_ref()
            .expect("resolved scope should have an executable boundary");
        assert_eq!(boundary.intervals.len(), 1);
        assert_eq!(boundary.intervals[0].start, matched.body_order);
        assert_eq!(boundary.intervals[0].end, matched.body_order + 2);
        let next = boundary
            .end_exclusive
            .as_ref()
            .expect("contiguous scope should expose the next owner metadata");
        assert_eq!(next.section_id.0, "section://2-replication");
        assert!(
            boundary
                .intervals
                .iter()
                .all(|interval| next.body_order < interval.start || next.body_order >= interval.end)
        );
    }
}

#[tokio::test]
async fn ambiguity_not_found_and_page_fallback_never_guess() {
    let mut ambiguous = numbered_document();
    ambiguous.root_sections.push(section(
        "section://3-introduction",
        "3 Introduction",
        1,
        "future-only-secret-body",
        vec![],
    ));
    let repository = repository_with(ambiguous.clone()).await;
    let use_case = GetDocumentStructureUseCase::new(repository);
    let normalized = ambiguous.normalized_document_hash().0;

    let ambiguous_result = use_case
        .resolve_named_section(command_for(&ambiguous, &normalized, "Introduction"))
        .await
        .expect("ambiguous structure should be returned as metadata");
    assert_eq!(
        ambiguous_result.resolution.status,
        NamedSectionResolutionStatus::Ambiguous
    );
    assert_eq!(ambiguous_result.resolution.candidates.len(), 2);
    assert!(ambiguous_result.resolution.matched.is_none());
    assert!(ambiguous_result.resolution.boundary.is_none());

    let not_found = use_case
        .resolve_named_section(command_for(&ambiguous, &normalized, "Does Not Exist"))
        .await
        .expect("not-found should be a normal structure outcome");
    assert_eq!(
        not_found.resolution.status,
        NamedSectionResolutionStatus::NotFound
    );

    let mut page_only = Document {
        id: DocumentId("doc:page-only".into()),
        source: DocumentSource("memory:page-only.pdf".into()),
        title: "Page fallback".into(),
        media_type: MediaType("application/pdf".into()),
        content_hash: ContentHash("sha256:page-only".into()),
        metadata: Default::default(),
        root_sections: vec![section(
            "section://page-1",
            "Page 1",
            1,
            "Introduction future lexical text must not become a heading",
            vec![],
        )],
    };
    page_only
        .metadata
        .insert("pdf_structure_provenance".into(), "page_fallback".into());
    let page_hash = page_only.normalized_document_hash().0;
    let page_repository = repository_with(page_only.clone()).await;
    let unavailable = GetDocumentStructureUseCase::new(page_repository)
        .resolve_named_section(command_for(&page_only, &page_hash, "Introduction"))
        .await
        .expect("page fallback should degrade explicitly");
    assert_eq!(
        unavailable.resolution.status,
        NamedSectionResolutionStatus::Unavailable
    );
    assert!(unavailable.resolution.candidates.is_empty());
}

#[tokio::test]
async fn stale_named_structure_identity_fails_closed() {
    let document = numbered_document();
    let normalized = document.normalized_document_hash().0;
    let repository = repository_with(document.clone()).await;
    let use_case = GetDocumentStructureUseCase::new(repository);

    for command in [
        ResolveNamedSectionCommand {
            document_id: document.id.clone(),
            query: "Introduction".into(),
            expected_content_hash: "sha256:wrong".into(),
            expected_normalized_document_hash: normalized.clone(),
            expected_structure_resolution_version: None,
        },
        ResolveNamedSectionCommand {
            document_id: document.id.clone(),
            query: "Introduction".into(),
            expected_content_hash: document.content_hash.0.clone(),
            expected_normalized_document_hash: "sha256:wrong-normalized".into(),
            expected_structure_resolution_version: None,
        },
        ResolveNamedSectionCommand {
            document_id: document.id.clone(),
            query: "Introduction".into(),
            expected_content_hash: document.content_hash.0.clone(),
            expected_normalized_document_hash: normalized.clone(),
            expected_structure_resolution_version: Some("named-section-resolution/v0".into()),
        },
    ] {
        let error = use_case
            .resolve_named_section(command)
            .await
            .expect_err("stale structure identity must fail closed");
        assert!(matches!(error, ApplicationError::StaleStructure(_)));
    }
}

#[tokio::test]
async fn epub_subtree_boundary_uses_body_order_intervals_not_tree_preorder() {
    let mut parent = section(
        "section://parent",
        "1 Parent",
        1,
        "parent body",
        vec![section(
            "section://child",
            "1.1 Child",
            2,
            "child body",
            vec![],
        )],
    );
    parent.children[0].parent_id = Some(parent.id.clone());
    let sibling = section("section://sibling", "2 Sibling", 1, "sibling body", vec![]);
    let mut document = Document {
        id: DocumentId("doc:epub-boundary".into()),
        source: DocumentSource("memory:boundary.epub".into()),
        title: "EPUB boundary".into(),
        media_type: MediaType("application/epub+zip".into()),
        content_hash: ContentHash("sha256:epub-boundary".into()),
        metadata: Default::default(),
        root_sections: vec![parent, sibling],
    };
    document.metadata.insert(
        "epub_structure_map".into(),
        r#"{"schema_version":"epub-structure-reconciliation/v1","sections":[{"section_id":"section://parent","source_order":0},{"section_id":"section://sibling","source_order":1},{"section_id":"section://child","source_order":2}]}"#.into(),
    );
    let normalized = document.normalized_document_hash().0;
    let repository = repository_with(document.clone()).await;
    let result = GetDocumentStructureUseCase::new(repository)
        .resolve_named_section(command_for(&document, &normalized, "1 Parent"))
        .await
        .expect("EPUB named scope should resolve");

    let boundary = result
        .resolution
        .boundary
        .expect("EPUB scope should remain executable");
    assert_eq!(boundary.intervals.len(), 2);
    assert_eq!(boundary.intervals[0].start, 0);
    assert_eq!(boundary.intervals[0].end, 1);
    assert_eq!(boundary.intervals[1].start, 2);
    assert_eq!(boundary.intervals[1].end, 3);
    assert!(boundary.end_exclusive.is_none());
}

fn command_for(
    document: &Document,
    normalized_hash: &str,
    query: &str,
) -> ResolveNamedSectionCommand {
    ResolveNamedSectionCommand {
        document_id: document.id.clone(),
        query: query.into(),
        expected_content_hash: document.content_hash.0.clone(),
        expected_normalized_document_hash: normalized_hash.into(),
        expected_structure_resolution_version: Some(NAMED_SECTION_RESOLUTION_VERSION.into()),
    }
}

async fn repository_with(document: Document) -> Arc<InMemoryDocumentRepository> {
    let repository = Arc::new(InMemoryDocumentRepository::default());
    repository
        .save(document)
        .await
        .expect("fixture document should save");
    repository
}

fn numbered_document() -> Document {
    let mut intro = section(
        "section://1-introduction",
        "1 Introduction",
        1,
        "intro-only-secret-body",
        vec![section(
            "section://1-introduction/scope",
            "1.1 Scope",
            2,
            "child-only-secret-body",
            vec![],
        )],
    );
    intro.children[0].parent_id = Some(intro.id.clone());
    Document {
        id: DocumentId("doc:named-boundary".into()),
        source: DocumentSource("memory:named-boundary.md".into()),
        title: "Named boundary".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:named-boundary".into()),
        metadata: Default::default(),
        root_sections: vec![
            intro,
            section(
                "section://2-replication",
                "2 Replication",
                1,
                "future-only-secret-body",
                vec![],
            ),
        ],
    }
}

fn section(id: &str, title: &str, level: u8, content: &str, children: Vec<Section>) -> Section {
    Section {
        id: SectionId(id.into()),
        parent_id: None,
        title: title.into(),
        level,
        content: content.into(),
        location: Location {
            section_path: vec![title.into()],
            native_location: Some(format!("memory:{id}")),
            ..Location::default()
        },
        children,
    }
}
