use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::env;

pub const MANAGED_BROWSER_NAME: &str = "managed";
pub const EXPLICIT_BROWSER_NAME: &str = "explicit";
pub const PERSONAL_BROWSER_NAME: &str = "personal";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedBrowserEndpoint {
    pub name: String,
    pub cdp_endpoint: String,
    pub vnc_endpoint: Option<String>,
    pub novnc_url: Option<String>,
    pub share_url: Option<String>,
    #[serde(default)]
    pub metadata: BrowserEndpointMetadata,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserEndpointMetadata {
    pub owner_id: Option<String>,
    pub owner_label: Option<String>,
    pub workspace_id: Option<String>,
    pub pool_id: Option<String>,
    pub installation_id: Option<String>,
    pub device_id: Option<String>,
    pub device_label: Option<String>,
    pub browser_kind: Option<String>,
    pub browser_label: Option<String>,
    pub platform: Option<String>,
    pub profile_label: Option<String>,
    pub last_seen_at: Option<String>,
}

impl BrowserEndpointMetadata {
    pub fn merge_missing_from(&mut self, other: &Self) {
        merge_option(&mut self.owner_id, &other.owner_id);
        merge_option(&mut self.owner_label, &other.owner_label);
        merge_option(&mut self.workspace_id, &other.workspace_id);
        merge_option(&mut self.pool_id, &other.pool_id);
        merge_option(&mut self.installation_id, &other.installation_id);
        merge_option(&mut self.device_id, &other.device_id);
        merge_option(&mut self.device_label, &other.device_label);
        merge_option(&mut self.browser_kind, &other.browser_kind);
        merge_option(&mut self.browser_label, &other.browser_label);
        merge_option(&mut self.platform, &other.platform);
        merge_option(&mut self.profile_label, &other.profile_label);
        merge_option(&mut self.last_seen_at, &other.last_seen_at);
    }

    pub fn merge_from(&mut self, other: &Self) {
        replace_option(&mut self.owner_id, &other.owner_id);
        replace_option(&mut self.owner_label, &other.owner_label);
        replace_option(&mut self.workspace_id, &other.workspace_id);
        replace_option(&mut self.pool_id, &other.pool_id);
        replace_option(&mut self.installation_id, &other.installation_id);
        replace_option(&mut self.device_id, &other.device_id);
        replace_option(&mut self.device_label, &other.device_label);
        replace_option(&mut self.browser_kind, &other.browser_kind);
        replace_option(&mut self.browser_label, &other.browser_label);
        replace_option(&mut self.platform, &other.platform);
        replace_option(&mut self.profile_label, &other.profile_label);
        replace_option(&mut self.last_seen_at, &other.last_seen_at);
    }

    pub fn stable_identity(&self) -> Option<&str> {
        self.installation_id
            .as_deref()
            .or(self.device_id.as_deref())
            .filter(|value| !value.trim().is_empty())
    }
}

impl NamedBrowserEndpoint {
    pub fn new(name: impl AsRef<str>, cdp_endpoint: impl AsRef<str>) -> Result<Self> {
        let name = normalize_configured_browser_name(name.as_ref())?;
        let cdp_endpoint = normalize_cdp_endpoint(cdp_endpoint.as_ref())?;
        Ok(Self {
            name,
            cdp_endpoint,
            vnc_endpoint: None,
            novnc_url: None,
            share_url: None,
            metadata: BrowserEndpointMetadata::default(),
        })
    }

    pub fn shared(name: impl AsRef<str>, share_url: impl AsRef<str>) -> Result<Self> {
        let name = normalize_configured_browser_name(name.as_ref())?;
        let share_url = normalize_share_url(share_url.as_ref())?;
        Ok(Self {
            name,
            cdp_endpoint: String::new(),
            vnc_endpoint: None,
            novnc_url: None,
            share_url: Some(share_url),
            metadata: BrowserEndpointMetadata::default(),
        })
    }

    pub fn with_vnc_endpoint(mut self, vnc_endpoint: impl AsRef<str>) -> Result<Self> {
        self.vnc_endpoint = Some(normalize_vnc_endpoint(vnc_endpoint.as_ref())?);
        Ok(self)
    }

    pub fn with_novnc_url(mut self, novnc_url: impl AsRef<str>) -> Result<Self> {
        self.novnc_url = Some(normalize_novnc_url(novnc_url.as_ref())?);
        Ok(self)
    }

    pub fn with_share_url(mut self, share_url: impl AsRef<str>) -> Result<Self> {
        self.share_url = Some(normalize_share_url(share_url.as_ref())?);
        Ok(self)
    }

    pub fn with_metadata(mut self, metadata: BrowserEndpointMetadata) -> Self {
        self.metadata = metadata;
        self
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

pub fn parse_named_browser_vnc_endpoint(value: &str) -> Result<(String, String)> {
    let (name, endpoint) = value
        .split_once('=')
        .ok_or_else(|| anyhow!("browser VNC endpoint must use NAME=HOST:PORT format"))?;
    Ok((
        normalize_configured_browser_name(name)?,
        normalize_vnc_endpoint(endpoint)?,
    ))
}

pub fn parse_named_browser_vnc_endpoints(value: &str) -> Result<Vec<(String, String)>> {
    value
        .split([',', ';'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(parse_named_browser_vnc_endpoint)
        .collect()
}

pub fn parse_named_browser_novnc_url(value: &str) -> Result<(String, String)> {
    let (name, url) = value
        .split_once('=')
        .ok_or_else(|| anyhow!("browser noVNC URL must use NAME=URL format"))?;
    Ok((
        normalize_configured_browser_name(name)?,
        normalize_novnc_url(url)?,
    ))
}

pub fn parse_named_browser_novnc_urls(value: &str) -> Result<Vec<(String, String)>> {
    value
        .split([',', ';'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(parse_named_browser_novnc_url)
        .collect()
}

pub fn parse_named_browser_share_url(value: &str) -> Result<(String, String)> {
    let (name, url) = value
        .split_once('=')
        .ok_or_else(|| anyhow!("browser share URL must use NAME=URL format"))?;
    Ok((
        normalize_configured_browser_name(name)?,
        normalize_share_url(url)?,
    ))
}

pub fn parse_named_browser_share_urls(value: &str) -> Result<Vec<(String, String)>> {
    value
        .split([',', ';'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(parse_named_browser_share_url)
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

    if let Some(value) = nonempty_env("BROWSER_CONNECTION_BROWSER_VNCS") {
        for (name, endpoint) in parse_named_browser_vnc_endpoints(&value)? {
            upsert_browser_vnc_endpoint(&mut endpoints, name, endpoint);
        }
    }

    if let Some(endpoint) = nonempty_env("BROWSER_CONNECTION_PERSONAL_VNC_ENDPOINT") {
        upsert_browser_vnc_endpoint(
            &mut endpoints,
            PERSONAL_BROWSER_NAME.to_string(),
            normalize_vnc_endpoint(&endpoint)?,
        );
    }

    if let Some(value) = nonempty_env("BROWSER_CONNECTION_BROWSER_NOVNC_URLS") {
        for (name, url) in parse_named_browser_novnc_urls(&value)? {
            upsert_browser_novnc_url(&mut endpoints, name, url);
        }
    }

    if let Some(url) = nonempty_env("BROWSER_CONNECTION_PERSONAL_NOVNC_URL") {
        upsert_browser_novnc_url(
            &mut endpoints,
            PERSONAL_BROWSER_NAME.to_string(),
            normalize_novnc_url(&url)?,
        );
    }

    if let Some(value) = nonempty_env("BROWSER_CONNECTION_BROWSER_SHARES") {
        for (name, url) in parse_named_browser_share_urls(&value)? {
            upsert_browser_share_url(&mut endpoints, name, url);
        }
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

pub(crate) fn normalize_vnc_endpoint(endpoint: &str) -> Result<String> {
    let endpoint = endpoint.trim().trim_end_matches('/');
    if endpoint.is_empty() {
        return Err(anyhow!("VNC endpoint must not be empty"));
    }
    if endpoint.starts_with("http://")
        || endpoint.starts_with("https://")
        || endpoint.starts_with("ws://")
        || endpoint.starts_with("wss://")
    {
        return Err(anyhow!(
            "VNC endpoint `{endpoint}` must use HOST:PORT, not a URL"
        ));
    }
    let Some((host, port)) = endpoint.rsplit_once(':') else {
        return Err(anyhow!("VNC endpoint `{endpoint}` must use HOST:PORT"));
    };
    let host = host.trim().trim_matches(['[', ']']);
    let port = port
        .trim()
        .parse::<u16>()
        .map_err(|_| anyhow!("VNC endpoint `{endpoint}` must end with a valid TCP port"))?;
    if host.is_empty() {
        return Err(anyhow!("VNC endpoint `{endpoint}` must include a host"));
    }
    Ok(format!("{host}:{port}"))
}

pub(crate) fn normalize_novnc_url(url: &str) -> Result<String> {
    let url = url.trim().trim_end_matches('/');
    if url.is_empty() {
        return Err(anyhow!("noVNC URL must not be empty"));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(anyhow!(
            "noVNC URL `{url}` must start with http:// or https://"
        ));
    }
    Ok(url.to_string())
}

pub(crate) fn normalize_share_url(url: &str) -> Result<String> {
    let url = url.trim();
    if url.is_empty() {
        return Err(anyhow!("browser share URL must not be empty"));
    }
    if !(url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("ws://")
        || url.starts_with("wss://"))
    {
        return Err(anyhow!(
            "browser share URL `{url}` must start with http://, https://, ws:// or wss://"
        ));
    }
    Ok(url.to_string())
}

pub(crate) fn upsert_browser_endpoint(
    endpoints: &mut Vec<NamedBrowserEndpoint>,
    endpoint: NamedBrowserEndpoint,
) {
    if let Some(existing) = endpoints
        .iter_mut()
        .find(|existing| existing.name == endpoint.name)
    {
        let vnc_endpoint = existing.vnc_endpoint.clone();
        let novnc_url = existing.novnc_url.clone();
        let share_url = existing.share_url.clone();
        let metadata = existing.metadata.clone();
        *existing = endpoint;
        if existing.vnc_endpoint.is_none() {
            existing.vnc_endpoint = vnc_endpoint;
        }
        if existing.novnc_url.is_none() {
            existing.novnc_url = novnc_url;
        }
        if existing.share_url.is_none() {
            existing.share_url = share_url;
        }
        existing.metadata.merge_missing_from(&metadata);
    } else {
        endpoints.push(endpoint);
    }
}

pub(crate) fn upsert_browser_vnc_endpoint(
    endpoints: &mut Vec<NamedBrowserEndpoint>,
    name: String,
    vnc_endpoint: String,
) {
    if let Some(existing) = endpoints.iter_mut().find(|existing| existing.name == name) {
        existing.vnc_endpoint = Some(vnc_endpoint);
    } else {
        endpoints.push(NamedBrowserEndpoint {
            name,
            cdp_endpoint: String::new(),
            vnc_endpoint: Some(vnc_endpoint),
            novnc_url: None,
            share_url: None,
            metadata: BrowserEndpointMetadata::default(),
        });
    }
}

pub(crate) fn upsert_browser_novnc_url(
    endpoints: &mut Vec<NamedBrowserEndpoint>,
    name: String,
    novnc_url: String,
) {
    if let Some(existing) = endpoints.iter_mut().find(|existing| existing.name == name) {
        existing.novnc_url = Some(novnc_url);
    } else {
        endpoints.push(NamedBrowserEndpoint {
            name,
            cdp_endpoint: String::new(),
            vnc_endpoint: None,
            novnc_url: Some(novnc_url),
            share_url: None,
            metadata: BrowserEndpointMetadata::default(),
        });
    }
}

pub(crate) fn upsert_browser_share_url(
    endpoints: &mut Vec<NamedBrowserEndpoint>,
    name: String,
    share_url: String,
) {
    if let Some(existing) = endpoints.iter_mut().find(|existing| existing.name == name) {
        existing.share_url = Some(share_url);
    } else {
        endpoints.push(NamedBrowserEndpoint {
            name,
            cdp_endpoint: String::new(),
            vnc_endpoint: None,
            novnc_url: None,
            share_url: Some(share_url),
            metadata: BrowserEndpointMetadata::default(),
        });
    }
}

pub(crate) fn upsert_browser_metadata(
    endpoints: &mut [NamedBrowserEndpoint],
    name: &str,
    metadata: &BrowserEndpointMetadata,
) {
    if let Some(existing) = endpoints.iter_mut().find(|existing| existing.name == name) {
        existing.metadata.merge_from(metadata);
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

fn merge_option(target: &mut Option<String>, source: &Option<String>) {
    if target.as_deref().unwrap_or("").trim().is_empty() {
        *target = source.clone();
    }
}

fn replace_option(target: &mut Option<String>, source: &Option<String>) {
    if source.as_deref().unwrap_or("").trim().is_empty() {
        return;
    }
    *target = source.clone();
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
        assert_eq!(endpoint.vnc_endpoint, None);
        assert_eq!(endpoint.novnc_url, None);
        assert_eq!(endpoint.share_url, None);
    }

    #[test]
    fn parses_named_vnc_endpoint() {
        let (name, endpoint) =
            parse_named_browser_vnc_endpoint("personal=host.docker.internal:5900")
                .expect("vnc endpoint parses");

        assert_eq!(name, PERSONAL_BROWSER_NAME);
        assert_eq!(endpoint, "host.docker.internal:5900");
    }

    #[test]
    fn rejects_url_as_vnc_endpoint() {
        let error = parse_named_browser_vnc_endpoint("personal=http://127.0.0.1:5900")
            .expect_err("VNC endpoint is not an HTTP URL");

        assert!(error.to_string().contains("HOST:PORT"));
    }

    #[test]
    fn parses_named_novnc_url() {
        let (name, url) = parse_named_browser_novnc_url("personal=http://127.0.0.1:6680/vnc.html")
            .expect("noVNC URL parses");

        assert_eq!(name, PERSONAL_BROWSER_NAME);
        assert_eq!(url, "http://127.0.0.1:6680/vnc.html");
    }

    #[test]
    fn parses_named_share_url() {
        let (name, url) =
            parse_named_browser_share_url("edge=https://relay.example/share/s1#agent=a1")
                .expect("share URL parses");

        assert_eq!(name, "edge");
        assert_eq!(url, "https://relay.example/share/s1#agent=a1");
    }

    #[test]
    fn deserializes_legacy_endpoint_without_metadata() {
        let endpoint: NamedBrowserEndpoint = serde_json::from_str(
            r#"{
                "name": "edge",
                "cdp_endpoint": "",
                "vnc_endpoint": null,
                "novnc_url": null,
                "share_url": "https://relay.example/share/s1#agent=a1"
            }"#,
        )
        .expect("legacy endpoint JSON deserializes");

        assert_eq!(endpoint.name, "edge");
        assert_eq!(endpoint.metadata, BrowserEndpointMetadata::default());
    }

    #[test]
    fn rejects_novnc_url_without_http_scheme() {
        let error = parse_named_browser_novnc_url("personal=127.0.0.1:6680/vnc.html")
            .expect_err("noVNC URL requires a URL scheme");

        assert!(error
            .to_string()
            .contains("must start with http:// or https://"));
    }

    #[test]
    fn upserting_cdp_preserves_display_metadata() {
        let mut endpoints =
            vec![
                NamedBrowserEndpoint::new(PERSONAL_BROWSER_NAME, "http://127.0.0.1:9222")
                    .expect("endpoint is valid")
                    .with_vnc_endpoint("host.docker.internal:5900")
                    .expect("vnc endpoint is valid")
                    .with_share_url("https://relay.example/share/s1#agent=a1")
                    .expect("share URL is valid"),
            ];

        upsert_browser_endpoint(
            &mut endpoints,
            NamedBrowserEndpoint::new(PERSONAL_BROWSER_NAME, "http://127.0.0.1:9333")
                .expect("endpoint is valid"),
        );

        assert_eq!(endpoints[0].cdp_endpoint, "http://127.0.0.1:9333");
        assert_eq!(
            endpoints[0].vnc_endpoint.as_deref(),
            Some("host.docker.internal:5900")
        );
        assert_eq!(
            endpoints[0].share_url.as_deref(),
            Some("https://relay.example/share/s1#agent=a1")
        );
    }

    #[test]
    fn rejects_reserved_configured_browser_names() {
        let error = NamedBrowserEndpoint::new(MANAGED_BROWSER_NAME, "http://127.0.0.1:9222")
            .expect_err("managed is reserved");

        assert!(error.to_string().contains("reserved"));
    }

    #[test]
    fn env_configuration_merges_personal_cdp_vnc_and_novnc() {
        let saved = [
            (
                "BROWSER_CONNECTION_PERSONAL_CDP_ENDPOINT",
                env::var("BROWSER_CONNECTION_PERSONAL_CDP_ENDPOINT").ok(),
            ),
            (
                "BROWSER_CONNECTION_PERSONAL_VNC_ENDPOINT",
                env::var("BROWSER_CONNECTION_PERSONAL_VNC_ENDPOINT").ok(),
            ),
            (
                "BROWSER_CONNECTION_PERSONAL_NOVNC_URL",
                env::var("BROWSER_CONNECTION_PERSONAL_NOVNC_URL").ok(),
            ),
            (
                "BROWSER_CONNECTION_BROWSERS",
                env::var("BROWSER_CONNECTION_BROWSERS").ok(),
            ),
            (
                "BROWSER_CONNECTION_BROWSER_VNCS",
                env::var("BROWSER_CONNECTION_BROWSER_VNCS").ok(),
            ),
            (
                "BROWSER_CONNECTION_BROWSER_NOVNC_URLS",
                env::var("BROWSER_CONNECTION_BROWSER_NOVNC_URLS").ok(),
            ),
            (
                "BROWSER_CONNECTION_BROWSER_SHARES",
                env::var("BROWSER_CONNECTION_BROWSER_SHARES").ok(),
            ),
        ];

        env::set_var(
            "BROWSER_CONNECTION_PERSONAL_CDP_ENDPOINT",
            "http://host.docker.internal:9222",
        );
        env::set_var(
            "BROWSER_CONNECTION_PERSONAL_VNC_ENDPOINT",
            "host.docker.internal:5900",
        );
        env::set_var(
            "BROWSER_CONNECTION_PERSONAL_NOVNC_URL",
            "http://127.0.0.1:6680/vnc.html",
        );
        env::remove_var("BROWSER_CONNECTION_BROWSERS");
        env::remove_var("BROWSER_CONNECTION_BROWSER_VNCS");
        env::remove_var("BROWSER_CONNECTION_BROWSER_NOVNC_URLS");
        env::set_var(
            "BROWSER_CONNECTION_BROWSER_SHARES",
            "edge=https://relay.example/share/s1#agent=a1",
        );

        let endpoints = browser_endpoints_from_env().expect("env endpoints parse");
        let personal = endpoints
            .iter()
            .find(|endpoint| endpoint.name == PERSONAL_BROWSER_NAME)
            .expect("personal endpoint exists");

        assert_eq!(personal.cdp_endpoint, "http://host.docker.internal:9222");
        assert_eq!(
            personal.vnc_endpoint.as_deref(),
            Some("host.docker.internal:5900")
        );
        assert_eq!(
            personal.novnc_url.as_deref(),
            Some("http://127.0.0.1:6680/vnc.html")
        );
        let edge = endpoints
            .iter()
            .find(|endpoint| endpoint.name == "edge")
            .expect("edge shared endpoint exists");
        assert_eq!(
            edge.share_url.as_deref(),
            Some("https://relay.example/share/s1#agent=a1")
        );

        for (name, value) in saved {
            match value {
                Some(value) => env::set_var(name, value),
                None => env::remove_var(name),
            }
        }
    }
}
