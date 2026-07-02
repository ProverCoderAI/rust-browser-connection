use super::{
    activity::{self, BrowserActivityLog},
    McpServerConfig,
};
use crate::browser_target::{
    configured_browser_kind, normalize_browser_name, normalize_cdp_endpoint, normalize_novnc_url,
    normalize_share_url, normalize_vnc_endpoint, upsert_browser_endpoint, upsert_browser_metadata,
    upsert_browser_novnc_url, upsert_browser_share_url, upsert_browser_vnc_endpoint,
    BrowserEndpointMetadata, NamedBrowserEndpoint, EXPLICIT_BROWSER_NAME, MANAGED_BROWSER_NAME,
    PERSONAL_BROWSER_NAME,
};
use crate::shared_browser::SharedBrowserClient;
use crate::{
    compute_browser_ports, render_browser_control_panel_url_for_port,
    render_browser_target_novnc_url, render_cdp_url, render_cdp_url_for_ports, BrowserConnection,
};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct McpRuntime {
    pub(super) config: McpServerConfig,
    pub(super) managed_cdp_endpoint: Option<String>,
    pub(super) active_browser: String,
    pub(super) activity: BrowserActivityLog,
}

impl McpRuntime {
    pub(super) fn new(mut config: McpServerConfig) -> Result<Self> {
        load_persisted_runtime_config(&mut config);
        let explicit_endpoint = explicit_cdp_endpoint(&config)?;
        let active_browser = config.active_browser.clone().unwrap_or_else(|| {
            if explicit_endpoint.is_some() {
                EXPLICIT_BROWSER_NAME.to_string()
            } else {
                MANAGED_BROWSER_NAME.to_string()
            }
        });

        let runtime = Self {
            config,
            managed_cdp_endpoint: None,
            active_browser,
            activity: BrowserActivityLog::new(),
        };
        runtime.ensure_browser_exists(&runtime.active_browser)?;
        Ok(runtime)
    }

    pub(super) fn cdp_endpoint(&mut self) -> Result<String> {
        match self.active_browser.as_str() {
            MANAGED_BROWSER_NAME => self.managed_cdp_endpoint(),
            EXPLICIT_BROWSER_NAME => explicit_cdp_endpoint(&self.config)?
                .ok_or_else(|| anyhow!("explicit CDP endpoint is not configured")),
            name => self
                .config
                .browser_endpoints
                .iter()
                .find(|endpoint| endpoint.name == name)
                .and_then(|endpoint| {
                    (!endpoint.cdp_endpoint.trim().is_empty())
                        .then(|| endpoint.cdp_endpoint.clone())
                })
                .ok_or_else(|| {
                    anyhow!(
                        "Browser `{name}` does not have a CDP endpoint configured. Available browsers: {}",
                        self.available_browser_names().join(", ")
                    )
                }),
        }
    }

    pub(super) fn active_share_url(&self) -> Option<String> {
        let name = self.active_browser.as_str();
        if name == MANAGED_BROWSER_NAME || name == EXPLICIT_BROWSER_NAME {
            return None;
        }
        self.config
            .browser_endpoints
            .iter()
            .find(|endpoint| endpoint.name == name)
            .and_then(|endpoint| endpoint.share_url.clone())
    }

    fn active_browser_mode(&self) -> String {
        match self.active_browser.as_str() {
            MANAGED_BROWSER_NAME => "managed".to_string(),
            EXPLICIT_BROWSER_NAME => "explicit".to_string(),
            name => self
                .config
                .browser_endpoints
                .iter()
                .find(|endpoint| endpoint.name == name)
                .map(browser_endpoint_kind)
                .unwrap_or("unknown")
                .to_string(),
        }
    }

    pub(super) fn browser_activity(&mut self, refresh: bool) -> Value {
        if refresh {
            self.refresh_active_shared_tabs();
        }
        self.activity
            .snapshot(&self.active_browser, &self.active_browser_mode())
    }

