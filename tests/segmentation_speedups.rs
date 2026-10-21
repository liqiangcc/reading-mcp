//! R5b: the segmentation speed-ups must not move one boundary, class or verdict.
//!
//! Every comparison here runs the pre-R5b implementation and the implementation that replaced it on
//! the same input and asserts that they agree. This file grows one test per speed-up:
//! - `parse_with_segmentation` returns the segmentation the returned document rebuilds;
//! - the in-section block lookup finds the class the whole-map scan finds, for every paragraph;
//! - indexed slicing yields the candidate list the linear slicing yields, for every section.

use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use reading_mcp::application::ports::{ApplicationError, Parser, RetrievedResource};
use reading_mcp::domain::{
    Document, DocumentSource, MediaType, Section, paragraph_content_class_in_section,
    paragraph_content_class_whole_map_scan, section_paragraph_candidates_indexed,
    section_paragraph_candidates_linear_scan,
};
use reading_mcp::parsing::{ArchiveLimits, ParserRouter};
use zip::write::{SimpleFileOptions, ZipWriter};

#[derive(Default)]
struct Comparisons {
    sections: usize,
    paragraphs: usize,
    count: usize,
    samples: Vec<String>,
}

impl Comparisons {
    fn mismatch(&mut self, message: String) {
        self.count += 1;
        if self.samples.len() < 20 {
            self.samples.push(message);
        }
    }

    fn total_mismatches(&self) -> usize {
        self.count
    }

    fn assert_clean(&self, label: &str) {
        assert_eq!(
            self.count,
            0,
            "{label}: {} mismatch(es) across {} sections and {} paragraphs:\n{}",
            self.count,
            self.sections,
            self.paragraphs,
            self.samples.join("\n")
        );
    }
}

fn collect_sections<'a>(sections: &'a [Section], out: &mut Vec<&'a Section>) {
    for section in sections {
        out.push(section);
        collect_sections(&section.children, out);
    }
}

/// Runs every old-versus-new comparison on one parsed document.
fn compare_document(label: &str, document: &Document, comparisons: &mut Comparisons) {
    // A fixture without a block map is valid: both comparisons still run, with no blocks to find
    // and with the fallback candidate list covering the whole section.
    let block_map = document
        .normalized_block_map()
        .expect("fixture block map decodes");
    let block_map = block_map.as_ref();
    let segmentation = document
        .try_segmentation()
        .expect("fixture document segments");

    let mut sections = Vec::new();
    collect_sections(&document.root_sections, &mut sections);
    let sections_by_id = sections
        .into_iter()
        .map(|section| (section.id.0.as_str(), section))
        .collect::<BTreeMap<&str, &Section>>();

    for section_segmentation in &segmentation.sections {
        let Some(section) = sections_by_id.get(section_segmentation.section_id.0.as_str()) else {
            comparisons.mismatch(format!(
                "{label} section={} has no owner Section",
                section_segmentation.section_id.0
            ));
            continue;
        };
        comparisons.sections += 1;
        let index = section.normalized_text_index();

        let indexed = section_paragraph_candidates_indexed(section, block_map);
        let linear = section_paragraph_candidates_linear_scan(section, block_map);
        if indexed != linear {
            comparisons.mismatch(format!(
                "{label} section={} candidate lists differ: indexed {} vs linear {} entries",
                section.id.0,
                indexed.len(),
                linear.len()
            ));
        }

        for paragraph in &section_segmentation.paragraphs {
            comparisons.paragraphs += 1;
            let range = paragraph.range();
            let Ok(text) = index.slice(range) else {
                comparisons.mismatch(format!(
                    "{label} section={} paragraph={} paragraph range does not slice",
                    section.id.0,
                    paragraph.paragraph_index()
                ));
                continue;
            };

            let in_section =
                paragraph_content_class_in_section(block_map, &section.id, range, text);
            let whole_map =
                paragraph_content_class_whole_map_scan(block_map, &section.id, range, text);
            if in_section != whole_map {
                comparisons.mismatch(format!(
                    "{label} section={} paragraph={} content class differs: in-section {in_section:?} vs whole-map {whole_map:?}",
                    section.id.0,
                    paragraph.paragraph_index()
                ));
            }
        }
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

fn build_zip(entries: &[(&str, &str)]) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        writer.start_file(*name, options).expect("ZIP entry starts");
        writer
            .write_all(content.as_bytes())
            .expect("ZIP entry writes");
    }
    writer.finish().expect("ZIP finishes").into_inner()
}

