use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

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
    assert!(stdout.contains("--no-start-browser"));
    assert!(!stdout.contains("playwright/mcp"));
    assert!(!stdout.contains("npx"));
}

#[test]
fn browser_connection_stdio_initializes_and_lists_browser_tools_without_docker() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_browser-connection"))
        .args(["--project", "dg-test", "--no-start-browser"])
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
}

#[test]
fn browser_connection_stdio_accepts_codex_line_delimited_initialize() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_browser-connection"))
        .args(["--project", "dg-test", "--no-start-browser"])
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
