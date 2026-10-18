use reading_mcp::application::ports::{ApplicationError, Parser, RetrievedResource};
use reading_mcp::domain::{DocumentSource, MediaType};
use reading_mcp::parsing::{MarkdownParser, OpenApiParser, TextParser};

fn resource(media_type: &str, source: &str) -> RetrievedResource {
    RetrievedResource {
        source: DocumentSource(source.into()),
        final_source: DocumentSource(source.into()),
        media_type: MediaType(media_type.into()),
        bytes: b"ok \xff bad".to_vec(),
        etag: None,
        last_modified: None,
        metadata: Default::default(),
    }
}

fn assert_parse_failed_prefix(error: ApplicationError, prefix: &str) {
    match error {
        ApplicationError::ParseFailed(message) => assert!(
            message.starts_with(prefix),
            "expected message starting with {prefix:?}, got {message:?}"
        ),
        other => panic!("expected ApplicationError::ParseFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn text_parser_reports_invalid_utf8() {
    let error = TextParser
        .parse(resource("text/plain", "memory:invalid.txt"))
        .await
        .expect_err("invalid UTF-8 text must fail");
    assert_parse_failed_prefix(error, "invalid UTF-8 text:");
}

#[tokio::test]
async fn markdown_parser_reports_invalid_utf8() {
    let error = MarkdownParser
        .parse(resource("text/markdown", "memory:invalid.md"))
        .await
        .expect_err("invalid UTF-8 markdown must fail");
    assert_parse_failed_prefix(error, "invalid UTF-8 markdown:");
}

#[tokio::test]
async fn openapi_parser_reports_invalid_utf8() {
    let error = OpenApiParser
        .parse(resource("application/json", "memory:invalid.json"))
        .await
        .expect_err("invalid UTF-8 OpenAPI must fail");
    assert_parse_failed_prefix(error, "OpenAPI document is not UTF-8:");
}
