use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

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
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            "\n",
        );
        stdin
            .write_all(input.as_bytes())
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

    let stdout = String::from_utf8_lossy(&output.stdout);
    let responses = stdout
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("stdout line is JSON"))
        .collect::<Vec<_>>();

    assert_eq!(responses.len(), 2, "stdout was: {stdout}");
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
