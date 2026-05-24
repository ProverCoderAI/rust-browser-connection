/*! CDP adapter for the Rust `browser-connection` MCP server.

CHANGE: drive Chromium directly through CDP from Rust instead of shelling out to an external upstream MCP command.
WHY: issue #347 requires docker-git's noVNC-visible browser and MCP browser tools to be one session.
QUOTE(ТЗ): "заменить использование команды playright на нашу кастомную имплементацию"
REF: https://github.com/ProverCoderAI/docker-git/issues/347
SOURCE: n/a
FORMAT THEOREM: tool(cdp_url, args) -> CDP command on the same endpoint that noVNC exposes.
PURITY: SHELL
EFFECT: HTTP probes via curl and WebSocket CDP commands.
INVARIANT: no tool starts a second browser; every command targets the configured CDP endpoint.
*/

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tungstenite::{connect, Message};

#[derive(Debug, Clone)]
pub struct CdpClient {
    endpoint: String,
}

impl CdpClient {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn navigate(&self, url: &str) -> Result<String> {
        require_non_empty(url, "url")?;
        self.command("Page.navigate", json!({ "url": url }))?;
        self.wait_for_navigation_ready()?;
        Ok(format!("Navigated to {url}"))
    }

    // CHANGE: wait for browser_navigate to leave the page in a DOM-readable state.
    // WHY: MCP clients commonly call browser_evaluate immediately after navigate; without this wait,
    //      Runtime.evaluate can race while document.body is still null and produce flaky smoke failures.
    // QUOTE(ТЗ): "проверил работате ли поднятие MCP Playright вместе с noVNC протоколом"
    // REF: proc_47c63ffc3276 JSONDecodeError after browser_evaluate saw document.body == null
    // SOURCE: Chrome DevTools Protocol Runtime.evaluate + document.readyState
    // FORMAT THEOREM: navigate(url) returns Ok -> document.body exists ∧ readyState ∈ {interactive, complete}
    // PURITY: SHELL
    // EFFECT: polls CDP Runtime.evaluate for up to 10 seconds after Page.navigate.
    // INVARIANT: no additional browser is started; polling uses the same CDP endpoint and page target.
    fn wait_for_navigation_ready(&self) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last_error = None;
        let mut last_status = Value::Null;

        while Instant::now() < deadline {
            match self.evaluate_value(NAVIGATION_READY_EXPRESSION) {
                Ok(status) => {
                    if navigation_ready(&status) {
                        return Ok(());
                    }
                    last_status = status;
                }
                Err(error) => {
                    last_error = Some(error.to_string());
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        Err(anyhow!(
            "page did not become DOM-ready after navigation; last status: {last_status}; last error: {}",
            last_error.unwrap_or_else(|| "none".to_string())
        ))
    }

    pub fn evaluate(&self, expression: &str) -> Result<String> {
        let value = self.evaluate_value(expression)?;
        serde_json::to_string_pretty(&value).context("failed to render evaluation result")
    }

    pub fn snapshot(&self) -> Result<String> {
        let value = self.evaluate_value(
            r#"(() => {
                const text = (document.body && document.body.innerText || document.documentElement.innerText || '').trim();
                const interactive = Array.from(document.querySelectorAll('a,button,input,textarea,select,[role="button"],[tabindex]'))
                    .slice(0, 80)
                    .map((el, index) => ({
                        ref: `@e${index + 1}`,
                        tag: el.tagName.toLowerCase(),
                        role: el.getAttribute('role') || '',
                        text: (el.innerText || el.value || el.getAttribute('aria-label') || el.getAttribute('title') || el.getAttribute('href') || '').trim().slice(0, 160),
                        selector: el.id ? `#${CSS.escape(el.id)}` : el.name ? `${el.tagName.toLowerCase()}[name="${CSS.escape(el.name)}"]` : el.tagName.toLowerCase()
                    }));
                return { url: location.href, title: document.title, text: text.slice(0, 4000), interactive };
            })()"#,
        )?;

        let title = value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("(untitled)");
        let url = value.get("url").and_then(Value::as_str).unwrap_or("");
        let text = value.get("text").and_then(Value::as_str).unwrap_or("");
        let mut output = format!("Title: {title}\nURL: {url}\n\n{text}");

        if let Some(items) = value.get("interactive").and_then(Value::as_array) {
            if !items.is_empty() {
                output.push_str("\n\nInteractive elements:\n");
                for item in items {
                    let reference = item.get("ref").and_then(Value::as_str).unwrap_or("@e?");
                    let tag = item.get("tag").and_then(Value::as_str).unwrap_or("element");
                    let label = item.get("text").and_then(Value::as_str).unwrap_or("");
                    let selector = item.get("selector").and_then(Value::as_str).unwrap_or(tag);
                    output.push_str(&format!(
                        "{reference} {tag} \"{label}\" selector={selector}\n"
                    ));
                }
            }
        }

        Ok(output)
    }

