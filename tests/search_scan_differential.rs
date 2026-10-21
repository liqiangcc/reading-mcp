//! Differential test for the fast lexical scan: the text cache, the parallel section scan and the
//! score-bound pruning must return exactly the hits of the plain scan — no cache, one thread, no
//! pruning — as whole result vectors.

use reading_mcp::application::ports::{DocumentRepository, Parser, RetrievedResource, SearchIndex};
use reading_mcp::domain::{Document, DocumentSource, MediaType};
use reading_mcp::infrastructure::{ScanOptions, SqliteDocumentRepository, SqliteSearchIndex};
use reading_mcp::parsing::{MarkdownParser, TextParser};

/// Every query of `tests/lexical_scan_parity.rs`, plus a single char that occurs everywhere.
const QUERIES: [&str; 15] = [
    "皇",
    "皇帝",
    "虚拟内存",
    "皇帝 内存",
    "memory",
    "virtual memory",
    "read-cursor/v2",
    "x86_64",
    "istanbul",
    "οδος",
    "layout",
    "memory quasar",
    "quasar",
    "the",
    "的",
];

const LIMITS: [usize; 4] = [1, 3, 10, 50];
const THREADS: [usize; 3] = [1, 2, 4];

#[tokio::test]
async fn the_fast_scan_matches_the_plain_scan() {
    let documents = [
        // Over one MiB of text, so the scan actually splits it across threads, and one sentence
        // repeated in every section, so many units tie.
        tied_scores_document().await,
        mixed_case_sections_document().await,
        final_sigma_document().await,
    ];
    for document in &documents {
        assert_parity(document).await;
    }
}

async fn assert_parity(document: &Document) {
    let directory = tempfile::tempdir().expect("temp directory");
    let database = directory.path().join("differential.sqlite");
    let repository = SqliteDocumentRepository::open(&database).expect("repository");
    repository
        .save(document.clone())
        .await
        .expect("save document");
    let index = SqliteSearchIndex::open(&database).expect("index");
    index.index(document).await.expect("index document");

    for query in QUERIES {
        for limit in LIMITS {
            let plain = index
                .search_lexical_with_options(&document.id, query, limit, ScanOptions::plain())
                .await
                .unwrap_or_else(|error| panic!("plain search {query:?} limit {limit}: {error}"));
            for threads in THREADS {
                let fast = index
                    .search_lexical_with_options(
                        &document.id,
                        query,
                        limit,
                        ScanOptions {
                            cache: true,
                            threads,
                            prune: true,
                        },
                    )
                    .await
                    .unwrap_or_else(|error| {
                        panic!("fast search {query:?} limit {limit} threads {threads}: {error}")
                    });
                assert_eq!(
                    fast, plain,
                    "query {query:?} at limit {limit} with {threads} threads on {}",
                    document.id.0
                );
            }
        }
    }
}

/// The same paragraph repeated in every section, and over one MiB of it in total, so every unit of
/// the document ties on score.
async fn tied_scores_document() -> Document {
    let sentence = "皇帝在朝堂上討論虛擬內存機制與頁表映射，群臣列坐，節度使入朝議事，\
                    地址空間隔離進程內存，頁表把虛擬頁映射到物理幀，進程只能看到自己的地址空間，\
                    頁表項記錄物理幀號，缺頁時由操作系統換入";
    let paragraph = format!("{sentence}{sentence}{sentence}{sentence}。");
    let paragraph_chars = paragraph.chars().count() + 2;
    let mut text = String::new();
    let mut chars = 0usize;
    let mut section = 0usize;
    while chars < 1_100_000 {
        let heading = format!("# 第{section}節\n\n");
        chars += heading.chars().count();
        text.push_str(&heading);
        for _ in 0..8 {
            text.push_str(&paragraph);
            text.push_str("\n\n");
            chars += paragraph_chars;
        }
        section += 1;
    }
    // Over the 1 MiB of text below which a scan stays serial.
    assert!(chars >= 1_048_576, "the document holds only {chars} chars");
    MarkdownParser
        .parse(resource("memory:tied.md", "text/markdown", text))
        .await
        .expect("parse tied scores document")
}

/// One section is all caseless CJK, so its whole-section lowercase is borrowed; another holds
/// uppercase Latin and a sigma, so it takes the owned path.
async fn mixed_case_sections_document() -> Document {
    let text = "# 宋史\n\n\
                皇帝在朝堂上讨论内存与页表。群臣议事，節度使入朝。\n\n\
                ## Notes\n\n\
                ΣΟΦΟΣ reads ΟΔΟΣ.ΟΔΟΣ beside the Uppercase Latin Layout. Memory layout.\n"
        .to_string();
    MarkdownParser
        .parse(resource("memory:cases.md", "text/markdown", text))
        .await
        .expect("parse mixed-case sections")
}

/// A final sigma, whose lowercase depends on the position in the slice it is cut from.
async fn final_sigma_document() -> Document {
    let mut text = String::new();
    for paragraph in 0..40 {
        text.push_str(&format!(
            "ΟΔΟΣ.ΟΔΟΣ closes paragraph {paragraph}. The layout of οδος.οδος repeats. \
             Memory of ΣΟΦΟΣ.ΟΔΟΣ stays.\n\n"
        ));
    }
    TextParser
        .parse(resource("memory:sigma.txt", "text/plain", text))
        .await
        .expect("parse final sigma text")
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
