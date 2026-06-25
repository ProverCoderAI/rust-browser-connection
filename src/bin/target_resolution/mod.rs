use anyhow::{anyhow, Context, Result};
use docker_git_browser_connection::browser_actions::BrowserTarget;
use docker_git_browser_connection::mcp::{
    active_browser_from_env, browser_endpoints_from_env, NamedBrowserEndpoint,
};
use docker_git_browser_connection::{
    compute_browser_control_panel_port, compute_browser_ports, render_cdp_url_for_ports,
};
use serde_json::Value;
use std::env;
use std::process::Command;

pub(super) struct ResolveOptions<'a> {
    pub share_url: Option<&'a str>,
    pub cdp_url: Option<&'a str>,
    pub control_port: Option<u16>,
    pub control_url: Option<&'a str>,
    pub target: &'a str,
}

pub(super) fn resolve_target(options: ResolveOptions<'_>) -> Result<BrowserTarget> {
    if let Some(url) = options.share_url {
        return Ok(BrowserTarget::shared(url));
    }
    if let Some(url) = options.cdp_url {
        return Ok(BrowserTarget::cdp(url));
    }

    let selector = options.target.trim();
    if selector.is_empty() {
        return Err(anyhow!("browser target must not be empty"));
    }

    if let Some(url) = configured_control_url(options.control_url) {
        let inventory = load_control_panel_inventory_url(&url)?;
        return target_from_inventory_for_selector(&inventory, selector)
            .with_context(|| format!("control panel {url} did not expose browser `{selector}`"));
    }

    if let Some(target) = target_from_env_endpoints(selector)? {
        return Ok(target);
    }

    if let Some(project) = project_id_from_env() {
        let port = options
            .control_port
            .unwrap_or_else(|| compute_browser_control_panel_port(&project));
        if let Ok(inventory) = load_control_panel_inventory(port) {
            if let Ok(target) = target_from_inventory_for_selector(&inventory, selector) {
                return Ok(target);
            }
        }
    }

    let port = options
        .control_port
        .unwrap_or_else(|| compute_browser_control_panel_port(selector));
    match load_control_panel_inventory(port) {
        Ok(inventory) => target_from_inventory(&inventory).with_context(|| {
            format!("control panel on port {port} did not expose an active browser")
        }),
        Err(_) => Ok(BrowserTarget::cdp(render_cdp_url_for_ports(
            compute_browser_ports(selector),
        ))),
    }
}

fn load_control_panel_inventory(port: u16) -> Result<Value> {
    let url = format!("http://127.0.0.1:{port}/api/browsers");
    load_control_panel_inventory_url(&url)
}

fn load_control_panel_inventory_url(url: &str) -> Result<Value> {
    let url = control_panel_inventory_url(url)?;
    let output = Command::new("curl")
        .args(["-fsS", url.as_str()])
        .output()
        .with_context(|| format!("failed to run curl for {url}"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "control panel request failed with status {}",
            output.status
        ));
    }
    serde_json::from_slice(&output.stdout).context("control panel inventory was not JSON")
}

fn target_from_inventory(inventory: &Value) -> Result<BrowserTarget> {
    target_from_inventory_for_selector(inventory, "active")
}

fn target_from_inventory_for_selector(inventory: &Value, selector: &str) -> Result<BrowserTarget> {
    let browser = browser_from_inventory(inventory, selector)?;
    target_from_browser_inventory(browser)
}

fn browser_from_inventory<'a>(inventory: &'a Value, selector: &str) -> Result<&'a Value> {
    let active = inventory
        .get("active")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("inventory did not include active browser"))?;
    let browsers = inventory
        .get("browsers")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("inventory did not include browsers"))?;
    let selector = selector.trim();
    if selector.eq_ignore_ascii_case("active") {
        return browsers
            .iter()
            .find(|browser| browser.get("active").and_then(Value::as_bool) == Some(true))
            .or_else(|| {
                browsers
                    .iter()
                    .find(|browser| browser.get("name").and_then(Value::as_str) == Some(active))
            })
            .ok_or_else(|| anyhow!("active browser `{active}` was not found in inventory"));
    }

    browsers
        .iter()
        .find(|browser| browser.get("name").and_then(Value::as_str) == Some(selector))
        .or_else(|| {
            selector.eq_ignore_ascii_case("chromium").then(|| {
                browsers
                    .iter()
                    .find(|browser| browser.get("name").and_then(Value::as_str) == Some("managed"))
            })?
        })
        .ok_or_else(|| {
            let names = browsers
                .iter()
                .filter_map(|browser| browser.get("name").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(", ");
            anyhow!("browser `{selector}` was not found in inventory; available: {names}")
        })
}

