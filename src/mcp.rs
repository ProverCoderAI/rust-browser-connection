/*! MCP stdio server for docker-git's custom Rust browser connection.

CHANGE: expose browser automation tools from the Rust crate as an MCP stdio server named `browser-connection`.
WHY: docker-git configs must replace external upstream Playwright MCP commands with a first-party Rust command.
QUOTE(ТЗ): "пусть называется browser-connection"
REF: https://github.com/ProverCoderAI/docker-git/issues/347
SOURCE: n/a
FORMAT THEOREM: initialize ∧ tools/list -> MCP-compatible JSON-RPC responses with browser tools.
PURITY: SHELL
EFFECT: stdio JSON-RPC, optional CDP/browser Docker startup, and active browser target selection.
INVARIANT: browser tools always target the active CDP endpoint: managed, explicit, or configured.
*/

use crate::browser_target::{
    configured_browser_kind, normalize_browser_name, normalize_cdp_endpoint, normalize_novnc_url,
    normalize_share_url, normalize_vnc_endpoint, upsert_browser_endpoint, upsert_browser_novnc_url,
    upsert_browser_share_url, upsert_browser_vnc_endpoint,
};
use crate::cdp::CdpClient;
use crate::shared_browser::SharedBrowserClient;
use crate::{
    compute_browser_ports, render_browser_control_panel_url_for_port,
    render_browser_target_novnc_url, render_cdp_url, render_cdp_url_for_ports, BrowserConnection,
};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::env;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

mod control_panel;

pub use crate::browser_target::{
    active_browser_from_env, browser_endpoints_from_env, parse_named_browser_endpoint,
    parse_named_browser_endpoints, parse_named_browser_novnc_url, parse_named_browser_novnc_urls,
    parse_named_browser_share_url, parse_named_browser_share_urls,
    parse_named_browser_vnc_endpoint, parse_named_browser_vnc_endpoints, NamedBrowserEndpoint,
    EXPLICIT_BROWSER_NAME, MANAGED_BROWSER_NAME, PERSONAL_BROWSER_NAME,
};
pub const SERVER_NAME: &str = "browser-connection";
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
pub const PREVIOUS_MCP_PROTOCOL_VERSION: &str = "2025-06-18";
pub const LEGACY_MCP_PROTOCOL_VERSION: &str = "2025-03-26";
pub const OLDEST_MCP_PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdioTransport {
    Framed,
    LineDelimited,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    pub project_id: String,
    pub network: Option<String>,
    pub cdp_endpoint: Option<String>,
    pub start_browser: bool,
    pub browser_endpoints: Vec<NamedBrowserEndpoint>,
    pub active_browser: Option<String>,
    pub control_port: Option<u16>,
}

impl McpServerConfig {
    pub fn new(
        project_id: impl Into<String>,
        network: Option<String>,
        cdp_endpoint: Option<String>,
        start_browser: bool,
    ) -> Self {
        Self {
            project_id: normalize_project_id(project_id.into()),
            network,
            cdp_endpoint,
            start_browser,
            browser_endpoints: Vec::new(),
            active_browser: None,
            control_port: None,
        }
    }

    pub fn with_browser_endpoints(mut self, endpoints: Vec<NamedBrowserEndpoint>) -> Self {
        for endpoint in endpoints {
            upsert_browser_endpoint(&mut self.browser_endpoints, endpoint);
        }
        self
    }

    pub fn with_browser_vnc_endpoint(
        mut self,
        name: impl AsRef<str>,
        vnc_endpoint: impl AsRef<str>,
    ) -> Result<Self> {
        let name = normalize_configured_browser_name_for_display(name.as_ref())?;
        upsert_browser_vnc_endpoint(
            &mut self.browser_endpoints,
            name,
            normalize_vnc_endpoint(vnc_endpoint.as_ref())?,
        );
        Ok(self)
    }

