use std::sync::Arc;

use reading_mcp::application::ports::DocumentRepository;
use reading_mcp::domain::{ContentHash, Document, DocumentId, DocumentSource, MediaType};
use reading_mcp::infrastructure::SqliteDocumentRepository;

fn document(n: usize) -> Document {
    Document {
        id: DocumentId(format!("doc-{n}")),
        source: DocumentSource(format!("memory:doc-{n}.txt")),
        title: format!("Doc {n}"),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash(format!("sha256:{n}")),
        metadata: Default::default(),
        root_sections: vec![],
    }
}

#[tokio::test(flavor = "current_thread")]
async fn repository_round_trips_on_current_thread_runtime() {
    let directory = tempfile::tempdir().expect("tempdir");
    let repository =
        SqliteDocumentRepository::open(directory.path().join("state.sqlite")).expect("open");
    repository.save(document(1)).await.expect("save");
    let loaded = repository
        .get(&DocumentId("doc-1".into()))
        .await
        .expect("get")
        .expect("saved");
    assert_eq!(loaded, document(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_saves_and_reads_complete() {
    let directory = tempfile::tempdir().expect("tempdir");
    let repository = Arc::new(
        SqliteDocumentRepository::open(directory.path().join("state.sqlite")).expect("open"),
    );
    let tasks: Vec<_> = (0..16)
        .map(|n| {
            let repository = Arc::clone(&repository);
            tokio::spawn(async move {
                repository.save(document(n)).await.expect("save");
                repository
                    .get(&DocumentId(format!("doc-{n}")))
                    .await
                    .expect("get")
                    .expect("saved")
            })
        })
        .collect();
    for (n, task) in tasks.into_iter().enumerate() {
        assert_eq!(task.await.expect("task"), document(n));
    }
}
