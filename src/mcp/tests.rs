use super::*;
use serde_json::{json, Value};
use std::fs;
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
    while let Some(message) =
        read_message(&mut cursor, &mut transport).expect("stdout frame parses")
    {
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
    let mut runtime = McpRuntime::new(config).expect("runtime config is valid");

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

    assert_eq!(runtime.managed_cdp_endpoint, None);
    assert_eq!(runtime.active_browser, MANAGED_BROWSER_NAME);
}

#[test]
fn initialize_negotiates_legacy_protocol_version() {
    let config = McpServerConfig::new("dg-test", None, None, false);
    let mut runtime = McpRuntime::new(config).expect("runtime config is valid");

    let response = handle_message(
        &mut runtime,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}"#,
    )
    .expect("initialize succeeds")
    .expect("request with id returns a response");

    assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(
        response["result"]["capabilities"]["tools"]["listChanged"],
        false
    );
}

#[test]
fn initialize_negotiates_latest_protocol_version() {
    let config = McpServerConfig::new("dg-test", None, None, false);
    let mut runtime = McpRuntime::new(config).expect("runtime config is valid");

    let response = handle_message(
        &mut runtime,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{"roots":{"listChanged":true}},"clientInfo":{"name":"claude-code","version":"2.1.160"}}}"#,
    )
    .expect("initialize succeeds")
    .expect("request with id returns a response");

    assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
}

#[test]
fn initialize_negotiates_previous_protocol_version() {
    let config = McpServerConfig::new("dg-test", None, None, false);
    let mut runtime = McpRuntime::new(config).expect("runtime config is valid");

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
    let mut runtime = McpRuntime::new(config).expect("runtime config is valid");

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
    let mut runtime = McpRuntime::new(config).expect("runtime config is valid");
    let expected_cdp_url = crate::render_cdp_url();

    let response = handle_message(
        &mut runtime,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"unknown","arguments":{}}}"#,
    )
    .expect("tools/call response serializes")
    .expect("request with id returns a response");

    assert_eq!(
        runtime.managed_cdp_endpoint.as_deref(),
        Some(expected_cdp_url.as_str())
    );
    assert_eq!(response["id"], 3);
    assert_eq!(response["result"]["isError"], true);
    assert!(response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool result text exists")
        .contains("Unknown browser-connection tool"));
}

#[test]
fn configured_personal_browser_can_be_active_without_docker() {
    let config = McpServerConfig::new("dg-test", None, None, true)
        .with_browser_endpoints(vec![NamedBrowserEndpoint::new(
            PERSONAL_BROWSER_NAME,
            "http://127.0.0.1:9333",
        )
        .expect("endpoint is valid")])
        .with_active_browser(Some(PERSONAL_BROWSER_NAME.to_string()))
        .expect("active browser name is valid");
    let mut runtime = McpRuntime::new(config).expect("runtime config is valid");

    assert_eq!(
        runtime.cdp_endpoint().expect("personal endpoint resolves"),
        "http://127.0.0.1:9333"
    );
    assert_eq!(runtime.managed_cdp_endpoint, None);
}

#[test]
fn browser_select_registers_ad_hoc_personal_endpoint_without_resolving_managed_browser() {
    let config = McpServerConfig::new("dg-test", None, None, true);
    let mut runtime = McpRuntime::new(config).expect("runtime config is valid");

    let response = handle_message(
        &mut runtime,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"browser_select","arguments":{"name":"personal","cdp_endpoint":"http://127.0.0.1:9444"}}}"#,
    )
    .expect("browser_select response serializes")
    .expect("request with id returns a response");

    assert_eq!(response["id"], 4);
    assert_eq!(response["result"]["isError"], false);
    assert_eq!(runtime.active_browser, PERSONAL_BROWSER_NAME);
    assert_eq!(runtime.managed_cdp_endpoint, None);
    assert_eq!(
        runtime.cdp_endpoint().expect("personal endpoint resolves"),
        "http://127.0.0.1:9444"
    );
}

#[test]
fn browser_select_registers_personal_display_metadata_without_docker_when_novnc_url_exists() {
    let config = McpServerConfig::new("dg-test", None, None, true);
    let mut runtime = McpRuntime::new(config).expect("runtime config is valid");

    let response = handle_message(
        &mut runtime,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"browser_select","arguments":{"name":"personal","cdp_endpoint":"http://127.0.0.1:9444","vnc_endpoint":"host.docker.internal:5900","novnc_url":"http://127.0.0.1:6680/vnc.html"}}}"#,
    )
    .expect("browser_select response serializes")
    .expect("request with id returns a response");

    assert_eq!(response["result"]["isError"], false);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool result text exists");
    let selection: Value = serde_json::from_str(text).expect("selection text is JSON");
    assert_eq!(
        selection["browser"]["vncEndpoint"],
        "host.docker.internal:5900"
    );
    assert_eq!(
        selection["browser"]["novncUrl"],
        "http://127.0.0.1:6680/vnc.html"
    );
    assert_eq!(runtime.managed_cdp_endpoint, None);
}