fn target_from_browser_inventory(browser: &Value) -> Result<BrowserTarget> {
    if let Some(share_url) = browser
        .get("shareUrl")
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
    {
        return Ok(BrowserTarget::shared(share_url));
    }
    if let Some(endpoint) = browser
        .get("cdpEndpoint")
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
    {
        return Ok(BrowserTarget::cdp(endpoint));
    }
    let name = browser
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("browser");
    Err(anyhow!(
        "browser `{name}` has neither shareUrl nor cdpEndpoint"
    ))
}

fn target_from_env_endpoints(selector: &str) -> Result<Option<BrowserTarget>> {
    let endpoints = browser_endpoints_from_env().context("failed to parse browser target env")?;
    let Some(endpoint) = endpoint_from_env_for_selector(&endpoints, selector) else {
        return Ok(None);
    };
    if let Some(share_url) = endpoint
        .share_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
    {
        return Ok(Some(BrowserTarget::shared(share_url)));
    }
    if !endpoint.cdp_endpoint.trim().is_empty() {
        return Ok(Some(BrowserTarget::cdp(&endpoint.cdp_endpoint)));
    }
    Ok(None)
}

fn endpoint_from_env_for_selector<'a>(
    endpoints: &'a [NamedBrowserEndpoint],
    selector: &str,
) -> Option<&'a NamedBrowserEndpoint> {
    let selector = selector.trim();
    if selector.eq_ignore_ascii_case("active") {
        return active_browser_from_env()
            .and_then(|active| endpoint_from_env_for_selector(endpoints, &active))
            .or_else(|| endpoints.first());
    }
    endpoints
        .iter()
        .find(|endpoint| endpoint.name == selector)
        .or_else(|| {
            selector
                .eq_ignore_ascii_case("chromium")
                .then(|| endpoints.iter().find(|endpoint| endpoint.name == "managed"))?
        })
}

fn configured_control_url(control_url: Option<&str>) -> Option<String> {
    control_url.and_then(nonempty_string).or_else(|| {
        env::var("BROWSER_CONNECTION_CONTROL_URL")
            .ok()
            .and_then(|value| nonempty_string(&value))
    })
}

fn control_panel_inventory_url(input: &str) -> Result<String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err(anyhow!("control URL must not be empty"));
    }
    let base = raw.trim_end_matches('/');
    if base.ends_with("/api/browsers") {
        return Ok(base.to_string());
    }
    Ok(format!("{base}/api/browsers"))
}

fn project_id_from_env() -> Option<String> {
    env::var("DOCKER_GIT_PROJECT_ID")
        .ok()
        .and_then(|value| nonempty_string(&value))
        .or_else(|| {
            env::var("PROJECT_ID")
                .ok()
                .and_then(|value| nonempty_string(&value))
        })
}

fn nonempty_string(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chooses_share_target_before_cdp_from_inventory() {
        let inventory = json!({
            "active": "edge",
            "browsers": [{
                "name": "edge",
                "active": true,
                "shareUrl": "https://relay.example/share/s#agent=a",
                "cdpEndpoint": "http://127.0.0.1:1"
            }]
        });
        let target = target_from_inventory(&inventory).unwrap();
        assert!(matches!(target, BrowserTarget::Shared { .. }));
    }

    #[test]
    fn selects_named_browser_even_when_inactive() {
        let inventory = json!({
            "active": "managed",
            "browsers": [
                {
                    "name": "managed",
                    "active": true,
                    "cdpEndpoint": "http://127.0.0.1:9223"
                },
                {
                    "name": "edge",
                    "active": false,
                    "shareUrl": "https://relay.example/share/s#agent=a"
                }
            ]
        });
        let target = target_from_inventory_for_selector(&inventory, "edge").unwrap();
        assert!(matches!(target, BrowserTarget::Shared { .. }));
    }

    #[test]
    fn chromium_alias_selects_managed_browser() {
        let inventory = json!({
            "active": "edge",
            "browsers": [
                {
                    "name": "edge",
                    "active": true,
                    "shareUrl": "https://relay.example/share/s#agent=a"
                },
                {
                    "name": "managed",
                    "active": false,
                    "cdpEndpoint": "http://127.0.0.1:9223"
                }
            ]
        });
        let target = target_from_inventory_for_selector(&inventory, "chromium").unwrap();
        assert_eq!(target, BrowserTarget::cdp("http://127.0.0.1:9223"));
    }

    #[test]
    fn active_selector_uses_inventory_active_browser() {
        let inventory = json!({
            "active": "edge",
            "browsers": [
                {
                    "name": "managed",
                    "active": false,
                    "cdpEndpoint": "http://127.0.0.1:9223"
                },
                {
                    "name": "edge",
                    "active": true,
                    "shareUrl": "https://relay.example/share/s#agent=a"
                }
            ]
        });
        let target = target_from_inventory_for_selector(&inventory, "active").unwrap();
        assert!(matches!(target, BrowserTarget::Shared { .. }));
    }
}
