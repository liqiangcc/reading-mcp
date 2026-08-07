use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn core_layers_do_not_gain_forbidden_concrete_dependencies() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");

    assert_tree_excludes(
        &root.join("domain"),
        &["crate::mcp", "rmcp::", "reqwest::", "lopdf::", "rusqlite::", "scraper::", "zip::"],
    );
    assert_tree_excludes(
        &root.join("application"),
        &["crate::mcp", "rmcp::", "reqwest::", "lopdf::", "rusqlite::", "scraper::", "zip::"],
    );
    assert_tree_excludes(
        &root.join("parsing"),
        &["crate::mcp", "rmcp::", "reqwest::", "rusqlite::"],
    );
    assert_tree_excludes(
        &root.join("retrieval"),
        &["crate::mcp", "rmcp::", "lopdf::", "scraper::", "rusqlite::"],
    );
    assert_tree_excludes(
        &root.join("security"),
        &["crate::mcp", "rmcp::", "lopdf::", "scraper::", "rusqlite::"],
    );
    assert_tree_excludes(&root.join("infrastructure"), &["crate::mcp", "rmcp::"]);
}

fn assert_tree_excludes(root: &Path, forbidden: &[&str]) {
    for file in rust_files(root) {
        let content = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("failed reading {}: {error}", file.display()));
        for needle in forbidden {
            assert!(
                !content.contains(needle),
                "{} contains forbidden dependency marker {needle:?}",
                file.display()
            );
        }
    }
}

fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rust_files(root, &mut files);
    files
}

fn collect_rust_files(root: &Path, output: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(root)
        .unwrap_or_else(|error| panic!("failed reading {}: {error}", root.display()));
    for entry in entries {
        let entry = entry.expect("source directory entry should be readable");
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, output);
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            output.push(path);
        }
    }
}
