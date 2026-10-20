//! Acceptance gate for the scan-based lexical search: `SqliteSearchIndex` must return exactly what
//! `InMemorySearchIndex` returns for the same document, query and limit.

use std::sync::Arc;

use reading_mcp::application::ports::{
    DocumentRepository, LexicalSearchHit, Parser, RetrievedResource, SearchIndex,
};
use reading_mcp::domain::{Document, DocumentSource, MediaType};
use reading_mcp::infrastructure::{
    InMemorySearchIndex, SqliteDocumentRepository, SqliteSearchIndex,
};
use reading_mcp::parsing::{MarkdownParser, TextParser};

/// One query of every shape the lexical contract distinguishes.
const QUERIES: [&str; 13] = [
    // A single CJK char.
    "皇",
    // A CJK bigram.
    "皇帝",
    // A CJK phrase of four chars.
    "虚拟内存",
    // An English word.
    "memory",
    // Two English words.
    "virtual memory",
    // Technical identifiers with punctuation.
    "read-cursor/v2",
    "x86_64",
    // A lowercase form of a text with a dotted capital I.
    "istanbul",
    // A lowercase form of a text with a final sigma.
    "οδος",
    // A title word.
    "layout",
    // A zero-hit multi-token query, which relaxes to the tokens that do occur.
    "memory quasar",
    // A single-token zero-hit query.
    "quasar",
    // A query matching very many sentences, to exercise the truncate path.
    "the",
];

const LIMITS: [usize; 4] = [1, 3, 10, 50];

#[tokio::test]
async fn scan_search_matches_the_in_memory_index() {
    let documents = [
        cjk_document().await,
        english_document().await,
        mixed_document().await,
        markdown_document().await,
    ];
    for document in &documents {
        assert_parity(document).await;
    }
}

async fn assert_parity(document: &Document) {
    let memory = InMemorySearchIndex::default();
    memory.index(document).await.expect("index in memory");

    let directory = tempfile::tempdir().expect("temp directory");

    let scanned_database = directory.path().join("scanned.sqlite");
    let scanned_repository =
        SqliteDocumentRepository::open(&scanned_database).expect("scanned repository");
    scanned_repository
        .save(document.clone())
        .await
        .expect("save scanned document");
    let scanned = SqliteSearchIndex::open(&scanned_database).expect("scanned index");
    scanned.index(document).await.expect("index stored text");

    let reused_database = directory.path().join("reused.sqlite");
    let reused_repository =
        SqliteDocumentRepository::open(&reused_database).expect("reused repository");
    reused_repository
        .save(document.clone())
        .await
        .expect("save reused document");
    let reused = SqliteSearchIndex::open(&reused_database).expect("reused index");
    reused
        .index_with_units(
            Arc::new(document.clone()),
            Arc::new(
                document
                    .try_paragraph_text_units()
                    .expect("paragraph units"),
            ),
            Arc::new(document.try_sentence_text_units().expect("sentence units")),
        )
        .await
        .expect("index caller units");

    for query in QUERIES {
        for limit in LIMITS {
            let expected = memory
                .search_lexical(&document.id, query, limit)
                .await
                .unwrap_or_else(|error| panic!("in-memory search {query:?}: {error}"));
            let scanned_hits = scanned
                .search_lexical(&document.id, query, limit)
                .await
                .unwrap_or_else(|error| panic!("scan search {query:?}: {error}"));
            assert_eq!(
                scanned_hits, expected,
                "scan hits differ for query {query:?} at limit {limit} on {}",
                document.id.0
            );
            let reused_hits = reused
                .search_lexical(&document.id, query, limit)
                .await
                .unwrap_or_else(|error| panic!("reused search {query:?}: {error}"));
            assert_eq!(
                reused_hits, expected,
                "caller-unit hits differ for query {query:?} at limit {limit} on {}",
                document.id.0
            );
            assert_ordered(&scanned_hits);
        }
    }
}

fn assert_ordered(hits: &[LexicalSearchHit]) {
    for pair in hits.windows(2) {
        assert!(
            pair[0].score >= pair[1].score,
            "hits must be ordered by score: {:?} then {:?}",
            pair[0].score,
            pair[1].score
        );
    }
}

fn resource(source: &str, media_type: &str, text: String) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource(source.into()),
        final_source: DocumentSource(source.into()),
        media_type: MediaType(media_type.into()),
        bytes: text.into_bytes(),
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

async fn cjk_document() -> Document {
    // More than two 16 KiB chunks, so one section spans several stored text chunks.
    let mut text = String::new();
    let mut paragraph = 0usize;
    while text.chars().count() < 40_000 {
        text.push_str(&format!(
            "第{paragraph}段：皇帝在朝堂上讨论虚拟内存机制与页表映射，页表把虚拟页映射到物理帧。\
             地址空间隔离进程内存，进程只能看到自己的地址空间。\n\n"
        ));
        paragraph += 1;
    }
    TextParser
        .parse(resource("memory:cjk.txt", "text/plain", text))
        .await
        .expect("parse CJK text")
}

async fn english_document() -> Document {
    let mut text = String::new();
    for paragraph in 0..40 {
        text.push_str(&format!(
            "Paragraph {paragraph} describes the virtual memory of the system. \
             The kernel maps each page into the address space. \
             The layout of the tables is the same for every process. \
             Memory access goes through the page tables.\n\n"
        ));
    }
    TextParser
        .parse(resource("memory:english.txt", "text/plain", text))
        .await
        .expect("parse English text")
}

async fn mixed_document() -> Document {
    let text = "Technical notes: use x86_64 with read-cursor/v2 and std::sync::Arc carefully. \
                İstanbul is a city. Fullwidth ＡＢＣ appears here. Greek letters ΟΔΟΣ.ΟΔΟΣ end this paragraph.\n\n\
                A second paragraph repeats x86_64 and İstanbul. \
                The last sentence mentions memory once.\n"
        .to_string();
    TextParser
        .parse(resource("memory:mixed.txt", "text/plain", text))
        .await
        .expect("parse mixed text")
}

async fn markdown_document() -> Document {
    let text = "# Virtual Memory\n\n\
                Memory is split into pages. The kernel maps each page.\n\n\
                ## Storage Layout\n\n\
                The layout stores sections. Each section has a title.\n\n\
                ### Page Tables\n\n\
                Page tables map virtual pages. Memory access goes through them.\n"
        .to_string();
    MarkdownParser
        .parse(resource("memory:sections.md", "text/markdown", text))
        .await
        .expect("parse markdown")
}
