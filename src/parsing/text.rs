use async_trait::async_trait;

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::{Document, Location, Section, SectionId};

use super::common::{content_hash, document_id, title_from_metadata};

#[derive(Default)]
pub struct TextParser;

#[async_trait]
impl Parser for TextParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let text = String::from_utf8(resource.bytes.clone())
            .map_err(|error| ApplicationError::ParseFailed(format!("invalid UTF-8 text: {error}")))?;
        let hash = content_hash(&resource.bytes);
        let id = document_id(&resource.final_source, &hash);
        let title = title_from_metadata(&resource.metadata, &resource.final_source);
        let char_count = text.chars().count();

        Ok(Document {
            id,
            source: resource.final_source,
            title: title.clone(),
            media_type: resource.media_type,
            content_hash: hash,
            metadata: resource.metadata,
            root_sections: vec![Section {
                id: SectionId("section://document".into()),
                parent_id: None,
                title,
                level: 1,
                content: text,
                location: Location {
                    section_path: vec!["document".into()],
                    char_start: Some(0),
                    char_end: Some(char_count),
                    native_location: Some("text:0".into()),
                    ..Location::default()
                },
                children: vec![],
            }],
        })
    }
}