    pub fn click(&self, selector: &str) -> Result<String> {
        require_non_empty(selector, "selector")?;
        let selector_json = serde_json::to_string(selector).context("failed to quote selector")?;
        let expression = format!(
            r#"(() => {{
                const selector = {selector_json};
                const el = document.querySelector(selector);
                if (!el) throw new Error(`No element matches selector: ${{selector}}`);
                el.scrollIntoView({{block: 'center', inline: 'center'}});
                el.click();
                return {{ clicked: true, selector, text: (el.innerText || el.value || el.getAttribute('aria-label') || '').trim() }};
            }})()"#
        );
        self.evaluate(&expression)
    }

    pub fn type_text(&self, selector: &str, text: &str) -> Result<String> {
        require_non_empty(selector, "selector")?;
        let selector_json = serde_json::to_string(selector).context("failed to quote selector")?;
        let text_json = serde_json::to_string(text).context("failed to quote text")?;
        let expression = format!(
            r#"(() => {{
                const selector = {selector_json};
                const text = {text_json};
                const el = document.querySelector(selector);
                if (!el) throw new Error(`No element matches selector: ${{selector}}`);
                el.focus();
                if ('value' in el) {{
                    el.value = text;
                    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                }} else {{
                    el.textContent = text;
                    el.dispatchEvent(new InputEvent('input', {{ bubbles: true, inputType: 'insertText', data: text }}));
                }}
                return {{ typed: true, selector, textLength: text.length }};
            }})()"#
        );
        self.evaluate(&expression)
    }

    pub fn press_key(&self, key: &str) -> Result<String> {
        require_non_empty(key, "key")?;
        let text = if key.chars().count() == 1 { key } else { "" };
        let params = json!({ "type": "keyDown", "key": key, "text": text });
        self.command("Input.dispatchKeyEvent", params)?;
        self.command(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyUp", "key": key }),
        )?;
        Ok(format!("Pressed key {key}"))
    }

    pub fn screenshot(&self, full_page: bool) -> Result<String> {
        let result = self.command(
            "Page.captureScreenshot",
            json!({ "format": "png", "captureBeyondViewport": full_page }),
        )?;
        let data = result
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("CDP screenshot response did not include data"))?;
        Ok(format!("data:image/png;base64,{data}"))
    }

    fn evaluate_value(&self, expression: &str) -> Result<Value> {
        require_non_empty(expression, "expression")?;
        let result = self.command(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "returnByValue": true,
                "awaitPromise": true,
                "userGesture": true
            }),
        )?;

        if let Some(details) = result.get("exceptionDetails") {
            return Err(anyhow!("CDP Runtime.evaluate exception: {details}"));
        }

        let remote = result
            .get("result")
            .ok_or_else(|| anyhow!("CDP Runtime.evaluate response did not include result"))?;

        if let Some(value) = remote.get("value") {
            return Ok(value.clone());
        }
        if let Some(description) = remote.get("description").and_then(Value::as_str) {
            return Ok(Value::String(description.to_string()));
        }
        Ok(remote.clone())
    }

    fn command(&self, method: &str, params: Value) -> Result<Value> {
        let websocket_url = self.page_websocket_url()?;
        send_cdp_command(&websocket_url, method, params)
    }

    fn page_websocket_url(&self) -> Result<String> {
        let list = self.curl_json("GET", "/json/list")?;
        if let Some(targets) = list.as_array() {
            if let Some(url) = targets.iter().find_map(page_target_websocket_url) {
                return rewrite_websocket_url(&self.endpoint, url);
            }
        }

        let created = self.curl_json("PUT", "/json/new?about:blank")?;
        let url = created
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("CDP /json/new did not return a page websocket URL"))?;
        rewrite_websocket_url(&self.endpoint, url)
    }

    fn curl_json(&self, method: &str, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.endpoint.trim_end_matches('/'), path);
        let output = Command::new("curl")
            .args([
                "-sSf",
                "--connect-timeout",
                "2",
                "--max-time",
                "10",
                "-X",
                method,
                "-H",
                "Host: 127.0.0.1:9222",
                &url,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("failed to execute curl for CDP endpoint {url}"))?;

        if !output.status.success() {
            return Err(anyhow!(
                "CDP HTTP request {method} {url} failed with status {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        serde_json::from_slice(&output.stdout)
            .with_context(|| format!("CDP HTTP response from {url} was not JSON"))
    }
}

const NAVIGATION_READY_EXPRESSION: &str = r#"(() => ({
    readyState: document.readyState,
    hasBody: document.body !== null
}))()"#;

