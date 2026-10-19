use reading_mcp::application::ports::{ApplicationError, Parser, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType};
use reading_mcp::parsing::{MarkdownParser, OpenApiParser, TextParser};

fn resource(media_type: &str, source: &str, bytes: &[u8]) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource(source.into()),
        final_source: DocumentSource(source.into()),
        media_type: MediaType(media_type.into()),
        bytes: bytes.to_vec(),
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

#[tokio::test]
async fn text_parser_decodes_declared_http_charset() {
    let document = TextParser
        .parse(resource(
            "text/plain; charset=gbk",
            "memory:gbk.txt",
            b"\xd6\xd0\xce\xc4",
        ))
        .await
        .expect("GBK text should parse");
    assert_eq!(document.root_sections[0].content, "中文");
    assert_eq!(
        document.metadata.get("text_charset").map(String::as_str),
        Some("GBK")
    );
    assert_eq!(
        document
            .metadata
            .get("text_charset_source")
            .map(String::as_str),
        Some("http")
    );
}

#[tokio::test]
async fn text_parser_decodes_byte_order_mark() {
    let document = TextParser
        .parse(resource(
            "text/plain",
            "memory:utf16.txt",
            b"\xff\xfe\x68\x00\x69\x00",
        ))
        .await
        .expect("UTF-16LE text should parse");
    assert_eq!(document.root_sections[0].content, "hi");
    assert_eq!(
        document.metadata.get("text_charset").map(String::as_str),
        Some("UTF-16LE")
    );
    assert_eq!(
        document
            .metadata
            .get("text_charset_source")
            .map(String::as_str),
        Some("bom")
    );
}

#[tokio::test]
async fn markdown_parser_decodes_declared_http_charset() {
    let document = MarkdownParser
        .parse(resource(
            "text/markdown; charset=gb18030",
            "memory:gb18030.md",
            b"# \xb1\xea\xcc\xe2\n\nbody\n",
        ))
        .await
        .expect("gb18030 markdown should parse");
    assert_eq!(document.title, "标题");
    assert_eq!(
        document.metadata.get("text_charset").map(String::as_str),
        Some("gb18030")
    );
}

#[tokio::test]
async fn openapi_parser_decodes_declared_http_charset() {
    let document = OpenApiParser
        .parse(resource(
            "application/json; charset=iso-8859-1",
            "memory:cafe.json",
            b"{\"openapi\":\"3.0.0\",\"info\":{\"title\":\"Caf\xe9\",\"version\":\"1\"},\"paths\":{}}",
        ))
        .await
        .expect("windows-1252 OpenAPI should parse");
    assert_eq!(document.title, "Café");
    assert_eq!(
        document.metadata.get("text_charset").map(String::as_str),
        Some("windows-1252")
    );
}

#[tokio::test]
async fn text_parser_rejects_unsupported_declared_charset() {
    let error = TextParser
        .parse(resource(
            "text/plain; charset=x-no-such-charset",
            "memory:unsupported.txt",
            b"ok \xff bad",
        ))
        .await
        .expect_err("unsupported declared charset must fail");
    assert!(
        matches!(error, ApplicationError::UnsupportedTextEncoding(_)),
        "expected UnsupportedTextEncoding, got {error:?}"
    );
}

#[tokio::test]
async fn valid_utf8_ignores_declared_charset() {
    let document = TextParser
        .parse(resource(
            "text/plain; charset=gbk",
            "memory:ascii.txt",
            b"plain ascii",
        ))
        .await
        .expect("valid UTF-8 text should parse");
    assert_eq!(document.root_sections[0].content, "plain ascii");
    assert!(!document.metadata.contains_key("text_charset"));
}
