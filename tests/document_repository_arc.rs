use std::sync::Arc;

use reading_mcp::application::ports::DocumentRepository;
use reading_mcp::domain::{ContentHash, Document, DocumentId, DocumentSource, MediaType};
use reading_mcp::infrastructure::InMemoryDocumentRepository;

fn document() -> Document {
    Document {
        id: DocumentId("doc-1".into()),
        source: DocumentSource("memory:doc-1.txt".into()),
        title: "Doc 1".into(),
        media_type: MediaType("text/plain".into()),
        content_hash: ContentHash("sha256:1".into()),
        metadata: Default::default(),
        root_sections: vec![],
    }
}

#[tokio::test]
async fn in_memory_get_shares_one_document_instead_of_cloning() {
    let repository = InMemoryDocumentRepository::default();
    repository.save(document()).await.expect("save");
    let id = DocumentId("doc-1".into());
    let first = repository.get(&id).await.expect("get").expect("saved");
    let second = repository.get(&id).await.expect("get").expect("saved");
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(*first, document());
}
