use std::path::{Path, PathBuf};

use reading_mcp::application::list_directories::{
    DirectoryEntryKind, ListDirectoryCommand, ListDirectoryUseCase,
};
use reading_mcp::application::list_documents::{ListDocumentsCommand, ListDocumentsUseCase};
use reading_mcp::application::ports::ApplicationError;

#[tokio::test]
async fn browses_a_nested_source_workspace_and_hands_directory_to_document_discovery() {
    let fixture = WorkspaceFixture::create().await;
    let use_case = ListDirectoryUseCase::new(vec![fixture.root.clone()]);

    let roots = list(&use_case, None, 10, None).await;
    assert_eq!(roots.entries.len(), 1);
    assert_eq!(roots.entries[0].kind, DirectoryEntryKind::Directory);
    assert_eq!(roots.entries[0].path, canonical(&fixture.root));

    let workspace = list(&use_case, Some(&fixture.root), 10, None).await;
    assert_eq!(entry_names(&workspace), ["papers"]);

    let papers = list(&use_case, Some(&fixture.papers), 10, None).await;
    assert_eq!(entry_names(&papers), ["kafka-2011-distributed-messaging"]);
    assert!(matches!(
        papers.entries[0].kind,
        DirectoryEntryKind::Directory
    ));

    let paper = list(&use_case, Some(&fixture.paper), 10, None).await;
    assert_eq!(entry_names(&paper), ["kafka-2011-netdb11"]);

    let revision = list(&use_case, Some(&fixture.revision), 10, None).await;
    assert_eq!(entry_names(&revision), ["paper.pdf", "source.json"]);
    assert!(
        revision
            .entries
            .iter()
            .all(|entry| entry.kind == DirectoryEntryKind::Document)
    );

    let documents = ListDocumentsUseCase::new(vec![fixture.root.clone()])
        .execute(ListDocumentsCommand {
            path: Some(fixture.revision.to_string_lossy().into_owned()),
            recursive: false,
            max_results: 10,
            cursor: None,
        })
        .await
        .expect("known directory should be a valid list_documents scope");
    assert_eq!(
        documents
            .documents
            .iter()
            .map(|document| document.name.as_str())
            .collect::<Vec<_>>(),
        ["paper.pdf", "source.json"]
    );
}

#[tokio::test]
async fn directory_pages_are_bounded_and_stale_after_a_child_change() {
    let fixture = WorkspaceFixture::create().await;
    tokio::fs::create_dir(fixture.papers.join("raft"))
        .await
        .expect("second child directory should be created");
    let use_case = ListDirectoryUseCase::new(vec![fixture.root.clone()]);

    let first = list(&use_case, Some(&fixture.papers), 1, None).await;
    assert!(!first.complete);
    let cursor = first
        .next_cursor
        .expect("first directory page should continue");

    tokio::fs::create_dir(fixture.papers.join("dynamo"))
        .await
        .expect("new child directory should be created");
    let error = use_case
        .execute(ListDirectoryCommand {
            path: Some(fixture.papers.to_string_lossy().into_owned()),
            max_results: 10,
            cursor: Some(cursor),
        })
        .await
        .expect_err("directory changes must stale continuation");
    assert!(matches!(error, ApplicationError::StaleCursor(_)));
}