    pub fn with_browser_novnc_url(
        mut self,
        name: impl AsRef<str>,
        novnc_url: impl AsRef<str>,
    ) -> Result<Self> {
        let name = normalize_configured_browser_name_for_display(name.as_ref())?;
        upsert_browser_novnc_url(
            &mut self.browser_endpoints,
            name,
            normalize_novnc_url(novnc_url.as_ref())?,
        );
        Ok(self)
    }

    pub fn with_browser_share_url(
        mut self,
        name: impl AsRef<str>,
        share_url: impl AsRef<str>,
    ) -> Result<Self> {
        let name = normalize_configured_browser_name_for_display(name.as_ref())?;
        upsert_browser_share_url(
            &mut self.browser_endpoints,
            name,
            normalize_share_url(share_url.as_ref())?,
        );
        Ok(self)
    }

    pub fn with_active_browser(mut self, active_browser: Option<String>) -> Result<Self> {
        self.active_browser = active_browser
            .as_deref()
            .map(normalize_browser_name)
            .transpose()?;
        Ok(self)
    }

    pub fn with_control_port(mut self, control_port: Option<u16>) -> Self {
        self.control_port = control_port;
        self
    }
}

pub fn project_id_from_env_or_default(project_id: Option<String>) -> String {
    project_id
        .or_else(|| env::var("DOCKER_GIT_PROJECT_ID").ok())
        .or_else(|| env::var("PROJECT_ID").ok())
        .map(normalize_project_id)
        .unwrap_or_else(|| "default".to_string())
}

pub fn run_stdio<R, W>(config: McpServerConfig, reader: R, mut writer: W) -> Result<()>
where
    R: BufRead,
    W: Write,
{
    let mut reader = reader;
    let runtime = Arc::new(Mutex::new(McpRuntime::new(config)?));
    control_panel::spawn_control_panel(Arc::clone(&runtime))?;
    let mut transport = None;

    while let Some(message) = read_message(&mut reader, &mut transport)? {
        let result = {
            let mut runtime = runtime
                .lock()
                .map_err(|_| anyhow!("MCP runtime lock was poisoned"))?;
            handle_message(&mut runtime, &message)
        };
        match result {
            Ok(Some(response)) => {
                let transport = transport.ok_or_else(|| {
                    anyhow!("stdio transport was unknown after reading a message")
                })?;
                write_message(&mut writer, &response, transport)?;
            }
            Ok(None) => {}
            Err(error) => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32603, "message": error.to_string() }
                });
                let transport = transport.unwrap_or(StdioTransport::Framed);
                write_message(&mut writer, &response, transport)?;
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct McpRuntime {
    config: McpServerConfig,
    managed_cdp_endpoint: Option<String>,
    active_browser: String,
}

