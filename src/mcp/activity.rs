use anyhow::Result;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_EVENTS: usize = 200;
const MAX_SCREENSHOTS: usize = 24;
const MAX_TEXT_CHARS: usize = 700;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct BrowserActivityLog {
    next_event_id: u64,
    events: VecDeque<Value>,
    screenshots: VecDeque<Value>,
    latest_tabs: Option<Value>,
    latest_tab_error: Option<String>,
}

impl BrowserActivityLog {
    pub(super) fn new() -> Self {
        Self {
            next_event_id: 1,
            events: VecDeque::new(),
            screenshots: VecDeque::new(),
            latest_tabs: None,
            latest_tab_error: None,
        }
    }

    pub(super) fn snapshot(&self, active_browser: &str, mode: &str) -> Value {
        json!({
            "activeBrowser": active_browser,
            "mode": mode,
            "tabs": self.latest_tabs.clone().unwrap_or(Value::Null),
            "tabsError": self.latest_tab_error,
            "screenshots": self.screenshots.iter().cloned().collect::<Vec<_>>(),
            "latestScreenshot": self.screenshots.front().cloned().unwrap_or(Value::Null),
            "events": self.events.iter().cloned().collect::<Vec<_>>(),
        })
    }

    pub(super) fn record_tabs(&mut self, value: Value) {
        self.latest_tabs = Some(tab_inventory(value));
        self.latest_tab_error = None;
    }

    pub(super) fn record_tab_error(&mut self, error: impl ToString) {
        self.latest_tab_error = Some(error.to_string());
    }

    pub(super) fn clear_tabs(&mut self) {
        self.latest_tabs = None;
        self.latest_tab_error = None;
    }

    pub(super) fn record_tool_result(
        &mut self,
        browser: &str,
        mode: &str,
        tool: &str,
        arguments: &Value,
        started_at_ms: u64,
        result: &Result<String>,
    ) {
        let event_id = self.next_event_id;
        self.next_event_id += 1;

        let duration_ms = now_ms().saturating_sub(started_at_ms);
        let (ok, error, result_summary) = match result {
            Ok(text) => (true, Value::Null, result_summary(tool, text)),
            Err(error) => (false, json!(error.to_string()), Value::Null),
        };
        let screenshot_id = result
            .as_ref()
            .ok()
            .and_then(|text| self.record_screenshot(event_id, browser, tool, text));

        self.events.push_front(json!({
            "id": event_id,
            "at": started_at_ms,
            "browser": browser,
            "mode": mode,
            "tool": tool,
            "arguments": sanitize_value(arguments),
            "ok": ok,
            "durationMs": duration_ms,
            "error": error,
            "result": result_summary,
            "screenshotId": screenshot_id,
        }));
        truncate(&mut self.events, MAX_EVENTS);
    }

    fn record_screenshot(
        &mut self,
        event_id: u64,
        browser: &str,
        tool: &str,
        text: &str,
    ) -> Option<u64> {
        let screenshot = extract_screenshot(text)?;
        let screenshot_id = event_id;
        self.screenshots.push_front(json!({
            "id": screenshot_id,
            "eventId": event_id,
            "at": now_ms(),
            "browser": browser,
            "tool": tool,
            "mimeType": screenshot.mime_type,
            "dataUrl": screenshot.data_url,
        }));
        truncate(&mut self.screenshots, MAX_SCREENSHOTS);
        Some(screenshot_id)
    }
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn truncate(values: &mut VecDeque<Value>, limit: usize) {
    while values.len() > limit {
        values.pop_back();
    }
}

fn sanitize_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut sanitized = Map::new();
            for (key, value) in object {
                let lower = key.to_ascii_lowercase();
                if lower.contains("token") || lower == "share_url" || lower == "shareurl" {
                    sanitized.insert(key.clone(), json!("[redacted]"));
                } else {
                    sanitized.insert(key.clone(), sanitize_value(value));
                }
            }
            Value::Object(sanitized)
        }
        Value::Array(values) => Value::Array(values.iter().map(sanitize_value).collect()),
        _ => value.clone(),
    }
}

