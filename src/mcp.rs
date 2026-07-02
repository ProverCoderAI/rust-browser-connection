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
    normalize_browser_name, normalize_novnc_url, normalize_share_url, normalize_vnc_endpoint,
    upsert_browser_endpoint, upsert_browser_novnc_url, upsert_browser_share_url,
    upsert_browser_vnc_endpoint,
};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::env;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

mod activity;
mod control_panel;
mod protocol;
mod runtime;
mod tools;

use protocol::{handle_message, read_message, write_message, StdioTransport};
#[cfg(test)]
use runtime::runtime_state_path;
use runtime::McpRuntime;

pub use crate::browser_target::{
    active_browser_from_env, browser_endpoints_from_env, parse_named_browser_endpoint,
    parse_named_browser_endpoints, parse_named_browser_novnc_url, parse_named_browser_novnc_urls,
    parse_named_browser_share_url, parse_named_browser_share_urls,
    parse_named_browser_vnc_endpoint, parse_named_browser_vnc_endpoints, BrowserEndpointMetadata,
    NamedBrowserEndpoint, EXPLICIT_BROWSER_NAME, MANAGED_BROWSER_NAME, PERSONAL_BROWSER_NAME,
};
pub const SERVER_NAME: &str = "browser-connection";
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
pub const PREVIOUS_MCP_PROTOCOL_VERSION: &str = "2025-06-18";
pub const LEGACY_MCP_PROTOCOL_VERSION: &str = "2025-03-26";
pub const OLDEST_MCP_PROTOCOL_VERSION: &str = "2024-11-05";

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

fn normalize_configured_browser_name_for_display(name: &str) -> Result<String> {
    let name = normalize_browser_name(name)?;
    if name == MANAGED_BROWSER_NAME || name == EXPLICIT_BROWSER_NAME {
        return Err(anyhow!(
            "`{name}` is reserved and cannot name a configured browser"
        ));
    }
    Ok(name)
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