impl McpRuntime {
    fn new(config: McpServerConfig) -> Result<Self> {
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
        };
        runtime.ensure_browser_exists(&runtime.active_browser)?;
        Ok(runtime)
    }

    fn cdp_endpoint(&mut self) -> Result<String> {
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

    fn active_share_url(&self) -> Option<String> {
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

    fn managed_cdp_endpoint(&mut self) -> Result<String> {
        if self.managed_cdp_endpoint.is_none() {
            self.managed_cdp_endpoint = Some(resolve_managed_cdp_endpoint(&self.config)?);
        }

        self.managed_cdp_endpoint
            .clone()
            .ok_or_else(|| anyhow!("managed CDP endpoint cache was empty after resolution"))
    }

    fn browser_inventory_text(&self) -> Result<String> {
        serde_json::to_string_pretty(&self.browser_inventory()?)
            .context("failed to render browser inventory")
    }

    fn browser_inventory(&self) -> Result<Value> {
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
        )
    }

    fn select_browser(
        &mut self,
        name: &str,
        cdp_endpoint: Option<&str>,
        vnc_endpoint: Option<&str>,
        novnc_url: Option<&str>,
        share_url: Option<&str>,
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

        self.ensure_browser_exists(&name)?;

        self.active_browser = name;
        self.ensure_active_display()?;
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

    fn ensure_active_display(&mut self) -> Result<()> {
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
) -> Value {
    json!({
        "name": name,
        "kind": kind,
        "cdpEndpoint": cdp_endpoint,
        "vncEndpoint": vnc_endpoint,
        "novncUrl": novnc_url,
        "shareUrl": share_url,
        "resolution": resolution,
        "active": active
    })
}

fn browser_endpoint_kind(endpoint: &NamedBrowserEndpoint) -> &'static str {
    if endpoint.share_url.is_some() && endpoint.cdp_endpoint.trim().is_empty() {
        "shared-extension"
    } else {
        configured_browser_kind(&endpoint.name)
    }
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

fn normalize_configured_browser_name_for_display(name: &str) -> Result<String> {
    let name = normalize_browser_name(name)?;
    if name == MANAGED_BROWSER_NAME || name == EXPLICIT_BROWSER_NAME {
        return Err(anyhow!(
            "`{name}` is reserved and cannot name a configured browser"
        ));
    }
    Ok(name)
}

fn read_message<R: BufRead>(
    reader: &mut R,
    transport: &mut Option<StdioTransport>,
) -> Result<Option<String>> {
    if transport.is_none() {
        *transport = detect_transport(reader)?;
    }

    let Some(transport) = transport else {
        return Ok(None);
    };

    match transport {
        StdioTransport::Framed => read_framed_message(reader),
        StdioTransport::LineDelimited => read_line_message(reader),
    }
}

fn detect_transport<R: BufRead>(reader: &mut R) -> Result<Option<StdioTransport>> {
    loop {
        let buffer = reader
            .fill_buf()
            .context("failed to inspect MCP stdin for transport detection")?;
        let Some(first_byte) = buffer.first().copied() else {
            return Ok(None);
        };

        match first_byte {
            b'{' | b'[' => return Ok(Some(StdioTransport::LineDelimited)),
            b'C' | b'c' => return Ok(Some(StdioTransport::Framed)),
            b' ' | b'\t' | b'\r' | b'\n' => reader.consume(1),
            _ => {
                return Err(anyhow!(
                    "MCP stdin transport was not recognized from first byte: {first_byte}"
                ))
            }
        }
    }
}

fn read_framed_message<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let content_length = read_content_length(reader)?;
    let Some(content_length) = content_length else {
        return Ok(None);
    };

    let mut body = vec![0_u8; content_length];
    reader
        .read_exact(&mut body)
        .context("failed to read MCP stdin body")?;

    String::from_utf8(body)
        .map(Some)
        .context("MCP stdin body was not utf8")
}

fn read_line_message<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .context("failed to read MCP stdin line")?;
        if read == 0 {
            return Ok(None);
        }

        let message = line.trim_end_matches(&['\r', '\n'][..]).trim();
        if !message.is_empty() {
            return Ok(Some(message.to_string()));
        }
    }
}

fn read_content_length<R: BufRead>(reader: &mut R) -> Result<Option<usize>> {
    let mut content_length = None;
    let mut saw_header = false;

    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .context("failed to read MCP stdin header")?;
        if read == 0 {
            return if saw_header {
                Err(anyhow!("MCP stdin closed before header terminator"))
            } else {
                Ok(None)
            };
        }

        let header = line.trim_end_matches(&['\r', '\n'][..]);
        if header.is_empty() {
            if saw_header {
                break;
            }
            continue;
        }
        saw_header = true;

        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| anyhow!("MCP stdin header was malformed"))?;

        if name.eq_ignore_ascii_case("Content-Length") {
            if content_length.is_some() {
                return Err(anyhow!("MCP stdin declared Content-Length more than once"));
            }
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .context("MCP stdin Content-Length was not a valid usize")?,
            );
        }
    }

    content_length
        .map(Some)
        .ok_or_else(|| anyhow!("MCP stdin header was missing Content-Length"))
}

