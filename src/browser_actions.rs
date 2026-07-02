use crate::cdp::CdpClient;
use crate::shared_browser::SharedBrowserClient;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserTarget {
    Cdp { endpoint: String },
    Shared { share_url: String },
}

impl BrowserTarget {
    pub fn cdp(endpoint: impl Into<String>) -> Self {
        Self::Cdp {
            endpoint: endpoint.into(),
        }
    }

    pub fn shared(share_url: impl Into<String>) -> Self {
        Self::Shared {
            share_url: share_url.into(),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Cdp { .. } => "cdp",
            Self::Shared { .. } => "shared-extension",
        }
    }

    pub fn safe_label(&self) -> String {
        match self {
            Self::Cdp { endpoint } => format!("cdp:{endpoint}"),
            Self::Shared { share_url } => {
                let redacted = share_url
                    .split_once("#agent=")
                    .map(|(base, _)| format!("{base}#agent=<redacted>"))
                    .unwrap_or_else(|| "<shared-browser-url>".to_string());
                format!("shared:{redacted}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserCommand {
    Navigate { url: String },
    Snapshot,
    Evaluate { expression: String },
    Click { selector: String },
    TypeText { selector: String, text: String },
    PressKey { key: String },
    Screenshot { full_page: bool },
    ListTabs,
    ActivateTab { tab_id: i64 },
}

impl BrowserCommand {
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::Navigate { .. } => "browser_navigate",
            Self::Snapshot => "browser_snapshot",
            Self::Evaluate { .. } => "browser_evaluate",
            Self::Click { .. } => "browser_click",
            Self::TypeText { .. } => "browser_type",
            Self::PressKey { .. } => "browser_press_key",
            Self::Screenshot { .. } => "browser_take_screenshot",
            Self::ListTabs => "browser_list_tabs",
            Self::ActivateTab { .. } => "browser_activate_tab",
        }
    }
}

pub fn command_from_browser_tool(name: &str, arguments: &Value) -> Result<BrowserCommand> {
    match name {
        "browser_navigate" => Ok(BrowserCommand::Navigate {
            url: required_str(arguments, "url")?.to_string(),
        }),
        "browser_snapshot" => Ok(BrowserCommand::Snapshot),
        "browser_evaluate" => Ok(BrowserCommand::Evaluate {
            expression: required_str(arguments, "expression")?.to_string(),
        }),
        "browser_click" => Ok(BrowserCommand::Click {
            selector: required_str(arguments, "selector")?.to_string(),
        }),
        "browser_type" => Ok(BrowserCommand::TypeText {
            selector: required_str(arguments, "selector")?.to_string(),
            text: required_str(arguments, "text")?.to_string(),
        }),
        "browser_press_key" => Ok(BrowserCommand::PressKey {
            key: required_str(arguments, "key")?.to_string(),
        }),
        "browser_take_screenshot" => Ok(BrowserCommand::Screenshot {
            full_page: arguments
                .get("full_page")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }),
        "browser_list_tabs" => Ok(BrowserCommand::ListTabs),
        "browser_activate_tab" => Ok(BrowserCommand::ActivateTab {
            tab_id: required_i64(arguments, "tab_id")?,
        }),
        "" => Err(anyhow!("tools/call params.name is required")),
        _ => Err(anyhow!("Unknown browser-connection tool: {name}")),
    }
}

pub fn dispatch_browser_command(
    target: &BrowserTarget,
    command: &BrowserCommand,
) -> Result<String> {
    match target {
        BrowserTarget::Cdp { endpoint } => dispatch_cdp_tool(endpoint, command),
        BrowserTarget::Shared { share_url } => dispatch_shared_tool(share_url, command),
    }
}

fn dispatch_cdp_tool(cdp_endpoint: &str, command: &BrowserCommand) -> Result<String> {
    let client = CdpClient::new(cdp_endpoint);
    match command {
        BrowserCommand::Navigate { url } => client.navigate(url),
        BrowserCommand::Snapshot => client.snapshot(),
        BrowserCommand::Evaluate { expression } => client.evaluate(expression),
        BrowserCommand::Click { selector } => client.click(selector),
        BrowserCommand::TypeText { selector, text } => client.type_text(selector, text),
        BrowserCommand::PressKey { key } => client.press_key(key),
        BrowserCommand::Screenshot { full_page } => client.screenshot(*full_page),
        BrowserCommand::ListTabs | BrowserCommand::ActivateTab { .. } => Err(anyhow!(
            "{} is only available for shared-extension browsers",
            command.tool_name()
        )),
    }
}

fn dispatch_shared_tool(share_url: &str, command: &BrowserCommand) -> Result<String> {
    let client = SharedBrowserClient::new(share_url);
    match command {
        BrowserCommand::Navigate { url } => client.navigate(url),
        BrowserCommand::Snapshot => client.snapshot(),
        BrowserCommand::Evaluate { expression } => client.evaluate(expression),
        BrowserCommand::Click { selector } => client.click(selector),
        BrowserCommand::TypeText { selector, text } => client.type_text(selector, text),
        BrowserCommand::PressKey { key } => client.press_key(key),
        BrowserCommand::Screenshot { full_page } => client.screenshot(*full_page),
        BrowserCommand::ListTabs => serde_json::to_string_pretty(&client.list_tabs()?)
            .context("failed to render shared browser tabs"),
        BrowserCommand::ActivateTab { tab_id } => client.activate_tab(*tab_id),
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

pub fn command_result_json(command: &BrowserCommand, text: &str) -> Value {
    serde_json::from_str::<Value>(text).unwrap_or_else(|_| {
        json!({
            "tool": command.tool_name(),
            "text": text
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_screenshot_tool_defaulting_to_full_page() {
        let command = command_from_browser_tool("browser_take_screenshot", &json!({})).unwrap();
        assert_eq!(command, BrowserCommand::Screenshot { full_page: true });
    }

    #[test]
    fn rejects_shared_only_commands_for_cdp_without_network() {
        let result = dispatch_browser_command(
            &BrowserTarget::cdp("http://127.0.0.1:1"),
            &BrowserCommand::ListTabs,
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("shared-extension browsers"));
    }

    #[test]
    fn redacts_shared_agent_token_in_safe_label() {
        let target = BrowserTarget::shared("https://relay.example/share/s1#agent=secret");
        assert_eq!(
            target.safe_label(),
            "shared:https://relay.example/share/s1#agent=<redacted>"
        );
    }
}
