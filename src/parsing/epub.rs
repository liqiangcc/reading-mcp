use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;
use std::sync::Arc;

use async_trait::async_trait;
use roxmltree::Document as XmlDocument;
use zip::ZipArchive;

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::{Document, DocumentSource, Location, MediaType, Section, SectionId};

use super::archive::{
    ArchiveLimits, read_entry, read_optional_entry, utf8_entry, validate_archive_entries,
};
use super::common::{content_hash, document_id, slugify, title_from_metadata};
use super::HtmlParser;

pub struct EpubParser {
    limits: ArchiveLimits,
    html: Arc<dyn Parser>,
}

impl EpubParser {
    pub fn new(limits: ArchiveLimits) -> Self {
        Self {
            limits,
            html: Arc::new(HtmlParser),
        }
    }
}

#[async_trait]
impl Parser for EpubParser {
    async fn parse(&self, resource: RetrievedResource) -> Result<Document, ApplicationError> {
        let hash = content_hash(&resource.bytes);
        let id = document_id(&resource.final_source, &hash);
        let mut archive = ZipArchive::new(Cursor::new(resource.bytes.as_slice())).map_err(|error| {
            ApplicationError::ParseFailed(format!("invalid EPUB ZIP archive: {error}"))
        })?;
        validate_archive_entries(&archive, &self.limits)?;
        let mut total_read = 0usize;

        let container = utf8_entry(
            read_entry(
                &mut archive,
                "META-INF/container.xml",
                &self.limits,
                &mut total_read,
            )?,
            "META-INF/container.xml",
        )?;
        let container_xml = XmlDocument::parse(&container).map_err(|error| {
            ApplicationError::ParseFailed(format!("invalid EPUB container.xml: {error}"))
        })?;
        let package_path = container_xml
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "rootfile")
            .and_then(|node| node.attribute("full-path"))
            .ok_or_else(|| {
                ApplicationError::ParseFailed("EPUB container has no rootfile".into())
            })?
            .to_string();

        let package = utf8_entry(
            read_entry(
                &mut archive,
                &package_path,
                &self.limits,
                &mut total_read,
            )?,
            &package_path,
        )?;
        let package_xml = XmlDocument::parse(&package).map_err(|error| {
            ApplicationError::ParseFailed(format!("invalid EPUB package document: {error}"))
        })?;

        let package_title = package_xml
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "title")
            .and_then(|node| node.text())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        let manifest = package_xml
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "item")
            .filter_map(|node| {
                Some((
                    node.attribute("id")?.to_string(),
                    (
                        node.attribute("href")?.to_string(),
                        node.attribute("media-type").unwrap_or_default().to_string(),
                    ),
                ))
            })
            .collect::<HashMap<_, _>>();
        let spine = package_xml
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "itemref")
            .filter_map(|node| node.attribute("idref").map(str::to_string))
            .collect::<Vec<_>>();
        if spine.is_empty() {
            return Err(ApplicationError::ParseFailed(
                "EPUB package has an empty spine".into(),
            ));
        }

        let mut root_sections = Vec::new();
        let mut parsed_spine = 0usize;
        for (spine_index, idref) in spine.iter().enumerate() {
            let Some((href, media_type)) = manifest.get(idref) else {
                continue;
            };
            if !matches!(
                media_type.as_str(),
                "application/xhtml+xml" | "text/html" | "application/xml" | "text/xml"
            ) {
                continue;
            }
            let entry_path = resolve_archive_path(&package_path, href)?;
            let xhtml = read_entry(
                &mut archive,
                &entry_path,
                &self.limits,
                &mut total_read,
            )?;
            let parsed = self
                .html
                .parse(RetrievedResource {
                    source: DocumentSource(format!("epub:{entry_path}")),
                    final_source: DocumentSource(format!("epub:{entry_path}")),
                    media_type: MediaType("text/html".into()),
                    bytes: xhtml,
                    etag: None,
                    last_modified: None,
                    metadata: BTreeMap::new(),
                })
                .await?;
            parsed_spine += 1;
            for section in parsed.root_sections {
                root_sections.push(remap_epub_section(
                    section,
                    spine_index + 1,
                    &entry_path,
                    None,
                ));
            }
        }

        if root_sections.is_empty() {
            return Err(ApplicationError::ParseFailed(
                "EPUB spine contains no readable XHTML content".into(),
            ));
        }

        let mut metadata = resource.metadata;
        metadata.insert("epub_package_path".into(), package_path);
        metadata.insert("epub_spine_items".into(), parsed_spine.to_string());
        if let Some(nav) = manifest
            .values()
            .find(|(_, media)| media == "application/xhtml+xml")
            .map(|(href, _)| href)
        {
            let _ = read_optional_entry(
                &mut archive,
                &resolve_archive_path(
                    metadata
                        .get("epub_package_path")
                        .expect("package path just inserted"),
                    nav,
                )?,
                &self.limits,
                &mut total_read,
            );
        }

        let title = package_title
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| title_from_metadata(&metadata, &resource.final_source));

        Ok(Document {
            id,
            source: resource.final_source,
            title,
            media_type: resource.media_type,
            content_hash: hash,
            metadata,
            root_sections,
        })
    }
}

fn remap_epub_section(
    mut section: Section,
    spine_index: usize,
    entry_path: &str,
    parent_id: Option<SectionId>,
) -> Section {
    let suffix = section
        .id
        .0
        .strip_prefix("section://")
        .unwrap_or(&section.id.0);
    let id = SectionId(format!("section://epub-{spine_index}/{suffix}"));
    let anchor = section.location.anchor.clone();
    section.id = id.clone();
    section.parent_id = parent_id;
    section.location.chapter = Some(format!("spine-{spine_index}"));
    section.location.native_location = Some(match anchor {
        Some(anchor) => format!("epub:{entry_path}#{anchor}"),
        None => format!("epub:{entry_path}"),
    });
    section.children = section
        .children
        .into_iter()
        .map(|child| remap_epub_section(child, spine_index, entry_path, Some(id.clone())))
        .collect();
    section
}

fn resolve_archive_path(package_path: &str, href: &str) -> Result<String, ApplicationError> {
    let href = href.split('#').next().unwrap_or(href).split('?').next().unwrap_or(href);
    let base = package_path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or_default();
    let combined = if base.is_empty() {
        href.to_string()
    } else {
        format!("{base}/{href}")
    };
    let mut segments = Vec::new();
    for segment in combined.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(ApplicationError::ParseFailed(
                        "EPUB manifest path escapes archive root".into(),
                    ));
                }
            }
            value => segments.push(value),
        }
    }
    if segments.is_empty() {
        return Err(ApplicationError::ParseFailed(
            "EPUB manifest contains an empty content path".into(),
        ));
    }
    Ok(segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::resolve_archive_path;

    #[test]
    fn epub_paths_are_resolved_without_leaving_archive_root() {
        assert_eq!(
            resolve_archive_path("OPS/package.opf", "text/ch1.xhtml").unwrap(),
            "OPS/text/ch1.xhtml"
        );
        assert!(resolve_archive_path("package.opf", "../outside.xhtml").is_err());
    }
}