fn navigation_ready(status: &Value) -> bool {
    let ready_state = status
        .get("readyState")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let has_body = status
        .get("hasBody")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    has_body && matches!(ready_state, "interactive" | "complete")
}

fn page_target_websocket_url(target: &Value) -> Option<&str> {
    let target_type = target.get("type").and_then(Value::as_str)?;
    if target_type != "page" {
        return None;
    }
    target.get("webSocketDebuggerUrl").and_then(Value::as_str)
}

// CHANGE: rewrite Chrome's self-reported websocket host to the configured CDP endpoint.
// WHY: Chrome usually reports ws://127.0.0.1:9222 even when docker-git exposes it via the stable :9223 proxy.
// QUOTE(ТЗ): "CDP port 9223 ... single session"
// REF: issue-347
// SOURCE: n/a
// FORMAT THEOREM: endpoint=http://h:p ∧ original=ws://x/devtools/y -> ws://h:p/devtools/y
// PURITY: CORE
// INVARIANT: MCP CDP WebSocket traffic uses the same endpoint that noVNC/CDP health checks verify.
// COMPLEXITY: O(n)/O(n), n = endpoint length + original length.
fn rewrite_websocket_url(endpoint: &str, original: &str) -> Result<String> {
    let scheme = if endpoint.trim_start().starts_with("https://") {
        "wss"
    } else {
        "ws"
    };
    let authority = endpoint
        .trim()
        .trim_end_matches('/')
        .strip_prefix("http://")
        .or_else(|| {
            endpoint
                .trim()
                .trim_end_matches('/')
                .strip_prefix("https://")
        })
        .unwrap_or_else(|| endpoint.trim().trim_end_matches('/'));
    let path = original
        .find("/devtools/")
        .map(|index| &original[index..])
        .ok_or_else(|| anyhow!("CDP websocket URL did not include /devtools/: {original}"))?;
    Ok(format!("{scheme}://{authority}{path}"))
}

fn send_cdp_command(websocket_url: &str, method: &str, params: Value) -> Result<Value> {
    let (mut socket, _) = connect(websocket_url)
        .with_context(|| format!("failed to connect to CDP websocket {websocket_url}"))?;
    let request = json!({ "id": 1, "method": method, "params": params });
    socket
        .send(Message::Text(request.to_string()))
        .with_context(|| format!("failed to send CDP command {method}"))?;

    loop {
        let message = socket
            .read()
            .with_context(|| format!("failed to read CDP response for {method}"))?;
        match message {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(text.as_ref())
                    .with_context(|| format!("CDP websocket response for {method} was not JSON"))?;
                if value.get("id").and_then(Value::as_u64) == Some(1) {
                    if let Some(error) = value.get("error") {
                        return Err(anyhow!("CDP command {method} failed: {error}"));
                    }
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
            }
            Message::Close(_) => {
                return Err(anyhow!("CDP websocket closed before {method} response"))
            }
            Message::Ping(payload) => {
                socket
                    .send(Message::Pong(payload))
                    .context("failed to answer CDP websocket ping")?;
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn require_non_empty(value: &str, name: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(anyhow!("{name} must not be empty"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{navigation_ready, rewrite_websocket_url};
    use serde_json::json;

    #[test]
    fn navigation_ready_requires_loaded_document_body() {
        assert!(!navigation_ready(
            &json!({ "readyState": "loading", "hasBody": false })
        ));
        assert!(!navigation_ready(
            &json!({ "readyState": "complete", "hasBody": false })
        ));
        assert!(navigation_ready(
            &json!({ "readyState": "interactive", "hasBody": true })
        ));
        assert!(navigation_ready(
            &json!({ "readyState": "complete", "hasBody": true })
        ));
    }

    #[test]
    fn rewrites_chrome_9222_websocket_to_stable_9223_endpoint() {
        let rewritten = rewrite_websocket_url(
            "http://127.0.0.1:9223",
            "ws://127.0.0.1:9222/devtools/page/ABC",
        )
        .expect("websocket URL rewrites");

        assert_eq!(rewritten, "ws://127.0.0.1:9223/devtools/page/ABC");
    }

    #[test]
    fn rewrites_https_endpoint_to_wss() {
        let rewritten = rewrite_websocket_url(
            "https://browser.example.test:9443/",
            "ws://127.0.0.1:9222/devtools/browser/XYZ",
        )
        .expect("websocket URL rewrites");

        assert_eq!(
            rewritten,
            "wss://browser.example.test:9443/devtools/browser/XYZ"
        );
    }
}
