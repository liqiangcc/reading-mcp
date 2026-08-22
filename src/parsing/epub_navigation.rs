use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek};

use roxmltree::{Document as XmlDocument, Node};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use zip::ZipArchive;

use crate::application::ports::ApplicationError;

use super::archive::{ArchiveLimits, read_entry, utf8_entry};

pub(crate) const EPUB_NAVIGATION_MAP_VERSION: &str = "epub-navigation-map/v1";
const EPUB_NCX_MEDIA_TYPE: &str = "application/x-dtbncx+xml";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManifestItem {
    pub(crate) id: String,
    pub(crate) href: String,
    pub(crate) media_type: String,
    pub(crate) properties: Vec<String>,
    pub(crate) fallback: Option<String>,
}

impl ManifestItem {
    pub(crate) fn has_property(&self, property: &str) -> bool {
        self.properties.iter().any(|value| value == property)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpineItem {
    pub(crate) idref: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EpubPackageFacts {
    pub(crate) version: Option<String>,
    pub(crate) manifest: Vec<ManifestItem>,
    pub(crate) spine: Vec<SpineItem>,
    pub(crate) spine_toc_id: Option<String>,
}

impl EpubPackageFacts {
    pub(crate) fn manifest_item(&self, id: &str) -> Option<&ManifestItem> {
        self.manifest.iter().find(|item| item.id == id)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EpubNavigationProvenance {
    EpubNav,
    EpubNcx,
}

impl EpubNavigationProvenance {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::EpubNav => "epub_nav",
            Self::EpubNcx => "epub_ncx",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NavigationResolutionStatus {
    ResolvedDocument,
    ResolvedFragment,
    MissingFragment,
    MissingResource,
    UnsupportedResource,
    InvalidPath,
    MalformedResource,
    Unlinked,
}

impl NavigationResolutionStatus {
    pub(crate) const fn is_resolved(self) -> bool {
        matches!(self, Self::ResolvedDocument | Self::ResolvedFragment)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EpubNavigationNode {
    pub(crate) label: String,
    pub(crate) depth: usize,
    pub(crate) href: Option<String>,
    pub(crate) resolved_entry_path: Option<String>,
    pub(crate) fragment: Option<String>,
    pub(crate) source_order: usize,
    pub(crate) provenance: EpubNavigationProvenance,
    pub(crate) resolution_status: NavigationResolutionStatus,
    pub(crate) diagnostic: Option<String>,
    pub(crate) children: Vec<EpubNavigationNode>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EpubNavigationDiagnostic {
    pub(crate) code: String,
    pub(crate) message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EpubNavigationMap {
    pub(crate) schema_version: String,
    pub(crate) package_version: Option<String>,
    pub(crate) provenance: Option<EpubNavigationProvenance>,
    pub(crate) source_manifest_id: Option<String>,
    pub(crate) source_path: Option<String>,
    pub(crate) source_properties: Vec<String>,
    pub(crate) nodes: Vec<EpubNavigationNode>,
    pub(crate) diagnostics: Vec<EpubNavigationDiagnostic>,
}

impl EpubNavigationMap {
    pub(crate) fn node_count(&self) -> usize {
        count_nodes(&self.nodes)
    }

    pub(crate) fn resolved_node_count(&self) -> usize {
        count_resolved_nodes(&self.nodes)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum FragmentIndex {
    Resolved(HashSet<String>),
    Malformed(String),
}

pub(crate) type FragmentCache = HashMap<String, FragmentIndex>;

pub(crate) fn parse_package_facts(package_xml: &XmlDocument<'_>) -> EpubPackageFacts {
    let package = package_xml
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "package");
    let version = package
        .and_then(|node| node.attribute("version"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let manifest = package_xml
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .filter_map(|node| {
            Some(ManifestItem {
                id: node.attribute("id")?.to_string(),
                href: node.attribute("href")?.to_string(),
                media_type: node.attribute("media-type").unwrap_or_default().to_string(),
                properties: node
                    .attribute("properties")
                    .unwrap_or_default()
                    .split_whitespace()
                    .map(str::to_string)
                    .collect(),
                fallback: node.attribute("fallback").map(str::to_string),
            })
        })
        .collect();

    let spine_node = package_xml
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "spine");
    let spine_toc_id = spine_node
        .and_then(|node| node.attribute("toc"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let spine = spine_node
        .into_iter()
        .flat_map(|node| node.children())
        .filter(|node| node.is_element() && node.tag_name().name() == "itemref")
        .filter_map(|node| {
            node.attribute("idref").map(|idref| SpineItem {
                idref: idref.to_string(),
            })
        })
        .collect();

    EpubPackageFacts {
        version,
        manifest,
        spine,
        spine_toc_id,
    }
}

pub(crate) fn remember_fragment_index(
    cache: &mut FragmentCache,
    entry_path: &str,
    media_type: &str,
    bytes: &[u8],
) {
    cache.insert(entry_path.to_string(), fragment_index(media_type, bytes));
}

pub(crate) fn build_navigation_map<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    package_path: &str,
    facts: &EpubPackageFacts,
    fragment_cache: &mut FragmentCache,
    limits: &ArchiveLimits,
    total_read: &mut usize,
) -> Result<EpubNavigationMap, ApplicationError> {
    let mut diagnostics = Vec::new();
    let nav_items = facts
        .manifest
        .iter()
        .filter(|item| item.has_property("nav"))
        .collect::<Vec<_>>();
    if nav_items.len() > 1 {
        diagnostics.push(EpubNavigationDiagnostic {
            code: "multiple_epub_nav_resources".into(),
            message: format!(
                "manifest declares {} resources with the nav property; first valid TOC is used",
                nav_items.len()
            ),
        });
    }

    for item in nav_items {
        match load_navigation_source(archive, package_path, item, limits, total_read) {
            Ok((source_path, source)) => match parse_epub_nav(&source) {
                Ok(raw_nodes) if !raw_nodes.is_empty() => {
                    let mut source_order = 0usize;
                    let nodes = resolve_nodes(
                        raw_nodes,
                        1,
                        EpubNavigationProvenance::EpubNav,
                        &source_path,
                        archive,
                        package_path,
                        facts,
                        fragment_cache,
                        limits,
                        total_read,
                        &mut source_order,
                    )?;
                    return Ok(EpubNavigationMap {
                        schema_version: EPUB_NAVIGATION_MAP_VERSION.into(),
                        package_version: facts.version.clone(),
                        provenance: Some(EpubNavigationProvenance::EpubNav),
                        source_manifest_id: Some(item.id.clone()),
                        source_path: Some(source_path),
                        source_properties: item.properties.clone(),
                        nodes,
                        diagnostics,
                    });
                }
                Ok(_) => diagnostics.push(EpubNavigationDiagnostic {
                    code: "epub_nav_has_no_toc_nodes".into(),
                    message: format!("EPUB navigation resource {:?} has no TOC nodes", item.href),
                }),
                Err(message) => diagnostics.push(EpubNavigationDiagnostic {
                    code: "malformed_epub_nav".into(),
                    message: format!("EPUB navigation resource {:?}: {message}", item.href),
                }),
            },
            Err(LoadNavigationError::ResourceLimit(error)) => return Err(error),
            Err(LoadNavigationError::Degraded(message)) => {
                diagnostics.push(EpubNavigationDiagnostic {
                    code: "unreadable_epub_nav".into(),
                    message,
                });
            }
        }
    }

    if let Some(item) = select_ncx_item(facts, &mut diagnostics) {
        match load_navigation_source(archive, package_path, item, limits, total_read) {
            Ok((source_path, source)) => match parse_ncx(&source) {
                Ok(raw_nodes) if !raw_nodes.is_empty() => {
                    let mut source_order = 0usize;
                    let nodes = resolve_nodes(
                        raw_nodes,
                        1,
                        EpubNavigationProvenance::EpubNcx,
                        &source_path,
                        archive,
                        package_path,
                        facts,
                        fragment_cache,
                        limits,
                        total_read,
                        &mut source_order,
                    )?;
                    return Ok(EpubNavigationMap {
                        schema_version: EPUB_NAVIGATION_MAP_VERSION.into(),
                        package_version: facts.version.clone(),
                        provenance: Some(EpubNavigationProvenance::EpubNcx),
                        source_manifest_id: Some(item.id.clone()),
                        source_path: Some(source_path),
                        source_properties: item.properties.clone(),
                        nodes,
                        diagnostics,
                    });
                }
                Ok(_) => diagnostics.push(EpubNavigationDiagnostic {
                    code: "epub_ncx_has_no_navpoints".into(),
                    message: format!("NCX resource {:?} has no navPoint entries", item.href),
                }),
                Err(message) => diagnostics.push(EpubNavigationDiagnostic {
                    code: "malformed_epub_ncx".into(),
                    message: format!("NCX resource {:?}: {message}", item.href),
                }),
            },
            Err(LoadNavigationError::ResourceLimit(error)) => return Err(error),
            Err(LoadNavigationError::Degraded(message)) => {
                diagnostics.push(EpubNavigationDiagnostic {
                    code: "unreadable_epub_ncx".into(),
                    message,
                });
            }
        }
    }

    diagnostics.push(EpubNavigationDiagnostic {
        code: "navigation_unavailable".into(),
        message: "no usable EPUB 3 TOC navigation or legacy NCX navigation was found; XHTML heading/spine fallback remains available for later reconciliation".into(),
    });
    Ok(EpubNavigationMap {
        schema_version: EPUB_NAVIGATION_MAP_VERSION.into(),
        package_version: facts.version.clone(),
        provenance: None,
        source_manifest_id: None,
        source_path: None,
        source_properties: Vec::new(),
        nodes: Vec::new(),
        diagnostics,
    })
}

enum LoadNavigationError {
    ResourceLimit(ApplicationError),
    Degraded(String),
}

fn load_navigation_source<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    package_path: &str,
    item: &ManifestItem,
    limits: &ArchiveLimits,
    total_read: &mut usize,
) -> Result<(String, String), LoadNavigationError> {
    let source_path = resolve_archive_path(package_path, &item.href).map_err(|message| {
        LoadNavigationError::Degraded(format!(
            "invalid navigation resource path {:?}: {message}",
            item.href
        ))
    })?;
    let bytes =
        read_entry(archive, &source_path, limits, total_read).map_err(|error| match error {
            ApplicationError::ResourceLimitExceeded(_) => LoadNavigationError::ResourceLimit(error),
            other => LoadNavigationError::Degraded(other.to_string()),
        })?;
    let source = utf8_entry(bytes, &source_path)
        .map_err(|error| LoadNavigationError::Degraded(error.to_string()))?;
    Ok((source_path, source))
}

fn select_ncx_item<'a>(
    facts: &'a EpubPackageFacts,
    diagnostics: &mut Vec<EpubNavigationDiagnostic>,
) -> Option<&'a ManifestItem> {
    if let Some(toc_id) = facts.spine_toc_id.as_deref() {
        if let Some(item) = facts.manifest_item(toc_id) {
            return Some(item);
        }
        diagnostics.push(EpubNavigationDiagnostic {
            code: "missing_spine_ncx_reference".into(),
            message: format!("spine toc reference {toc_id:?} does not resolve to a manifest item"),
        });
    }

    let candidates = facts
        .manifest
        .iter()
        .filter(|item| item.media_type == EPUB_NCX_MEDIA_TYPE)
        .collect::<Vec<_>>();
    if candidates.len() > 1 {
        diagnostics.push(EpubNavigationDiagnostic {
            code: "multiple_epub_ncx_resources".into(),
            message: format!(
                "manifest contains {} NCX resources without an unambiguous spine toc reference; first manifest-order NCX is used",
                candidates.len()
            ),
        });
    }
    candidates.into_iter().next()
}

#[derive(Clone, Debug)]
struct RawNavigationNode {
    label: String,
    href: Option<String>,
    children: Vec<RawNavigationNode>,
}

fn parse_epub_nav(source: &str) -> Result<Vec<RawNavigationNode>, String> {
    let xml = XmlDocument::parse(source).map_err(|error| error.to_string())?;
    let toc = xml
        .descendants()
        .find(|node| {
            node.is_element()
                && node.tag_name().name() == "nav"
                && node.attributes().any(|attribute| {
                    attribute.name() == "type"
                        && attribute
                            .value()
                            .split_whitespace()
                            .any(|value| value == "toc")
                })
        })
        .ok_or_else(|| "navigation document has no nav element with epub:type=toc".to_string())?;
    let ol = toc
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "ol")
        .ok_or_else(|| "TOC nav has no ordered list".to_string())?;
    Ok(parse_epub_nav_list(ol))
}

fn parse_epub_nav_list(ol: Node<'_, '_>) -> Vec<RawNavigationNode> {
    ol.children()
        .filter(|node| node.is_element() && node.tag_name().name() == "li")
        .map(|li| {
            let target = li
                .children()
                .find(|node| node.is_element() && matches!(node.tag_name().name(), "a" | "span"));
            let label = target.map(normalized_node_text).unwrap_or_default();
            let href = target
                .filter(|node| node.tag_name().name() == "a")
                .and_then(|node| node.attribute("href"))
                .map(str::to_string);
            let children = li
                .children()
                .find(|node| node.is_element() && node.tag_name().name() == "ol")
                .map(parse_epub_nav_list)
                .unwrap_or_default();
            RawNavigationNode {
                label,
                href,
                children,
            }
        })
        .collect()
}

fn parse_ncx(source: &str) -> Result<Vec<RawNavigationNode>, String> {
    let xml = XmlDocument::parse(source).map_err(|error| error.to_string())?;
    let nav_map = xml
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "navMap")
        .ok_or_else(|| "NCX has no navMap".to_string())?;
    Ok(parse_ncx_children(nav_map))
}

fn parse_ncx_children(parent: Node<'_, '_>) -> Vec<RawNavigationNode> {
    parent
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "navPoint")
        .map(|point| {
            let label = point
                .children()
                .find(|node| node.is_element() && node.tag_name().name() == "navLabel")
                .and_then(|label| {
                    label
                        .descendants()
                        .find(|node| node.is_element() && node.tag_name().name() == "text")
                })
                .map(normalized_node_text)
                .unwrap_or_default();
            let href = point
                .children()
                .find(|node| node.is_element() && node.tag_name().name() == "content")
                .and_then(|node| node.attribute("src"))
                .map(str::to_string);
            RawNavigationNode {
                label,
                href,
                children: parse_ncx_children(point),
            }
        })
        .collect()
}

fn normalized_node_text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(|descendant| descendant.is_text())
        .filter_map(|descendant| descendant.text())
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

#[allow(clippy::too_many_arguments)]
fn resolve_nodes<R: Read + Seek>(
    raw_nodes: Vec<RawNavigationNode>,
    depth: usize,
    provenance: EpubNavigationProvenance,
    source_path: &str,
    archive: &mut ZipArchive<R>,
    package_path: &str,
    facts: &EpubPackageFacts,
    fragment_cache: &mut FragmentCache,
    limits: &ArchiveLimits,
    total_read: &mut usize,
    source_order: &mut usize,
) -> Result<Vec<EpubNavigationNode>, ApplicationError> {
    let mut output = Vec::with_capacity(raw_nodes.len());
    for raw in raw_nodes {
        let current_order = *source_order;
        *source_order += 1;
        let resolution = resolve_target(
            raw.href.as_deref(),
            source_path,
            archive,
            package_path,
            facts,
            fragment_cache,
            limits,
            total_read,
        )?;
        let children = resolve_nodes(
            raw.children,
            depth + 1,
            provenance,
            source_path,
            archive,
            package_path,
            facts,
            fragment_cache,
            limits,
            total_read,
            source_order,
        )?;
        output.push(EpubNavigationNode {
            label: raw.label,
            depth,
            href: raw.href,
            resolved_entry_path: resolution.entry_path,
            fragment: resolution.fragment,
            source_order: current_order,
            provenance,
            resolution_status: resolution.status,
            diagnostic: resolution.diagnostic,
            children,
        });
    }
    Ok(output)
}

struct TargetResolution {
    entry_path: Option<String>,
    fragment: Option<String>,
    status: NavigationResolutionStatus,
    diagnostic: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn resolve_target<R: Read + Seek>(
    href: Option<&str>,
    source_path: &str,
    archive: &mut ZipArchive<R>,
    package_path: &str,
    facts: &EpubPackageFacts,
    fragment_cache: &mut FragmentCache,
    limits: &ArchiveLimits,
    total_read: &mut usize,
) -> Result<TargetResolution, ApplicationError> {
    let Some(href) = href.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(TargetResolution {
            entry_path: None,
            fragment: None,
            status: NavigationResolutionStatus::Unlinked,
            diagnostic: Some("navigation node has no href".into()),
        });
    };
    if is_external_href(href) {
        return Ok(TargetResolution {
            entry_path: None,
            fragment: None,
            status: NavigationResolutionStatus::UnsupportedResource,
            diagnostic: Some(format!(
                "external navigation target {href:?} is outside the EPUB archive"
            )),
        });
    }

    let (path_part, raw_fragment) = split_navigation_href(href);
    let entry_path = match if path_part.is_empty() {
        Ok(source_path.to_string())
    } else {
        resolve_archive_path(source_path, path_part)
    } {
        Ok(path) => path,
        Err(message) => {
            return Ok(TargetResolution {
                entry_path: None,
                fragment: raw_fragment.map(str::to_string),
                status: NavigationResolutionStatus::InvalidPath,
                diagnostic: Some(message),
            });
        }
    };
    let fragment = match raw_fragment {
        Some(value) if !value.is_empty() => match percent_decode(value) {
            Ok(value) => Some(value),
            Err(message) => {
                return Ok(TargetResolution {
                    entry_path: Some(entry_path),
                    fragment: Some(value.to_string()),
                    status: NavigationResolutionStatus::InvalidPath,
                    diagnostic: Some(format!("invalid fragment encoding: {message}")),
                });
            }
        },
        _ => None,
    };

    let manifest_item = facts.manifest.iter().find(|item| {
        resolve_archive_path(package_path, &item.href)
            .is_ok_and(|candidate| candidate == entry_path)
    });
    let Some(manifest_item) = manifest_item else {
        return Ok(TargetResolution {
            entry_path: Some(entry_path),
            fragment,
            status: NavigationResolutionStatus::MissingResource,
            diagnostic: Some("navigation target does not resolve to a manifest resource".into()),
        });
    };
    if archive.index_for_name(&entry_path).is_none() {
        return Ok(TargetResolution {
            entry_path: Some(entry_path),
            fragment,
            status: NavigationResolutionStatus::MissingResource,
            diagnostic: Some(
                "navigation target manifest resource is absent from the archive".into(),
            ),
        });
    }
    if !is_supported_content_media_type(&manifest_item.media_type) {
        return Ok(TargetResolution {
            entry_path: Some(entry_path),
            fragment,
            status: NavigationResolutionStatus::UnsupportedResource,
            diagnostic: Some(format!(
                "navigation target media type {:?} is not in the reflowable text profile",
                manifest_item.media_type
            )),
        });
    }
    let Some(fragment) = fragment else {
        return Ok(TargetResolution {
            entry_path: Some(entry_path),
            fragment: None,
            status: NavigationResolutionStatus::ResolvedDocument,
            diagnostic: None,
        });
    };

    if !fragment_cache.contains_key(&entry_path) {
        let bytes = match read_entry(archive, &entry_path, limits, total_read) {
            Ok(bytes) => bytes,
            Err(error @ ApplicationError::ResourceLimitExceeded(_)) => return Err(error),
            Err(error) => {
                return Ok(TargetResolution {
                    entry_path: Some(entry_path),
                    fragment: Some(fragment),
                    status: NavigationResolutionStatus::MissingResource,
                    diagnostic: Some(error.to_string()),
                });
            }
        };
        fragment_cache.insert(
            entry_path.clone(),
            fragment_index(&manifest_item.media_type, &bytes),
        );
    }

    match fragment_cache.get(&entry_path) {
        Some(FragmentIndex::Resolved(ids)) if ids.contains(&fragment) => Ok(TargetResolution {
            entry_path: Some(entry_path),
            fragment: Some(fragment),
            status: NavigationResolutionStatus::ResolvedFragment,
            diagnostic: None,
        }),
        Some(FragmentIndex::Resolved(_)) => Ok(TargetResolution {
            entry_path: Some(entry_path),
            fragment: Some(fragment.clone()),
            status: NavigationResolutionStatus::MissingFragment,
            diagnostic: Some(format!(
                "fragment {fragment:?} was not found in the target document"
            )),
        }),
        Some(FragmentIndex::Malformed(message)) => Ok(TargetResolution {
            entry_path: Some(entry_path),
            fragment: Some(fragment),
            status: NavigationResolutionStatus::MalformedResource,
            diagnostic: Some(message.clone()),
        }),
        None => unreachable!("fragment cache entry must exist after insertion"),
    }
}

fn fragment_index(media_type: &str, bytes: &[u8]) -> FragmentIndex {
    let source = match std::str::from_utf8(bytes) {
        Ok(source) => source,
        Err(error) => {
            return FragmentIndex::Malformed(format!("target content is not UTF-8: {error}"));
        }
    };
    if matches!(
        media_type,
        "application/xhtml+xml" | "application/xml" | "text/xml"
    ) {
        return match XmlDocument::parse(source) {
            Ok(document) => FragmentIndex::Resolved(
                document
                    .descendants()
                    .filter(|node| node.is_element())
                    .flat_map(|node| {
                        node.attributes()
                            .filter(|attribute| matches!(attribute.name(), "id" | "name"))
                            .map(|attribute| attribute.value().to_string())
                            .collect::<Vec<_>>()
                    })
                    .collect(),
            ),
            Err(error) => {
                FragmentIndex::Malformed(format!("target XHTML/XML is malformed: {error}"))
            }
        };
    }
    if media_type == "text/html" {
        let document = Html::parse_document(source);
        let selector = match Selector::parse("[id], a[name]") {
            Ok(selector) => selector,
            Err(error) => {
                return FragmentIndex::Malformed(format!(
                    "failed building HTML fragment selector: {error}"
                ));
            }
        };
        return FragmentIndex::Resolved(
            document
                .select(&selector)
                .flat_map(|element| {
                    [element.value().attr("id"), element.value().attr("name")]
                        .into_iter()
                        .flatten()
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .collect(),
        );
    }
    FragmentIndex::Malformed(format!(
        "unsupported fragment-index media type {media_type:?}"
    ))
}

pub(crate) fn is_supported_content_media_type(media_type: &str) -> bool {
    matches!(
        media_type,
        "application/xhtml+xml" | "text/html" | "application/xml" | "text/xml"
    )
}

pub(crate) fn resolve_archive_path(base_document_path: &str, href: &str) -> Result<String, String> {
    let href_path = href
        .split('#')
        .next()
        .unwrap_or(href)
        .split('?')
        .next()
        .unwrap_or(href);
    if href_path.starts_with('/') || href_path.contains('\\') {
        return Err(format!("archive href {href:?} is not a safe relative path"));
    }
    let base = base_document_path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or_default();
    let combined = if href_path.is_empty() {
        base_document_path.to_string()
    } else if base.is_empty() {
        href_path.to_string()
    } else {
        format!("{base}/{href_path}")
    };
    let mut segments = Vec::new();
    for raw_segment in combined.split('/') {
        let segment = percent_decode(raw_segment)?;
        if segment.contains('/') || segment.contains('\\') {
            return Err("percent-decoded EPUB path segment contains a path separator".into());
        }
        match segment.as_str() {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err("EPUB path escapes archive root".into());
                }
            }
            _ => segments.push(segment),
        }
    }
    if segments.is_empty() {
        return Err("EPUB path resolves to an empty archive path".into());
    }
    Ok(segments.join("/"))
}

fn split_navigation_href(href: &str) -> (&str, Option<&str>) {
    let (before_fragment, fragment) = href
        .split_once('#')
        .map_or((href, None), |(path, fragment)| (path, Some(fragment)));
    let path = before_fragment
        .split_once('?')
        .map_or(before_fragment, |(path, _)| path);
    (path, fragment)
}

fn is_external_href(href: &str) -> bool {
    let before_fragment = href.split('#').next().unwrap_or(href);
    let scheme_end = before_fragment.find(':');
    let slash = before_fragment.find('/');
    scheme_end.is_some_and(|index| slash.is_none_or(|slash| index < slash))
}

fn percent_decode(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(format!("incomplete percent escape in {value:?}"));
            }
            let high = hex_value(bytes[index + 1])
                .ok_or_else(|| format!("invalid percent escape in {value:?}"))?;
            let low = hex_value(bytes[index + 2])
                .ok_or_else(|| format!("invalid percent escape in {value:?}"))?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .map_err(|error| format!("percent-decoded value is not UTF-8: {error}"))
}

const fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn count_nodes(nodes: &[EpubNavigationNode]) -> usize {
    nodes
        .iter()
        .map(|node| 1 + count_nodes(&node.children))
        .sum()
}

fn count_resolved_nodes(nodes: &[EpubNavigationNode]) -> usize {
    nodes
        .iter()
        .map(|node| {
            usize::from(node.resolution_status.is_resolved()) + count_resolved_nodes(&node.children)
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::{percent_decode, resolve_archive_path};

    #[test]
    fn archive_paths_resolve_relative_and_percent_encoded_segments() {
        assert_eq!(
            resolve_archive_path("OPS/nav/nav.xhtml", "../text/chapter%201.xhtml#start").unwrap(),
            "OPS/text/chapter 1.xhtml"
        );
        assert_eq!(percent_decode("sec%20one").unwrap(), "sec one");
    }

    #[test]
    fn archive_paths_reject_root_escape_even_when_percent_encoded() {
        assert!(resolve_archive_path("package.opf", "%2e%2e/outside.xhtml").is_err());
        assert!(resolve_archive_path("OPS/package.opf", "/absolute.xhtml").is_err());
        assert!(resolve_archive_path("OPS/package.opf", "text\\chapter.xhtml").is_err());
    }
}
