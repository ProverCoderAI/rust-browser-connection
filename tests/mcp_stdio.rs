use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

fn encode_message(value: Value) -> Vec<u8> {
    let body = serde_json::to_vec(&value).expect("message body serializes");
    let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    framed.extend_from_slice(&body);
    framed
}

fn decode_messages(mut stdout: &[u8]) -> Vec<Value> {
    let mut responses = Vec::new();

    while !stdout.is_empty() {
        let split = stdout
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("frame separator present");
        let (header, rest) = stdout.split_at(split);
        let body_start = &rest[4..];
        let header_text = String::from_utf8(header.to_vec()).expect("header is utf8");
        let content_length = header_text
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    if name.eq_ignore_ascii_case("Content-Length") {
                        value.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
            })
            .expect("Content-Length header present");
        let (body, remaining) = body_start.split_at(content_length);
        responses.push(serde_json::from_slice(body).expect("frame body is JSON"));
        stdout = remaining;
    }

    responses
}

fn decode_line_messages(stdout: &[u8]) -> Vec<Value> {
    let text = String::from_utf8(stdout.to_vec()).expect("line output is utf8");
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("line output is JSON"))
        .collect()
}

fn unused_local_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind an unused local port");
    listener
        .local_addr()
        .expect("read local listener address")
        .port()
}

fn http_request(port: u16, request: &str) -> String {
    let mut last_error = None;
    for _ in 0..50 {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(mut stream) => {
                stream
                    .write_all(request.as_bytes())
                    .expect("write HTTP request");
                let mut response = String::new();
                stream
                    .read_to_string(&mut response)
                    .expect("read HTTP response");
                return response;
            }
            Err(error) => {
                last_error = Some(error);
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
    panic!("control panel did not accept connections: {last_error:?}");
}

fn websocket_connect(url: &str) -> WebSocket<MaybeTlsStream<TcpStream>> {
    let mut last_error = None;
    for _ in 0..50 {
        match connect(url) {
            Ok((socket, _response)) => return socket,
            Err(error) => {
                last_error = Some(error);
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
    panic!("control panel websocket did not accept connections: {last_error:?}");
}

fn control_panel_token(port: u16) -> String {
    let response = http_request(
        port,
        "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    assert!(response.contains("200 OK"), "{response}");
    let marker = "const controlToken = \"";
    let token = response
        .split_once(marker)
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(token, _)| token)
        .expect("control token is embedded in panel HTML");
    token.to_string()
}

fn initialize_request(id: u64) -> Vec<u8> {
    encode_message(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "probe", "version": "0" }
        }
    }))
}

#[test]
fn browser_connection_help_exposes_custom_mcp_command_without_npx() {
    let output = Command::new(env!("CARGO_BIN_EXE_browser-connection"))
        .arg("--help")
        .output()
        .expect("Failed to execute browser-connection --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("browser-connection"));
    assert!(stdout.contains("--project"));
    assert!(stdout.contains("--browser"));
    assert!(stdout.contains("--browser-vnc"));
    assert!(stdout.contains("--browser-novnc"));
    assert!(stdout.contains("--browser-share"));
    assert!(stdout.contains("--personal-browser"));
    assert!(stdout.contains("--personal-vnc"));
    assert!(stdout.contains("--personal-novnc"));
    assert!(stdout.contains("--active-browser"));
    assert!(stdout.contains("--control-port"));
    assert!(stdout.contains("--no-control-panel"));
    assert!(stdout.contains("--no-start-browser"));
    assert!(!stdout.contains("playwright/mcp"));
    assert!(!stdout.contains("npx"));
}

#[test]
fn browser_connection_stdio_initializes_and_lists_browser_tools_without_docker() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_browser-connection"))
        .args([
            "--project",
            "dg-test",
            "--no-start-browser",
            "--no-control-panel",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn browser-connection MCP server");

    {
        let stdin = child.stdin.as_mut().expect("stdin is piped");
        let mut input = Vec::new();
        input.extend(encode_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "probe", "version": "0" }
            }
        })));
        input.extend(encode_message(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        })));
        input.extend(encode_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        })));
        stdin
            .write_all(&input)
            .expect("write MCP handshake requests");
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("browser-connection process exits after stdin EOF");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let responses = decode_messages(&output.stdout);

    assert_eq!(
        responses.len(),
        2,
        "stdout was: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(
        responses[0]["result"]["serverInfo"]["name"],
        "browser-connection"
    );
    assert_eq!(responses[1]["id"], 2);

    let tools = responses[1]["result"]["tools"]
        .as_array()
        .expect("tools/list returns an array");
    let names = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();

    assert!(names.contains(&"browser_navigate"));
    assert!(names.contains(&"browser_snapshot"));
    assert!(names.contains(&"browser_evaluate"));
    assert!(names.contains(&"browser_click"));
    assert!(names.contains(&"browser_type"));
    assert!(names.contains(&"browser_press_key"));
    assert!(names.contains(&"browser_take_screenshot"));
    assert!(names.contains(&"browser_list"));
    assert!(names.contains(&"browser_select"));
}