#[test]
fn browser_list_reports_active_personal_browser() {
    let config = McpServerConfig::new("dg-test", None, None, false)
        .with_browser_endpoints(vec![NamedBrowserEndpoint::new(
            PERSONAL_BROWSER_NAME,
            "http://127.0.0.1:9222",
        )
        .expect("endpoint is valid")])
        .with_active_browser(Some(PERSONAL_BROWSER_NAME.to_string()))
        .expect("active browser name is valid");
    let runtime = McpRuntime::new(config).expect("runtime config is valid");

    let inventory = runtime.browser_inventory().expect("inventory renders");

    assert_eq!(inventory["active"], PERSONAL_BROWSER_NAME);
    assert!(inventory["browsers"]
        .as_array()
        .expect("browsers array")
        .iter()
        .any(|browser| {
            browser["name"] == PERSONAL_BROWSER_NAME
                && browser["kind"] == "personal"
                && browser["active"] == true
        }));
}

#[test]
fn browser_list_reports_active_shared_browser_without_cdp() {
    let config = McpServerConfig::new("dg-test", None, None, false)
        .with_browser_share_url("edge", "https://relay.example/share/s1#agent=a1")
        .expect("share URL is valid")
        .with_active_browser(Some("edge".to_string()))
        .expect("active browser name is valid");
    let runtime = McpRuntime::new(config).expect("runtime config is valid");

    let inventory = runtime.browser_inventory().expect("inventory renders");

    assert_eq!(inventory["active"], "edge");
    assert!(inventory["browsers"]
        .as_array()
        .expect("browsers array")
        .iter()
        .any(|browser| {
            browser["name"] == "edge"
                && browser["kind"] == "shared-extension"
                && browser["shareUrl"] == "https://relay.example/share/s1#agent=a1"
                && browser["active"] == true
        }));
}

#[test]
fn browser_list_reports_control_panel_url_when_enabled() {
    let config = McpServerConfig::new("dg-test", None, None, false).with_control_port(Some(6888));
    let runtime = McpRuntime::new(config).expect("runtime config is valid");

    let inventory = runtime.browser_inventory().expect("inventory renders");

    assert_eq!(inventory["controlPanelUrl"], "http://127.0.0.1:6888/");
}

#[test]
fn runtime_restores_panel_selected_shared_browser_from_state_file() {
    let project = format!(
        "dg-test-persist-{}-{}",
        std::process::id(),
        activity::now_ms()
    );
    let config = McpServerConfig::new(&project, None, None, false).with_control_port(Some(6888));
    let mut runtime = McpRuntime::new(config).expect("runtime config is valid");

    runtime
        .select_browser(
            "edge",
            None,
            None,
            None,
            Some("https://relay.example/share/s1#agent=a1"),
        )
        .expect("shared browser selection persists");

    let restored = McpRuntime::new(
        McpServerConfig::new(&project, None, None, false).with_control_port(Some(6888)),
    )
    .expect("runtime restores persisted shared browser");
    let inventory = restored.browser_inventory().expect("inventory renders");

    assert_eq!(inventory["active"], "edge");
    assert!(inventory["browsers"]
        .as_array()
        .expect("browsers array")
        .iter()
        .any(|browser| {
            browser["name"] == "edge"
                && browser["kind"] == "shared-extension"
                && browser["shareUrl"] == "https://relay.example/share/s1#agent=a1"
                && browser["active"] == true
        }));

    let _ = fs::remove_file(runtime_state_path(&project));
}

#[test]
fn browser_list_reports_null_control_panel_url_when_disabled() {
    let config = McpServerConfig::new("dg-test", None, None, false);
    let runtime = McpRuntime::new(config).expect("runtime config is valid");

    let inventory = runtime.browser_inventory().expect("inventory renders");

    assert!(inventory["controlPanelUrl"].is_null());
}

#[test]
fn browser_list_reports_personal_vnc_and_deterministic_novnc_url_without_docker() {
    let config = McpServerConfig::new("dg-test", None, None, false).with_browser_endpoints(vec![
        NamedBrowserEndpoint::new(PERSONAL_BROWSER_NAME, "http://127.0.0.1:9222")
            .expect("endpoint is valid")
            .with_vnc_endpoint("host.docker.internal:5900")
            .expect("vnc endpoint is valid"),
    ]);
    let runtime = McpRuntime::new(config).expect("runtime config is valid");

    let inventory = runtime.browser_inventory().expect("inventory renders");

    let personal = inventory["browsers"]
        .as_array()
        .expect("browsers array")
        .iter()
        .find(|browser| browser["name"] == PERSONAL_BROWSER_NAME)
        .expect("personal browser is listed");
    assert_eq!(personal["vncEndpoint"], "host.docker.internal:5900");
    assert_eq!(
        personal["novncUrl"],
        crate::render_browser_target_novnc_url("dg-test", PERSONAL_BROWSER_NAME)
    );
    assert_eq!(runtime.managed_cdp_endpoint, None);
}

#[test]
fn invalid_active_browser_fails_runtime_creation() {
    let config = McpServerConfig::new("dg-test", None, None, false)
        .with_active_browser(Some("personal".to_string()))
        .expect("active browser name normalizes");

    let error = McpRuntime::new(config).expect_err("unknown active browser fails");

    assert!(error.to_string().contains("Unknown browser `personal`"));
}

#[test]
fn malformed_explicit_cdp_endpoint_fails_runtime_creation() {
    let config = McpServerConfig::new("dg-test", None, Some("127.0.0.1:9222".to_string()), false);

    let error = McpRuntime::new(config).expect_err("malformed explicit endpoint fails");

    assert!(error
        .to_string()
        .contains("must start with http:// or https://"));
}
