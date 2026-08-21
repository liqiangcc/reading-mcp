use std::path::{Path, PathBuf};

use crate::application::ports::ApplicationError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListDocumentsCommand {
    pub path: Option<String>,
    pub recursive: bool,
    pub max_results: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedDocument {
    pub path: String,
    pub name: String,
    pub media_type: String,
    pub size_bytes: u64,
}

pub struct ListDocumentsUseCase {
    allowed_roots: Vec<PathBuf>,
}

impl ListDocumentsUseCase {
    pub fn new(allowed_roots: Vec<PathBuf>) -> Self {
        Self { allowed_roots }
    }

    pub async fn execute(
        &self,
        command: ListDocumentsCommand,
    ) -> Result<Vec<ListedDocument>, ApplicationError> {
        if command.max_results == 0 {
            return Err(ApplicationError::InvalidRequest(
                "max_results must be greater than zero".into(),
            ));
        }

        let roots = canonical_roots(&self.allowed_roots).await;
        if roots.is_empty() {
            return Ok(Vec::new());
        }

        let search_roots = match command.path {
            Some(path) => {
                let path = PathBuf::from(path.strip_prefix("file://").unwrap_or(&path));
                let canonical = tokio::fs::canonicalize(&path).await.map_err(|error| {
                    ApplicationError::RetrievalFailed(format!("{}: {error}", path.display()))
                })?;
                if !roots.iter().any(|root| canonical.starts_with(root)) {
                    return Err(ApplicationError::BlockedSource(format!(
                        "document listing path is outside configured roots: {}",
                        canonical.display()
                    )));
                }
                vec![canonical]
            }
            None => roots.clone(),
        };

        let mut documents = Vec::new();
        for root in search_roots {
            collect_documents(
                &root,
                &roots,
                command.recursive,
                command.max_results,
                &mut documents,
            )
            .await?;
            if documents.len() >= command.max_results {
                break;
            }
        }

        documents.sort_by(|left, right| left.path.cmp(&right.path));
        documents.truncate(command.max_results);
        Ok(documents)
    }
}

async fn canonical_roots(allowed_roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for root in allowed_roots {
        if let Ok(canonical) = tokio::fs::canonicalize(root).await
            && !roots.iter().any(|existing| existing == &canonical)
        {
            roots.push(canonical);
        }
    }
    roots
}

async fn collect_documents(
    directory: &Path,
    allowed_roots: &[PathBuf],
    recursive: bool,
    max_results: usize,
    output: &mut Vec<ListedDocument>,
) -> Result<(), ApplicationError> {
    let mut directories = vec![directory.to_path_buf()];
    while let Some(current) = directories.pop() {
        let mut entries = tokio::fs::read_dir(&current).await.map_err(|error| {
            ApplicationError::RetrievalFailed(format!("{}: {error}", current.display()))
        })?;

        while let Some(entry) = entries.next_entry().await.map_err(|error| {
            ApplicationError::RetrievalFailed(format!("{}: {error}", current.display()))
        })? {
            if output.len() >= max_results {
                return Ok(());
            }

            let file_type = entry.file_type().await.map_err(|error| {
                ApplicationError::RetrievalFailed(format!("{}: {error}", entry.path().display()))
            })?;
            let path = entry.path();

            if file_type.is_dir() {
                if recursive {
                    directories.push(path);
                }
                continue;
            }
            if !file_type.is_file() || !is_supported_document(&path) {
                continue;
            }

            let canonical = tokio::fs::canonicalize(&path).await.map_err(|error| {
                ApplicationError::RetrievalFailed(format!("{}: {error}", path.display()))
            })?;
            if !allowed_roots.iter().any(|root| canonical.starts_with(root)) {
                continue;
            }

            let metadata = tokio::fs::metadata(&canonical).await.map_err(|error| {
                ApplicationError::RetrievalFailed(format!("{}: {error}", canonical.display()))
            })?;
            let name = canonical
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_owned();
            output.push(ListedDocument {
                path: canonical.to_string_lossy().into_owned(),
                name,
                media_type: media_type_for_path(&canonical).to_owned(),
                size_bytes: metadata.len(),
            });
        }
    }

    Ok(())
}

fn is_supported_document(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("md")
            | Some("markdown")
            | Some("txt")
            | Some("text")
            | Some("html")
            | Some("htm")
            | Some("pdf")
            | Some("epub")
            | Some("docx")
            | Some("json")
            | Some("yaml")
            | Some("yml")
    )
}

fn media_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("md") | Some("markdown") => "text/markdown",
        Some("txt") | Some("text") => "text/plain",
        Some("html") | Some("htm") => "text/html",
        Some("pdf") => "application/pdf",
        Some("epub") => "application/epub+zip",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("json") => "application/json",
        Some("yaml") | Some("yml") => "application/yaml",
        _ => "application/octet-stream",
    }
}
