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
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
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
    let mut reader = reader;
    let mut runtime = McpRuntime::new(config);
    let mut transport = None;

    while let Some(message) = read_message(&mut reader, &mut transport)? {
        match handle_message(&mut runtime, &message) {
            Ok(Some(response)) => {
                let transport = transport
                    .ok_or_else(|| anyhow!("stdio transport was unknown after reading a message"))?;
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
    cdp_endpoint: Option<String>,
}

impl McpRuntime {
    fn new(config: McpServerConfig) -> Self {
        Self {
            config,
            cdp_endpoint: None,
        }
    }

    fn cdp_endpoint(&mut self) -> Result<&str> {
        if self.cdp_endpoint.is_none() {
            self.cdp_endpoint = Some(resolve_cdp_endpoint(&self.config)?);
        }

        self.cdp_endpoint
            .as_deref()
            .ok_or_else(|| anyhow!("CDP endpoint cache was empty after resolution"))
    }
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

fn handle_tool_call(runtime: &mut McpRuntime, request: &Value) -> Value {
    let params = request.get("params").unwrap_or(&Value::Null);
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").unwrap_or(&Value::Null);

    let result = runtime
        .cdp_endpoint()
        .and_then(|cdp_endpoint| dispatch_tool(cdp_endpoint, name, arguments));
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

    fn encode_message(value: Value) -> Vec<u8> {
        let body = serde_json::to_vec(&value).expect("message body serializes");
        let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        framed.extend_from_slice(&body);
        framed
    }

    fn decode_messages(bytes: &[u8]) -> Vec<Value> {
        let mut cursor = Cursor::new(bytes);
        let mut responses = Vec::new();

        let mut transport = Some(StdioTransport::Framed);
        while let Some(message) = read_message(&mut cursor, &mut transport).expect("stdout frame parses") {
            responses.push(serde_json::from_str(&message).expect("stdout frame body is JSON"));
        }

        responses
    }

    fn decode_line_messages(bytes: &[u8]) -> Vec<Value> {
        let text = String::from_utf8(bytes.to_vec()).expect("stdout lines are utf8");
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("stdout line is JSON"))
            .collect()
    }

    #[test]
    fn env_fallback_prefers_explicit_project() {
        assert_eq!(
            project_id_from_env_or_default(Some(" dg-x ".to_string())),
            "dg-x"
        );
    }

    #[test]
    fn stdio_initialize_and_list_use_framed_mcp_transport() {
        let config = McpServerConfig::new("dg-test", None, None, false);
        let mut input = Vec::new();
        input.extend(encode_message(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "probe", "version": "0" }
            }
        })));
        input.extend(encode_message(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        })));
        input.extend(encode_message(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        })));

        let input = Cursor::new(input);
        let mut output = Vec::new();

        run_stdio(config, input, &mut output).expect("stdio loop succeeds");

        let responses = decode_messages(&output);
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[0]["result"]["serverInfo"]["name"], SERVER_NAME);
        assert_eq!(responses[1]["id"], 2);
        assert!(responses[1]["result"]["tools"]
            .as_array()
            .expect("tools/list returns an array")
            .iter()
            .any(|tool| tool["name"] == "browser_navigate"));
    }

    #[test]
    fn stdio_initialize_and_list_use_line_delimited_transport() {
        let config = McpServerConfig::new("dg-test", None, None, false);
        let input = Cursor::new(
            [
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{"elicitation":{}},"clientInfo":{"name":"codex-mcp-client","version":"0"}}}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            ]
            .join("\n"),
        );
        let mut output = Vec::new();

        run_stdio(config, input, &mut output).expect("stdio loop succeeds");

        let responses = decode_line_messages(&output);
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(responses[1]["id"], 2);
        assert!(responses[1]["result"]["tools"]
            .as_array()
            .expect("tools/list returns an array")
            .iter()
            .any(|tool| tool["name"] == "browser_navigate"));
    }

    #[test]
    fn initialize_and_tools_list_do_not_resolve_cdp_endpoint() {
        let config = McpServerConfig::new("dg-test", None, None, true);
        let mut runtime = McpRuntime::new(config);

        handle_message(
            &mut runtime,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        )
        .expect("initialize succeeds");
        handle_message(
            &mut runtime,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        )
        .expect("tools/list succeeds");

        assert_eq!(runtime.cdp_endpoint, None);
    }

    #[test]
    fn initialize_negotiates_legacy_protocol_version() {
        let config = McpServerConfig::new("dg-test", None, None, false);
        let mut runtime = McpRuntime::new(config);

        let response = handle_message(
            &mut runtime,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}"#,
        )
        .expect("initialize succeeds")
        .expect("request with id returns a response");

        assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(response["result"]["capabilities"]["tools"]["listChanged"], false);
    }

    #[test]
    fn initialize_negotiates_latest_protocol_version() {
        let config = McpServerConfig::new("dg-test", None, None, false);
        let mut runtime = McpRuntime::new(config);

        let response = handle_message(
            &mut runtime,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{"elicitation":{}},"clientInfo":{"name":"codex-mcp-client","version":"0"}}}"#,
        )
        .expect("initialize succeeds")
        .expect("request with id returns a response");

        assert_eq!(response["result"]["protocolVersion"], "2025-06-18");
    }

    #[test]
    fn initialize_rejects_unknown_protocol_version() {
        let config = McpServerConfig::new("dg-test", None, None, false);
        let mut runtime = McpRuntime::new(config);

        let response = handle_message(
            &mut runtime,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2099-01-01","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}"#,
        )
        .expect("initialize response serializes")
        .expect("request with id returns a response");

        assert_eq!(response["error"]["code"], -32602);
        assert!(response["error"]["message"]
            .as_str()
            .expect("error message exists")
            .contains("Unsupported MCP protocol version"));
    }

    #[test]
    fn tools_call_resolves_cdp_endpoint_lazily() {
        let config = McpServerConfig::new("dg-test", None, None, false);
        let mut runtime = McpRuntime::new(config);
        let expected_cdp_url = render_cdp_url();

        let response = handle_message(
            &mut runtime,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"unknown","arguments":{}}}"#,
        )
        .expect("tools/call response serializes")
        .expect("request with id returns a response");

        assert_eq!(
            runtime.cdp_endpoint.as_deref(),
            Some(expected_cdp_url.as_str())
        );
        assert_eq!(response["id"], 3);
        assert_eq!(response["result"]["isError"], true);
        assert!(response["result"]["content"][0]["text"]
            .as_str()
            .expect("tool result text exists")
            .contains("Unknown browser-connection tool"));
    }
}