    fn refresh_active_shared_tabs(&mut self) {
        let Some(share_url) = self.active_share_url() else {
            self.activity.clear_tabs();
            return;
        };
        match SharedBrowserClient::new(share_url).list_tabs() {
            Ok(tabs) => self.activity.record_tabs(tabs),
            Err(error) => self.activity.record_tab_error(error),
        }
    }

    pub(super) fn record_tool_activity(
        &mut self,
        tool: &str,
        arguments: &Value,
        started_at_ms: u64,
        result: &Result<String>,
    ) {
        let browser = self.active_browser.clone();
        let mode = self.active_browser_mode();
        self.activity
            .record_tool_result(&browser, &mode, tool, arguments, started_at_ms, result);

        let Ok(text) = result else {
            return;
        };
        if tool == "browser_list_tabs" {
            match serde_json::from_str::<Value>(text) {
                Ok(tabs) => self.activity.record_tabs(tabs),
                Err(error) => self.activity.record_tab_error(error),
            }
            return;
        }
        if self.active_share_url().is_some() && refresh_tabs_after_tool(tool) {
            self.refresh_active_shared_tabs();
        }
    }

    pub(super) fn activate_shared_tab_from_panel(&mut self, tab_id: i64) -> Result<Value> {
        let started_at_ms = activity::now_ms();
        let result = self
            .active_share_url()
            .ok_or_else(|| anyhow!("active browser is not a shared-extension browser"))
            .and_then(|share_url| SharedBrowserClient::new(share_url).activate_tab(tab_id));
        let arguments = json!({ "tab_id": tab_id, "source": "control_panel" });
        self.record_tool_activity("browser_activate_tab", &arguments, started_at_ms, &result);
        result
            .and_then(|text| {
                serde_json::from_str::<Value>(&text).or_else(|_| Ok(json!({ "result": text })))
            })
            .context("failed to activate shared browser tab")
    }

    pub(super) fn shared_recording_state_from_panel(&self) -> Result<Value> {
        let share_url = self
            .active_share_url()
            .ok_or_else(|| anyhow!("active browser is not a shared-extension browser"))?;
        SharedBrowserClient::new(share_url).recording_state()
    }

    pub(super) fn start_shared_recording_from_panel(&self) -> Result<Value> {
        let share_url = self
            .active_share_url()
            .ok_or_else(|| anyhow!("active browser is not a shared-extension browser"))?;
        SharedBrowserClient::new(share_url).start_recording()
    }

    pub(super) fn set_shared_recording_mode_from_panel(&self, mode: &str) -> Result<Value> {
        let share_url = self
            .active_share_url()
            .ok_or_else(|| anyhow!("active browser is not a shared-extension browser"))?;
        SharedBrowserClient::new(share_url).set_recording_mode(mode)
    }

    pub(super) fn stop_shared_recording_from_panel(&self) -> Result<Value> {
        let share_url = self
            .active_share_url()
            .ok_or_else(|| anyhow!("active browser is not a shared-extension browser"))?;
        SharedBrowserClient::new(share_url).stop_recording()
    }

    pub(super) fn clear_shared_recording_from_panel(&self) -> Result<Value> {
        let share_url = self
            .active_share_url()
            .ok_or_else(|| anyhow!("active browser is not a shared-extension browser"))?;
        SharedBrowserClient::new(share_url).clear_recording()
    }

    pub(super) fn play_shared_recording_from_panel(&self) -> Result<Value> {
        let share_url = self
            .active_share_url()
            .ok_or_else(|| anyhow!("active browser is not a shared-extension browser"))?;
        SharedBrowserClient::new(share_url).play_recording()
    }

    fn managed_cdp_endpoint(&mut self) -> Result<String> {
        if self.managed_cdp_endpoint.is_none() {
            self.managed_cdp_endpoint = Some(resolve_managed_cdp_endpoint(&self.config)?);
        }

        self.managed_cdp_endpoint
            .clone()
            .ok_or_else(|| anyhow!("managed CDP endpoint cache was empty after resolution"))
    }