#[test]
fn browser_connection_stdio_selects_personal_browser_without_docker() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_browser-connection"))
        .args([
            "--project",
            "dg-test",
            "--no-start-browser",
            "--no-control-panel",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn browser-connection MCP server");

    {
        let stdin = child.stdin.as_mut().expect("stdin is piped");
        let mut input = Vec::new();
        input.extend(encode_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "probe", "version": "0" }
            }
        })));
        input.extend(encode_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "browser_select",
                "arguments": {
                    "name": "personal",
                    "cdp_endpoint": "http://127.0.0.1:9444/json/version",
                    "vnc_endpoint": "host.docker.internal:5900",
                    "novnc_url": "http://127.0.0.1:6680/vnc.html"
                }
            }
        })));
        input.extend(encode_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "browser_list",
                "arguments": {}
            }
        })));
        stdin
            .write_all(&input)
            .expect("write MCP browser selection requests");
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("browser-connection process exits after stdin EOF");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let responses = decode_messages(&output.stdout);
    assert_eq!(responses.len(), 3);
    assert_eq!(responses[1]["result"]["isError"], false);
    let inventory_text = responses[2]["result"]["content"][0]["text"]
        .as_str()
        .expect("browser_list returns text content");
    let inventory: Value = serde_json::from_str(inventory_text).expect("browser_list text is JSON");

    assert_eq!(inventory["active"], "personal");
    assert!(inventory["browsers"]
        .as_array()
        .expect("browsers array")
        .iter()
        .any(|browser| {
            browser["name"] == "personal"
                && browser["cdpEndpoint"] == "http://127.0.0.1:9444"
                && browser["vncEndpoint"] == "host.docker.internal:5900"
                && browser["novncUrl"] == "http://127.0.0.1:6680/vnc.html"
                && browser["active"] == true
        }));
}

#[test]
fn control_panel_selection_updates_mcp_browser_list_without_restart() {
    let port = unused_local_port();
    let port_arg = port.to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_browser-connection"))
        .args([
            "--project",
            "dg-test-control-panel",
            "--no-start-browser",
            "--control-port",
            &port_arg,
            "--browser",
            "personal=http://127.0.0.1:9444",
            "--browser-novnc",
            "personal=http://127.0.0.1:6680/vnc.html",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn browser-connection MCP server");

    {
        let stdin = child.stdin.as_mut().expect("stdin is piped");
        stdin
            .write_all(&encode_message(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "probe", "version": "0" }
                }
            })))
            .expect("write MCP initialize request");

        let token = control_panel_token(port);
        let response = http_request(
            port,
            &format!(
                "POST /api/select?name=personal HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Browser-Control-Token: {token}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ),
        );
        assert!(response.contains("200 OK"), "{response}");
        assert!(
            response.contains("\"selected\": \"personal\""),
            "{response}"
        );

        stdin
            .write_all(&encode_message(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "browser_list",
                    "arguments": {}
                }
            })))
            .expect("write MCP browser_list request");
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("browser-connection process exits after stdin EOF");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let responses = decode_messages(&output.stdout);
    assert_eq!(responses.len(), 2);
    let inventory_text = responses[1]["result"]["content"][0]["text"]
        .as_str()
        .expect("browser_list returns text content");
    let inventory: Value = serde_json::from_str(inventory_text).expect("browser_list text is JSON");

    assert_eq!(inventory["active"], "personal");
    assert_eq!(
        inventory["controlPanelUrl"],
        format!("http://127.0.0.1:{port}/")
    );
}

