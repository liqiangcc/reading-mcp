use std::sync::Arc;

use async_trait::async_trait;
use reading_mcp::application::ports::{ApplicationError, DocumentRepository};
use reading_mcp::domain::{
    ContentHash, Document, DocumentId, DocumentSource, MediaType, NormalizedDocumentHash, Section,
    SectionId,
};
use reading_mcp::infrastructure::ObservedDocumentRepository;

struct FakeStore {
    document: Document,
    hash: NormalizedDocumentHash,
}

#[async_trait]
impl DocumentRepository for FakeStore {
    async fn save(&self, _document: Document) -> Result<(), ApplicationError> {
        panic!("observed wrapper must not call save");
    }

    async fn get(&self, _id: &DocumentId) -> Result<Option<Arc<Document>>, ApplicationError> {
        panic!("observed wrapper must not call get");
    }

    async fn get_with_hash(
        &self,
        id: &DocumentId,
    ) -> Result<Option<(Arc<Document>, NormalizedDocumentHash)>, ApplicationError> {
        if &self.document.id == id {
            Ok(Some((Arc::new(self.document.clone()), self.hash.clone())))
        } else {
            Ok(None)
        }
    }
}

fn fixture_document() -> Document {
    Document {
        id: DocumentId("doc:observed-repository".into()),
        source: DocumentSource("memory:observed-repository.md".into()),
        title: "Observed repository".into(),
        media_type: MediaType("text/markdown".into()),
        content_hash: ContentHash("sha256:observed-repository".into()),
        metadata: Default::default(),
        root_sections: vec![Section {
            id: SectionId("section://root".into()),
            parent_id: None,
            title: "Root".into(),
            level: 1,
            content: "Observed content.".into(),
            location: Default::default(),
            children: vec![],
        }],
    }
}

#[tokio::test]
async fn observed_repository_forwards_get_with_hash_to_the_inner_override() {
    let document = fixture_document();
    let hash = document.normalized_document_hash();
    let observed = ObservedDocumentRepository::new(Arc::new(FakeStore {
        document: document.clone(),
        hash: hash.clone(),
    }));

    let (loaded, loaded_hash) = observed
        .get_with_hash(&document.id)
        .await
        .expect("observed get_with_hash should succeed")
        .expect("document should exist");
    assert_eq!(*loaded, document);
    assert_eq!(loaded_hash, hash);
}
