/*! MCP stdio server for docker-git's custom Rust browser connection.

CHANGE: expose browser automation tools from the Rust crate as an MCP stdio server named `browser-connection`.
WHY: docker-git configs must replace external upstream Playwright MCP commands with a first-party Rust command.
QUOTE(ТЗ): "пусть называется browser-connection"
REF: https://github.com/ProverCoderAI/docker-git/issues/347
SOURCE: n/a
FORMAT THEOREM: initialize ∧ tools/list -> MCP-compatible JSON-RPC responses with browser tools.
PURITY: SHELL
EFFECT: stdio JSON-RPC and optional CDP/browser Docker startup.
INVARIANT: MCP startup resolves exactly one CDP endpoint from BrowserConnection or an explicit override.
*/

use crate::cdp::CdpClient;
use crate::{render_cdp_url, BrowserConnection};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::env;
use std::io::{BufRead, Write};

pub const SERVER_NAME: &str = "browser-connection";
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    pub project_id: String,
    pub network: Option<String>,
    pub cdp_endpoint: Option<String>,
    pub start_browser: bool,
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
        }
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
    let cdp_endpoint = resolve_cdp_endpoint(&config)?;

    for line in reader.lines() {
        let line = line.context("failed to read MCP stdin")?;
        if line.trim().is_empty() {
            continue;
        }

        match handle_message(&cdp_endpoint, &line) {
            Ok(Some(response)) => {
                writeln!(writer, "{response}").context("failed to write MCP stdout")?;
                writer.flush().context("failed to flush MCP stdout")?;
            }
            Ok(None) => {}
            Err(error) => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32603, "message": error.to_string() }
                });
                writeln!(writer, "{response}").context("failed to write MCP error")?;
                writer.flush().context("failed to flush MCP error")?;
            }
        }
    }

    Ok(())
}

fn resolve_cdp_endpoint(config: &McpServerConfig) -> Result<String> {
    if let Some(endpoint) = config
        .cdp_endpoint
        .as_deref()
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
    {
        return Ok(endpoint.to_string());
    }

    if !config.start_browser {
        return Ok(render_cdp_url());
    }

    let connection = BrowserConnection::new()?;
    let info = connection.start_browser(&config.project_id, config.network.as_deref())?;
    Ok(info.cdp_url)
}

fn handle_message(cdp_endpoint: &str, line: &str) -> Result<Option<Value>> {
    let request: Value = serde_json::from_str(line).context("MCP stdin line was not JSON")?;
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
        "initialize" => success_response(id, initialize_result()),
        "tools/list" => success_response(id, json!({ "tools": tool_definitions() })),
        "tools/call" => success_response(id, handle_tool_call(cdp_endpoint, &request)),
        _ => error_response(id, -32601, &format!("Unknown MCP method: {method}")),
    };

    Ok(Some(response))
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "browser_navigate",
            "Navigate the noVNC-visible Chromium page to a URL through the Rust CDP adapter.",
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

fn handle_tool_call(cdp_endpoint: &str, request: &Value) -> Value {
    let params = request.get("params").unwrap_or(&Value::Null);
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").unwrap_or(&Value::Null);

    let result = dispatch_tool(cdp_endpoint, name, arguments);
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
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn env_fallback_prefers_explicit_project() {
        assert_eq!(
            project_id_from_env_or_default(Some(" dg-x ".to_string())),
            "dg-x"
        );
    }

    #[test]
    fn stdio_initialize_is_docker_free_when_start_browser_is_disabled() {
        let config = McpServerConfig::new("dg-test", None, None, false);
        let input = Cursor::new(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
"#,
        );
        let mut output = Vec::new();

        run_stdio(config, input, &mut output).expect("stdio loop succeeds");

        let stdout = String::from_utf8(output).expect("stdout is utf8");
        assert!(stdout.contains("\"name\":\"browser-connection\""));
        assert!(stdout.contains("browser_navigate"));
        assert!(!stdout.contains("playwright/mcp"));
    }
}
