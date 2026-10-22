//! Resolve a paper's HTML landing page to the PDF behind it (PDF-3).
//!
//! A user who opens `https://arxiv.org/abs/2309.06180` (or a `doi.org`, OpenReview forum or ACL
//! Anthology link) today gets the abstract page, because `text/html` is an allowed media type.
//! This module puts a small, safe resolver in front of the HTTP retriever:
//!
//! 1. pure URL rules, run before any fetch, that rewrite a known landing-page URL to the PDF URL of
//!    the same paper (arXiv `/abs/<id>`, OpenReview `/forum?id=`, ACL Anthology `/<id>/`);
//! 2. a generic rule for everything else: when the fetched resource is an HTML landing page, look
//!    for `<meta name="citation_pdf_url" content="...">` in its first 256 KiB.
//!
//! The resolved URL is untrusted input from a web page, so it goes through exactly the same checks
//! as a URL the user typed: the [`HttpAccessPolicy`] (scheme, credentials, non-public addresses),
//! the same redirect limit, size limit and timeout (it is fetched by the same retriever), and no
//! cookies. Resolution depth is one: a resolved target is never resolved again, and it must come
//! back as `application/pdf`, otherwise the landing page is kept exactly as today.

use std::sync::Arc;

use async_trait::async_trait;
use url::Url;

use crate::application::ports::{ApplicationError, RetrievalOptions, RetrievedResource, Retriever};
use crate::domain::DocumentSource;
use crate::security::HttpAccessPolicy;

/// Bytes of a landing page scanned for `<meta name="citation_pdf_url">`.
pub const META_SCAN_LIMIT: usize = 256 * 1024;

/// Metadata key naming the URL the user asked for when the returned resource is a resolved PDF.
pub const LANDING_REQUESTED_SOURCE: &str = "landing_requested_source";
/// Metadata key naming the rule that resolved the landing page.
pub const LANDING_RULE: &str = "landing_rule";
/// Metadata key naming why a landing page was kept instead of its PDF.
pub const LANDING_RESOLUTION_FAILED: &str = "landing_resolution_failed";

/// The arXiv `/abs/<id>` rule.
pub const RULE_ARXIV_ABS: &str = "arxiv_abs";
/// The OpenReview `/forum?id=<id>` rule.
pub const RULE_OPENREVIEW_FORUM: &str = "openreview_forum";
/// The ACL Anthology `/<id>/` rule.
pub const RULE_ACL_ANTHOLOGY: &str = "acl_anthology";
/// The generic `<meta name="citation_pdf_url">` rule.
pub const RULE_CITATION_PDF_URL: &str = "citation_pdf_url";

/// Why a landing page was returned instead of its PDF.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionFailure {
    /// The target was fetched but is not `application/pdf`.
    NotPdf,
    /// The access policy refused the target (scheme, credentials, non-public address).
    RefusedByPolicy,
    /// The target could not be fetched at all.
    FetchFailed,
    /// The landing page carries no `citation_pdf_url` meta tag.
    NoMeta,
}

impl ResolutionFailure {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotPdf => "not_pdf",
            Self::RefusedByPolicy => "refused_by_policy",
            Self::FetchFailed => "fetch_failed",
            Self::NoMeta => "no_meta",
        }
    }
}

/// How the resolver is configured for one deployment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LandingPageConfig {
    /// `READING_MCP_RESOLVE_LANDING_PAGES`; off keeps the retriever byte-identical to no resolver.
    pub enabled: bool,
    /// `READING_MCP_ALLOW_HTTP`: the only case where a plain-HTTP landing page is resolved.
    pub allow_http: bool,
}

impl Default for LandingPageConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_http: false,
        }
    }
}

/// A landing-page URL that names the PDF of the same paper, with the rule that recognised it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub url: Url,
    pub rule: &'static str,
}

/// The pure rules of step 1: no network, `https` only, no credentials in the URL.
///
/// Returns `None` for every URL the rules do not recognise, including look-alike hosts
/// (`arxiv.org.evil.com`), `http://`, embedded credentials and ids that do not match.
pub fn known_landing_target(url: &Url) -> Option<ResolvedTarget> {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    let path = url.path();

    if matches!(
        host.as_str(),
        "arxiv.org" | "www.arxiv.org" | "export.arxiv.org"
    ) {
        let id = arxiv_abs_id(path)?;
        let url = Url::parse(&format!("https://arxiv.org/pdf/{id}")).ok()?;
        return Some(ResolvedTarget {
            url,
            rule: RULE_ARXIV_ABS,
        });
    }

    if host == "openreview.net" && path == "/forum" {
        let id = url
            .query_pairs()
            .find(|(key, _)| key == "id")
            .map(|(_, value)| value.into_owned())?;
        if !is_openreview_id(&id) {
            return None;
        }
        let url = Url::parse(&format!("https://openreview.net/pdf?id={id}")).ok()?;
        return Some(ResolvedTarget {
            url,
            rule: RULE_OPENREVIEW_FORUM,
        });
    }

    if host == "aclanthology.org" {
        let id = acl_anthology_id(path)?;
        let url = Url::parse(&format!("https://aclanthology.org/{id}.pdf")).ok()?;
        return Some(ResolvedTarget {
            url,
            rule: RULE_ACL_ANTHOLOGY,
        });
    }

    None
}

/// The id of `/abs/<id>`, with its version suffix when present; query and fragment are dropped.
fn arxiv_abs_id(path: &str) -> Option<&str> {
    let id = path.strip_prefix("/abs/")?;
    let base = match id.rsplit_once('v') {
        Some((base, version))
            if !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            base
        }
        _ => id,
    };
    if is_new_style_arxiv_id(base) || is_old_style_arxiv_id(base) {
        Some(id)
    } else {
        None
    }
}

