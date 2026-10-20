use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::path::PathBuf;

use lopdf::content::{Content, Operation};
use lopdf::{Document as PdfDocument, Object, Stream, dictionary, text_string};
use reading_mcp::application::ports::{Parser, RetrievedResource};
use reading_mcp::domain::{
    Document, DocumentSource, MediaType, NormalizedTextRangeError, Section, SentenceTextUnit,
    TextUnit,
};
use reading_mcp::parsing::{ArchiveLimits, ParserRouter};
use zip::write::{SimpleFileOptions, ZipWriter};

#[derive(Debug, Clone)]
struct Mismatch {
    section_id: String,
    kind: &'static str,
    paragraph_index: usize,
    sentence_index: Option<usize>,
    expected_start: String,
    actual_start: String,
}

#[derive(Debug, Default)]
struct Report {
    units_checked: usize,
    mismatches: Vec<Mismatch>,
    mismatch_counts: BTreeMap<&'static str, usize>,
}

fn classify(expected: &str, actual: &str) -> &'static str {
    if expected.trim() == actual.trim() {
        "trim"
    } else if collapse_whitespace(expected) == collapse_whitespace(actual) {
        "whitespace"
    } else {
        "other"
    }
}

fn collapse_whitespace(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut in_run = false;
    for ch in value.chars() {
        if ch.is_whitespace() {
            if !in_run {
                out.push(' ');
                in_run = true;
            }
        } else {
            out.push(ch);
            in_run = false;
        }
    }
    out
}

fn first_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

fn check_document(document: &Document) -> Report {
    let mut report = Report::default();

    fn collect<'a>(section: &'a Section, out: &mut Vec<&'a Section>) {
        out.push(section);
        for child in &section.children {
            collect(child, out);
        }
    }
    let mut sections = Vec::new();
    for root in &document.root_sections {
        collect(root, &mut sections);
    }
    let sections_by_id: BTreeMap<&str, &Section> =
        sections.iter().map(|s| (s.id.0.as_str(), *s)).collect();

    let (paragraphs, sentences) = match document.try_text_unit_sets() {
        Ok(sets) => sets,
        Err(error) => {
            report.mismatches.push(Mismatch {
                section_id: "<document>".into(),
                kind: "other",
                paragraph_index: 0,
                sentence_index: None,
                expected_start: format!("text unit set error: {error:?}"),
                actual_start: String::new(),
            });
            *report.mismatch_counts.entry("other").or_insert(0) += 1;
            return report;
        }
    };

    let mut record = |report: &mut Report,
                      section: &Section,
                      paragraph_index: usize,
                      sentence_index: Option<usize>,
                      slice: Result<&str, NormalizedTextRangeError>,
                      actual: &str| {
        report.units_checked += 1;
        let mismatch = match slice {
            Ok(expected) => {
                if expected == actual {
                    return;
                }
                Mismatch {
                    section_id: section.id.0.clone(),
                    kind: classify(expected, actual),
                    paragraph_index,
                    sentence_index,
                    expected_start: first_chars(expected, 60),
                    actual_start: first_chars(actual, 60),
                }
            }
            Err(_error) => Mismatch {
                section_id: section.id.0.clone(),
                kind: "slice_error",
                paragraph_index,
                sentence_index,
                expected_start: String::new(),
                actual_start: first_chars(actual, 60),
            },
        };
        *report.mismatch_counts.entry(mismatch.kind).or_insert(0) += 1;
        if report.mismatches.len() < 10 {
            report.mismatches.push(mismatch);
        }
    };

    for paragraph in &paragraphs.units {
        let TextUnit {
            owner_section_id,
            normalized_range,
            text,
            paragraph_index,
            ..
        } = paragraph;
        if let Some(section) = sections_by_id.get(owner_section_id.0.as_str()) {
            let slice = section.normalized_text_slice(*normalized_range);
            record(&mut report, section, *paragraph_index, None, slice, text);
        }
    }

    for sentence in &sentences.units {
        let SentenceTextUnit {
            owner_section_id,
            normalized_range,
            text,
            paragraph_index,
            sentence_index,
            ..
        } = sentence;
        if let Some(section) = sections_by_id.get(owner_section_id.0.as_str()) {
            let slice = section.normalized_text_slice(*normalized_range);
            record(
                &mut report,
                section,
                *paragraph_index,
                Some(*sentence_index),
                slice,
                text,
            );
        }
    }

    report
}