fn result_summary(tool: &str, text: &str) -> Value {
    if extract_screenshot(text).is_some() {
        return json!({ "kind": "screenshot", "text": "screenshot captured" });
    }

    if let Ok(value) = serde_json::from_str::<Value>(text) {
        let tab = value.get("tab").cloned().unwrap_or(Value::Null);
        let snapshot = value.get("snapshot").cloned().unwrap_or(Value::Null);
        return json!({
            "kind": "json",
            "tool": tool,
            "title": snapshot.get("title").or_else(|| tab.get("title")).cloned().unwrap_or(Value::Null),
            "url": snapshot.get("url").or_else(|| tab.get("url")).cloned().unwrap_or(Value::Null),
            "tab": tab,
        });
    }

    json!({
        "kind": "text",
        "text": truncate_text(text, MAX_TEXT_CHARS),
    })
}

struct Screenshot {
    data_url: String,
    mime_type: String,
}

fn extract_screenshot(text: &str) -> Option<Screenshot> {
    let trimmed = text.trim();
    if trimmed.starts_with("data:image/") {
        return Some(Screenshot {
            mime_type: mime_type_from_data_url(trimmed),
            data_url: trimmed.to_string(),
        });
    }

    let value = serde_json::from_str::<Value>(trimmed).ok()?;
    extract_screenshot_from_value(&value)
}

fn find_data_url(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if text.starts_with("data:image/") => Some(text.clone()),
        Value::Array(values) => values.iter().find_map(find_data_url),
        Value::Object(object) => object.values().find_map(find_data_url),
        _ => None,
    }
}

fn extract_screenshot_from_value(value: &Value) -> Option<Screenshot> {
    if let Some(data_url) = find_data_url(value) {
        return Some(Screenshot {
            mime_type: mime_type_from_data_url(&data_url),
            data_url,
        });
    }

    let object = value.as_object()?;
    let data = object.get("data").and_then(Value::as_str)?;
    let mime_type = object
        .get("mimeType")
        .and_then(Value::as_str)
        .filter(|value| value.starts_with("image/"))
        .unwrap_or("image/png")
        .to_string();
    Some(Screenshot {
        data_url: format!("data:{mime_type};base64,{data}"),
        mime_type,
    })
}

fn mime_type_from_data_url(data_url: &str) -> String {
    data_url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(';').map(|(mime, _)| mime.to_string()))
        .unwrap_or_else(|| "image/png".to_string())
}

fn tab_inventory(value: Value) -> Value {
    let tabs = value
        .get("tabs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut windows = BTreeMap::<i64, Vec<Value>>::new();
    let mut active_tab = Value::Null;

    for tab in &tabs {
        let window_id = tab.get("windowId").and_then(Value::as_i64).unwrap_or(-1);
        if tab.get("active").and_then(Value::as_bool) == Some(true) {
            active_tab = tab.clone();
        }
        windows.entry(window_id).or_default().push(tab.clone());
    }

    let windows = windows
        .into_iter()
        .map(|(window_id, tabs)| {
            let active = tabs
                .iter()
                .any(|tab| tab.get("active").and_then(Value::as_bool) == Some(true));
            json!({
                "windowId": window_id,
                "active": active,
                "tabCount": tabs.len(),
                "tabs": tabs,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "totalTabs": tabs.len(),
        "totalWindows": windows.len(),
        "activeTab": active_tab,
        "windows": windows,
        "tabs": tabs,
    })
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index >= max_chars {
            output.push_str("...");
            return output;
        }
        output.push(ch);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_data_url_from_screenshot_json() {
        let screenshot = extract_screenshot(r#"{"dataUrl":"data:image/png;base64,abc"}"#).unwrap();

        assert_eq!(screenshot.mime_type, "image/png");
        assert_eq!(screenshot.data_url, "data:image/png;base64,abc");
    }

    #[test]
    fn extracts_base64_data_from_screenshot_json() {
        let screenshot = extract_screenshot(r#"{"mimeType":"image/jpeg","data":"abc"}"#).unwrap();

        assert_eq!(screenshot.mime_type, "image/jpeg");
        assert_eq!(screenshot.data_url, "data:image/jpeg;base64,abc");
    }

    #[test]
    fn groups_tabs_by_window() {
        let inventory = tab_inventory(json!({
            "tabs": [
                {"id": 1, "windowId": 10, "active": true},
                {"id": 2, "windowId": 10, "active": false},
                {"id": 3, "windowId": 11, "active": false}
            ]
        }));

        assert_eq!(inventory["totalTabs"], 3);
        assert_eq!(inventory["totalWindows"], 2);
        assert_eq!(inventory["activeTab"]["id"], 1);
    }
}