fn epub_with_chapters(title: &str, chapters: &[&str]) -> Vec<u8> {
    let container = r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;
    let manifest = chapters
        .iter()
        .enumerate()
        .map(|(index, _)| {
            format!(r#"<item id="chapter{index}" href="chapter{index}.xhtml" media-type="application/xhtml+xml"/>"#)
        })
        .collect::<String>();
    let spine = (0..chapters.len())
        .map(|index| format!(r#"<itemref idref="chapter{index}"/>"#))
        .collect::<String>();
    let package = format!(
        r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{title}</dc:title></metadata>
  <manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>{manifest}</manifest>
  <spine>{spine}</spine>
</package>"#
    );
    let nav = format!(
        r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol>{}</ol></nav></body></html>"#,
        (0..chapters.len())
            .map(|index| format!(r#"<li><a href="chapter{index}.xhtml">Chapter {index}</a></li>"#))
            .collect::<String>()
    );

    let mut entries: Vec<(String, String)> = vec![
        ("mimetype".into(), "application/epub+zip".into()),
        ("META-INF/container.xml".into(), container.into()),
        ("OPS/package.opf".into(), package),
        ("OPS/nav.xhtml".into(), nav),
    ];
    for (index, chapter) in chapters.iter().enumerate() {
        entries.push((chapter_path(index), (*chapter).to_string()));
    }
    let borrowed = entries
        .iter()
        .map(|(name, content)| (name.as_str(), content.as_str()))
        .collect::<Vec<_>>();
    build_zip(&borrowed)
}

fn chapter_path(index: usize) -> String {
    format!("OPS/chapter{index}.xhtml")
}

fn epub_fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "epub-native-blocks",
            epub_with_chapters(
                "Native Blocks",
                &[
                    r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><main><h1 id="vm">Virtual Memory</h1><p>Orbital memory in EPUB.</p><p>A second paragraph. With two sentences.</p></main></body></html>"#,
                ],
            ),
        ),
        (
            "epub-two-spines",
            epub_with_chapters(
                "Two Spines",
                &[
                    r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><main><h1 id="c1">Chapter One</h1><p>天地玄黄，宇宙洪荒。日月盈昃，辰宿列张。</p><p>StrongInference JohnR.Platt 3642SCIENCE ,NewSeries others.John )provided</p><p>谢谢谢谢谢谢</p></main></body></html>"#,
                    r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><main><h1 id="c2">Chapter Two</h1><p>Vol. 4, No. 2, pp. 100-110, Jan 1901 http://example.com (c) 1901</p><p>Plain English paragraph. Two sentences here.</p></main></body></html>"#,
                ],
            ),
        ),
    ]
}

/// Every EPUB fixture on disk under `tests/`, so a new checked-in fixture is covered too.
fn disk_epub_fixtures() -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().and_then(|value| value.to_str()) == Some("epub") {
                out.push(path);
            }
        }
    }

    let mut paths = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests"),
        &mut paths,
    );
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let name = path
                .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")))
                .unwrap_or(&path)
                .display()
                .to_string();
            std::fs::read(&path).ok().map(|bytes| (name, bytes))
        })
        .collect()
}

async fn parse_fixture(
    name: &str,
    media_type: &str,
    bytes: Vec<u8>,
) -> Result<Document, ApplicationError> {
    let router = ParserRouter::release(100, ArchiveLimits::default());
    router.parse(resource(name, media_type, bytes)).await
}

fn text_fixtures() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "plain",
            "text/plain",
            "One. Two. Three.\n\nSecond paragraph here.\n\nStrongInference JohnR.Platt 3642SCIENCE ,NewSeries others.John )provided\n\n天地玄黄，宇宙洪荒。日月盈昃，辰宿列张。\n",
        ),
        (
            "markdown",
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

Vol. 4, No. 2, pp. 100-110, Jan 1901 http://example.com (c) 1901

NNoottee tthhaatt eevveerryy cchhaarraacctteerr iiss ddoouubblleedd.
"#,
        ),
        (
            "html",
            "text/html",
            r#"<!doctype html><html><head><title>Memory</title></head><body><h1>Virtual Memory</h1><p>Orbital memory in HTML.</p><ul><li>List one.</li><li>List two. More text.</li></ul><blockquote>Quote text. Second sentence.</blockquote><pre><code>code block text</code></pre><table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table><div>Loose div text. Second sentence. Vol. 4, No. 2, pp. 1-2</div></body></html>"#,
        ),
    ]
}

