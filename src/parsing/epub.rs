use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

use async_trait::async_trait;
use roxmltree::Document as XmlDocument;
use zip::ZipArchive;

use crate::application::ports::{ApplicationError, Parser, RetrievedResource};
use crate::domain::{
    Document, DocumentSource, MediaType, NormalizedBlock, NormalizedBlockMap, Section, SectionId,
};

use super::HtmlParser;
use super::archive::{ArchiveLimits, read_entry, utf8_entry, validate_archive_entries};
use super::common::{content_hash, document_id, title_from_metadata};
use super::epub_navigation::{
    EPUB_NAVIGATION_MAP_VERSION, FragmentCache, build_navigation_map,
    is_supported_content_media_type, parse_package_facts, remember_fragment_index,
    resolve_archive_path,
};
use super::epub_structure::{
    EPUB_STRUCTURE_MAP_VERSION, EpubSpineParseStatus, EpubSpineRecord, ParsedSpineDocument,
    reconcile_epub_structure,
};

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
        let mut archive =
            ZipArchive::new(Cursor::new(resource.bytes.as_slice())).map_err(|error| {
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
            .ok_or_else(|| ApplicationError::ParseFailed("EPUB container has no rootfile".into()))?
            .to_string();

        let package = utf8_entry(
            read_entry(&mut archive, &package_path, &self.limits, &mut total_read)?,
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
        let package_facts = parse_package_facts(&package_xml);
        if package_facts.spine.is_empty() {
            return Err(ApplicationError::ParseFailed(
                "EPUB package has an empty spine".into(),
            ));
        }
        let spine_linearity = package_xml
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "itemref")
            .map(|node| {
                !node
                    .attribute("linear")
                    .is_some_and(|value| value.eq_ignore_ascii_case("no"))
            })
            .collect::<Vec<_>>();

        let mut parsed_spine_documents = Vec::new();
        let mut spine_records = Vec::with_capacity(package_facts.spine.len());
        let mut normalized_blocks = Vec::<NormalizedBlock>::new();
        let mut parsed_spine = 0usize;
        let mut fragment_cache = FragmentCache::new();

        for (spine_index, spine_item) in package_facts.spine.iter().enumerate() {
            let ordinal = spine_index + 1;
            let linear = spine_linearity.get(spine_index).copied().unwrap_or(true);
            let Some(manifest_item) = package_facts.manifest_item(&spine_item.idref) else {
                spine_records.push(EpubSpineRecord {
                    spine_index: ordinal,
                    idref: spine_item.idref.clone(),
                    linear,
                    manifest_href: None,
                    resolved_entry_path: None,
                    media_type: None,
                    parse_status: EpubSpineParseStatus::MissingManifest,
                });
                continue;
            };

            if !is_supported_content_media_type(&manifest_item.media_type) {
                spine_records.push(EpubSpineRecord {
                    spine_index: ordinal,
                    idref: spine_item.idref.clone(),
                    linear,
                    manifest_href: Some(manifest_item.href.clone()),
                    resolved_entry_path: resolve_archive_path(&package_path, &manifest_item.href)
                        .ok(),
                    media_type: Some(manifest_item.media_type.clone()),
                    parse_status: EpubSpineParseStatus::UnsupportedMedia,
                });
                continue;
            }

            let entry_path =
                resolve_archive_path(&package_path, &manifest_item.href).map_err(|message| {
                    ApplicationError::ParseFailed(format!(
                        "EPUB manifest path {:?} is invalid: {message}",
                        manifest_item.href
                    ))
                })?;
            let xhtml = read_entry(&mut archive, &entry_path, &self.limits, &mut total_read)?;
            remember_fragment_index(
                &mut fragment_cache,
                &entry_path,
                &manifest_item.media_type,
                &xhtml,
            );
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
            let parsed_blocks = parsed
                .normalized_block_map()
                .map_err(|error| ApplicationError::ParseFailed(error.to_string()))?
                .unwrap_or_else(|| NormalizedBlockMap::new(Vec::new()));
            parsed_spine += 1;
            let sections = parsed
                .root_sections
                .into_iter()
                .map(|section| remap_epub_section(section, ordinal, &entry_path, None))
                .collect::<Vec<_>>();
            append_remapped_blocks(parsed_blocks, ordinal, &entry_path, &mut normalized_blocks);
            parsed_spine_documents.push(ParsedSpineDocument {
                spine_index: ordinal,
                linear,
                entry_path: entry_path.clone(),
                sections,
            });
            spine_records.push(EpubSpineRecord {
                spine_index: ordinal,
                idref: spine_item.idref.clone(),
                linear,
                manifest_href: Some(manifest_item.href.clone()),
                resolved_entry_path: Some(entry_path),
                media_type: Some(manifest_item.media_type.clone()),
                parse_status: EpubSpineParseStatus::Parsed,
            });
        }

        if parsed_spine_documents.is_empty() {
            return Err(ApplicationError::ParseFailed(
                "EPUB spine contains no readable XHTML content".into(),
            ));
        }

        let navigation_map = build_navigation_map(
            &mut archive,
            &package_path,
            &package_facts,
            &mut fragment_cache,
            &self.limits,
            &mut total_read,
        )?;
        let navigation_json = serde_json::to_string(&navigation_map)
            .map_err(|error| ApplicationError::ParseFailed(error.to_string()))?;
        let reconciled =
            reconcile_epub_structure(parsed_spine_documents, spine_records, &navigation_map);
        let structure_json = serde_json::to_string(&reconciled.structure_map)
            .map_err(|error| ApplicationError::ParseFailed(error.to_string()))?;

        drop(archive);
        let mut metadata = resource.metadata;
        metadata.insert("epub_package_path".into(), package_path);
        metadata.insert(
            "epub_package_version".into(),
            package_facts
                .version
                .clone()
                .unwrap_or_else(|| "unknown".into()),
        );
        metadata.insert(
            "epub_manifest_items".into(),
            package_facts.manifest.len().to_string(),
        );
        metadata.insert(
            "epub_spine_items_total".into(),
            package_facts.spine.len().to_string(),
        );
        metadata.insert("epub_spine_items".into(), parsed_spine.to_string());
        metadata.insert(
            "epub_navigation_map_version".into(),
            EPUB_NAVIGATION_MAP_VERSION.into(),
        );
        metadata.insert(
            "epub_navigation_provenance".into(),
            navigation_map
                .provenance
                .map(|value| value.as_str())
                .unwrap_or("none")
                .into(),
        );
        if let Some(source_path) = &navigation_map.source_path {
            metadata.insert("epub_navigation_source_path".into(), source_path.clone());
        }
        metadata.insert(
            "epub_navigation_nodes".into(),
            navigation_map.node_count().to_string(),
        );
        metadata.insert(
            "epub_navigation_resolved_nodes".into(),
            navigation_map.resolved_node_count().to_string(),
        );
        metadata.insert(
            "epub_navigation_diagnostics".into(),
            navigation_map.diagnostics.len().to_string(),
        );
        metadata.insert("epub_navigation_map".into(), navigation_json);
        metadata.insert(
            "epub_structure_map_version".into(),
            EPUB_STRUCTURE_MAP_VERSION.into(),
        );
        metadata.insert(
            "epub_structure_sections".into(),
            reconciled.structure_map.section_count().to_string(),
        );
        metadata.insert(
            "epub_structure_applied_navigation_nodes".into(),
            reconciled
                .structure_map
                .applied_navigation_nodes
                .to_string(),
        );
        metadata.insert(
            "epub_linear_spine_items".into(),
            reconciled.structure_map.linear_spine_items.to_string(),
        );
        metadata.insert(
            "epub_non_linear_spine_items".into(),
            reconciled.structure_map.non_linear_spine_items.to_string(),
        );
        metadata.insert(
            "epub_structure_diagnostics".into(),
            reconciled.structure_map.diagnostics.len().to_string(),
        );
        metadata.insert("epub_structure_map".into(), structure_json);
        let title = package_title
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| title_from_metadata(&metadata, &resource.final_source));

        let mut document = Document {
            id,
            source: resource.final_source,
            title,
            media_type: resource.media_type,
            content_hash: hash,
            metadata,
            root_sections: reconciled.root_sections,
        };
        document
            .set_normalized_block_map(NormalizedBlockMap::new(normalized_blocks))
            .map_err(|error| ApplicationError::ParseFailed(error.to_string()))?;
        Ok(document)
    }
}

fn append_remapped_blocks(
    map: NormalizedBlockMap,
    spine_index: usize,
    entry_path: &str,
    output: &mut Vec<NormalizedBlock>,
) {
    for mut block in map.blocks {
        block.owner_section_id = remap_epub_section_id(&block.owner_section_id, spine_index);
        block.source_order = output.len();
        block.native_location = Some(match block.native_anchor.as_deref() {
            Some(anchor) => format!("epub:{entry_path}#{anchor}"),
            None => format!(
                "epub:{entry_path};{}",
                block.native_location.as_deref().unwrap_or("html:block")
            ),
        });
        output.push(block);
    }
}

fn remap_epub_section(
    mut section: Section,
    spine_index: usize,
    entry_path: &str,
    parent_id: Option<SectionId>,
) -> Section {
    let id = remap_epub_section_id(&section.id, spine_index);
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

fn remap_epub_section_id(section_id: &SectionId, spine_index: usize) -> SectionId {
    let suffix = section_id
        .0
        .strip_prefix("section://")
        .unwrap_or(&section_id.0);
    SectionId(format!("section://epub-{spine_index}/{suffix}"))
}
