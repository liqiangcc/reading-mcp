use std::future::Future;
use std::io::{Cursor, Write};
use std::pin::Pin;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use reading_mcp::application::ports::{Parser, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType};
use reading_mcp::parsing::{ArchiveLimits, EpubParser, MarkdownParser};
use zip::write::{SimpleFileOptions, ZipWriter};

/// An offloaded parse is Pending on its first poll: under a current_thread
/// runtime an inline synchronous body would resolve in that single poll while
/// pinning the only worker thread for the entire parse.
fn assert_first_poll_pending<F: Future>(future: Pin<&mut F>, label: &str) {
    let mut context = Context::from_waker(Waker::noop());
    match future.poll(&mut context) {
        Poll::Pending => {}
        Poll::Ready(_) => {
            panic!("{label} resolved inline instead of yielding to the blocking pool")
        }
    }
}

fn resource(bytes: Vec<u8>, media_type: &str, source: &str) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource(source.into()),
        final_source: DocumentSource(source.into()),
        media_type: MediaType(media_type.into()),
        bytes,
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn markdown_parser_runs_on_blocking_pool_and_returns_correct_document() {
    let mut body = String::new();
    for index in 0..20_000 {
        body.push_str(&format!("# Heading {index}\n\nParagraph body {index}.\n\n"));
    }
    let mut parse = pin!(MarkdownParser.parse(resource(
        body.into_bytes(),
        "text/markdown",
        "memory:isolation.md",
    )));

    assert_first_poll_pending(parse.as_mut(), "markdown parse");
    let document = parse.await.expect("markdown parse completes");
    assert!(document.root_sections.len() > 10_000);
    assert_eq!(document.title, "Heading 0");
}

#[tokio::test(flavor = "current_thread")]
async fn epub_parser_runs_on_blocking_pool_and_returns_correct_document() {
    let parser = EpubParser::new(ArchiveLimits::default());
    let mut parse = pin!(parser.parse(resource(
        minimal_epub(),
        "application/epub+zip",
        "memory:isolation.epub",
    )));

    assert_first_poll_pending(parse.as_mut(), "epub parse");
    let document = parse.await.expect("epub parse completes");
    assert_eq!(document.title, "Isolation Book");
    assert!(
        document
            .root_sections
            .iter()
            .any(|section| section.content.contains("Chapter body"))
    );
}

fn minimal_epub() -> Vec<u8> {
    build_zip(vec![
        ("mimetype", "application/epub+zip".into()),
        (
            "META-INF/container.xml",
            r#"<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles>
<rootfile full-path="OPS/package.opf" media-type="application/oebps-package+xml"/>
</rootfiles></container>"#
                .into(),
        ),
        (
            "OPS/package.opf",
            r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Isolation Book</dc:title></metadata>
<manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest>
<spine><itemref idref="chapter"/></spine></package>"#
                .into(),
        ),
        (
            "OPS/chapter.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="chapter">Chapter</h1><p>Chapter body.</p></body></html>"#
                .into(),
        ),
    ])
}

fn build_zip(entries: Vec<(&str, String)>) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        writer.start_file(name, options).expect("ZIP entry");
        writer.write_all(content.as_bytes()).expect("ZIP content");
    }
    writer.finish().expect("ZIP finish").into_inner()
}