fn render_report(report: &Report, label: &str) -> String {
    let mut lines = vec![format!(
        "{label}: units_checked={} mismatch_counts={:?}",
        report.units_checked, report.mismatch_counts
    )];
    for mismatch in &report.mismatches {
        lines.push(format!(
            "  mismatch section={} kind={} paragraph={} sentence={:?} expected={:?} actual={:?}",
            mismatch.section_id,
            mismatch.kind,
            mismatch.paragraph_index,
            mismatch.sentence_index,
            mismatch.expected_start,
            mismatch.actual_start
        ));
    }
    lines.join("\n")
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

fn flatten<'a>(section: &'a Section) -> Vec<&'a Section> {
    let mut sections = vec![section];
    for child in &section.children {
        sections.extend(flatten(child));
    }
    sections
}

fn build_zip(entries: &[(&str, &str)]) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        writer
            .start_file(*name, options)
            .expect("ZIP fixture entry should start");
        writer
            .write_all(content.as_bytes())
            .expect("ZIP fixture entry should write");
    }
    writer
        .finish()
        .expect("ZIP fixture should finish")
        .into_inner()
}

fn build_epub() -> Vec<u8> {
    build_zip(&[
        ("mimetype", "application/epub+zip"),
        (
            "META-INF/container.xml",
            r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
        ),
        (
            "OPS/package.opf",
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Operating Systems EPUB</dc:title></metadata>
  <manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="chapter"/></spine>
</package>"#,
        ),
        (
            "OPS/chapter.xhtml",
            r#"<!doctype html><html><head><title>Virtual Memory</title></head><body><main><h1 id="vm">Virtual Memory</h1><p>Orbital memory in EPUB.</p><p>A second paragraph. With two sentences.</p></main></body></html>"#,
        ),
    ])
}

fn build_pdf_bytes(page_texts: &[&str]) -> Vec<u8> {
    let mut document = PdfDocument::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! {
            "F1" => font_id,
        },
    });

    let mut page_ids = Vec::new();
    for text in page_texts {
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal(*text)]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = document.add_object(Stream::new(
            dictionary! {},
            content.encode().expect("fixture content should encode"),
        ));
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        page_ids.push(page_id);
    }

    let kids = page_ids
        .iter()
        .copied()
        .map(Object::Reference)
        .collect::<Vec<_>>();
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => page_ids.len() as i64,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );

    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    let info_id = document.add_object(dictionary! {
        "Title" => text_string("Operating Systems"),
    });
    document.trailer.set("Root", catalog_id);
    document.trailer.set("Info", info_id);
    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("PDF fixture should serialize");
    bytes
}

async fn parse_fixture(
    router: &ParserRouter,
    name: &str,
    media_type: &str,
    bytes: Vec<u8>,
) -> Document {
    router
        .parse(resource(name, media_type, bytes))
        .await
        .expect("fixture should parse")
}

