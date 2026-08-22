use std::path::PathBuf;

use reading_mcp::application::list_documents::{ListDocumentsCommand, ListDocumentsUseCase};
use reading_mcp::application::ports::ApplicationError;

#[tokio::test]
async fn paginates_deterministically_without_gap_or_overlap_and_allows_page_size_changes() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    write_file(directory.path().join("b.md"), "b").await;
    write_file(directory.path().join("a.md"), "a").await;
    let nested = directory.path().join("nested");
    tokio::fs::create_dir(&nested)
        .await
        .expect("nested directory should be created");
    write_file(nested.join("c.md"), "c").await;

    let use_case = ListDocumentsUseCase::new(vec![directory.path().to_path_buf()]);
    let first = list(&use_case, directory.path(), true, 1, None).await;
    assert_eq!(names(&first.documents), ["a.md"]);
    assert!(!first.complete);
    let cursor = first.next_cursor.expect("first page should continue");

    let second = list(&use_case, directory.path(), true, 10, Some(cursor)).await;
    assert_eq!(names(&second.documents), ["b.md", "c.md"]);
    assert!(second.complete);
    assert!(second.next_cursor.is_none());
}

#[tokio::test]
async fn recursive_and_non_recursive_scopes_are_isolated() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    write_file(directory.path().join("root.md"), "root").await;
    let nested = directory.path().join("nested");
    tokio::fs::create_dir(&nested)
        .await
        .expect("nested directory should be created");
    write_file(nested.join("child.md"), "child").await;

    let use_case = ListDocumentsUseCase::new(vec![directory.path().to_path_buf()]);
    let shallow = list(&use_case, directory.path(), false, 10, None).await;
    assert_eq!(names(&shallow.documents), ["root.md"]);
    assert!(shallow.complete);

    let recursive = list(&use_case, directory.path(), true, 10, None).await;
    assert_eq!(names(&recursive.documents), ["child.md", "root.md"]);
    assert!(recursive.complete);
}

#[tokio::test]
async fn changed_candidate_manifest_stales_continuation() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    write_file(directory.path().join("a.md"), "a").await;
    write_file(directory.path().join("b.md"), "b").await;
    let use_case = ListDocumentsUseCase::new(vec![directory.path().to_path_buf()]);

    let first = list(&use_case, directory.path(), true, 1, None).await;
    let cursor = first.next_cursor.expect("first page should continue");
    write_file(directory.path().join("c.md"), "c").await;

    let error = list_result(&use_case, directory.path(), true, 1, Some(cursor))
        .await
        .expect_err("changed candidate manifest must stale continuation");
    assert!(matches!(error, ApplicationError::StaleCursor(_)));
}

#[tokio::test]
async fn scope_and_authorization_mismatches_fail_closed() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let other = tempfile::tempdir().expect("second directory should be created");
    write_file(directory.path().join("a.md"), "a").await;
    write_file(directory.path().join("b.md"), "b").await;

    let use_case = ListDocumentsUseCase::new(vec![directory.path().to_path_buf()]);
    let first = list(&use_case, directory.path(), true, 1, None).await;
    let cursor = first.next_cursor.expect("first page should continue");

    let recursive_error = list_result(&use_case, directory.path(), false, 1, Some(cursor.clone()))
        .await
        .expect_err("recursive mismatch must fail");
    assert!(matches!(
        recursive_error,
        ApplicationError::CursorTargetMismatch(_)
    ));

    let unauthorized = ListDocumentsUseCase::new(vec![other.path().to_path_buf()]);
    let error = list_result(&unauthorized, directory.path(), true, 1, Some(cursor))
        .await
        .expect_err("continued path authorization must fail closed");
    assert!(matches!(error, ApplicationError::BlockedSource(_)));
}

#[tokio::test]
async fn empty_roots_are_complete_and_discovery_does_not_read_file_contents() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let invalid = directory.path().join("invalid.md");
    tokio::fs::write(&invalid, [0_u8, 159, 146, 150])
        .await
        .expect("invalid document bytes should be written");

    let empty = ListDocumentsUseCase::new(Vec::new())
        .execute(ListDocumentsCommand {
            path: None,
            recursive: true,
            max_results: 10,
            cursor: None,
        })
        .await
        .expect("empty configured roots should be complete");
    assert!(empty.documents.is_empty());
    assert!(empty.complete);
    assert!(empty.next_cursor.is_none());

    let listed = ListDocumentsUseCase::new(vec![directory.path().to_path_buf()])
        .execute(ListDocumentsCommand {
            path: Some(directory.path().to_string_lossy().into_owned()),
            recursive: true,
            max_results: 10,
            cursor: None,
        })
        .await
        .expect("discovery should inspect metadata without parsing");
    assert_eq!(listed.documents[0].name, "invalid.md");
}

#[tokio::test]
async fn malformed_and_oversized_cursors_are_invalid() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    write_file(directory.path().join("a.md"), "a").await;
    write_file(directory.path().join("b.md"), "b").await;
    let use_case = ListDocumentsUseCase::new(vec![directory.path().to_path_buf()]);

    let malformed = list_result(
        &use_case,
        directory.path(),
        true,
        1,
        Some("dc1.not-a-cursor".into()),
    )
    .await
    .expect_err("malformed cursor must fail");
    assert!(matches!(malformed, ApplicationError::InvalidCursor(_)));

    let oversized = list_result(
        &use_case,
        directory.path(),
        true,
        1,
        Some("x".repeat(16 * 1024 + 1)),
    )
    .await
    .expect_err("oversized cursor must fail");
    assert!(matches!(oversized, ApplicationError::InvalidCursor(_)));
}

async fn list(
    use_case: &ListDocumentsUseCase,
    directory: &std::path::Path,
    recursive: bool,
    max_results: usize,
    cursor: Option<String>,
) -> reading_mcp::application::list_documents::ListDocumentsResult {
    list_result(use_case, directory, recursive, max_results, cursor)
        .await
        .expect("discovery page should succeed")
}

async fn list_result(
    use_case: &ListDocumentsUseCase,
    directory: &std::path::Path,
    recursive: bool,
    max_results: usize,
    cursor: Option<String>,
) -> Result<reading_mcp::application::list_documents::ListDocumentsResult, ApplicationError> {
    use_case
        .execute(ListDocumentsCommand {
            path: Some(directory.to_string_lossy().into_owned()),
            recursive,
            max_results,
            cursor,
        })
        .await
}

fn names(documents: &[reading_mcp::application::list_documents::ListedDocument]) -> Vec<&str> {
    documents
        .iter()
        .map(|document| document.name.as_str())
        .collect()
}

async fn write_file(path: PathBuf, content: &str) {
    tokio::fs::write(path, content)
        .await
        .expect("document fixture should be written");
}
