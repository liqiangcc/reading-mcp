use std::collections::BTreeMap;
use std::sync::Arc;

use reading_mcp::application::ports::{DocumentRepository, LEXICAL_TOKENIZER_VERSION, SearchIndex};
use reading_mcp::application::search_document::{
    LocatedSearchHit, SearchCandidateKind, SearchDocumentCommand, SearchDocumentUseCase,
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
        Some("text-segmentation/v3")
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

#[tokio::test]
async fn missing_derived_lexical_state_rebuilds_from_persisted_canonical_document() {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("rebuild.sqlite");
    let document = fixture();
    let repository = Arc::new(SqliteDocumentRepository::open(&database).expect("repository"));
    repository
        .save(document.clone())
        .await
        .expect("save canonical document");
    let index = Arc::new(SqliteSearchIndex::open(&database).expect("empty lexical index"));

    let result = SearchDocumentUseCase::new(index, repository)
        .execute(SearchDocumentCommand {
            document_id: document.id,
            query: "物理帧".into(),
            limit: 10,
        })
        .await
        .expect("search should rebuild missing derived state without re-opening source");

    assert!(
        result
            .hits
            .iter()
            .any(|hit| hit.candidate_kind == SearchCandidateKind::Sentence)
    );
}

#[tokio::test]
async fn strict_multi_token_hits_do_not_fall_back_to_relaxed() {
    let document = relaxed_fixture();
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemorySearchIndex::default());
    repository.save(document.clone()).await.expect("save");
    index.index(&document).await.expect("index");
    let search = SearchDocumentUseCase::new(index, repository);

    // "shared" also lives in section B; a relaxed pass would leak B into the
    // results, so asserting A-only hits proves strict AND stayed authoritative.
    let result = search
        .execute(SearchDocumentCommand {
            document_id: document.id,
            query: "shared apple".into(),
            limit: 10,
        })
        .await
        .expect("strict search");

    assert!(!result.hits.is_empty());
    assert!(
        result
            .hits
            .iter()
            .all(|hit| hit.section_id.0 == "section://alpha"),
        "relaxed fallback must not run when strict AND has hits"
    );
}