#[tokio::test]
async fn path_containment_rejects_traversal_and_sibling_prefixes() {
    let fixture = WorkspaceFixture::create().await;
    let sibling = fixture
        .root
        .parent()
        .expect("fixture root should have a parent")
        .join(format!(
            "{}-outside",
            fixture.root.file_name().unwrap().to_string_lossy()
        ));
    tokio::fs::create_dir(&sibling)
        .await
        .expect("sibling directory should be created");

    let use_case = ListDirectoryUseCase::new(vec![fixture.root.clone()]);
    let prefix_error = use_case
        .execute(ListDirectoryCommand {
            path: Some(sibling.to_string_lossy().into_owned()),
            max_results: 10,
            cursor: None,
        })
        .await
        .expect_err("sibling prefix must not be treated as contained");
    assert!(matches!(prefix_error, ApplicationError::BlockedSource(_)));

    let traversal = fixture.root.join("..").join(
        fixture
            .root
            .file_name()
            .expect("fixture root should have a name"),
    );
    let traversal_error = use_case
        .execute(ListDirectoryCommand {
            path: Some(traversal.to_string_lossy().into_owned()),
            max_results: 10,
            cursor: None,
        })
        .await
        .expect_err("parent traversal must be rejected before canonicalization");
    assert!(matches!(
        traversal_error,
        ApplicationError::InvalidRequest(_)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_escape_is_not_discoverable() {
    use std::os::unix::fs::symlink;

    let fixture = WorkspaceFixture::create().await;
    let outside = tempfile::tempdir().expect("outside directory should be created");
    tokio::fs::write(outside.path().join("secret.md"), "secret")
        .await
        .expect("outside document should be written");
    symlink(outside.path(), fixture.papers.join("escape")).expect("symlink should be created");

    let use_case = ListDirectoryUseCase::new(vec![fixture.root.clone()]);
    let entries = list(&use_case, Some(&fixture.papers), 10, None).await;
    assert!(entries.entries.iter().all(|entry| entry.name != "escape"));

    let error = use_case
        .execute(ListDirectoryCommand {
            path: Some(fixture.papers.join("escape").to_string_lossy().into_owned()),
            max_results: 10,
            cursor: None,
        })
        .await
        .expect_err("explicit symlink escape must be blocked");
    assert!(matches!(error, ApplicationError::BlockedSource(_)));
}

#[cfg(unix)]
#[tokio::test]
async fn changed_authorized_root_stales_a_continuation() {
    use std::os::unix::fs::symlink;

    let container = tempfile::tempdir().expect("root container should be created");
    let first_target = container.path().join("first");
    let second_target = container.path().join("second");
    let stable_target = container.path().join("stable");
    tokio::fs::create_dir_all(&first_target)
        .await
        .expect("first target should be created");
    tokio::fs::create_dir_all(&second_target)
        .await
        .expect("second target should be created");
    tokio::fs::create_dir_all(&stable_target)
        .await
        .expect("stable target should be created");
    tokio::fs::create_dir(first_target.join("one"))
        .await
        .expect("first child should be created");
    tokio::fs::create_dir(first_target.join("two"))
        .await
        .expect("second child should be created");
    tokio::fs::create_dir(second_target.join("three"))
        .await
        .expect("replacement child should be created");
    let configured_root = container.path().join("authorized");
    symlink(&first_target, &configured_root).expect("authorized root symlink should be created");

    let use_case = ListDirectoryUseCase::new(vec![configured_root.clone(), stable_target]);
    let first = list(&use_case, None, 1, None).await;
    let cursor = first.next_cursor.expect("root page should continue");

    tokio::fs::remove_file(&configured_root)
        .await
        .expect("old authorized root symlink should be removed");
    symlink(&second_target, &configured_root).expect("replacement root symlink should be created");

    let error = use_case
        .execute(ListDirectoryCommand {
            path: None,
            max_results: 1,
            cursor: Some(cursor),
        })
        .await
        .expect_err("changed authorized roots must stale continuation");
    assert!(matches!(error, ApplicationError::StaleCursor(_)));
}

async fn list(
    use_case: &ListDirectoryUseCase,
    path: Option<&Path>,
    max_results: usize,
    cursor: Option<String>,
) -> reading_mcp::application::list_directories::ListDirectoryResult {
    use_case
        .execute(ListDirectoryCommand {
            path: path.map(|path| path.to_string_lossy().into_owned()),
            max_results,
            cursor,
        })
        .await
        .expect("directory page should succeed")
}

fn entry_names(
    result: &reading_mcp::application::list_directories::ListDirectoryResult,
) -> Vec<&str> {
    result
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect()
}

fn canonical(path: &Path) -> String {
    std::fs::canonicalize(path)
        .expect("fixture path should canonicalize")
        .to_string_lossy()
        .into_owned()
}

struct WorkspaceFixture {
    _tempdir: tempfile::TempDir,
    root: PathBuf,
    papers: PathBuf,
    paper: PathBuf,
    revision: PathBuf,
}

impl WorkspaceFixture {
    async fn create() -> Self {
        let tempdir = tempfile::tempdir().expect("source workspace should be created");
        let root = tempdir.path().to_path_buf();
        let papers = root.join("papers");
        let paper = papers.join("kafka-2011-distributed-messaging");
        let revision = paper.join("kafka-2011-netdb11");
        tokio::fs::create_dir_all(&revision)
            .await
            .expect("nested source workspace should be created");
        tokio::fs::write(revision.join("paper.pdf"), b"%PDF fixture")
            .await
            .expect("PDF fixture should be written");
        tokio::fs::write(revision.join("source.json"), b"{}")
            .await
            .expect("metadata fixture should be written");
        Self {
            _tempdir: tempdir,
            root,
            papers,
            paper,
            revision,
        }
    }
}
