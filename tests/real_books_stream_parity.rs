//! Real-book stream, position and summary parity: every section's streams, every unit's position
//! and every summary from `section_layout_chunk_v2` against the in-memory planner. Ignored by
//! default: it needs the downloaded books.
//!
//! Run with `READING_MCP_BOOK_DIR=<dir> cargo test --release --test real_books_stream_parity --
//! --ignored --nocapture`. Every book in that directory must report 0 mismatches.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use reading_mcp::application::get_text_units::{
    EffectiveTextUnitKind, RequestedTextUnitKind, TextUnitCoveragePolicy, TextUnitDirection,
};
use reading_mcp::application::ports::{
    Parser, RetrievedResource, SectionTextUnits, TextUnitIndex, TextUnitStreamKey,
};
use reading_mcp::application::text_unit_stream::{StreamUnit, plan_section_stream};
use reading_mcp::domain::{DocumentSource, MediaType, Section, SectionId};
use reading_mcp::infrastructure::{InMemoryTextUnitIndex, SqliteTextUnitIndex};
use reading_mcp::parsing::{ArchiveLimits, ParserRouter};
use tempfile::tempdir;

const PAGE: usize = 256;
const KINDS: [RequestedTextUnitKind; 2] = [
    RequestedTextUnitKind::Paragraph,
    RequestedTextUnitKind::Sentence,
];
const POLICIES: [TextUnitCoveragePolicy; 2] = [
    TextUnitCoveragePolicy::PreserveSource,
    TextUnitCoveragePolicy::EligibleOnly,
];

#[derive(Default)]
struct Report {
    sections: usize,
    streams: usize,
    pages: usize,
    positions: usize,
    mismatches: Vec<String>,
}

#[tokio::test]
#[ignore = "needs READING_MCP_BOOK_DIR with the downloaded books"]
async fn real_books_streams_match_the_in_memory_planner() {
    let directory = std::env::var("READING_MCP_BOOK_DIR")
        .expect("READING_MCP_BOOK_DIR must point at the downloaded books");
    let books = book_files(Path::new(&directory));
    assert!(!books.is_empty(), "no books found in {directory}");

    let router = ParserRouter::release(2048, ArchiveLimits::default());
    let mut total = Report::default();
    for book in &books {
        println!("=== {}", book.display());
        let mut report = Report::default();
        compare_book(&router, book, &mut report).await;
        println!(
            "{}: sections={} streams={} pages={} positions={} mismatches={}",
            book.display(),
            report.sections,
            report.streams,
            report.pages,
            report.positions,
            report.mismatches.len()
        );
        total.sections += report.sections;
        total.streams += report.streams;
        total.pages += report.pages;
        total.positions += report.positions;
        total.mismatches.extend(report.mismatches);
    }
    for mismatch in &total.mismatches {
        println!("  mismatch {mismatch}");
    }
    println!(
        "TOTAL sections={} streams={} pages={} positions={} mismatches={}",
        total.sections,
        total.streams,
        total.pages,
        total.positions,
        total.mismatches.len()
    );
    assert_eq!(
        total.mismatches.len(),
        0,
        "stored streams must match the in-memory planner"
    );
}

fn book_files(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("epub") {
            files.push(path);
        }
    }
    files.sort();
    files
}

async fn compare_book(router: &ParserRouter, path: &Path, report: &mut Report) {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("<book>")
        .to_owned();
    let bytes = std::fs::read(path).expect("book should read");
    let document = Arc::new(
        router
            .parse(resource(&name, bytes))
            .await
            .expect("book should parse"),
    );
    let (paragraphs, sentences) = document.try_text_unit_sets().expect("book should segment");
    let (paragraphs, sentences) = (Arc::new(paragraphs), Arc::new(sentences));

    let directory = tempdir().expect("temporary directory should be created");
    let sqlite = SqliteTextUnitIndex::open(directory.path().join("units.sqlite"))
        .expect("sqlite index should open");
    let memory = InMemoryTextUnitIndex::default();
    for index in [&sqlite as &dyn TextUnitIndex, &memory as &dyn TextUnitIndex] {
        index
            .replace_document_sets(&document, Arc::clone(&paragraphs), Arc::clone(&sentences))
            .await
            .expect("unit sets should persist");
    }

    let mut sections = Vec::new();
    for root in &document.root_sections {
        collect_sections(root, &mut sections);
    }
    report.sections += sections.len();

    for section_id in sections {
        let units = SectionTextUnits::from_sets(&paragraphs, &sentences, &section_id);
        for kind in KINDS {
            for policy in POLICIES {
                let key = TextUnitStreamKey {
                    document_id: &document.id,
                    section_id: &section_id,
                    kind,
                    policy,
                };
                let label = format!(
                    "{name} section {} kind {kind:?} policy {policy:?}",
                    section_id.0
                );
                compare_stream(&sqlite, &memory, key, &units, &label, report).await;
            }
        }
    }
}

fn collect_sections(section: &Section, out: &mut Vec<SectionId>) {
    out.push(section.id.clone());
    for child in &section.children {
        collect_sections(child, out);
    }
}