#[tokio::test]
async fn fixtures_unit_text_equals_range_slice() {
    let router = ParserRouter::release(100, ArchiveLimits::default());

    let plain = parse_fixture(
        &router,
        "plain.txt",
        "text/plain",
        "One. Two. Three.\n\nSecond paragraph here."
            .as_bytes()
            .to_vec(),
    )
    .await;

    let markdown = parse_fixture(
        &router,
        "book.md",
        "text/markdown",
        r#"# Virtual Memory

Address spaces give each process an isolated memory view.

## Page tables

- First item with one sentence.
- Second item. With two sentences.

> A quoted thought. It spans sentences.

```rust
fn main() { println!("Hi."); }
```

| Column A | Column B |
|----------|----------|
| one      | two      |
"#
        .as_bytes()
        .to_vec(),
    )
    .await;

    let html = parse_fixture(
        &router,
        "page.html",
        "text/html",
        r#"<!doctype html><html><head><title>Memory</title></head><body><h1>Virtual Memory</h1><p>Orbital memory in HTML.</p><ul><li>List one.</li><li>List two. More text.</li></ul><blockquote>Quote text. Second sentence.</blockquote><pre><code>code block text</code></pre><table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table></body></html>"#.as_bytes().to_vec(),
    )
    .await;

    let epub = parse_fixture(&router, "book.epub", "application/epub+zip", build_epub()).await;

    let pdf = parse_fixture(
        &router,
        "book.pdf",
        "application/pdf",
        build_pdf_bytes(&[
            "Address spaces give each process an isolated memory view.",
            "Page table entries map virtual pages to physical frames.",
        ]),
    )
    .await;

    for (label, document) in [
        ("plain", &plain),
        ("markdown", &markdown),
        ("html", &html),
        ("epub", &epub),
        ("pdf", &pdf),
    ] {
        let report = check_document(document);
        assert!(
            report.mismatch_counts.is_empty(),
            "fixture {label} had mismatches:\n{}",
            render_report(&report, label)
        );
        assert!(report.units_checked > 0, "fixture {label} checked no units");
    }
}

#[tokio::test]
#[ignore]
async fn real_books_unit_text_equals_range_slice() {
    let dir = match std::env::var("READING_MCP_SLICE_CHECK_DIR") {
        Ok(value) if !value.is_empty() => PathBuf::from(value),
        _ => return,
    };

    let router = ParserRouter::release(2048, ArchiveLimits::default());

    let mut entries: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(read_dir) => read_dir
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("epub") | Some("txt") | Some("md") | Some("html") | Some("pdf")
                )
            })
            .collect(),
        Err(error) => {
            println!("slice-check dir unreadable: {error}");
            return;
        }
    };
    entries.sort();

    let mut total_units = 0usize;
    let mut total_mismatches = 0usize;

    for path in &entries {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_string();
        let media_type = match extension.as_str() {
            "epub" => "application/epub+zip",
            "txt" => "text/plain",
            "md" => "text/markdown",
            "html" | "htm" => "text/html",
            "pdf" => "application/pdf",
            _ => continue,
        };
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                println!("{name} units_checked=0 mismatch_counts=read-error:{error}");
                continue;
            }
        };
        let document = match router.parse(resource(name, media_type, bytes)).await {
            Ok(document) => document,
            Err(error) => {
                println!("{name} units_checked=0 mismatch_counts=parse-error");
                println!("  parse error: {error}");
                continue;
            }
        };
        let report = check_document(&document);
        let mismatches: usize = report.mismatch_counts.values().sum();
        total_units += report.units_checked;
        total_mismatches += mismatches;
        println!(
            "{name} units_checked={} mismatch_counts={:?}",
            report.units_checked, report.mismatch_counts
        );
        if mismatches > 0 {
            for mismatch in &report.mismatches {
                println!(
                    "  mismatch section={} kind={} paragraph={} sentence={:?} expected={:?} actual={:?}",
                    mismatch.section_id,
                    mismatch.kind,
                    mismatch.paragraph_index,
                    mismatch.sentence_index,
                    mismatch.expected_start,
                    mismatch.actual_start
                );
            }
        }
    }

    println!("TOTAL units_checked={total_units} mismatches={total_mismatches}");
    assert_eq!(
        total_mismatches, 0,
        "real books had unit text / range slice mismatches"
    );
}
