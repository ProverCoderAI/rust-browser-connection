use anyhow::{anyhow, Result};
use std::env;

pub const MANAGED_BROWSER_NAME: &str = "managed";
pub const EXPLICIT_BROWSER_NAME: &str = "explicit";
pub const PERSONAL_BROWSER_NAME: &str = "personal";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedBrowserEndpoint {
    pub name: String,
    pub cdp_endpoint: String,
}

impl NamedBrowserEndpoint {
    pub fn new(name: impl AsRef<str>, cdp_endpoint: impl AsRef<str>) -> Result<Self> {
        let name = normalize_configured_browser_name(name.as_ref())?;
        let cdp_endpoint = normalize_cdp_endpoint(cdp_endpoint.as_ref())?;
        Ok(Self { name, cdp_endpoint })
    }
}

pub fn parse_named_browser_endpoint(value: &str) -> Result<NamedBrowserEndpoint> {
    let (name, endpoint) = value
        .split_once('=')
        .ok_or_else(|| anyhow!("browser endpoint must use NAME=CDP_ENDPOINT format"))?;
    NamedBrowserEndpoint::new(name, endpoint)
}

pub fn parse_named_browser_endpoints(value: &str) -> Result<Vec<NamedBrowserEndpoint>> {
    value
        .split([',', ';'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(parse_named_browser_endpoint)
        .collect()
}

pub fn browser_endpoints_from_env() -> Result<Vec<NamedBrowserEndpoint>> {
    let mut endpoints = Vec::new();

    if let Some(value) = nonempty_env("BROWSER_CONNECTION_BROWSERS") {
        endpoints.extend(parse_named_browser_endpoints(&value)?);
    }

    if let Some(endpoint) = nonempty_env("BROWSER_CONNECTION_PERSONAL_CDP_ENDPOINT") {
        endpoints.push(NamedBrowserEndpoint::new(PERSONAL_BROWSER_NAME, endpoint)?);
    }

    Ok(endpoints)
}

pub fn active_browser_from_env() -> Option<String> {
    nonempty_env("BROWSER_CONNECTION_ACTIVE_BROWSER")
}

pub(crate) fn configured_browser_kind(name: &str) -> &'static str {
    if name == PERSONAL_BROWSER_NAME {
        "personal"
    } else {
        "external"
    }
}

pub(crate) fn normalize_browser_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(anyhow!("browser name must not be empty"));
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(anyhow!(
            "browser name `{name}` may only contain ASCII letters, digits, '.', '_' or '-'"
        ));
    }
    Ok(name.to_string())
}

pub(crate) fn normalize_cdp_endpoint(endpoint: &str) -> Result<String> {
    let endpoint = endpoint.trim().trim_end_matches('/');
    if endpoint.is_empty() {
        return Err(anyhow!("CDP endpoint must not be empty"));
    }
    let endpoint = endpoint
        .strip_suffix("/json/version")
        .or_else(|| endpoint.strip_suffix("/json/list"))
        .unwrap_or(endpoint)
        .trim_end_matches('/');
    if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
        return Err(anyhow!(
            "CDP endpoint `{endpoint}` must start with http:// or https://"
        ));
    }
    Ok(endpoint.to_string())
}

pub(crate) fn upsert_browser_endpoint(
    endpoints: &mut Vec<NamedBrowserEndpoint>,
    endpoint: NamedBrowserEndpoint,
) {
    if let Some(existing) = endpoints
        .iter_mut()
        .find(|existing| existing.name == endpoint.name)
    {
        *existing = endpoint;
    } else {
        endpoints.push(endpoint);
    }
}

fn normalize_configured_browser_name(name: &str) -> Result<String> {
    let name = normalize_browser_name(name)?;
    if name == MANAGED_BROWSER_NAME || name == EXPLICIT_BROWSER_NAME {
        return Err(anyhow!(
            "`{name}` is reserved and cannot name a configured browser"
        ));
    }
    Ok(name)
}

fn nonempty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_personal_browser_endpoint() {
        let endpoint = parse_named_browser_endpoint("personal=http://127.0.0.1:9222/json/version")
            .expect("personal endpoint parses");

        assert_eq!(endpoint.name, PERSONAL_BROWSER_NAME);
        assert_eq!(endpoint.cdp_endpoint, "http://127.0.0.1:9222");
    }

    #[test]
    fn rejects_reserved_configured_browser_names() {
        let error = NamedBrowserEndpoint::new(MANAGED_BROWSER_NAME, "http://127.0.0.1:9222")
            .expect_err("managed is reserved");

        assert!(error.to_string().contains("reserved"));
    }
}