#[tokio::test]
async fn zero_hit_multi_token_query_falls_back_to_relaxed_or() {
    let document = relaxed_fixture();
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemorySearchIndex::default());
    repository.save(document.clone()).await.expect("save");
    index.index(&document).await.expect("index");
    let search = SearchDocumentUseCase::new(index, repository);

    // "quasar" appears nowhere, so strict AND cannot hit; the relaxed pass
    // must surface only candidates that actually contain "apple".
    let result = search
        .execute(SearchDocumentCommand {
            document_id: document.id.clone(),
            query: "apple quasar".into(),
            limit: 10,
        })
        .await
        .expect("relaxed search");
    assert!(!result.hits.is_empty());
    assert!(
        result
            .hits
            .iter()
            .all(|hit| hit.section_id.0 == "section://alpha"),
        "unrelated tokens must not pull in unrelated sections"
    );

    let again = search
        .execute(SearchDocumentCommand {
            document_id: document.id,
            query: "apple quasar".into(),
            limit: 10,
        })
        .await
        .expect("relaxed search repeat");
    let key = |hits: &[LocatedSearchHit]| {
        hits.iter()
            .map(|hit| {
                (
                    hit.section_id.0.clone(),
                    hit.text_locator
                        .normalized_range
                        .map(|r| (r.start(), r.end())),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        key(&result.hits),
        key(&again.hits),
        "ordering must be deterministic"
    );
}

#[tokio::test]
async fn single_token_query_is_never_relaxed() {
    let document = relaxed_fixture();
    let repository = Arc::new(InMemoryDocumentRepository::default());
    let index = Arc::new(InMemorySearchIndex::default());
    repository.save(document.clone()).await.expect("save");
    index.index(&document).await.expect("index");

    let result = SearchDocumentUseCase::new(index, repository)
        .execute(SearchDocumentCommand {
            document_id: document.id,
            query: "quasar".into(),
            limit: 10,
        })
        .await
        .expect("single token search");
    assert!(result.hits.is_empty());
}

#[tokio::test]
async fn sqlite_and_in_memory_relaxed_fallback_have_parity() {
    let document = relaxed_fixture();
    let directory = tempfile::tempdir().expect("temp directory");

    let memory_repository = Arc::new(InMemoryDocumentRepository::default());
    let memory_index = Arc::new(InMemorySearchIndex::default());
    memory_repository
        .save(document.clone())
        .await
        .expect("save");
    memory_index.index(&document).await.expect("index");

    let sqlite_repository =
        Arc::new(SqliteDocumentRepository::open(directory.path().join("r.sqlite")).expect("repo"));
    let sqlite_index =
        Arc::new(SqliteSearchIndex::open(directory.path().join("r.sqlite")).expect("index"));
    sqlite_repository
        .save(document.clone())
        .await
        .expect("save");
    sqlite_index.index(&document).await.expect("index");

    let key = |hits: &[LocatedSearchHit]| {
        let mut keys = hits
            .iter()
            .map(|hit| {
                (
                    hit.section_id.0.clone(),
                    match hit.candidate_kind {
                        SearchCandidateKind::Section => 0u8,
                        SearchCandidateKind::Paragraph => 1,
                        SearchCandidateKind::Sentence => 2,
                    },
                    hit.text_locator
                        .normalized_range
                        .map(|r| (r.start(), r.end())),
                )
            })
            .collect::<Vec<_>>();
        keys.sort();
        keys
    };

    for query in ["shared apple", "apple quasar"] {
        let memory = SearchDocumentUseCase::new(memory_index.clone(), memory_repository.clone())
            .execute(SearchDocumentCommand {
                document_id: document.id.clone(),
                query: query.into(),
                limit: 10,
            })
            .await
            .expect("in-memory search");
        let sqlite = SearchDocumentUseCase::new(sqlite_index.clone(), sqlite_repository.clone())
            .execute(SearchDocumentCommand {
                document_id: document.id.clone(),
                query: query.into(),
                limit: 10,
            })
            .await
            .expect("sqlite search");
        assert_eq!(
            key(&memory.hits),
            key(&sqlite.hits),
            "backend parity for query {query:?}"
        );
    }
}

#[tokio::test]
async fn long_paragraph_snippet_is_centered_on_late_match() {
    let mut document = fixture();
    let tail = "zephyr";
    let mut paragraph = "lead-in ".repeat(60);
    paragraph.push_str("terminus ");
    paragraph.push_str(tail);
    paragraph.push('.');
    document.root_sections[0].content = paragraph;

    for index in [
        Arc::new(InMemorySearchIndex::default()) as Arc<dyn SearchIndex>,
        {
            let directory = tempfile::tempdir().expect("temp directory");
            let path = directory.keep().join("snippet.sqlite");
            Arc::new(SqliteSearchIndex::open(path).expect("sqlite index")) as Arc<dyn SearchIndex>
        },
    ] {
        index.index(&document).await.expect("index");
        let repository = Arc::new(InMemoryDocumentRepository::default());
        repository.save(document.clone()).await.expect("save");
        let result = SearchDocumentUseCase::new(index, repository)
            .execute(SearchDocumentCommand {
                document_id: document.id.clone(),
                query: tail.into(),
                limit: 10,
            })
            .await
            .expect("search");
        let paragraph_hit = result
            .hits
            .iter()
            .find(|hit| hit.candidate_kind == SearchCandidateKind::Paragraph)
            .expect("paragraph hit");
        assert!(
            paragraph_hit.snippet.contains(tail),
            "snippet must contain the late match: {:?}",
            paragraph_hit.snippet
        );
        assert!(
            paragraph_hit.snippet.starts_with('…'),
            "clipped leading side must be marked: {:?}",
            paragraph_hit.snippet
        );
        assert!(
            paragraph_hit.snippet.chars().count() <= 320,
            "snippet must stay bounded"
        );
    }
}

#[tokio::test]
async fn sqlite_search_is_scoped_to_the_matched_document() {
    let directory = tempfile::tempdir().expect("temp directory");
    let document_a = fixture();
    let mut document_b = fixture();
    document_b.id = DocumentId("doc:other".into());
    document_b.source = DocumentSource("memory:other".into());
    document_b.title = "另一系统机制".into();
    document_b.content_hash = ContentHash("sha256:other".into());
    document_b.root_sections[0].id = SectionId("section://other".into());

    let alone = Arc::new(
        SqliteSearchIndex::open(directory.path().join("alone.sqlite")).expect("alone index"),
    );
    alone.index(&document_a).await.expect("index alone");
    let alone_hits = alone
        .search_lexical(&document_a.id, "物理帧", 10)
        .await
        .expect("search alone");
    assert!(!alone_hits.is_empty());

    let shared = Arc::new(
        SqliteSearchIndex::open(directory.path().join("shared.sqlite")).expect("shared index"),
    );
    shared.index(&document_a).await.expect("index a");
    shared.index(&document_b).await.expect("index b");

    let scoped_hits = shared
        .search_lexical(&document_a.id, "物理帧", 10)
        .await
        .expect("search a");
    assert_eq!(scoped_hits.len(), alone_hits.len());
    for (scoped, alone) in scoped_hits.iter().zip(alone_hits.iter()) {
        assert_eq!(scoped.section_id, alone.section_id);
        assert_eq!(scoped.candidate_kind, alone.candidate_kind);
        assert_eq!(scoped.snippet, alone.snippet);
        assert_eq!(scoped.text_locator, alone.text_locator);
        assert_eq!(scoped.source, document_a.source);
    }

    let other_hits = shared
        .search_lexical(&document_b.id, "物理帧", 10)
        .await
        .expect("search b");
    assert!(!other_hits.is_empty());
    assert!(
        other_hits
            .iter()
            .all(|hit| hit.section_id == SectionId("section://other".into())),
        "document B must only surface its own candidates"
    );
}

#[tokio::test]
async fn sqlite_reindex_replaces_previous_candidate_rows() {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("reindex.sqlite");
    let document = fixture();
    let index = Arc::new(SqliteSearchIndex::open(&database).expect("index"));

    index.index(&document).await.expect("first index");
    let connection = rusqlite::Connection::open(&database).expect("inspect");
    let first_candidates: i64 = connection
        .query_row(
            "SELECT count(*) FROM lexical_search_candidates_v5",
            [],
            |row| row.get(0),
        )
        .expect("candidate count");
    let first_fts: i64 = connection
        .query_row("SELECT count(*) FROM lexical_search_fts_v5", [], |row| {
            row.get(0)
        })
        .expect("fts count");
    assert!(first_candidates > 0);
    assert_eq!(first_candidates, first_fts);
    drop(connection);

    index.index(&document).await.expect("second index");
    let connection = rusqlite::Connection::open(&database).expect("inspect again");
    let candidates: i64 = connection
        .query_row(
            "SELECT count(*) FROM lexical_search_candidates_v5",
            [],
            |row| row.get(0),
        )
        .expect("candidate count");
    let fts: i64 = connection
        .query_row("SELECT count(*) FROM lexical_search_fts_v5", [], |row| {
            row.get(0)
        })
        .expect("fts count");
    assert_eq!(candidates, first_candidates);
    assert_eq!(fts, first_fts);

    let hits = index
        .search_lexical(&document.id, "物理帧", 50)
        .await
        .expect("search after reindex");
    let mut seen = std::collections::HashSet::new();
    for hit in &hits {
        assert!(
            seen.insert((
                hit.candidate_kind,
                hit.section_id.clone(),
                hit.location.paragraph,
                hit.location.native_location.clone(),
            )),
            "re-index must not produce duplicate hits"
        );
    }
}

fn relaxed_fixture() -> Document {
    Document {
        id: DocumentId("doc:relaxed".into()),
        source: DocumentSource("memory:relaxed".into()),
        title: "Relaxed".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:relaxed".into()),
        metadata: BTreeMap::new(),
        root_sections: vec![
            Section {
                id: SectionId("section://alpha".into()),
                parent_id: None,
                title: "Alpha".into(),
                level: 1,
                content: "Alpha paragraph has shared anchor plus apple term.".into(),
                location: Location::default(),
                children: vec![],
            },
            Section {
                id: SectionId("section://beta".into()),
                parent_id: None,
                title: "Beta".into(),
                level: 1,
                content: "Beta paragraph has shared anchor plus banana term.".into(),
                location: Location::default(),
                children: vec![],
            },
        ],
    }
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
