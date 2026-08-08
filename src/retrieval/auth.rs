use url::Url;

use crate::application::ports::ApplicationError;

pub trait CredentialProvider: Send + Sync {
    fn bearer_token(&self, profile: &str, url: &Url) -> Result<String, ApplicationError>;
}

#[derive(Default)]
pub struct NoCredentialProvider;

impl CredentialProvider for NoCredentialProvider {
    fn bearer_token(&self, profile: &str, _url: &Url) -> Result<String, ApplicationError> {
        Err(ApplicationError::AuthenticationFailed(format!(
            "auth profile {profile:?} is not configured"
        )))
    }
}

#[derive(Default)]
pub struct EnvironmentCredentialProvider;

impl CredentialProvider for EnvironmentCredentialProvider {
    fn bearer_token(&self, profile: &str, url: &Url) -> Result<String, ApplicationError> {
        let key = profile_key(profile)?;
        let hosts_name = format!("READING_MCP_AUTH_{key}_HOSTS");
        let token_name = format!("READING_MCP_AUTH_{key}_BEARER_TOKEN");

        let hosts = std::env::var(&hosts_name).map_err(|_| {
            ApplicationError::AuthenticationFailed(format!(
                "auth profile {profile:?} has no host allowlist"
            ))
        })?;
        let host = url.host_str().ok_or_else(|| {
            ApplicationError::AuthenticationFailed("authenticated URL has no host".into())
        })?;
        if !hosts
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .any(|pattern| host_matches(pattern, host))
        {
            return Err(ApplicationError::AuthenticationFailed(format!(
                "auth profile {profile:?} is not allowed for host {host}"
            )));
        }

        let token = std::env::var(&token_name).map_err(|_| {
            ApplicationError::AuthenticationFailed(format!(
                "auth profile {profile:?} has no bearer token"
            ))
        })?;
        if token.trim().is_empty() {
            return Err(ApplicationError::AuthenticationFailed(format!(
                "auth profile {profile:?} has an empty bearer token"
            )));
        }
        Ok(token)
    }
}

fn profile_key(profile: &str) -> Result<String, ApplicationError> {
    let trimmed = profile.trim();
    if trimmed.is_empty() {
        return Err(ApplicationError::InvalidRequest(
            "auth_profile must not be empty".into(),
        ));
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err(ApplicationError::InvalidRequest(
            "auth_profile may contain only ASCII letters, numbers, '-' and '_'".into(),
        ));
    }
    Ok(trimmed
        .chars()
        .map(|ch| {
            if ch == '-' {
                '_'
            } else {
                ch.to_ascii_uppercase()
            }
        })
        .collect())
}

fn host_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim().to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    if let Some(suffix) = pattern.strip_prefix("*.") {
        host != suffix && host.ends_with(&format!(".{suffix}"))
    } else {
        host == pattern
    }
}

#[cfg(test)]
mod tests {
    use super::host_matches;

    #[test]
    fn wildcard_hosts_do_not_match_apex() {
        assert!(host_matches("*.example.com", "docs.example.com"));
        assert!(!host_matches("*.example.com", "example.com"));
        assert!(!host_matches("*.example.com", "evil-example.com"));
    }
}
