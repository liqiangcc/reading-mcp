//! Differential test on the real books: the fast lexical scan — the text cache, the parallel section
//! scan and the score-bound pruning — must return exactly the plain scan's hits. Ignored by default;
//! the book-profile workflow runs it with `READING_MCP_SEARCH_PROFILE_DIR` pointing at the downloaded
//! books, and it reports the number of mismatching results.

use std::path::PathBuf;

use reading_mcp::application::ports::{DocumentRepository, Parser, RetrievedResource, SearchIndex};
use reading_mcp::domain::{DocumentSource, MediaType};
use reading_mcp::infrastructure::{ScanOptions, SqliteDocumentRepository, SqliteSearchIndex};
use reading_mcp::parsing::{ArchiveLimits, ParserRouter};

/// The queries the book profile reports.
const QUERIES: [&str; 8] = [
    "皇帝",
    "節度使",
    "太祖",
    "Pierre",
    "寶玉",
    "不存在的詞組甲乙",
    "的",
    "Pierre Natasha quasar",
];

const LIMITS: [usize; 3] = [1, 10, 50];

#[tokio::test]
#[ignore]
async fn the_fast_scan_matches_the_plain_scan_on_real_books() {
    let dir = match std::env::var("READING_MCP_SEARCH_PROFILE_DIR") {
        Ok(value) if !value.is_empty() => PathBuf::from(value),
        _ => {
            println!("READING_MCP_SEARCH_PROFILE_DIR is unset; skipping the scan parity profile");
            return;
        }
    };

    let mut books: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(read_dir) => read_dir
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("epub"))
            .collect(),
        Err(error) => {
            println!("scan-parity dir unreadable: {error}");
            return;
        }
    };
    books.sort();
    if books.is_empty() {
        println!("scan-parity dir {} holds no .epub book", dir.display());
        return;
    }

    let router = ParserRouter::release(2048, ArchiveLimits::default());
    let mut mismatches = 0usize;

    println!("| Book | Query | Limit | Plain hits | Fast hits | Mismatch |");
    println!("|---|---|---|---|---|---|");

    for path in &books {
        let Some(label) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                println!("{label}: read error: {error}");
                continue;
            }
        };
        let document = match router
            .parse(resource(&label, "application/epub+zip", bytes))
            .await
        {
            Ok(document) => document,
            Err(error) => {
                println!("{label}: parse error: {error}");
                continue;
            }
        };
        let directory = match tempfile::tempdir() {
            Ok(directory) => directory,
            Err(error) => {
                println!("{label}: temp directory error: {error}");
                continue;
            }
        };
        let database = directory.path().join("parity.sqlite");
        let repository = match SqliteDocumentRepository::open(&database) {
            Ok(repository) => repository,
            Err(error) => {
                println!("{label}: repository open error: {error}");
                continue;
            }
        };
        if let Err(error) = repository.save(document.clone()).await {
            println!("{label}: save error: {error}");
            continue;
        }
        let index = match SqliteSearchIndex::open(&database) {
            Ok(index) => index,
            Err(error) => {
                println!("{label}: index open error: {error}");
                continue;
            }
        };
        if let Err(error) = index.index(&document).await {
            println!("{label}: index error: {error}");
            continue;
        }

        for query in QUERIES {
            for limit in LIMITS {
                let plain = match index
                    .search_lexical_with_options(&document.id, query, limit, ScanOptions::plain())
                    .await
                {
                    Ok(hits) => hits,
                    Err(error) => {
                        println!("| {label} | {query} | {limit} | error: {error} | | |");
                        mismatches += 1;
                        continue;
                    }
                };
                let fast = match index
                    .search_lexical_with_options(
                        &document.id,
                        query,
                        limit,
                        ScanOptions::production(),
                    )
                    .await
                {
                    Ok(hits) => hits,
                    Err(error) => {
                        println!("| {label} | {query} | {limit} | | error: {error} | |");
                        mismatches += 1;
                        continue;
                    }
                };
                let mismatch = plain != fast;
                if mismatch {
                    mismatches += 1;
                }
                println!(
                    "| {label} | {query} | {limit} | {} | {} | {} |",
                    plain.len(),
                    fast.len(),
                    if mismatch { "yes" } else { "no" }
                );
            }
        }
    }

    println!();
    println!("scan parity mismatches: {mismatches}");
    assert_eq!(mismatches, 0, "the fast scan differs from the plain scan");
}

fn resource(name: &str, media_type: &str, bytes: Vec<u8>) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource(format!("memory:{name}")),
        final_source: DocumentSource(format!("memory:{name}")),
        media_type: MediaType(media_type.into()),
        bytes,
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}
