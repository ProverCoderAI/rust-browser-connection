use super::activity;
use super::McpRuntime;
use crate::cdp::CdpClient;
use crate::shared_browser::SharedBrowserClient;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

pub(super) fn tool_definitions() -> Vec<Value> {
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
        tool(
            "browser_list_tabs",
            "List tabs and windows visible to the active shared browser extension.",
            json!({}),
            vec![],
        ),
        tool(
            "browser_activate_tab",
            "Activate a tab in the active shared browser extension.",
            json!({ "tab_id": { "type": "integer", "description": "Chrome tab id to activate" } }),
            vec!["tab_id"],
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

pub(super) fn handle_tool_call(runtime: &mut McpRuntime, request: &Value) -> Value {
    let params = request.get("params").unwrap_or(&Value::Null);
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments").unwrap_or(&Value::Null);
    let started_at_ms = activity::now_ms();

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

    runtime.record_tool_activity(name, arguments, started_at_ms, &result);

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
        "browser_list_tabs" | "browser_activate_tab" => Err(anyhow!(
            "{name} is only available for shared-extension browsers"
        )),
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
        "browser_list_tabs" => serde_json::to_string_pretty(&client.list_tabs()?)
            .context("failed to render shared browser tabs"),
        "browser_activate_tab" => client.activate_tab(required_i64(arguments, "tab_id")?),
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

fn required_i64(arguments: &Value, name: &str) -> Result<i64> {
    arguments
        .get(name)
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("argument `{name}` is required"))
}

fn tool_result(text: String, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error
    })
}