fn write_message<W: Write>(
    writer: &mut W,
    response: &Value,
    transport: StdioTransport,
) -> Result<()> {
    match transport {
        StdioTransport::Framed => {
            let body = serde_json::to_vec(response).context("failed to encode MCP stdout JSON")?;
            write!(writer, "Content-Length: {}\r\n\r\n", body.len())
                .context("failed to write MCP stdout header")?;
            writer
                .write_all(&body)
                .context("failed to write MCP stdout body")?;
        }
        StdioTransport::LineDelimited => {
            serde_json::to_writer(&mut *writer, response)
                .context("failed to encode MCP stdout JSON")?;
            writer
                .write_all(b"\n")
                .context("failed to write MCP stdout line terminator")?;
        }
    }
    writer.flush().context("failed to flush MCP stdout")?;
    Ok(())
}

fn handle_message(runtime: &mut McpRuntime, message: &str) -> Result<Option<Value>> {
    let request: Value = serde_json::from_str(message).context("MCP stdin body was not JSON")?;
    let id = request.get("id").cloned();
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("MCP request did not include method"))?;

    if id.is_none() {
        return Ok(None);
    }
    let id = id.unwrap_or(Value::Null);

    let response = match method {
        "initialize" => match requested_protocol_version(&request) {
            Ok(protocol_version) => success_response(id, initialize_result(protocol_version)),
            Err(error) => error_response(id, -32602, &error.to_string()),
        },
        "tools/list" => success_response(id, json!({ "tools": tool_definitions() })),
        "tools/call" => success_response(id, handle_tool_call(runtime, &request)),
        _ => error_response(id, -32601, &format!("Unknown MCP method: {method}")),
    };

    Ok(Some(response))
}

fn requested_protocol_version(request: &Value) -> Result<&'static str> {
    let requested = request
        .get("params")
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or(MCP_PROTOCOL_VERSION);

    match requested {
        MCP_PROTOCOL_VERSION => Ok(MCP_PROTOCOL_VERSION),
        PREVIOUS_MCP_PROTOCOL_VERSION => Ok(PREVIOUS_MCP_PROTOCOL_VERSION),
        LEGACY_MCP_PROTOCOL_VERSION => Ok(LEGACY_MCP_PROTOCOL_VERSION),
        OLDEST_MCP_PROTOCOL_VERSION => Ok(OLDEST_MCP_PROTOCOL_VERSION),
        _ => Err(anyhow!("Unsupported MCP protocol version: {requested}")),
    }
}

fn initialize_result(protocol_version: &str) -> Value {
    json!({
        "protocolVersion": protocol_version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "browser_list",
            "List available browser targets and show which target is active.",
            json!({}),
            vec![],
        ),
        tool(
            "browser_select",
            "Switch the active browser target by name, optionally registering CDP and display endpoints first.",
            json!({
                "name": { "type": "string", "description": "Browser target name, e.g. managed or personal" },
                "cdp_endpoint": { "type": "string", "description": "Optional CDP endpoint to register for this browser name" },
                "vnc_endpoint": { "type": "string", "description": "Optional VNC endpoint to register as HOST:PORT" },
                "novnc_url": { "type": "string", "description": "Optional pre-existing noVNC URL for this browser name" },
                "share_url": { "type": "string", "description": "Optional shared browser extension link for this browser name" }
            }),
            vec!["name"],
        ),
        tool(
            "browser_navigate",
            "Navigate the active browser page to a URL through the Rust CDP adapter.",
            json!({ "url": { "type": "string", "description": "Absolute URL to open" } }),
            vec!["url"],
        ),
        tool(
            "browser_snapshot",
            "Return page title, URL, visible text and simple interactive element selectors.",
            json!({}),
            vec![],
        ),
        tool(
            "browser_evaluate",
            "Evaluate JavaScript in the current page and return a JSON/text result.",
            json!({ "expression": { "type": "string", "description": "JavaScript expression" } }),
            vec!["expression"],
        ),
        tool(
            "browser_click",
            "Click an element by CSS selector in the current page.",
            json!({ "selector": { "type": "string", "description": "CSS selector" } }),
            vec!["selector"],
        ),
        tool(
            "browser_type",
            "Set text in an input-like element by CSS selector and dispatch input/change events.",
            json!({
                "selector": { "type": "string", "description": "CSS selector" },
                "text": { "type": "string", "description": "Text to type" }
            }),
            vec!["selector", "text"],
        ),
        tool(
            "browser_press_key",
            "Press a key through CDP Input.dispatchKeyEvent.",
            json!({ "key": { "type": "string", "description": "Key name, e.g. Enter or a" } }),
            vec!["key"],
        ),
        tool(
            "browser_take_screenshot",
            "Capture a PNG screenshot and return it as a data URL.",
            json!({ "full_page": { "type": "boolean", "description": "Capture beyond viewport", "default": true } }),
            vec![],
        ),
    ]
}

