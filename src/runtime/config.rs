use std::path::PathBuf;
use std::time::Duration;

use crate::infrastructure::ResourceBudget;
use crate::retrieval::HttpRetrieverConfig;

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub local_roots: Vec<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub allow_http: bool,
    pub telemetry: bool,
    pub resource_budget: ResourceBudget,
    pub http: HttpRetrieverConfig,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        let resource_budget = ResourceBudget::default();
        let mut http = HttpRetrieverConfig::default();
        http.max_response_bytes = resource_budget.max_document_bytes;
        Self {
            local_roots: vec![],
            state_dir: default_state_dir(),
            allow_http: false,
            telemetry: true,
            resource_budget,
            http,
        }
    }
}

impl RuntimeConfig {
    pub fn from_env() -> Result<Self, String> {
        let mut config = Self::default();

        config.local_roots = std::env::var_os("READING_MCP_LOCAL_ROOTS")
            .map(|value| std::env::split_paths(&value).collect())
            .unwrap_or_default();

        if let Some(value) = std::env::var_os("READING_MCP_STATE_DIR") {
            let value = value.to_string_lossy();
            config.state_dir = if value.trim().is_empty() || value == "memory" {
                None
            } else {
                Some(PathBuf::from(value.as_ref()))
            };
        }

        config.allow_http = env_bool("READING_MCP_ALLOW_HTTP", config.allow_http)?;
        config.telemetry = env_bool("READING_MCP_TELEMETRY", config.telemetry)?;

        config.resource_budget.max_document_bytes = env_usize(
            "READING_MCP_MAX_DOCUMENT_BYTES",
            config.resource_budget.max_document_bytes,
        )?;
        config.resource_budget.max_pdf_pages = env_usize(
            "READING_MCP_MAX_PDF_PAGES",
            config.resource_budget.max_pdf_pages,
        )?;
        config.resource_budget.max_sections = env_usize(
            "READING_MCP_MAX_SECTIONS",
            config.resource_budget.max_sections,
        )?;
        config.resource_budget.max_section_depth = env_usize(
            "READING_MCP_MAX_SECTION_DEPTH",
            config.resource_budget.max_section_depth,
        )?;
        config.resource_budget.max_normalized_chars = env_usize(
            "READING_MCP_MAX_NORMALIZED_CHARS",
            config.resource_budget.max_normalized_chars,
        )?;
        config.resource_budget.parse_timeout = Duration::from_secs(env_u64(
            "READING_MCP_PARSE_TIMEOUT_SECS",
            config.resource_budget.parse_timeout.as_secs(),
        )?);

        config.http.max_redirects =
            env_usize("READING_MCP_HTTP_MAX_REDIRECTS", config.http.max_redirects)?;
        config.http.max_response_bytes = config.resource_budget.max_document_bytes;
        config.http.max_concurrency = env_usize(
            "READING_MCP_HTTP_MAX_CONCURRENCY",
            config.http.max_concurrency,
        )?;
        config.http.request_timeout = Duration::from_secs(env_u64(
            "READING_MCP_HTTP_TIMEOUT_SECS",
            config.http.request_timeout.as_secs(),
        )?);
        config.http.connect_timeout = Duration::from_secs(env_u64(
            "READING_MCP_HTTP_CONNECT_TIMEOUT_SECS",
            config.http.connect_timeout.as_secs(),
        )?);

        validate(&config)?;
        Ok(config)
    }
}

fn validate(config: &RuntimeConfig) -> Result<(), String> {
    if config.resource_budget.max_document_bytes == 0 {
        return Err("READING_MCP_MAX_DOCUMENT_BYTES must be greater than zero".into());
    }
    if config.resource_budget.max_pdf_pages == 0 {
        return Err("READING_MCP_MAX_PDF_PAGES must be greater than zero".into());
    }
    if config.resource_budget.parse_timeout.is_zero() {
        return Err("READING_MCP_PARSE_TIMEOUT_SECS must be greater than zero".into());
    }
    if config.http.max_concurrency == 0 {
        return Err("READING_MCP_HTTP_MAX_CONCURRENCY must be greater than zero".into());
    }
    Ok(())
}

fn default_state_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| home.join(".reading-mcp"))
}

fn env_bool(name: &str, default: bool) -> Result<bool, String> {
    let Ok(value) = std::env::var(name) else {
        return Ok(default);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!("{name} must be true/false")),
    }
}

fn env_usize(name: &str, default: usize) -> Result<usize, String> {
    let Ok(value) = std::env::var(name) else {
        return Ok(default);
    };
    value
        .parse::<usize>()
        .map_err(|_| format!("{name} must be a non-negative integer"))
}

fn env_u64(name: &str, default: u64) -> Result<u64, String> {
    let Ok(value) = std::env::var(name) else {
        return Ok(default);
    };
    value
        .parse::<u64>()
        .map_err(|_| format!("{name} must be a non-negative integer"))
}
