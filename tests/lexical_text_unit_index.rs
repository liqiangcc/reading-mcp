use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::ports::{DocumentRepository, LEXICAL_TOKENIZER_VERSION, SearchIndex};
use reading_mcp::application::search_document::{
    SearchCandidateKind, SearchDocumentCommand, SearchDocumentUseCase,
};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, Location, MediaType, Section, SectionId,
};
use reading_mcp::infrastructure::{
    InMemoryDocumentRepository, InMemorySearchIndex, SqliteDocumentRepository, SqliteSearchIndex,
};

#[tokio::test]
async fn in_memory_search_emits_truthful_section_paragraph_and_sentence_candidates() {
    let document = fixture();
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemorySearchIndex::default());
    repository.save(document.clone()).await.expect("save");
    index.index(&document).await.expect("index");
    let search = SearchDocumentUseCase::new(index, repository);

    let title = search
        .execute(SearchDocumentCommand {
            document_id: document.id.clone(),
            query: "内存机制".into(),
            limit: 10,
        })
        .await
        .expect("CJK title substring search");
    assert_eq!(title.tokenizer_version, LEXICAL_TOKENIZER_VERSION);
    assert_eq!(title.hits[0].candidate_kind, SearchCandidateKind::Section);
    assert!(title.hits[0].text_locator.normalized_range.is_none());

    let sentence = search
        .execute(SearchDocumentCommand {
            document_id: document.id.clone(),
            query: "物理帧".into(),
            limit: 10,
        })
        .await
        .expect("CJK sentence search");
    let sentence_hit = sentence
        .hits
        .iter()
        .find(|hit| hit.candidate_kind == SearchCandidateKind::Sentence)
        .expect("sentence candidate");
    assert_eq!(sentence_hit.text_locator.paragraph_index, Some(1));
    assert_eq!(sentence_hit.text_locator.sentence_index, Some(2));
    assert!(sentence_hit.text_locator.normalized_range.is_some());
    assert_eq!(
        sentence_hit.text_locator.segmentation_version.as_deref(),
        Some("text-segmentation/v1")
    );

    let technical = search
        .execute(SearchDocumentCommand {
            document_id: document.id,
            query: "read-cursor/v2".into(),
            limit: 10,
        })
        .await
        .expect("technical identifier search");
    assert!(
        technical
            .hits
            .iter()
            .any(|hit| hit.candidate_kind == SearchCandidateKind::Paragraph)
    );
    assert!(
        technical
            .hits
            .iter()
            .any(|hit| hit.candidate_kind == SearchCandidateKind::Sentence)
    );
}

#[tokio::test]
async fn non_prose_is_searchable_as_paragraph_without_fake_sentence_candidate() {
    let mut document = fixture();
    document.root_sections[0].content =
        "```rust\nlet unsafe_marker = read_cursor_v2();\n```".into();
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemorySearchIndex::default());
    repository.save(document.clone()).await.expect("save");
    index.index(&document).await.expect("index");

    let result = SearchDocumentUseCase::new(index, repository)
        .execute(SearchDocumentCommand {
            document_id: document.id,
            query: "unsafe_marker".into(),
            limit: 10,
        })
        .await
        .expect("search code paragraph");

    assert!(
        result
            .hits
            .iter()
            .any(|hit| hit.candidate_kind == SearchCandidateKind::Paragraph)
    );
    assert!(
        !result
            .hits
            .iter()
            .any(|hit| hit.candidate_kind == SearchCandidateKind::Sentence)
    );
}

#[tokio::test]
async fn sqlite_lexical_candidates_and_tokenizer_version_survive_reopen() {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("reading.sqlite");
    let document = fixture();

    {
        let repository = Arc::new(SqliteDocumentRepository::open(&database).expect("repository"));
        let index = Arc::new(SqliteSearchIndex::open(&database).expect("index"));
        repository.save(document.clone()).await.expect("save");
        index.index(&document).await.expect("index document");
        let result = SearchDocumentUseCase::new(index, repository)
            .execute(SearchDocumentCommand {
                document_id: document.id.clone(),
                query: "物理帧".into(),
                limit: 10,
            })
            .await
            .expect("initial search");
        assert!(
            result
                .hits
                .iter()
                .any(|hit| hit.candidate_kind == SearchCandidateKind::Sentence)
        );
    }

    let repository =
        Arc::new(SqliteDocumentRepository::open(&database).expect("reopen repository"));
    let index = Arc::new(SqliteSearchIndex::open(&database).expect("reopen index"));
    assert_eq!(index.tokenizer_version(), LEXICAL_TOKENIZER_VERSION);
    let result = SearchDocumentUseCase::new(index, repository)
        .execute(SearchDocumentCommand {
            document_id: document.id,
            query: "内存机制".into(),
            limit: 10,
        })
        .await
        .expect("persistent CJK search");
    assert_eq!(result.tokenizer_version, LEXICAL_TOKENIZER_VERSION);
    assert!(
        result
            .hits
            .iter()
            .any(|hit| hit.candidate_kind == SearchCandidateKind::Section)
    );
}

fn fixture() -> Document {
    Document {
        id: DocumentId("doc:lexical-v2".into()),
        source: DocumentSource("memory:lexical-v2".into()),
        title: "系统机制".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:lexical-v2".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![Section {
            id: SectionId("section://virtual-memory".into()),
            parent_id: None,
            title: "虚拟内存机制".into(),
            level: 1,
            content: "地址空间隔离进程内存。页表把虚拟页映射到物理帧。\n\nUse read-cursor/v2 with std::sync::Arc safely.".into(),
            location: Location {
                section_path: vec!["虚拟内存机制".into()],
                native_location: Some("markdown:#virtual-memory".into()),
                ..Location::default()
            },
            children: vec![],
        }],
    }
}