#[test]
fn control_panel_embeds_browser_share_relay() {
    let port = unused_local_port();
    let port_arg = port.to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_browser-connection"))
        .args([
            "--project",
            "dg-test-embedded-relay",
            "--no-start-browser",
            "--control-port",
            &port_arg,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn browser-connection MCP server");

    {
        let stdin = child.stdin.as_mut().expect("stdin is piped");
        stdin
            .write_all(&initialize_request(1))
            .expect("write MCP initialize request");

        let browser_url = format!(
            "ws://127.0.0.1:{port}/ws/browser/session-1?token=browser-token&agent_token=agent-token"
        );
        let agent_url = format!("ws://127.0.0.1:{port}/ws/agent/session-1?token=agent-token");
        let mut browser = websocket_connect(&browser_url);
        let mut agent = websocket_connect(&agent_url);

        agent
            .send(Message::Text(r#"{"id":"1","method":"ping"}"#.to_string()))
            .expect("send agent command");
        let forwarded_to_browser = browser
            .read()
            .expect("browser receives command")
            .to_text()
            .expect("command is text")
            .to_string();
        let forwarded_json: Value =
            serde_json::from_str(&forwarded_to_browser).expect("forwarded command is JSON");
        let relay_id = forwarded_json["id"]
            .as_str()
            .expect("relay request id is a string")
            .to_string();
        assert!(relay_id.ends_with(":\"1\""));
        assert_eq!(forwarded_json["method"], "ping");

        browser
            .send(Message::Text(
                json!({ "id": relay_id, "result": "pong" }).to_string(),
            ))
            .expect("send browser response");
        let forwarded_to_agent = agent
            .read()
            .expect("agent receives response")
            .to_text()
            .expect("response is text")
            .to_string();
        assert_eq!(forwarded_to_agent, r#"{"id":"1","result":"pong"}"#);
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("browser-connection process exits after stdin EOF");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn browser_connection_stdio_accepts_claude_framed_initialize() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_browser-connection"))
        .args([
            "--project",
            "dg-test",
            "--no-start-browser",
            "--no-control-panel",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn browser-connection MCP server");

    {
        let stdin = child.stdin.as_mut().expect("stdin is piped");
        let mut input = Vec::new();
        input.extend(encode_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": { "roots": { "listChanged": true } },
                "clientInfo": { "name": "claude-code", "version": "2.1.160" }
            }
        })));
        input.extend(encode_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        })));
        stdin
            .write_all(&input)
            .expect("write Claude MCP handshake requests");
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("browser-connection process exits after stdin EOF");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let responses = decode_messages(&output.stdout);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-11-25");
    assert!(responses[1]["result"]["tools"]
        .as_array()
        .expect("tools/list returns an array")
        .iter()
        .any(|tool| tool["name"] == "browser_navigate"));
}

#[test]
fn browser_connection_stdio_accepts_codex_line_delimited_initialize() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_browser-connection"))
        .args([
            "--project",
            "dg-test",
            "--no-start-browser",
            "--no-control-panel",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn browser-connection MCP server");

    {
        let stdin = child.stdin.as_mut().expect("stdin is piped");
        stdin
            .write_all(
                br#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{"elicitation":{}},"clientInfo":{"name":"codex-mcp-client","title":"Codex","version":"0.136.0"}}}
{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}
"#,
            )
            .expect("write MCP line-delimited requests");
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("browser-connection process exits after stdin EOF");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let responses = decode_line_messages(&output.stdout);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
    assert!(responses[1]["result"]["tools"]
        .as_array()
        .expect("tools/list returns an array")
        .iter()
        .any(|tool| tool["name"] == "browser_navigate"));
}
