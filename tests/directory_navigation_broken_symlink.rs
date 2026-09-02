#[cfg(unix)]
mod unix {
    use std::os::unix::fs::symlink;

    use reading_mcp::application::list_directories::{
        DirectoryEntryKind, ListDirectoryCommand, ListDirectoryUseCase,
    };

    #[tokio::test]
    async fn broken_symlink_is_skipped_without_aborting_sibling_discovery() {
        let root = tempfile::tempdir().expect("source root should be created");
        tokio::fs::create_dir(root.path().join("papers"))
            .await
            .expect("directory sibling should be created");
        tokio::fs::write(root.path().join("paper.md"), "# Paper\n")
            .await
            .expect("document sibling should be created");
        symlink(root.path().join("missing-target"), root.path().join("broken"))
            .expect("broken symlink should be created");

        let result = ListDirectoryUseCase::new(vec![root.path().to_path_buf()])
            .execute(ListDirectoryCommand {
                path: Some(root.path().to_string_lossy().into_owned()),
                max_results: 10,
                cursor: None,
            })
            .await
            .expect("broken symlink must not abort directory discovery");

        assert_eq!(
            result
                .entries
                .iter()
                .map(|entry| (entry.name.as_str(), entry.kind))
                .collect::<Vec<_>>(),
            vec![
                ("paper.md", DirectoryEntryKind::Document),
                ("papers", DirectoryEntryKind::Directory),
            ]
        );
        assert!(result.entries.iter().all(|entry| entry.name != "broken"));
        assert!(result.complete);
        assert!(result.next_cursor.is_none());
    }
}