#[tokio::test]
async fn fixtures_keep_every_segmentation_decision() {
    let mut comparisons = Comparisons::default();

    for (label, media_type, content) in text_fixtures() {
        let document = parse_fixture(label, media_type, content.as_bytes().to_vec())
            .await
            .expect("fixture parses");
        compare_document(label, &document, &mut comparisons);
    }

    for (label, bytes) in epub_fixtures() {
        let document = parse_fixture(label, "application/epub+zip", bytes)
            .await
            .expect("EPUB fixture parses");
        compare_document(label, &document, &mut comparisons);
    }

    for (label, bytes) in disk_epub_fixtures() {
        match parse_fixture(&label, "application/epub+zip", bytes).await {
            Ok(document) => compare_document(&label, &document, &mut comparisons),
            Err(error) => println!("{label} skipped: parse error {error}"),
        }
    }

    assert!(
        comparisons.sections > 0 && comparisons.paragraphs > 0,
        "fixtures compared no sections or paragraphs"
    );
    comparisons.assert_clean("fixtures");
    println!(
        "fixtures: {} sections, {} paragraphs, 0 mismatches",
        comparisons.sections, comparisons.paragraphs
    );
}

#[tokio::test]
async fn epub_parse_with_segmentation_returns_the_segmentation_the_document_rebuilds() {
    let mut checked = 0usize;
    let mut fixtures = epub_fixtures()
        .into_iter()
        .map(|(label, bytes)| (label.to_string(), bytes, true))
        .collect::<Vec<_>>();
    fixtures.extend(
        disk_epub_fixtures()
            .into_iter()
            .map(|(label, bytes)| (label, bytes, false)),
    );

    for (label, bytes, required) in fixtures {
        let router = ParserRouter::release(100, ArchiveLimits::default());
        let parsed = router
            .parse_with_segmentation(resource(&label, "application/epub+zip", bytes.clone()))
            .await;
        let (document, parsed) = match parsed {
            Ok(value) => value,
            Err(error) if !required => {
                println!("{label} skipped: parse error {error}");
                continue;
            }
            Err(error) => panic!("{label} must parse: {error}"),
        };
        let parsed = parsed.expect("the EPUB parser builds a segmentation");
        let rebuilt = document
            .try_segmentation()
            .expect("the returned document segments");

        assert_eq!(
            parsed.as_ref(),
            &rebuilt,
            "{label}: parse_with_segmentation must return exactly the segmentation the document rebuilds"
        );

        // The parse-only path must hand back the same document: same facts, same report.
        let plain = router
            .parse(resource(&label, "application/epub+zip", bytes))
            .await
            .expect("EPUB parses through parse()");
        assert_eq!(
            plain.metadata, document.metadata,
            "{label}: parse() and parse_with_segmentation() must attach the same metadata"
        );
        assert_eq!(
            plain.normalized_document_hash(),
            document.normalized_document_hash(),
            "{label}: parse() and parse_with_segmentation() must return the same document"
        );
        checked += 1;
    }

    assert!(checked > 0, "no EPUB fixture was checked");
    println!("epub fixtures: {checked} compared, 0 mismatches");
}

/// The same check on real books. `book-profile.yml` points `READING_MCP_SEGMENTATION_CHECK_DIR` at
/// the downloaded books and fails the job unless this reports 0 mismatches.
#[tokio::test]
#[ignore]
async fn real_books_keep_every_segmentation_decision() {
    let dir = match std::env::var("READING_MCP_SEGMENTATION_CHECK_DIR") {
        Ok(value) if !value.is_empty() => PathBuf::from(value),
        _ => return,
    };

    let mut entries: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(read_dir) => read_dir
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("epub"))
            .collect(),
        Err(error) => {
            println!("segmentation-check dir unreadable: {error}");
            return;
        }
    };
    entries.sort();

    let mut comparisons = Comparisons::default();
    let mut books = 0usize;
    for path in &entries {
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                println!("{name} skipped: read error {error}");
                continue;
            }
        };
        let router = ParserRouter::release(2048, ArchiveLimits::default());
        let (document, parsed) = router
            .parse_with_segmentation(resource(name, "application/epub+zip", bytes))
            .await
            .expect("real book parses");
        let parsed = parsed.expect("the EPUB parser builds a segmentation");
        let rebuilt = document.try_segmentation().expect("real book segments");

        let mut book_mismatches = 0usize;
        if parsed.as_ref() != &rebuilt {
            book_mismatches += 1;
            comparisons.mismatch(format!(
                "{name}: parse_with_segmentation returned a different segmentation than the document rebuilds"
            ));
        }
        let before = comparisons.total_mismatches();
        compare_document(name, &document, &mut comparisons);
        book_mismatches += comparisons.total_mismatches() - before;
        books += 1;
        println!("{name} mismatches={book_mismatches}");
    }

    println!(
        "TOTAL books={books} sections={} paragraphs={} mismatches={}",
        comparisons.sections,
        comparisons.paragraphs,
        comparisons.total_mismatches()
    );
    assert_eq!(
        comparisons.total_mismatches(),
        0,
        "real books changed a segmentation decision"
    );
}
