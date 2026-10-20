//! Benchmark-only profile of the section-text scan on real books: how long each phase of
//! `SqliteTextUnitIndex::scan_lexical` takes, and how many sections, paragraphs and sentences it
//! evaluates. Ignored by default; the book-profile workflow runs it with
//! `READING_MCP_SEARCH_PROFILE_DIR` pointing at the downloaded books.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use reading_mcp::application::ports::{DocumentRepository, Parser, RetrievedResource, SearchIndex};
use reading_mcp::domain::{DocumentSource, MediaType};
use reading_mcp::infrastructure::{
    ScanTimings, SqliteDocumentRepository, SqliteSearchIndex, SqliteTextUnitIndex,
};
use reading_mcp::parsing::{ArchiveLimits, ParserRouter};

/// The queries the book profile reports: one per shape the books are known for, plus a zero-hit one.
const QUERIES: [&str; 6] = [
    "皇帝",
    "節度使",
    "太祖",
    "Pierre",
    "寶玉",
    "不存在的詞組甲乙",
];

const RUNS: usize = 5;
const LIMIT: usize = 10;

#[tokio::test]
#[ignore]
async fn scan_search_phases_on_real_books() {
    let dir = match std::env::var("READING_MCP_SEARCH_PROFILE_DIR") {
        Ok(value) if !value.is_empty() => PathBuf::from(value),
        _ => {
            println!("READING_MCP_SEARCH_PROFILE_DIR is unset; skipping the scan phase profile");
            return;
        }
    };

    let mut books: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(read_dir) => read_dir
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("epub"))
            .collect(),
        Err(error) => {
            println!("search-profile dir unreadable: {error}");
            return;
        }
    };
    books.sort();
    if books.is_empty() {
        println!("search-profile dir {} holds no .epub book", dir.display());
        return;
    }

    let router = ParserRouter::release(2048, ArchiveLimits::default());

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
        let database = directory.path().join("profile.sqlite");
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
        let units = match SqliteTextUnitIndex::open(&database) {
            Ok(units) => units,
            Err(error) => {
                println!("{label}: text unit store open error: {error}");
                continue;
            }
        };

        println!("### {label}");
        println!();
        println!(
            "| Query | Hits | search ms (median) | read_text | section_prefilter | layout_decode | \
             paragraphs | sentences | sort | sections passed / scanned | units evaluated |"
        );
        println!("|---|---|---|---|---|---|---|---|---|---|---|");

        for query in QUERIES {
            let mut times: Vec<Duration> = Vec::with_capacity(RUNS);
            let mut hits = 0usize;
            let mut timings: Option<ScanTimings> = None;
            for run in 1..=RUNS {
                let start = Instant::now();
                match index.search_lexical(&document.id, query, LIMIT).await {
                    Ok(found) => hits = found.len(),
                    Err(error) => {
                        println!("| {query} | search error: {error} | | | | | | | | | |");
                        break;
                    }
                }
                times.push(start.elapsed());
                if run == RUNS {
                    match units.scan_lexical_profile(&document.id, query, LIMIT).await {
                        Ok(profile) => timings = profile,
                        Err(error) => {
                            println!("| {query} | profile error: {error} | | | | | | | | | |");
                            break;
                        }
                    }
                }
            }
            if times.len() != RUNS {
                continue;
            }
            times.sort();
            let median = times[RUNS / 2];
            let timings = timings.unwrap_or_default();
            println!(
                "| {query} | {hits} | {} | {} | {} | {} | {} | {} | {} | {} / {} | {} |",
                millis(median),
                millis(timings.read_text),
                millis(timings.section_prefilter),
                millis(timings.layout_decode),
                millis(timings.paragraphs),
                millis(timings.sentences),
                millis(timings.sort),
                timings.sections_passed_prefilter,
                timings.sections_scanned,
                timings.paragraphs_evaluated + timings.sentences_evaluated,
            );
        }
        println!();
    }
}

fn millis(elapsed: Duration) -> String {
    format!("{:.2}", elapsed.as_secs_f64() * 1000.0)
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
