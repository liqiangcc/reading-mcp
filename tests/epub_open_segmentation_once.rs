//! An EPUB open must segment the document exactly once: the parser builds the segmentation the
//! validator needs and hands it to ingest, which must not build a second one.
//!
//! This is its own test binary so the process-wide `SEGMENTATION_BUILDS` counter has exactly one
//! test writing it.

use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use reading_mcp::application::open_document::{OpenDocumentCommand, OpenDocumentUseCase};
use reading_mcp::application::ports::{
    ApplicationError, RetrievalOptions, RetrievedResource, Retriever, SectionTextUnits,
    SourcePolicy, TextUnitIndex,
};
use reading_mcp::domain::{
    Document, DocumentId, DocumentSegmentation, DocumentSource, MediaType, ParagraphTextUnitSet,
    SEGMENTATION_BUILDS, SectionId, SentenceTextUnitSet, TextUnit,
};
use reading_mcp::infrastructure::{InMemoryDocumentRepository, NoopSearchIndex};
use reading_mcp::parsing::{ArchiveLimits, ParserRouter};
use zip::write::{SimpleFileOptions, ZipWriter};

struct AllowAllPolicy;

#[async_trait]
impl SourcePolicy for AllowAllPolicy {
    async fn validate(&self, _source: &DocumentSource) -> Result<(), ApplicationError> {
        Ok(())
    }
}

struct EpubRetriever {
    bytes: Vec<u8>,
}

#[async_trait]
impl Retriever for EpubRetriever {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        _options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        Ok(RetrievedResource {
            source: source.clone(),
            final_source: source.clone(),
            media_type: MediaType("application/epub+zip".into()),
            bytes: self.bytes.clone(),
            etag: None,
            last_modified: None,
            metadata: BTreeMap::new(),
        })
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
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Once EPUB</dc:title></metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="chapter"/></spine>
</package>"#,
        ),
        (
            "OPS/nav.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="toc"><ol><li><a href="chapter.xhtml#vm">Virtual Memory</a></li></ol></nav>
</body></html>"#,
        ),
        (
            "OPS/chapter.xhtml",
            r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><main><h1 id="vm">Virtual Memory</h1><p>Orbital memory in EPUB.</p><p>A second paragraph. With two sentences.</p><blockquote><p>Quoted thought.</p></blockquote></main></body></html>"#,
        ),
    ])
}

#[derive(Default)]
struct RecordingTextUnits {
    segmentations: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl TextUnitIndex for RecordingTextUnits {
    async fn replace_document(
        &self,
        _document_id: &DocumentId,
        _units: &[TextUnit],
    ) -> Result<(), ApplicationError> {
        Ok(())
    }

    async fn list_document(
        &self,
        _document_id: &DocumentId,
    ) -> Result<Vec<TextUnit>, ApplicationError> {
        Ok(vec![])
    }

    async fn replace_document_sets(
        &self,
        _document: &Arc<Document>,
        _paragraphs: Arc<ParagraphTextUnitSet>,
        _sentences: Arc<SentenceTextUnitSet>,
    ) -> Result<(), ApplicationError> {
        panic!("open must not build whole-document unit sets");
    }

    async fn replace_document_segmentation(
        &self,
        _document: &Arc<Document>,
        _segmentation: Arc<DocumentSegmentation>,
    ) -> Result<(), ApplicationError> {
        self.segmentations.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn section_units(
        &self,
        _document_id: &DocumentId,
        _section_id: &SectionId,
    ) -> Result<Option<SectionTextUnits>, ApplicationError> {
        Ok(None)
    }
}

#[tokio::test]
async fn epub_open_segments_exactly_once() {
    let text_units = Arc::new(RecordingTextUnits::default());
    let use_case = OpenDocumentUseCase::with_text_unit_index(
        Arc::new(AllowAllPolicy),
        Arc::new(EpubRetriever {
            bytes: build_epub(),
        }),
        Arc::new(ParserRouter::release(100, ArchiveLimits::default())),
        Arc::new(InMemoryDocumentRepository::default()),
        text_units.clone(),
        Arc::new(NoopSearchIndex),
    );

    let before = SEGMENTATION_BUILDS.load(Ordering::SeqCst);
    let result = use_case
        .execute(OpenDocumentCommand {
            source: DocumentSource("/books/once.epub".into()),
            options: RetrievalOptions::default(),
        })
        .await
        .expect("EPUB open succeeds");
    let builds = SEGMENTATION_BUILDS.load(Ordering::SeqCst) - before;

    assert_eq!(
        builds, 1,
        "an EPUB open must segment once, not {builds} times"
    );
    assert_eq!(text_units.segmentations.load(Ordering::SeqCst), 1);
    assert_eq!(result.section_count, 1);
    assert!(
        result
            .reading_profile
            .canonical_text_coverage
            .paragraph_count
            >= 1,
        "the open must still profile the paragraphs it segmented"
    );
}