async fn compare_stream(
    sqlite: &dyn TextUnitIndex,
    memory: &dyn TextUnitIndex,
    key: TextUnitStreamKey<'_>,
    units: &SectionTextUnits,
    label: &str,
    report: &mut Report,
) {
    report.streams += 1;
    let sqlite_summary = sqlite.section_stream_summary(key).await;
    let memory_summary = memory.section_stream_summary(key).await;
    let (sqlite_summary, memory_summary) = match (sqlite_summary, memory_summary) {
        (Ok(Some(left)), Ok(Some(right))) => (left, right),
        (left, right) => {
            if left != right {
                report.mismatches.push(format!(
                    "{label}: summary differs: sqlite={left:?} memory={right:?}"
                ));
            }
            return;
        }
    };
    if sqlite_summary != memory_summary {
        report.mismatches.push(format!(
            "{label}: summary differs: sqlite={sqlite_summary:?} memory={memory_summary:?}"
        ));
    }

    compare_pages(
        sqlite,
        memory,
        key,
        TextUnitDirection::Forward,
        sqlite_summary.total_items,
        label,
        report,
    )
    .await;
    compare_pages(
        sqlite,
        memory,
        key,
        TextUnitDirection::Backward,
        sqlite_summary.total_items,
        label,
        report,
    )
    .await;
    compare_positions(sqlite, key, units, label, report).await;
}

async fn compare_pages(
    sqlite: &dyn TextUnitIndex,
    memory: &dyn TextUnitIndex,
    key: TextUnitStreamKey<'_>,
    direction: TextUnitDirection,
    total_items: usize,
    label: &str,
    report: &mut Report,
) {
    let mut position = match direction {
        TextUnitDirection::Forward => 0,
        TextUnitDirection::Backward => total_items,
    };
    loop {
        let left = sqlite
            .section_stream_items(key, direction, position, PAGE)
            .await;
        let right = memory
            .section_stream_items(key, direction, position, PAGE)
            .await;
        report.pages += 1;
        let length = match (&left, &right) {
            (Ok(left), Ok(right)) => {
                if left != right {
                    report.mismatches.push(format!(
                        "{label}: {direction:?} page at {position} differs: \
                         sqlite={} memory={} items",
                        left.len(),
                        right.len()
                    ));
                }
                left.len()
            }
            (left, right) => {
                if left != right {
                    report.mismatches.push(format!(
                        "{label}: {direction:?} page at {position} differs: \
                         sqlite={left:?} memory={right:?}"
                    ));
                }
                return;
            }
        };
        match direction {
            TextUnitDirection::Forward => {
                position += length;
                if length == 0 || position >= total_items {
                    return;
                }
            }
            TextUnitDirection::Backward => {
                if length == 0 {
                    return;
                }
                position = position.saturating_sub(length);
                if position == 0 {
                    return;
                }
            }
        }
    }
}

async fn compare_positions(
    sqlite: &dyn TextUnitIndex,
    key: TextUnitStreamKey<'_>,
    units: &SectionTextUnits,
    label: &str,
    report: &mut Report,
) {
    // The default position lookup plans the whole section per call, so on the largest real
    // sections probing every unit against it is quadratic. Plan once and scan that plan instead;
    // the first entry of a unit wins, exactly as the planner's own lookup does.
    let paragraphs = units.paragraphs.iter().collect::<Vec<_>>();
    let sentences = units.sentences.iter().collect::<Vec<_>>();
    let sentence_coverage = units.sentence_coverage.iter().collect::<Vec<_>>();
    let plan = plan_section_stream(
        key.section_id,
        &paragraphs,
        units.paragraph_coverage.as_ref(),
        &sentences,
        &sentence_coverage,
        key.kind,
    )
    .expect("the planner should plan every stored section");
    let mut expected: BTreeMap<(&'static str, usize, Option<usize>), usize> = BTreeMap::new();
    let mut eligible_position = 0usize;
    for (position, entry) in plan.entries.iter().enumerate() {
        let position = match key.policy {
            TextUnitCoveragePolicy::PreserveSource => position,
            TextUnitCoveragePolicy::EligibleOnly => {
                if !entry.eligible {
                    continue;
                }
                let position = eligible_position;
                eligible_position += 1;
                position
            }
        };
        let unit = match entry.unit {
            StreamUnit::Paragraph(unit) => (
                EffectiveTextUnitKind::Paragraph.as_str(),
                unit.paragraph_index,
                None,
            ),
            StreamUnit::Sentence(unit) => (
                EffectiveTextUnitKind::Sentence.as_str(),
                unit.paragraph_index,
                Some(unit.sentence_index),
            ),
        };
        expected.entry(unit).or_insert(position);
    }

    let mut probes: Vec<(EffectiveTextUnitKind, usize, Option<usize>)> = units
        .paragraphs
        .iter()
        .map(|unit| (EffectiveTextUnitKind::Paragraph, unit.paragraph_index, None))
        .collect();
    probes.extend(units.sentences.iter().map(|unit| {
        (
            EffectiveTextUnitKind::Sentence,
            unit.paragraph_index,
            Some(unit.sentence_index),
        )
    }));
    probes.push((EffectiveTextUnitKind::Paragraph, 9999, None));
    probes.push((EffectiveTextUnitKind::Sentence, 9999, Some(0)));

    for (unit_kind, paragraph_index, sentence_index) in probes {
        report.positions += 1;
        let actual = match sqlite
            .section_stream_position(key, unit_kind, paragraph_index, sentence_index)
            .await
        {
            Ok(position) => position,
            Err(error) => {
                report.mismatches.push(format!(
                    "{label}: position of {unit_kind:?} {paragraph_index}/{sentence_index:?} \
                     failed: {error:?}"
                ));
                continue;
            }
        };
        let expected = expected
            .get(&(unit_kind.as_str(), paragraph_index, sentence_index))
            .copied();
        if actual != expected {
            report.mismatches.push(format!(
                "{label}: position of {unit_kind:?} {paragraph_index}/{sentence_index:?} \
                 differs: sqlite={actual:?} plan={expected:?}"
            ));
        }
    }
}

fn resource(name: &str, bytes: Vec<u8>) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource(format!("memory:{name}")),
        final_source: DocumentSource(format!("memory:{name}")),
        media_type: MediaType("application/epub+zip".into()),
        bytes,
        etag: None,
        last_modified: None,
        metadata: BTreeMap::new(),
    }
}