    pub(super) fn browser_inventory_text(&self) -> Result<String> {
        serde_json::to_string_pretty(&self.browser_inventory()?)
            .context("failed to render browser inventory")
    }

    pub(super) fn browser_inventory(&self) -> Result<Value> {
        let mut browsers = vec![self.managed_browser_entry()];

        if let Some(endpoint) = explicit_cdp_endpoint(&self.config)? {
            browsers.push(browser_entry(
                EXPLICIT_BROWSER_NAME,
                "explicit",
                Some(endpoint),
                None,
                None,
                None,
                "configured",
                self.active_browser == EXPLICIT_BROWSER_NAME,
                None,
            ));
        }

        for endpoint in &self.config.browser_endpoints {
            browsers.push(browser_entry(
                &endpoint.name,
                browser_endpoint_kind(endpoint),
                (!endpoint.cdp_endpoint.trim().is_empty()).then(|| endpoint.cdp_endpoint.clone()),
                endpoint.vnc_endpoint.clone(),
                endpoint_novnc_url(&self.config, endpoint),
                endpoint.share_url.clone(),
                "configured",
                self.active_browser == endpoint.name,
                Some(&endpoint.metadata),
            ));
        }

        Ok(json!({
            "active": self.active_browser,
            "controlPanelUrl": control_panel_url(&self.config),
            "browsers": browsers
        }))
    }

    fn managed_browser_entry(&self) -> Value {
        let cached_endpoint = self.managed_cdp_endpoint.clone();
        let endpoint = cached_endpoint
            .clone()
            .or_else(|| (!self.config.start_browser).then(render_cdp_url))
            .or_else(|| {
                let ports = compute_browser_ports(&self.config.project_id);
                Some(render_cdp_url_for_ports(ports))
            });
        let resolution = if cached_endpoint.is_some() {
            "resolved"
        } else if self.config.start_browser {
            "auto-start"
        } else {
            "default-localhost"
        };

        browser_entry(
            MANAGED_BROWSER_NAME,
            "managed",
            endpoint,
            None,
            Some(managed_novnc_url(&self.config)),
            None,
            resolution,
            self.active_browser == MANAGED_BROWSER_NAME,
            None,
        )
    }

    pub(super) fn select_browser(
        &mut self,
        name: &str,
        cdp_endpoint: Option<&str>,
        vnc_endpoint: Option<&str>,
        novnc_url: Option<&str>,
        share_url: Option<&str>,
    ) -> Result<String> {
        self.select_browser_with_metadata(
            name,
            cdp_endpoint,
            vnc_endpoint,
            novnc_url,
            share_url,
            None,
        )
    }

    pub(super) fn register_shared_browser(
        &mut self,
        name: Option<&str>,
        share_url: &str,
        metadata: BrowserEndpointMetadata,
    ) -> Result<String> {
        let name = match name.map(str::trim).filter(|value| !value.is_empty()) {
            Some(name) => normalize_browser_name(name)?,
            None => self.shared_browser_name_for_metadata(&metadata)?,
        };
        self.select_browser_with_metadata(&name, None, None, None, Some(share_url), Some(metadata))
    }

