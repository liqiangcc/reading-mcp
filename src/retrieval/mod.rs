mod auth;
mod file;
mod http;
mod landing;
mod limited_file;
mod revalidating_http;
mod router;

pub use auth::{CredentialProvider, EnvironmentCredentialProvider, NoCredentialProvider};
pub use file::{FileRetriever, LocalFileSourcePolicy};
pub use http::{HttpRetrievalOutcome, HttpRetriever, HttpRetrieverConfig, HttpValidators};
pub use landing::{
    LANDING_REQUESTED_SOURCE, LANDING_RESOLUTION_FAILED, LANDING_RULE, LandingPageConfig,
    LandingPageRetriever, META_SCAN_LIMIT, RULE_ACL_ANTHOLOGY, RULE_ARXIV_ABS,
    RULE_CITATION_PDF_URL, RULE_OPENREVIEW_FORUM, ResolutionFailure, ResolvedTarget,
    extract_citation_pdf_url, known_landing_target,
};
pub use limited_file::LimitedFileRetriever;
pub use revalidating_http::RevalidatingHttpRetriever;
pub use router::{RetrieverRouter, SourcePolicyRouter};