/// `YYMM.NNNNN`: four digits, a dot, four or five digits.
fn is_new_style_arxiv_id(id: &str) -> bool {
    let Some((year_month, number)) = id.split_once('.') else {
        return false;
    };
    year_month.len() == 4
        && year_month.bytes().all(|byte| byte.is_ascii_digit())
        && matches!(number.len(), 4 | 5)
        && number.bytes().all(|byte| byte.is_ascii_digit())
}

/// `archive/YYMMNNN`, where the archive is lowercase words joined by `-` and an optional
/// two-letter subject class (`math.GT`, `cond-mat.stat-mech`).
fn is_old_style_arxiv_id(id: &str) -> bool {
    let Some((archive, number)) = id.split_once('/') else {
        return false;
    };
    if number.len() != 7 || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let (name, subject) = match archive.split_once('.') {
        Some((name, subject)) => (name, Some(subject)),
        None => (archive, None),
    };
    let name_ok = !name.is_empty()
        && name
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_lowercase()));
    // The subject class is a short token such as `GT`, `CO`, `soft` or `stat-mech`.
    let subject_ok = match subject {
        None => true,
        Some(subject) => {
            (2..=12).contains(&subject.len())
                && subject
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        }
    };
    name_ok && subject_ok
}

fn is_openreview_id(id: &str) -> bool {
    (6..=20).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// `/<id>/` where `<id>` is a single path segment of `[A-Za-z0-9.-]` that is not already a `.pdf`.
fn acl_anthology_id(path: &str) -> Option<&str> {
    let id = path.strip_prefix('/')?.strip_suffix('/')?;
    if id.is_empty() || id.contains('/') {
        return None;
    }
    let allowed = id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'));
    if !allowed || id.to_ascii_lowercase().ends_with(".pdf") {
        return None;
    }
    Some(id)
}

/// The generic rule: the first `citation_pdf_url` meta tag of the first [`META_SCAN_LIMIT`] bytes,
/// resolved against the landing page URL. Malformed tags and unresolvable values are skipped.
pub fn extract_citation_pdf_url(body: &[u8], base: &Url) -> Option<Url> {
    let window = &body[..body.len().min(META_SCAN_LIMIT)];
    for attributes in meta_tags(window) {
        let mut name = None;
        let mut content = None;
        for (key, value) in &attributes {
            match key.as_str() {
                "name" | "property" if name.is_none() => name = Some(value.as_str()),
                "content" if content.is_none() => content = Some(value.as_str()),
                _ => {}
            }
        }
        let (Some(name), Some(content)) = (name, content) else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("citation_pdf_url") {
            continue;
        }
        // An empty value resolves to the landing page itself, which is not a PDF target.
        let value = content.trim();
        if value.is_empty() {
            continue;
        }
        let Ok(resolved) = base.join(value) else {
            continue;
        };
        return Some(resolved);
    }
    None
}

/// Every `<meta ...>` tag in `html`, as its lowercased attribute names and raw values.
fn meta_tags(html: &[u8]) -> Vec<Vec<(String, String)>> {
    let lower = html.to_ascii_lowercase();
    let mut tags = Vec::new();
    let mut index = 0;
    while let Some(found) = find(&lower[index..], b"<meta") {
        let start = index + found;
        let after = start + "<meta".len();
        match lower.get(after) {
            None => break,
            Some(b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>') => {}
            Some(_) => {
                index = after;
                continue;
            }
        }
        let (attributes, cursor) = parse_attributes(html, &lower, after);
        tags.push(attributes);
        index = cursor.max(after);
    }
    tags
}

fn parse_attributes(
    html: &[u8],
    lower: &[u8],
    mut cursor: usize,
) -> (Vec<(String, String)>, usize) {
    let mut attributes = Vec::new();
    loop {
        while matches!(lower.get(cursor), Some(b' ' | b'\t' | b'\n' | b'\r' | b'/')) {
            cursor += 1;
        }
        match lower.get(cursor) {
            None => return (attributes, cursor),
            Some(b'>') => return (attributes, cursor + 1),
            Some(_) => {}
        }
        let name_start = cursor;
        while !matches!(
            lower.get(cursor),
            None | Some(b' ' | b'\t' | b'\n' | b'\r' | b'=' | b'>' | b'/')
        ) {
            cursor += 1;
        }
        let name = String::from_utf8_lossy(&html[name_start..cursor]).to_ascii_lowercase();
        while matches!(lower.get(cursor), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            cursor += 1;
        }
        if name.is_empty() {
            cursor += 1;
            continue;
        }
        if lower.get(cursor) != Some(&b'=') {
            attributes.push((name, String::new()));
            continue;
        }
        cursor += 1;
        while matches!(lower.get(cursor), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            cursor += 1;
        }
        let value = match lower.get(cursor) {
            Some(&quote) if quote == b'"' || quote == b'\'' => {
                cursor += 1;
                let value_start = cursor;
                let mut closed = false;
                while let Some(&byte) = lower.get(cursor) {
                    if byte == quote {
                        closed = true;
                        break;
                    }
                    cursor += 1;
                }
                if !closed {
                    // An unterminated value is a truncated or malformed tag: drop the attribute.
                    return (attributes, cursor);
                }
                let value = decode_entities(&String::from_utf8_lossy(&html[value_start..cursor]));
                cursor += 1;
                value
            }
            _ => {
                let value_start = cursor;
                while !matches!(
                    lower.get(cursor),
                    None | Some(b' ' | b'\t' | b'\n' | b'\r' | b'>')
                ) {
                    cursor += 1;
                }
                decode_entities(&String::from_utf8_lossy(&html[value_start..cursor]))
            }
        };
        attributes.push((name, value));
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The character references a `content` attribute value realistically carries.
fn decode_entities(value: &str) -> String {
    if !value.contains('&') {
        return value.to_owned();
    }
    let mut decoded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('&') {
        decoded.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let Some(end) = tail.find(';') else {
            decoded.push_str(&rest[start..]);
            return decoded;
        };
        let entity = &tail[..end];
        let replacement = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                .and_then(char::from_u32),
        };
        match replacement {
            Some(character) => {
                decoded.push(character);
                rest = &tail[end + 1..];
            }
            None => {
                decoded.push('&');
                rest = tail;
            }
        }
    }
    decoded.push_str(rest);
    decoded
}

/// The resolver itself: a [`Retriever`] that returns the PDF of a paper landing page.
pub struct LandingPageRetriever {
    inner: Arc<dyn Retriever>,
    policy: Arc<dyn HttpAccessPolicy>,
    config: LandingPageConfig,
}

impl LandingPageRetriever {
    pub fn new(
        inner: Arc<dyn Retriever>,
        policy: Arc<dyn HttpAccessPolicy>,
        config: LandingPageConfig,
    ) -> Self {
        Self {
            inner,
            policy,
            config,
        }
    }

    /// Checks a resolved target the way a URL the user typed is checked: same scheme as the
    /// landing page (no upgrade or downgrade), `https` unless the deployment allows plain HTTP, no
    /// credentials, and the access policy's non-public-address rules.
    fn check_target(&self, landing: &Url, target: &Url) -> Result<(), ResolutionFailure> {
        if target.scheme() != landing.scheme() {
            return Err(ResolutionFailure::RefusedByPolicy);
        }
        let scheme_allowed =
            target.scheme() == "https" || (self.config.allow_http && target.scheme() == "http");
        if !scheme_allowed {
            return Err(ResolutionFailure::RefusedByPolicy);
        }
        if !target.username().is_empty() || target.password().is_some() {
            return Err(ResolutionFailure::RefusedByPolicy);
        }
        self.policy
            .validate_url(target)
            .map_err(|_| ResolutionFailure::RefusedByPolicy)
    }

    async fn fetch_target(
        &self,
        requested: &DocumentSource,
        landing: &Url,
        target: &Url,
        rule: &'static str,
        options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ResolutionFailure> {
        self.check_target(landing, target)?;
        let mut resource = self
            .inner
            .retrieve(&DocumentSource(target.to_string()), options)
            .await
            .map_err(|error| match error {
                ApplicationError::BlockedSource(_) => ResolutionFailure::RefusedByPolicy,
                _ => ResolutionFailure::FetchFailed,
            })?;
        if !resource.media_type.is_pdf() {
            return Err(ResolutionFailure::NotPdf);
        }
        // The resource belongs to the URL the user asked for; `final_source` is the PDF.
        resource.source = requested.clone();
        resource
            .metadata
            .insert(LANDING_REQUESTED_SOURCE.into(), requested.0.clone());
        resource.metadata.insert(LANDING_RULE.into(), rule.into());
        Ok(resource)
    }

    /// The generic rule: only for an HTML landing page whose final URL is `https` (or plain HTTP
    /// when the deployment allows it).
    fn landing_page_url(&self, landing: &RetrievedResource) -> Option<Url> {
        let essence = landing.media_type.essence().to_ascii_lowercase();
        if essence != "text/html" && essence != "application/xhtml+xml" {
            return None;
        }
        let url = Url::parse(&landing.final_source.0).ok()?;
        let scheme_allowed =
            url.scheme() == "https" || (self.config.allow_http && url.scheme() == "http");
        if !scheme_allowed {
            return None;
        }
        Some(url)
    }

    async fn resolve(
        &self,
        source: &DocumentSource,
        options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        let Ok(requested) = self.policy.parse_and_validate(source) else {
            // Not a URL this deployment accepts: let the retriever report the error it reports
            // today, unchanged.
            return self.inner.retrieve(source, options).await;
        };

        if let Some(target) = known_landing_target(&requested) {
            match self
                .fetch_target(source, &requested, &target.url, target.rule, options)
                .await
            {
                Ok(resource) => return Ok(resource),
                Err(failure) => {
                    let landing = self.inner.retrieve(source, options).await?;
                    return Ok(landing_with_failure(landing, failure));
                }
            }
        }

        let landing = self.inner.retrieve(source, options).await?;
        let Some(final_url) = self.landing_page_url(&landing) else {
            return Ok(landing);
        };

        // A redirect chain may end on a landing page the pure rules know (`doi.org/...` on
        // `arxiv.org/abs/...`).
        if let Some(target) = known_landing_target(&final_url) {
            return match self
                .fetch_target(source, &final_url, &target.url, target.rule, options)
                .await
            {
                Ok(resource) => Ok(resource),
                Err(failure) => Ok(landing_with_failure(landing, failure)),
            };
        }

        let Some(target) = extract_citation_pdf_url(&landing.bytes, &final_url) else {
            return Ok(landing_with_failure(landing, ResolutionFailure::NoMeta));
        };
        match self
            .fetch_target(source, &final_url, &target, RULE_CITATION_PDF_URL, options)
            .await
        {
            Ok(resource) => Ok(resource),
            Err(failure) => Ok(landing_with_failure(landing, failure)),
        }
    }
}

fn landing_with_failure(
    mut landing: RetrievedResource,
    failure: ResolutionFailure,
) -> RetrievedResource {
    landing
        .metadata
        .insert(LANDING_RESOLUTION_FAILED.into(), failure.as_str().into());
    landing
}

#[async_trait]
impl Retriever for LandingPageRetriever {
    async fn retrieve(
        &self,
        source: &DocumentSource,
        options: &RetrievalOptions,
    ) -> Result<RetrievedResource, ApplicationError> {
        if !self.config.enabled || !is_http_source(source) {
            return self.inner.retrieve(source, options).await;
        }
        self.resolve(source, options).await
    }
}

fn is_http_source(source: &DocumentSource) -> bool {
    let value = source.0.trim().to_ascii_lowercase();
    value.starts_with("https://") || value.starts_with("http://")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;
    use crate::domain::MediaType;
    use crate::security::PublicHttpAccessPolicy;

    fn url(value: &str) -> Url {
        Url::parse(value).expect("test URL should parse")
    }

    fn target(value: &str) -> Option<(String, &'static str)> {
        known_landing_target(&url(value)).map(|target| (target.url.to_string(), target.rule))
    }

    #[test]
    fn arxiv_abs_links_resolve_to_the_pdf_of_the_same_id() {
        for (source, expected) in [
            (
                "https://arxiv.org/abs/2309.06180",
                "https://arxiv.org/pdf/2309.06180",
            ),
            (
                "https://arxiv.org/abs/2309.06180v1",
                "https://arxiv.org/pdf/2309.06180v1",
            ),
            (
                "https://arxiv.org/abs/2309.06180v12",
                "https://arxiv.org/pdf/2309.06180v12",
            ),
            (
                "https://www.arxiv.org/abs/2309.06180",
                "https://arxiv.org/pdf/2309.06180",
            ),
            (
                "https://export.arxiv.org/abs/2309.06180",
                "https://arxiv.org/pdf/2309.06180",
            ),
            (
                "https://arxiv.org/abs/2309.06180?context=cs.LG",
                "https://arxiv.org/pdf/2309.06180",
            ),
            (
                "https://arxiv.org/abs/2309.06180#references",
                "https://arxiv.org/pdf/2309.06180",
            ),
            (
                "https://arxiv.org/abs/2309.06180v2?context=cs#top",
                "https://arxiv.org/pdf/2309.06180v2",
            ),
            (
                "https://arxiv.org/abs/hep-th/9901001",
                "https://arxiv.org/pdf/hep-th/9901001",
            ),
            (
                "https://arxiv.org/abs/hep-th/9901001v3",
                "https://arxiv.org/pdf/hep-th/9901001v3",
            ),
            (
                "https://arxiv.org/abs/math.GT/0309136",
                "https://arxiv.org/pdf/math.GT/0309136",
            ),
            (
                "https://arxiv.org/abs/cond-mat.stat-mech/0401010v1",
                "https://arxiv.org/pdf/cond-mat.stat-mech/0401010v1",
            ),
        ] {
            assert_eq!(
                target(source),
                Some((expected.to_string(), RULE_ARXIV_ABS)),
                "{source} should resolve to {expected}"
            );
        }
    }

    #[test]
    fn arxiv_look_alikes_and_non_abs_urls_are_not_resolved() {
        for source in [
            // wrong scheme
            "http://arxiv.org/abs/2309.06180",
            // look-alike hosts
            "https://arxiv.org.evil.com/abs/2309.06180",
            "https://evil.com/arxiv.org/abs/2309.06180",
            "https://arxiv.org@evil.com/abs/2309.06180",
            "https://notarxiv.org/abs/2309.06180",
            // credentials in the URL
            "https://x@arxiv.org/abs/2309.06180",
            "https://user:pass@arxiv.org/abs/2309.06180",
            // already the PDF, or not an /abs/ page
            "https://arxiv.org/pdf/2309.06180",
            "https://arxiv.org/html/2309.06180",
            "https://arxiv.org/abs/",
            "https://arxiv.org/abs/2309",
            "https://arxiv.org/abs/2309.06",
            "https://arxiv.org/abs/2309.061801",
            "https://arxiv.org/abs/2309.06180/extra",
            "https://arxiv.org/abs/2309.06180v",
            "https://arxiv.org/abs/2309.06180va",
            "https://arxiv.org/abs/hep-th/99010",
            "https://arxiv.org/abs/hep-th/99010011",
            "https://arxiv.org/abs/HEP-TH/9901001",
            "https://arxiv.org/abs/hep-th/",
        ] {
            assert_eq!(target(source), None, "{source} must not be resolved");
        }
    }

    #[test]
    fn openreview_forum_links_resolve_to_the_pdf() {
        assert_eq!(
            target("https://openreview.net/forum?id=abcdef123456"),
            Some((
                "https://openreview.net/pdf?id=abcdef123456".to_string(),
                RULE_OPENREVIEW_FORUM
            ))
        );
        assert_eq!(
            target("https://openreview.net/forum?id=Zk1_a-B2cD3&noteId=Zk1_a-B2cD3"),
            Some((
                "https://openreview.net/pdf?id=Zk1_a-B2cD3".to_string(),
                RULE_OPENREVIEW_FORUM
            ))
        );
        for source in [
            "https://openreview.net/forum?id=abc",
            "https://openreview.net/forum?id=abcdef1234567890123456",
            "https://openreview.net/forum?id=abc%2Fdef",
            "https://openreview.net/forum?id=",
            "https://openreview.net/forum",
            "https://openreview.net/pdf?id=abcdef123456",
            "http://openreview.net/forum?id=abcdef123456",
            "https://openreview.net.evil.com/forum?id=abcdef123456",
        ] {
            assert_eq!(target(source), None, "{source} must not be resolved");
        }
    }

    #[test]
    fn acl_anthology_pages_resolve_to_the_pdf() {
        assert_eq!(
            target("https://aclanthology.org/2023.acl-long.1/"),
            Some((
                "https://aclanthology.org/2023.acl-long.1.pdf".to_string(),
                RULE_ACL_ANTHOLOGY
            ))
        );
        for source in [
            "https://aclanthology.org/2023.acl-long.1.pdf/",
            "https://aclanthology.org/2023.acl-long.1",
            "https://aclanthology.org/2023.acl-long.1/extra/",
            "https://aclanthology.org/2023.acl_long.1/",
            "https://aclanthology.org//",
            "http://aclanthology.org/2023.acl-long.1/",
            "https://aclanthology.org.evil.com/2023.acl-long.1/",
        ] {
            assert_eq!(target(source), None, "{source} must not be resolved");
        }
    }

    #[test]
    fn meta_tag_extraction_accepts_the_documented_variants() {
        let base = url("https://publisher.test/articles/42");
        for body in [
            r#"<meta name="citation_pdf_url" content="/paper.pdf">"#,
            r#"<meta property="citation_pdf_url" content="/paper.pdf">"#,
            r#"<meta NAME="Citation_PDF_URL" CONTENT="/paper.pdf">"#,
            r#"<meta content="/paper.pdf" name="citation_pdf_url">"#,
            r#"<meta name='citation_pdf_url' content='/paper.pdf' />"#,
            r#"<meta name=citation_pdf_url content=/paper.pdf>"#,
            r#"<html><head><meta charset="utf-8"><meta name="citation_pdf_url" content="/paper.pdf"></head></html>"#,
            "<meta\n  name=\"citation_pdf_url\"\n  content=\"/paper.pdf\"\n>",
        ] {
            assert_eq!(
                extract_citation_pdf_url(body.as_bytes(), &base).map(|value| value.to_string()),
                Some("https://publisher.test/paper.pdf".to_string()),
                "{body} should yield the PDF URL"
            );
        }
    }

    #[test]
    fn meta_tag_extraction_resolves_relative_and_absolute_urls() {
        let base = url("https://publisher.test/articles/42");
        for (body, expected) in [
            (
                r#"<meta name="citation_pdf_url" content="paper.pdf">"#,
                "https://publisher.test/articles/paper.pdf",
            ),
            (
                r#"<meta name="citation_pdf_url" content="../files/paper.pdf">"#,
                "https://publisher.test/files/paper.pdf",
            ),
            (
                r#"<meta name="citation_pdf_url" content="//cdn.test/paper.pdf">"#,
                "https://cdn.test/paper.pdf",
            ),
            (
                r#"<meta name="citation_pdf_url" content="https://elsewhere.test/p.pdf?a=1&amp;b=2">"#,
                "https://elsewhere.test/p.pdf?a=1&b=2",
            ),
        ] {
            assert_eq!(
                extract_citation_pdf_url(body.as_bytes(), &base).map(|value| value.to_string()),
                Some(expected.to_string()),
                "{body} should yield {expected}"
            );
        }
    }

    #[test]
    fn meta_tag_extraction_takes_the_first_tag_and_ignores_later_ones() {
        let base = url("https://publisher.test/a");
        let body = r#"<meta name="citation_pdf_url" content="/first.pdf">
            <meta name="citation_pdf_url" content="/second.pdf">"#;
        assert_eq!(
            extract_citation_pdf_url(body.as_bytes(), &base).map(|value| value.to_string()),
            Some("https://publisher.test/first.pdf".to_string())
        );
    }

    #[test]
    fn meta_tags_beyond_the_scan_limit_are_ignored() {
        let base = url("https://publisher.test/a");
        let mut body = vec![b' '; META_SCAN_LIMIT];
        body.extend_from_slice(br#"<meta name="citation_pdf_url" content="/paper.pdf">"#);
        assert_eq!(extract_citation_pdf_url(&body, &base), None);

        let mut body = vec![b' '; META_SCAN_LIMIT - 64];
        body.extend_from_slice(br#"<meta name="citation_pdf_url" content="/paper.pdf">"#);
        assert_eq!(
            extract_citation_pdf_url(&body, &base).map(|value| value.to_string()),
            Some("https://publisher.test/paper.pdf".to_string())
        );
    }

    #[test]
    fn malformed_html_yields_no_target() {
        let base = url("https://publisher.test/a");
        for body in [
            "",
            "<html><head></head><body>No meta here.</body></html>",
            r#"<meta name="citation_pdf_url">"#,
            r#"<meta content="/paper.pdf">"#,
            r#"<meta name="citation_title" content="A paper">"#,
            r#"<meta name="citation_pdf_url" content="">"#,
            r#"<meta name="citation_pdf_url" content="http://[oops">"#,
            r#"<meta name="citation_pdf_url" content="/paper.pdf"#,
            "<met name=\"citation_pdf_url\" content=\"/paper.pdf\">",
            "<metadata name=\"citation_pdf_url\" content=\"/paper.pdf\">",
        ] {
            assert_eq!(
                extract_citation_pdf_url(body.as_bytes(), &base),
                None,
                "{body} must not yield a target"
            );
        }
    }

    // ----------------------------------------------------------------------------------------
    // The resolver against a fixture retriever and a fixture policy.
    // ----------------------------------------------------------------------------------------

    struct FixturePolicy {
        refused_hosts: Vec<&'static str>,
    }

    #[async_trait]
    impl HttpAccessPolicy for FixturePolicy {
        fn parse_and_validate(&self, source: &DocumentSource) -> Result<Url, ApplicationError> {
            let url = Url::parse(source.0.trim())
                .map_err(|error| ApplicationError::InvalidRequest(error.to_string()))?;
            self.validate_url(&url)?;
            Ok(url)
        }

        fn validate_url(&self, url: &Url) -> Result<(), ApplicationError> {
            if url.scheme() != "https" && url.scheme() != "http" {
                return Err(ApplicationError::BlockedSource("unsupported scheme".into()));
            }
            if !url.username().is_empty() || url.password().is_some() {
                return Err(ApplicationError::BlockedSource("credentials".into()));
            }
            if let Some(host) = url.host_str()
                && self.refused_hosts.contains(&host)
            {
                return Err(ApplicationError::BlockedSource(format!(
                    "refused host {host}"
                )));
            }
            Ok(())
        }

        async fn resolve_public_endpoint(
            &self,
            url: &Url,
        ) -> Result<std::net::SocketAddr, ApplicationError> {
            self.validate_url(url)?;
            Err(ApplicationError::RetrievalFailed(
                "no network in tests".into(),
            ))
        }
    }

    /// `(source, final_source, media type, body)` per fixture page.
    type FixturePage = (&'static str, &'static str, &'static str, &'static str);

    struct FixtureRetriever {
        pages: Vec<FixturePage>,
        calls: Mutex<Vec<String>>,
    }

    impl FixtureRetriever {
        fn new(pages: Vec<FixturePage>) -> Self {
            Self {
                pages,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    #[async_trait]
    impl Retriever for FixtureRetriever {
        async fn retrieve(
            &self,
            source: &DocumentSource,
            _options: &RetrievalOptions,
        ) -> Result<RetrievedResource, ApplicationError> {
            self.calls
                .lock()
                .expect("calls lock")
                .push(source.0.clone());
            let page = self
                .pages
                .iter()
                .find(|page| page.0 == source.0.as_str())
                .ok_or_else(|| {
                    ApplicationError::RetrievalFailed(format!("no fixture for {source:?}"))
                })?;
            let (_, final_source, media_type, body) = *page;
            Ok(RetrievedResource {
                source: source.clone(),
                final_source: DocumentSource(final_source.into()),
                media_type: MediaType(media_type.into()),
                bytes: body.as_bytes().to_vec(),
                etag: None,
                last_modified: None,
                metadata: BTreeMap::new(),
            })
        }
    }

    fn resolver(
        pages: Vec<FixturePage>,
        refused_hosts: Vec<&'static str>,
        config: LandingPageConfig,
    ) -> (Arc<FixtureRetriever>, LandingPageRetriever) {
        let inner = Arc::new(FixtureRetriever::new(pages));
        let policy: Arc<dyn HttpAccessPolicy> = Arc::new(FixturePolicy { refused_hosts });
        (
            inner.clone(),
            LandingPageRetriever::new(inner, policy, config),
        )
    }

    fn enabled() -> LandingPageConfig {
        LandingPageConfig {
            enabled: true,
            allow_http: true,
        }
    }

    #[tokio::test]
    async fn citation_pdf_url_meta_tag_returns_the_pdf() {
        let (inner, resolver) = resolver(
            vec![
                (
                    "https://publisher.test/article",
                    "https://publisher.test/article",
                    "text/html; charset=utf-8",
                    r#"<html><head><meta name="citation_pdf_url" content="/paper.pdf"></head></html>"#,
                ),
                (
                    "https://publisher.test/paper.pdf",
                    "https://publisher.test/paper.pdf",
                    "application/pdf",
                    "%PDF-1.5 fixture",
                ),
            ],
            vec![],
            enabled(),
        );
        let resource = resolver
            .retrieve(
                &DocumentSource("https://publisher.test/article".into()),
                &RetrievalOptions::default(),
            )
            .await
            .expect("the landing page should resolve");

        assert_eq!(resource.final_source.0, "https://publisher.test/paper.pdf");
        assert_eq!(resource.source.0, "https://publisher.test/article");
        assert_eq!(resource.media_type.0, "application/pdf");
        assert_eq!(
            resource.metadata.get(LANDING_RULE).map(String::as_str),
            Some(RULE_CITATION_PDF_URL)
        );
        assert_eq!(
            resource
                .metadata
                .get(LANDING_REQUESTED_SOURCE)
                .map(String::as_str),
            Some("https://publisher.test/article")
        );
        assert!(!resource.metadata.contains_key(LANDING_RESOLUTION_FAILED));
        assert_eq!(
            inner.calls(),
            vec![
                "https://publisher.test/article".to_string(),
                "https://publisher.test/paper.pdf".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn a_target_that_is_not_a_pdf_keeps_the_landing_page() {
        let (_, resolver) = resolver(
            vec![
                (
                    "https://publisher.test/article",
                    "https://publisher.test/article",
                    "text/html",
                    r#"<meta name="citation_pdf_url" content="/page2">"#,
                ),
                (
                    "https://publisher.test/page2",
                    "https://publisher.test/page2",
                    "text/html",
                    "<html><body>Still HTML.</body></html>",
                ),
            ],
            vec![],
            enabled(),
        );
        let resource = resolver
            .retrieve(
                &DocumentSource("https://publisher.test/article".into()),
                &RetrievalOptions::default(),
            )
            .await
            .expect("the landing page should be returned");

        assert_eq!(resource.final_source.0, "https://publisher.test/article");
        assert_eq!(resource.media_type.essence(), "text/html");
        assert_eq!(
            resource
                .metadata
                .get(LANDING_RESOLUTION_FAILED)
                .map(String::as_str),
            Some("not_pdf")
        );
        assert!(!resource.metadata.contains_key(LANDING_RULE));
    }

    #[tokio::test]
    async fn a_target_the_policy_refuses_is_never_fetched() {
        let (inner, resolver) = resolver(
            vec![(
                "https://publisher.test/article",
                "https://publisher.test/article",
                "text/html",
                r#"<meta name="citation_pdf_url" content="https://blocked.test/paper.pdf">"#,
            )],
            vec!["blocked.test"],
            enabled(),
        );
        let resource = resolver
            .retrieve(
                &DocumentSource("https://publisher.test/article".into()),
                &RetrievalOptions::default(),
            )
            .await
            .expect("the landing page should be returned");

        assert_eq!(
            resource
                .metadata
                .get(LANDING_RESOLUTION_FAILED)
                .map(String::as_str),
            Some("refused_by_policy")
        );
        assert_eq!(
            inner.calls(),
            vec!["https://publisher.test/article".to_string()]
        );
    }

    #[tokio::test]
    async fn a_target_on_a_non_public_address_is_refused() {
        let inner = Arc::new(FixtureRetriever::new(vec![(
            "https://publisher.test/article",
            "https://publisher.test/article",
            "text/html",
            r#"<meta name="citation_pdf_url" content="https://169.254.169.254/latest/meta-data">"#,
        )]));
        let policy: Arc<dyn HttpAccessPolicy> = Arc::new(PublicHttpAccessPolicy::allow_http());
        let resolver = LandingPageRetriever::new(inner.clone(), policy, enabled());
        let resource = resolver
            .retrieve(
                &DocumentSource("https://publisher.test/article".into()),
                &RetrievalOptions::default(),
            )
            .await
            .expect("the landing page should be returned");

        assert_eq!(
            resource
                .metadata
                .get(LANDING_RESOLUTION_FAILED)
                .map(String::as_str),
            Some("refused_by_policy")
        );
        assert_eq!(
            inner.calls(),
            vec!["https://publisher.test/article".to_string()]
        );
    }

    #[tokio::test]
    async fn a_target_that_changes_the_scheme_is_refused() {
        for (landing, tag) in [
            (
                "https://publisher.test/article",
                r#"<meta name="citation_pdf_url" content="http://publisher.test/paper.pdf">"#,
            ),
            (
                "https://publisher.test/article",
                r#"<meta name="citation_pdf_url" content="ftp://publisher.test/paper.pdf">"#,
            ),
        ] {
            let (inner, resolver) = resolver(
                vec![
                    (landing, landing, "text/html", tag),
                    (
                        "http://publisher.test/paper.pdf",
                        "http://publisher.test/paper.pdf",
                        "application/pdf",
                        "%PDF-1.5",
                    ),
                    (
                        "ftp://publisher.test/paper.pdf",
                        "ftp://publisher.test/paper.pdf",
                        "application/pdf",
                        "%PDF-1.5",
                    ),
                ],
                vec![],
                enabled(),
            );
            let resource = resolver
                .retrieve(
                    &DocumentSource(landing.into()),
                    &RetrievalOptions::default(),
                )
                .await
                .expect("the landing page should be returned");
            assert_eq!(
                resource
                    .metadata
                    .get(LANDING_RESOLUTION_FAILED)
                    .map(String::as_str),
                Some("refused_by_policy"),
                "{tag} must be refused"
            );
            assert_eq!(inner.calls(), vec![landing.to_string()]);
        }
    }

    #[tokio::test]
    async fn a_target_with_embedded_credentials_is_refused() {
        let (inner, resolver) = resolver(
            vec![(
                "https://publisher.test/article",
                "https://publisher.test/article",
                "text/html",
                r#"<meta name="citation_pdf_url" content="https://user:pass@publisher.test/paper.pdf">"#,
            )],
            vec![],
            enabled(),
        );
        let resource = resolver
            .retrieve(
                &DocumentSource("https://publisher.test/article".into()),
                &RetrievalOptions::default(),
            )
            .await
            .expect("the landing page should be returned");
        assert_eq!(
            resource
                .metadata
                .get(LANDING_RESOLUTION_FAILED)
                .map(String::as_str),
            Some("refused_by_policy")
        );
        assert_eq!(
            inner.calls(),
            vec!["https://publisher.test/article".to_string()]
        );
    }

    #[tokio::test]
    async fn a_target_that_cannot_be_fetched_keeps_the_landing_page() {
        let (_, resolver) = resolver(
            vec![(
                "https://publisher.test/article",
                "https://publisher.test/article",
                "text/html",
                r#"<meta name="citation_pdf_url" content="/missing.pdf">"#,
            )],
            vec![],
            enabled(),
        );
        let resource = resolver
            .retrieve(
                &DocumentSource("https://publisher.test/article".into()),
                &RetrievalOptions::default(),
            )
            .await
            .expect("the landing page should be returned");
        assert_eq!(
            resource
                .metadata
                .get(LANDING_RESOLUTION_FAILED)
                .map(String::as_str),
            Some("fetch_failed")
        );
    }

    #[tokio::test]
    async fn a_target_that_is_itself_a_landing_page_is_not_followed() {
        let (inner, resolver) = resolver(
            vec![
                (
                    "https://publisher.test/article",
                    "https://publisher.test/article",
                    "text/html",
                    r#"<meta name="citation_pdf_url" content="/intermediate">"#,
                ),
                (
                    "https://publisher.test/intermediate",
                    "https://publisher.test/intermediate",
                    "text/html",
                    r#"<meta name="citation_pdf_url" content="/paper.pdf">"#,
                ),
                (
                    "https://publisher.test/paper.pdf",
                    "https://publisher.test/paper.pdf",
                    "application/pdf",
                    "%PDF-1.5",
                ),
            ],
            vec![],
            enabled(),
        );
        let resource = resolver
            .retrieve(
                &DocumentSource("https://publisher.test/article".into()),
                &RetrievalOptions::default(),
            )
            .await
            .expect("the landing page should be returned");
        assert_eq!(
            resource
                .metadata
                .get(LANDING_RESOLUTION_FAILED)
                .map(String::as_str),
            Some("not_pdf")
        );
        assert_eq!(
            inner.calls(),
            vec![
                "https://publisher.test/article".to_string(),
                "https://publisher.test/intermediate".to_string()
            ],
            "resolution depth is one"
        );
    }

    #[tokio::test]
    async fn an_html_page_without_the_meta_tag_reports_no_meta() {
        let (_, resolver) = resolver(
            vec![(
                "https://publisher.test/article",
                "https://publisher.test/article",
                "text/html",
                "<html><head><title>Abstract</title></head><body>Abstract.</body></html>",
            )],
            vec![],
            enabled(),
        );
        let resource = resolver
            .retrieve(
                &DocumentSource("https://publisher.test/article".into()),
                &RetrievalOptions::default(),
            )
            .await
            .expect("the landing page should be returned");
        assert_eq!(
            resource
                .metadata
                .get(LANDING_RESOLUTION_FAILED)
                .map(String::as_str),
            Some("no_meta")
        );
    }

    #[tokio::test]
    async fn non_html_sources_are_returned_unchanged() {
        let (_, resolver) = resolver(
            vec![(
                "https://publisher.test/notes.md",
                "https://publisher.test/notes.md",
                "text/markdown",
                "# Notes\n\nNo resolution.",
            )],
            vec![],
            enabled(),
        );
        let resource = resolver
            .retrieve(
                &DocumentSource("https://publisher.test/notes.md".into()),
                &RetrievalOptions::default(),
            )
            .await
            .expect("markdown should be returned");
        assert_eq!(resource.media_type.0, "text/markdown");
        assert!(resource.metadata.is_empty());
    }

    #[tokio::test]
    async fn the_disabled_knob_returns_the_landing_page_untouched() {
        let (inner, resolver) = resolver(
            vec![
                (
                    "https://publisher.test/article",
                    "https://publisher.test/article",
                    "text/html",
                    r#"<meta name="citation_pdf_url" content="/paper.pdf">"#,
                ),
                (
                    "https://arxiv.org/pdf/2309.06180",
                    "https://arxiv.org/pdf/2309.06180",
                    "application/pdf",
                    "%PDF-1.5",
                ),
                (
                    "https://arxiv.org/abs/2309.06180",
                    "https://arxiv.org/abs/2309.06180",
                    "text/html",
                    "<html><body>Abstract.</body></html>",
                ),
            ],
            vec![],
            LandingPageConfig {
                enabled: false,
                allow_http: true,
            },
        );
        for source in [
            "https://publisher.test/article",
            "https://arxiv.org/abs/2309.06180",
        ] {
            let resource = resolver
                .retrieve(&DocumentSource(source.into()), &RetrievalOptions::default())
                .await
                .expect("the source should be returned unchanged");
            assert_eq!(resource.final_source.0, source);
            assert!(resource.metadata.is_empty());
        }
        assert_eq!(
            inner.calls(),
            vec![
                "https://publisher.test/article".to_string(),
                "https://arxiv.org/abs/2309.06180".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn the_pure_rule_chooses_the_pdf_before_any_landing_page_fetch() {
        let (inner, resolver) = resolver(
            vec![(
                "https://arxiv.org/pdf/2309.06180",
                "https://arxiv.org/pdf/2309.06180",
                "application/pdf",
                "%PDF-1.5",
            )],
            vec![],
            enabled(),
        );
        let resource = resolver
            .retrieve(
                &DocumentSource("https://arxiv.org/abs/2309.06180".into()),
                &RetrievalOptions::default(),
            )
            .await
            .expect("the abs link should resolve");
        assert_eq!(resource.final_source.0, "https://arxiv.org/pdf/2309.06180");
        assert_eq!(
            resource.metadata.get(LANDING_RULE).map(String::as_str),
            Some(RULE_ARXIV_ABS)
        );
        assert_eq!(
            inner.calls(),
            vec!["https://arxiv.org/pdf/2309.06180".to_string()]
        );
    }

    #[tokio::test]
    async fn a_pure_rule_whose_target_is_not_a_pdf_falls_back_to_the_landing_page() {
        let (inner, resolver) = resolver(
            vec![
                (
                    "https://arxiv.org/pdf/2309.06180",
                    "https://arxiv.org/pdf/2309.06180",
                    "text/html",
                    "<html><body>Not a PDF.</body></html>",
                ),
                (
                    "https://arxiv.org/abs/2309.06180",
                    "https://arxiv.org/abs/2309.06180",
                    "text/html",
                    "<html><body>Abstract.</body></html>",
                ),
            ],
            vec![],
            enabled(),
        );
        let resource = resolver
            .retrieve(
                &DocumentSource("https://arxiv.org/abs/2309.06180".into()),
                &RetrievalOptions::default(),
            )
            .await
            .expect("the landing page should be returned");
        assert_eq!(resource.final_source.0, "https://arxiv.org/abs/2309.06180");
        assert_eq!(
            resource
                .metadata
                .get(LANDING_RESOLUTION_FAILED)
                .map(String::as_str),
            Some("not_pdf")
        );
        assert_eq!(
            inner.calls(),
            vec![
                "https://arxiv.org/pdf/2309.06180".to_string(),
                "https://arxiv.org/abs/2309.06180".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn the_pure_rule_also_runs_on_the_final_url_of_a_redirect_chain() {
        let (_, resolver) = resolver(
            vec![
                (
                    "https://doi.test/10.48550/arXiv.2309.06180",
                    "https://arxiv.org/abs/2309.06180",
                    "text/html",
                    "<html><body>Abstract.</body></html>",
                ),
                (
                    "https://arxiv.org/pdf/2309.06180",
                    "https://arxiv.org/pdf/2309.06180",
                    "application/pdf",
                    "%PDF-1.5",
                ),
            ],
            vec![],
            enabled(),
        );
        let resource = resolver
            .retrieve(
                &DocumentSource("https://doi.test/10.48550/arXiv.2309.06180".into()),
                &RetrievalOptions::default(),
            )
            .await
            .expect("the DOI link should resolve");
        assert_eq!(resource.final_source.0, "https://arxiv.org/pdf/2309.06180");
        assert_eq!(
            resource.metadata.get(LANDING_RULE).map(String::as_str),
            Some(RULE_ARXIV_ABS)
        );
        assert_eq!(
            resource
                .metadata
                .get(LANDING_REQUESTED_SOURCE)
                .map(String::as_str),
            Some("https://doi.test/10.48550/arXiv.2309.06180")
        );
    }

    #[tokio::test]
    async fn an_http_landing_page_is_only_resolved_when_plain_http_is_allowed() {
        let pages = vec![
            (
                "http://publisher.test/article",
                "http://publisher.test/article",
                "text/html",
                r#"<meta name="citation_pdf_url" content="/paper.pdf">"#,
            ),
            (
                "http://publisher.test/paper.pdf",
                "http://publisher.test/paper.pdf",
                "application/pdf",
                "%PDF-1.5",
            ),
        ];
        let (_, allowed) = resolver(
            pages.clone(),
            vec![],
            LandingPageConfig {
                enabled: true,
                allow_http: true,
            },
        );
        let resource = allowed
            .retrieve(
                &DocumentSource("http://publisher.test/article".into()),
                &RetrievalOptions::default(),
            )
            .await
            .expect("plain HTTP should resolve when the deployment allows it");
        assert_eq!(resource.media_type.0, "application/pdf");

        let (_, refused) = resolver(
            pages,
            vec![],
            LandingPageConfig {
                enabled: true,
                allow_http: false,
            },
        );
        let resource = refused
            .retrieve(
                &DocumentSource("http://publisher.test/article".into()),
                &RetrievalOptions::default(),
            )
            .await
            .expect("the landing page should be returned");
        assert_eq!(resource.media_type.essence(), "text/html");
        assert!(!resource.metadata.contains_key(LANDING_RESOLUTION_FAILED));
    }
}