    fn select_browser_with_metadata(
        &mut self,
        name: &str,
        cdp_endpoint: Option<&str>,
        vnc_endpoint: Option<&str>,
        novnc_url: Option<&str>,
        share_url: Option<&str>,
        metadata: Option<BrowserEndpointMetadata>,
    ) -> Result<String> {
        let name = normalize_browser_name(name)?;

        if let Some(endpoint) = cdp_endpoint {
            if name == MANAGED_BROWSER_NAME || name == EXPLICIT_BROWSER_NAME {
                return Err(anyhow!(
                    "`{name}` is reserved; choose a custom name such as `{PERSONAL_BROWSER_NAME}`"
                ));
            }
            let endpoint = NamedBrowserEndpoint::new(&name, endpoint)?;
            upsert_browser_endpoint(&mut self.config.browser_endpoints, endpoint);
        }
        if let Some(endpoint) = vnc_endpoint {
            if name == MANAGED_BROWSER_NAME || name == EXPLICIT_BROWSER_NAME {
                return Err(anyhow!(
                    "`{name}` is reserved; VNC metadata belongs to custom browser targets"
                ));
            }
            upsert_browser_vnc_endpoint(
                &mut self.config.browser_endpoints,
                name.clone(),
                normalize_vnc_endpoint(endpoint)?,
            );
        }
        if let Some(url) = novnc_url {
            if name == MANAGED_BROWSER_NAME || name == EXPLICIT_BROWSER_NAME {
                return Err(anyhow!(
                    "`{name}` is reserved; noVNC metadata belongs to custom browser targets"
                ));
            }
            upsert_browser_novnc_url(
                &mut self.config.browser_endpoints,
                name.clone(),
                normalize_novnc_url(url)?,
            );
        }
        if let Some(url) = share_url {
            if name == MANAGED_BROWSER_NAME || name == EXPLICIT_BROWSER_NAME {
                return Err(anyhow!(
                    "`{name}` is reserved; shared browser links belong to custom browser targets"
                ));
            }
            upsert_browser_share_url(
                &mut self.config.browser_endpoints,
                name.clone(),
                normalize_share_url(url)?,
            );
        }
        if let Some(metadata) = metadata {
            upsert_browser_metadata(&mut self.config.browser_endpoints, &name, &metadata);
        }

        self.ensure_browser_exists(&name)?;

        self.active_browser = name;
        self.ensure_active_display()?;
        self.persist_runtime_config()?;
        let inventory = self.browser_inventory()?;
        serde_json::to_string_pretty(&json!({
            "selected": self.active_browser,
            "browser": inventory
                .get("browsers")
                .and_then(Value::as_array)
                .and_then(|browsers| browsers.iter().find(|browser| {
                    browser.get("name").and_then(Value::as_str) == Some(self.active_browser.as_str())
                }))
                .cloned()
                .unwrap_or(Value::Null)
        }))
        .context("failed to render browser selection")
    }

    fn shared_browser_name_for_metadata(
        &self,
        metadata: &BrowserEndpointMetadata,
    ) -> Result<String> {
        if let Some(identity) = metadata.stable_identity() {
            if let Some(existing) = self
                .config
                .browser_endpoints
                .iter()
                .find(|endpoint| endpoint.metadata.stable_identity() == Some(identity))
            {
                return Ok(existing.name.clone());
            }
        }

        let owner = metadata
            .owner_label
            .as_deref()
            .or(metadata.owner_id.as_deref())
            .or(metadata.workspace_id.as_deref())
            .unwrap_or("user");
        let browser = metadata
            .browser_label
            .as_deref()
            .or(metadata.browser_kind.as_deref())
            .unwrap_or("browser");
        let device = metadata
            .device_label
            .as_deref()
            .or(metadata.platform.as_deref())
            .unwrap_or("device");
        let suffix = metadata
            .stable_identity()
            .map(short_identity)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "share".to_string());
        let base = slug_from_parts([owner, browser, device, suffix.as_str()]);

        for index in 0..1000 {
            let candidate = if index == 0 {
                base.clone()
            } else {
                format!("{base}-{}", index + 1)
            };
            if candidate == MANAGED_BROWSER_NAME || candidate == EXPLICIT_BROWSER_NAME {
                continue;
            }
            if self
                .config
                .browser_endpoints
                .iter()
                .all(|endpoint| endpoint.name != candidate)
            {
                return Ok(candidate);
            }
        }

