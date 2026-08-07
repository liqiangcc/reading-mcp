use reading_mcp::application::ports::{
    ParsedCacheKey, ParsedDocumentCache, Parser, RawResourceCache, RetrievedResource,
};
use reading_mcp::domain::{DocumentSource, MediaType};
use reading_mcp::infrastructure::{FileParsedDocumentCache, FileRawResourceCache};
use reading_mcp::parsing::ParserRouter;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

#[tokio::test]
async fn persistent_raw_and_parsed_caches_survive_adapter_recreation() {
    let directory = tempdir().expect("cache directory should be created");
    let source = DocumentSource("https://example.test/book.md".into());
    let resource = RetrievedResource {
        source: source.clone(),
        final_source: DocumentSource("https://cdn.example.test/book.md".into()),
        media_type: MediaType("text/markdown".into()),
        bytes: b"# Operating Systems\n\n## Virtual Memory\n\nPage tables map memory.\n".to_vec(),
        etag: Some("\"fixture-v1\"".into()),
        last_modified: Some("Fri, 07 Aug 2026 00:00:00 GMT".into()),
        metadata: Default::default(),
    };

    let raw_cache = FileRawResourceCache::new(directory.path());
    raw_cache
        .put(&source, resource.clone())
        .await
        .expect("raw resource should persist");
    drop(raw_cache);

    let reopened_raw_cache = FileRawResourceCache::new(directory.path());
    assert_eq!(
        reopened_raw_cache
            .get(&source)
            .await
            .expect("raw cache should be readable after recreation"),
        Some(resource.clone())
    );

    let document = ParserRouter::phase4()
        .parse(resource.clone())
        .await
        .expect("fixture should parse");
    let key = ParsedCacheKey {
        final_source: resource.final_source.clone(),
        raw_sha256: format!("sha256:{:x}", Sha256::digest(&resource.bytes)),
    };

    let parsed_cache = FileParsedDocumentCache::new(directory.path());
    parsed_cache
        .put(key.clone(), document.clone())
        .await
        .expect("parsed document should persist");
    drop(parsed_cache);

    let reopened_parsed_cache = FileParsedDocumentCache::new(directory.path());
    assert_eq!(
        reopened_parsed_cache
            .get(&key)
            .await
            .expect("parsed cache should be readable after recreation"),
        Some(document)
    );
}