fn tool(name: &str, description: &str, properties: Value, required: Vec<&str>) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required
        }
    })
}

fn handle_tool_call(runtime: &mut McpRuntime, request: &Value) -> Value {
    let params = request.get("params").unwrap_or(&Value::Null);
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").unwrap_or(&Value::Null);

    let result = match name {
        "browser_list" => runtime.browser_inventory_text(),
        "browser_select" => runtime.select_browser(
            required_str(arguments, "name").unwrap_or(""),
            arguments.get("cdp_endpoint").and_then(Value::as_str),
            arguments.get("vnc_endpoint").and_then(Value::as_str),
            arguments.get("novnc_url").and_then(Value::as_str),
            arguments.get("share_url").and_then(Value::as_str),
        ),
        _ => runtime.ensure_active_display().and_then(|_| {
            if let Some(share_url) = runtime.active_share_url() {
                dispatch_shared_tool(&share_url, name, arguments)
            } else {
                runtime
                    .cdp_endpoint()
                    .and_then(|cdp_endpoint| dispatch_tool(&cdp_endpoint, name, arguments))
            }
        }),
    };
    match result {
        Ok(text) => tool_result(text, false),
        Err(error) => tool_result(format!("{error:#}"), true),
    }
}

fn dispatch_tool(cdp_endpoint: &str, name: &str, arguments: &Value) -> Result<String> {
    let client = CdpClient::new(cdp_endpoint);
    match name {
        "browser_navigate" => client.navigate(required_str(arguments, "url")?),
        "browser_snapshot" => client.snapshot(),
        "browser_evaluate" => client.evaluate(required_str(arguments, "expression")?),
        "browser_click" => client.click(required_str(arguments, "selector")?),
        "browser_type" => client.type_text(
            required_str(arguments, "selector")?,
            required_str(arguments, "text")?,
        ),
        "browser_press_key" => client.press_key(required_str(arguments, "key")?),
        "browser_take_screenshot" => {
            let full_page = arguments
                .get("full_page")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            client.screenshot(full_page)
        }
        "" => Err(anyhow!("tools/call params.name is required")),
        _ => Err(anyhow!("Unknown browser-connection tool: {name}")),
    }
}

fn dispatch_shared_tool(share_url: &str, name: &str, arguments: &Value) -> Result<String> {
    let client = SharedBrowserClient::new(share_url);
    match name {
        "browser_navigate" => client.navigate(required_str(arguments, "url")?),
        "browser_snapshot" => client.snapshot(),
        "browser_evaluate" => client.evaluate(required_str(arguments, "expression")?),
        "browser_click" => client.click(required_str(arguments, "selector")?),
        "browser_type" => client.type_text(
            required_str(arguments, "selector")?,
            required_str(arguments, "text")?,
        ),
        "browser_press_key" => client.press_key(required_str(arguments, "key")?),
        "browser_take_screenshot" => {
            let full_page = arguments
                .get("full_page")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            client.screenshot(full_page)
        }
        "" => Err(anyhow!("tools/call params.name is required")),
        _ => Err(anyhow!("Unknown browser-connection tool: {name}")),
    }
}

fn required_str<'a>(arguments: &'a Value, name: &str) -> Result<&'a str> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("argument `{name}` is required"))
}

fn tool_result(text: String, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error
    })
}

fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn normalize_project_id(project_id: String) -> String {
    let trimmed = project_id.trim();
    if trimmed.is_empty() {
        "default".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests;