        Err(anyhow!(
            "failed to allocate shared browser name for metadata"
        ))
    }

    fn ensure_browser_exists(&self, name: &str) -> Result<()> {
        if name == MANAGED_BROWSER_NAME {
            return Ok(());
        }
        if name == EXPLICIT_BROWSER_NAME {
            if explicit_cdp_endpoint(&self.config)?.is_some() {
                return Ok(());
            }
            return Err(anyhow!("explicit CDP endpoint is not configured"));
        }
        if let Some(endpoint) = self
            .config
            .browser_endpoints
            .iter()
            .find(|endpoint| endpoint.name == name)
        {
            if endpoint.cdp_endpoint.trim().is_empty() && endpoint.share_url.is_none() {
                return Err(anyhow!(
                    "Browser `{name}` does not have a CDP endpoint or share URL configured"
                ));
            }
            return Ok(());
        }

        Err(anyhow!(
            "Unknown browser `{name}`. Available browsers: {}",
            self.available_browser_names().join(", ")
        ))
    }

    fn persist_runtime_config(&self) -> Result<()> {
        if self.config.control_port.is_none() {
            return Ok(());
        }
        let state = PersistedRuntimeState {
            active_browser: Some(self.active_browser.clone()),
            browser_endpoints: self.config.browser_endpoints.clone(),
        };
        let path = runtime_state_path(&self.config.project_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let body = serde_json::to_vec_pretty(&state).context("failed to encode runtime state")?;
        fs::write(&path, body).with_context(|| format!("failed to write {}", path.display()))
    }

    pub(super) fn ensure_active_display(&mut self) -> Result<()> {
        if !self.config.start_browser {
            return Ok(());
        }

        let name = self.active_browser.clone();
        if name == MANAGED_BROWSER_NAME || name == EXPLICIT_BROWSER_NAME {
            return Ok(());
        }

        let Some(index) = self
            .config
            .browser_endpoints
            .iter()
            .position(|endpoint| endpoint.name == name)
        else {
            return Ok(());
        };
        let endpoint = self.config.browser_endpoints[index].clone();
        let Some(vnc_endpoint) = endpoint.vnc_endpoint else {
            return Ok(());
        };
        if endpoint.novnc_url.is_some() {
            return Ok(());
        }

        let connection = BrowserConnection::new()?;
        let info = connection.start_browser_target_display(
            &self.config.project_id,
            self.config.network.as_deref(),
            &name,
            &vnc_endpoint,
        )?;
        self.config.browser_endpoints[index].novnc_url = Some(info.novnc_url);
        Ok(())
    }

    fn available_browser_names(&self) -> Vec<String> {
        let mut names = vec![MANAGED_BROWSER_NAME.to_string()];
        if explicit_cdp_endpoint(&self.config).ok().flatten().is_some() {
            names.push(EXPLICIT_BROWSER_NAME.to_string());
        }
        names.extend(
            self.config
                .browser_endpoints
                .iter()
                .map(|endpoint| endpoint.name.clone()),
        );
        names
    }
}

fn resolve_managed_cdp_endpoint(config: &McpServerConfig) -> Result<String> {
    if !config.start_browser {
        return Ok(render_cdp_url());
    }

    let connection = BrowserConnection::new()?;
    let info = connection.start_browser(&config.project_id, config.network.as_deref())?;
    Ok(info.cdp_url)
}

fn explicit_cdp_endpoint(config: &McpServerConfig) -> Result<Option<String>> {
    config
        .cdp_endpoint
        .as_deref()
        .map(normalize_cdp_endpoint)
        .transpose()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedRuntimeState {
    active_browser: Option<String>,
    browser_endpoints: Vec<NamedBrowserEndpoint>,
}

fn load_persisted_runtime_config(config: &mut McpServerConfig) {
    if config.control_port.is_none() {
        return;
    }
    let path = runtime_state_path(&config.project_id);
    let Ok(body) = fs::read_to_string(&path) else {
        return;
    };
    let Ok(state) = serde_json::from_str::<PersistedRuntimeState>(&body) else {
        return;
    };
    for endpoint in state.browser_endpoints {
        if !config
            .browser_endpoints
            .iter()
            .any(|existing| existing.name == endpoint.name)
        {
            config.browser_endpoints.push(endpoint);
        }
    }
    if config.active_browser.is_none() {
        config.active_browser = state
            .active_browser
            .and_then(|name| normalize_browser_name(&name).ok());
    }
}

pub(super) fn runtime_state_path(project_id: &str) -> PathBuf {
    runtime_state_dir().join(format!("{}.json", runtime_state_file_stem(project_id)))
}

fn runtime_state_dir() -> PathBuf {
    env::var_os("BROWSER_CONNECTION_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir().join("browser-connection"))
}

fn runtime_state_file_stem(project_id: &str) -> String {
    let stem = project_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if stem.is_empty() {
        "default".to_string()
    } else {
        stem
    }
}

fn slug_from_parts<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let slug = parts
        .into_iter()
        .filter_map(slug_part)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "browser-share".to_string()
    } else {
        slug
    }
}

