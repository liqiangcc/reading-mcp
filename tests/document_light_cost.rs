//! The cache-free cost of the two new repository calls on a real book.
//!
//! `#[ignore]`d because it ingests a whole book: run it with
//! `READING_MCP_LIGHT_COST_DOCUMENT=<path> cargo test --release --test document_light_cost --
//! --ignored --nocapture`. It ingests the file into a fresh state directory the way the open use
//! case does (section text first, then the `documents` row), opens the repository with the decoded
//! document cache off, and times `get_light_with_hash` and `load_section_text` for one mid-book
//! section over 200 calls each, with the read-character count of the same window.
//!
//! The target reported as pass/fail is 3 ms for both, on all three profile inputs.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reading_mcp::application::ports::{
    DocumentRepository, Parser, RetrievedResource, TextUnitIndex,
};
use reading_mcp::domain::{Document, DocumentSource, MediaType, Section, SectionId};
use reading_mcp::infrastructure::{SqliteDocumentRepository, SqliteTextUnitIndex};
use reading_mcp::parsing::{ArchiveLimits, ParserRouter};
use rusqlite::{Connection, params};

const CALLS: usize = 200;
const TARGET_MS: f64 = 3.0;

fn media_type_for(path: &PathBuf) -> Option<&'static str> {
    match path.extension().and_then(|extension| extension.to_str())? {
        "epub" => Some("application/epub+zip"),
        "txt" => Some("text/plain"),
        "md" => Some("text/markdown"),
        "html" | "htm" => Some("text/html"),
        "pdf" => Some("application/pdf"),
        _ => None,
    }
}

fn resource(name: &str, media_type: &str, bytes: Vec<u8>) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource(format!("memory:{name}")),
        final_source: DocumentSource(format!("memory:{name}")),
        media_type: MediaType(media_type.into()),
        bytes,
        etag: None,
        last_modified: None,
        metadata: BTreeMap::new(),
    }
}

/// The `rchar` counter of this process: the bytes it has read through any read call.
fn rchar() -> u64 {
    let mut text = String::new();
    std::fs::File::open("/proc/self/io")
        .and_then(|mut file| file.read_to_string(&mut text))
        .map(|_| {
            text.lines()
                .find_map(|line| line.strip_prefix("rchar: "))
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn summary(samples: &mut [f64]) -> (f64, f64, f64) {
    samples.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
    let count = samples.len();
    let mean = samples.iter().sum::<f64>() / count as f64;
    let median = samples[count / 2];
    let p95 = samples[((count as f64 * 0.95) as usize).min(count - 1)];
    (mean, median, p95)
}

fn all_sections(sections: &[Section], out: &mut Vec<SectionId>) {
    for section in sections {
        out.push(section.id.clone());
        all_sections(&section.children, out);
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn the_light_reads_cost_a_page() {
    let Ok(document_path) = std::env::var("READING_MCP_LIGHT_COST_DOCUMENT") else {
        println!("READING_MCP_LIGHT_COST_DOCUMENT is not set; nothing to measure");
        return;
    };
    let document_path = PathBuf::from(document_path);
    let label =
        std::env::var("READING_MCP_LIGHT_COST_LABEL").unwrap_or_else(|_| "document".to_string());
    let Some(media_type) = media_type_for(&document_path) else {
        panic!("unsupported input: {}", document_path.display());
    };
    let bytes = std::fs::read(&document_path).expect("input should read");
    let name = document_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document")
        .to_string();

    let directory = tempfile::tempdir().expect("temporary directory");
    let database = directory.path().join("state.sqlite");
    let router = ParserRouter::release(2048, ArchiveLimits::default());
    let document: Arc<Document> = Arc::new(
        router
            .parse(resource(&name, media_type, bytes))
            .await
            .expect("input should parse"),
    );
    let segmentation = Arc::new(
        document
            .try_segmentation()
            .expect("document should segment into boundaries"),
    );
    let text_units = SqliteTextUnitIndex::open(&database).expect("text unit index should open");
    text_units
        .replace_document_segmentation(&document, segmentation)
        .await
        .expect("section text should store");
    // Capacity 0: the cache-free read this measures must not be a cache hit.
    let repository = SqliteDocumentRepository::open_with_cache_capacity(&database, 0)
        .expect("repository should open");
    repository
        .save_shared(Arc::clone(&document))
        .await
        .expect("document should save");

    let mut sections = Vec::new();
    all_sections(&document.root_sections, &mut sections);
    assert!(!sections.is_empty(), "the document must have sections");
    let mid_book = sections[sections.len() / 2].clone();
    let document_id = document.id.clone();

    // The stored document_json of the row, for the record.
    let document_json_bytes: i64 = Connection::open(&database)
        .expect("database should open")
        .query_row(
            "SELECT length(CAST(document_json AS BLOB)) FROM documents WHERE id = ?1",
            params![&document_id.0],
            |row| row.get(0),
        )
        .expect("document_json should read");

    for _ in 0..5 {
        repository
            .get_light_with_hash(&document_id)
            .await
            .expect("warm-up light read");
        repository
            .load_section_text(&document_id, &mid_book)
            .await
            .expect("warm-up section read");
    }

    let mut light_samples = Vec::with_capacity(CALLS);
    let before = rchar();
    for _ in 0..CALLS {
        let started = Instant::now();
        let loaded = repository
            .get_light_with_hash(&document_id)
            .await
            .expect("light read");
        light_samples.push(millis(started.elapsed()));
        assert!(loaded.is_some(), "the document must exist");
    }
    let light_rchar = rchar().saturating_sub(before) as f64 / CALLS as f64;

    let mut section_samples = Vec::with_capacity(CALLS);
    let before = rchar();
    for _ in 0..CALLS {
        let started = Instant::now();
        let text = repository
            .load_section_text(&document_id, &mid_book)
            .await
            .expect("section read");
        section_samples.push(millis(started.elapsed()));
        assert!(text.is_some(), "the mid-book section must have text");
    }
    let section_rchar = rchar().saturating_sub(before) as f64 / CALLS as f64;

    let (light_mean, light_median, light_p95) = summary(&mut light_samples);
    let (section_mean, section_median, section_p95) = summary(&mut section_samples);
    let light_pass = light_p95 <= TARGET_MS;
    let section_pass = section_p95 <= TARGET_MS;
    println!(
        "label={label} document_json_bytes={document_json_bytes} sections={} \
         light_mean_ms={light_mean:.3} light_median_ms={light_median:.3} light_p95_ms={light_p95:.3} \
         light_rchar_per_call={light_rchar:.0} light_target={} \
         section_mean_ms={section_mean:.3} section_median_ms={section_median:.3} \
         section_p95_ms={section_p95:.3} section_rchar_per_call={section_rchar:.0} section_target={}",
        sections.len(),
        if light_pass { "pass" } else { "fail" },
        if section_pass { "pass" } else { "fail" },
    );
}
