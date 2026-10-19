use async_trait::async_trait;

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::{Document, Location, Section, SectionId};

use super::blocking::run_blocking;
use super::charset::decode_declared_charset;
use super::common::{content_hash, document_id, title_from_metadata};

#[derive(Default)]
pub struct TextParser;

impl TextParser {
    fn parse_sync(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let hash = content_hash(&resource.bytes);
        let id = document_id(&resource.final_source, &hash);
        let mut metadata = resource.metadata;
        let text = match String::from_utf8(resource.bytes) {
            Ok(text) => text,
            Err(error) => {
                let utf8_error = error.utf8_error();
                let bytes = error.into_bytes();
                let Some((text, (encoding, origin))) =
                    decode_declared_charset(&bytes, &resource.media_type.0)?
                else {
                    return Err(ApplicationError::ParseFailed(format!(
                        "invalid UTF-8 text: {utf8_error}"
                    )));
                };
                metadata.insert("text_charset".into(), encoding.name().into());
                metadata.insert("text_charset_source".into(), origin.into());
                text.into_owned()
            }
        };
        let title = title_from_metadata(&metadata, &resource.final_source);
        let char_count = text.chars().count();

        Ok(Document {
            id,
            source: resource.final_source,
            title: title.clone(),
            media_type: resource.media_type,
            content_hash: hash,
            metadata,
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

#[async_trait]
impl Parser for TextParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        run_blocking(move || TextParser.parse_sync(resource)).await
    }
}