fn slug_part(value: &str) -> Option<String> {
    let mut output = String::new();
    let mut last_was_dash = false;
    for ch in value.trim().chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            output.push(ch);
            last_was_dash = false;
        } else if !last_was_dash && !output.is_empty() {
            output.push('-');
            last_was_dash = true;
        }
    }
    while output.ends_with('-') {
        output.pop();
    }
    if output.is_empty() {
        None
    } else {
        Some(output.chars().take(36).collect())
    }
}

fn short_identity(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .take(8)
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn browser_entry(
    name: &str,
    kind: &str,
    cdp_endpoint: Option<String>,
    vnc_endpoint: Option<String>,
    novnc_url: Option<String>,
    share_url: Option<String>,
    resolution: &str,
    active: bool,
    metadata: Option<&BrowserEndpointMetadata>,
) -> Value {
    let metadata = metadata.cloned().unwrap_or_default();
    let connected = cdp_endpoint
        .as_deref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
        || share_url
            .as_deref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
    json!({
        "name": name,
        "kind": kind,
        "cdpEndpoint": cdp_endpoint,
        "vncEndpoint": vnc_endpoint,
        "novncUrl": novnc_url,
        "shareUrl": share_url,
        "resolution": resolution,
        "status": resolution,
        "connected": connected,
        "active": active,
        "ownerId": metadata.owner_id,
        "ownerLabel": metadata.owner_label,
        "workspaceId": metadata.workspace_id,
        "poolId": metadata.pool_id,
        "installationId": metadata.installation_id,
        "deviceId": metadata.device_id,
        "deviceLabel": metadata.device_label,
        "browserKind": metadata.browser_kind,
        "browserLabel": metadata.browser_label,
        "platform": metadata.platform,
        "profileLabel": metadata.profile_label,
        "lastSeenAt": metadata.last_seen_at
    })
}

fn browser_endpoint_kind(endpoint: &NamedBrowserEndpoint) -> &'static str {
    if endpoint.share_url.is_some() && endpoint.cdp_endpoint.trim().is_empty() {
        "shared-extension"
    } else {
        configured_browser_kind(&endpoint.name)
    }
}

fn refresh_tabs_after_tool(tool: &str) -> bool {
    matches!(
        tool,
        "browser_navigate"
            | "browser_snapshot"
            | "browser_evaluate"
            | "browser_click"
            | "browser_type"
            | "browser_press_key"
            | "browser_take_screenshot"
            | "browser_activate_tab"
    )
}

fn managed_novnc_url(config: &McpServerConfig) -> String {
    if !config.start_browser {
        return crate::render_novnc_url();
    }

    let ports = compute_browser_ports(&config.project_id);
    crate::render_novnc_url_for_ports(ports)
}

fn endpoint_novnc_url(config: &McpServerConfig, endpoint: &NamedBrowserEndpoint) -> Option<String> {
    endpoint.novnc_url.clone().or_else(|| {
        endpoint
            .vnc_endpoint
            .as_ref()
            .map(|_| render_browser_target_novnc_url(&config.project_id, &endpoint.name))
    })
}

fn control_panel_url(config: &McpServerConfig) -> Option<String> {
    config
        .control_port
        .map(render_browser_control_panel_url_for_port)
}
