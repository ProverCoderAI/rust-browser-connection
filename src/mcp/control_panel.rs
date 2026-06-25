use super::{McpRuntime, PERSONAL_BROWSER_NAME};
use crate::shared_browser::BrowserShareRelay;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const MAX_HTTP_REQUEST_BYTES: usize = 64 * 1024;

pub(super) fn spawn_control_panel(runtime: Arc<Mutex<McpRuntime>>) -> Result<()> {
    let port = {
        let runtime = runtime
            .lock()
            .map_err(|_| anyhow!("MCP runtime lock was poisoned"))?;
        runtime.config.control_port
    };
    let Some(port) = port else {
        return Ok(());
    };

    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("failed to bind browser control panel on 127.0.0.1:{port}"))?;
    let relay = BrowserShareRelay::new();
    let control_token = Arc::new(generate_control_token()?);
    thread::Builder::new()
        .name("browser-control-panel".to_string())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let runtime = Arc::clone(&runtime);
                let relay = relay.clone();
                let control_token = Arc::clone(&control_token);
                thread::Builder::new()
                    .name("browser-control-panel-client".to_string())
                    .spawn(move || {
                        let _ = handle_connection(runtime, relay, control_token, stream);
                    })
                    .ok();
            }
        })
        .context("failed to spawn browser control panel thread")?;

    Ok(())
}

fn handle_connection(
    runtime: Arc<Mutex<McpRuntime>>,
    relay: BrowserShareRelay,
    control_token: Arc<String>,
    mut stream: TcpStream,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(3))).ok();

    if is_relay_websocket_request(&stream)? {
        return relay
            .handle_stream(stream)
            .context("failed to handle embedded browser share relay client");
    }

    let request = read_http_request(&mut stream)?;
    let response = route_request(runtime, control_token.as_str(), &request);
    write_http_response(&mut stream, response)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: String,
}

struct HttpResponse {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest> {
    let mut data = Vec::new();
    let mut buffer = [0_u8; 4096];

    loop {
        let read = stream
            .read(&mut buffer)
            .context("failed to read control panel HTTP request")?;
        if read == 0 {
            break;
        }
        data.extend_from_slice(&buffer[..read]);
        if data.len() > MAX_HTTP_REQUEST_BYTES {
            return Err(anyhow!("control panel HTTP request was too large"));
        }
        if let Some(header_end) = header_end(&data) {
            let content_length = content_length(&data[..header_end])?;
            let request_end = header_end + 4 + content_length;
            while data.len() < request_end {
                let read = stream
                    .read(&mut buffer)
                    .context("failed to read control panel HTTP request body")?;
                if read == 0 {
                    break;
                }
                data.extend_from_slice(&buffer[..read]);
                if data.len() > MAX_HTTP_REQUEST_BYTES {
                    return Err(anyhow!("control panel HTTP request was too large"));
                }
            }
            break;
        }
    }

    parse_http_request(&data)
}

fn is_relay_websocket_request(stream: &TcpStream) -> Result<bool> {
    let mut data = [0_u8; 2048];
    let read = stream
        .peek(&mut data)
        .context("failed to peek control panel request")?;
    if read == 0 {
        return Ok(false);
    }
    let head = String::from_utf8_lossy(&data[..read]);
    let Some(request_line) = head.lines().next() else {
        return Ok(false);
    };
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    Ok(method.eq_ignore_ascii_case("GET") && is_relay_path(request_path(target)))
}

fn is_relay_path(path: &str) -> bool {
    path.starts_with("/ws/browser/") || path.starts_with("/ws/agent/")
}

fn header_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_length(header: &[u8]) -> Result<usize> {
    let header = String::from_utf8_lossy(header);
    for line in header.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("Content-Length") {
            return value
                .trim()
                .parse::<usize>()
                .context("control panel Content-Length was invalid");
        }
    }
    Ok(0)
}

fn parse_http_request(data: &[u8]) -> Result<HttpRequest> {
    let header_end = header_end(data).ok_or_else(|| anyhow!("HTTP header terminator missing"))?;
    let head = String::from_utf8_lossy(&data[..header_end]);
    let request_line = head
        .lines()
        .next()
        .ok_or_else(|| anyhow!("HTTP request line missing"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow!("HTTP method missing"))?
        .to_string();
    let target = parts
        .next()
        .ok_or_else(|| anyhow!("HTTP target missing"))?
        .to_string();
    let headers = head
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();
    let body = String::from_utf8_lossy(&data[header_end + 4..]).to_string();

    Ok(HttpRequest {
        method,
        target,
        headers,
        body,
    })
}

fn route_request(
    runtime: Arc<Mutex<McpRuntime>>,
    control_token: &str,
    request: &HttpRequest,
) -> HttpResponse {
    match (request.method.as_str(), request_path(&request.target)) {
        ("OPTIONS", _) => empty_response(204, "No Content"),
        ("GET", "/") | ("GET", "/index.html") => control_panel_response(runtime, control_token),
        ("GET", "/api/browsers") => browser_inventory_response(runtime),
        ("GET", "/api/activity") => {
            if !has_valid_control_token(request, control_token) {
                return json_response(
                    403,
                    "Forbidden",
                    json!({ "error": "invalid control token" }),
                );
            }
            activity_response(runtime, request)
        }
        ("POST", "/api/activate-tab") | ("GET", "/api/activate-tab") => {
            if !has_valid_control_token(request, control_token) {
                return json_response(
                    403,
                    "Forbidden",
                    json!({ "error": "invalid control token" }),
                );
            }
            activate_tab_response(runtime, request)
        }
        ("POST", "/api/share") | ("GET", "/api/share") => {
            if !has_valid_control_token(request, control_token) {
                return json_response(
                    403,
                    "Forbidden",
                    json!({ "error": "invalid control token" }),
                );
            }
            register_share_response(runtime, request)
        }
        ("POST", "/api/select") | ("GET", "/api/select") => {
            if !has_valid_control_token(request, control_token) {
                return json_response(
                    403,
                    "Forbidden",
                    json!({ "error": "invalid control token" }),
                );
            }
            select_browser_response(runtime, request)
        }
        _ => json_response(404, "Not Found", json!({ "error": "not found" })),
    }
}

fn request_path(target: &str) -> &str {
    target
        .split_once('?')
        .map(|(path, _)| path)
        .unwrap_or(target)
}

fn control_panel_response(runtime: Arc<Mutex<McpRuntime>>, control_token: &str) -> HttpResponse {
    let project_id = runtime
        .lock()
        .map(|runtime| runtime.config.project_id.clone())
        .unwrap_or_else(|_| "browser-connection".to_string());
    html_response(control_panel_html(control_token, &project_id))
}

fn has_valid_control_token(request: &HttpRequest, control_token: &str) -> bool {
    let provided = request
        .header("x-browser-control-token")
        .or_else(|| query_param(&request.target, "control_token"))
        .or_else(|| query_param(&request.body, "control_token"));
    provided.as_deref() == Some(control_token)
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<String> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find_map(|(key, value)| (key == &name).then(|| value.clone()))
    }
}

fn browser_inventory_response(runtime: Arc<Mutex<McpRuntime>>) -> HttpResponse {
    let result = runtime
        .lock()
        .map_err(|_| anyhow!("MCP runtime lock was poisoned"))
        .and_then(|runtime| runtime.browser_inventory());
    match result {
        Ok(inventory) => json_response(200, "OK", inventory),
        Err(error) => json_response(
            500,
            "Internal Server Error",
            json!({ "error": error.to_string() }),
        ),
    }
}

fn activity_response(runtime: Arc<Mutex<McpRuntime>>, request: &HttpRequest) -> HttpResponse {
    let refresh = query_param(&request.target, "refresh").as_deref() == Some("1");
    let result = runtime
        .lock()
        .map_err(|_| anyhow!("MCP runtime lock was poisoned"))
        .map(|mut runtime| runtime.browser_activity(refresh));
    match result {
        Ok(activity) => json_response(200, "OK", activity),
        Err(error) => json_response(
            500,
            "Internal Server Error",
            json!({ "error": error.to_string() }),
        ),
    }
}

fn activate_tab_response(runtime: Arc<Mutex<McpRuntime>>, request: &HttpRequest) -> HttpResponse {
    let Some(tab_id) =
        query_param(&request.target, "tab_id").or_else(|| query_param(&request.body, "tab_id"))
    else {
        return json_response(400, "Bad Request", json!({ "error": "tab_id is required" }));
    };
    let Ok(tab_id) = tab_id.parse::<i64>() else {
        return json_response(
            400,
            "Bad Request",
            json!({ "error": "tab_id must be an integer" }),
        );
    };

    let result = runtime
        .lock()
        .map_err(|_| anyhow!("MCP runtime lock was poisoned"))
        .and_then(|mut runtime| runtime.activate_shared_tab_from_panel(tab_id));
    match result {
        Ok(value) => json_response(200, "OK", value),
        Err(error) => json_response(400, "Bad Request", json!({ "error": error.to_string() })),
    }
}

fn select_browser_response(runtime: Arc<Mutex<McpRuntime>>, request: &HttpRequest) -> HttpResponse {
    let Some(name) =
        query_param(&request.target, "name").or_else(|| query_param(&request.body, "name"))
    else {
        return json_response(400, "Bad Request", json!({ "error": "name is required" }));
    };

    let result = runtime
        .lock()
        .map_err(|_| anyhow!("MCP runtime lock was poisoned"))
        .and_then(|mut runtime| runtime.select_browser(&name, None, None, None, None));
    match result {
        Ok(text) => {
            let value = serde_json::from_str::<Value>(&text)
                .unwrap_or_else(|_| json!({ "selected": name }));
            json_response(200, "OK", value)
        }
        Err(error) => json_response(400, "Bad Request", json!({ "error": error.to_string() })),
    }
}

fn register_share_response(runtime: Arc<Mutex<McpRuntime>>, request: &HttpRequest) -> HttpResponse {
    let Some(name) =
        query_param(&request.target, "name").or_else(|| query_param(&request.body, "name"))
    else {
        return json_response(400, "Bad Request", json!({ "error": "name is required" }));
    };
    let Some(share_url) = query_param(&request.target, "share_url")
        .or_else(|| query_param(&request.body, "share_url"))
    else {
        return json_response(
            400,
            "Bad Request",
            json!({ "error": "share_url is required" }),
        );
    };

    let result = runtime
        .lock()
        .map_err(|_| anyhow!("MCP runtime lock was poisoned"))
        .and_then(|mut runtime| runtime.select_browser(&name, None, None, None, Some(&share_url)));
    match result {
        Ok(text) => {
            let value = serde_json::from_str::<Value>(&text)
                .unwrap_or_else(|_| json!({ "selected": name }));
            json_response(200, "OK", value)
        }
        Err(error) => json_response(400, "Bad Request", json!({ "error": error.to_string() })),
    }
}

fn query_param(target: &str, name: &str) -> Option<String> {
    let query = target
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or(target);
    query.split('&').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        (url_decode(key) == name).then(|| url_decode(value))
    })
}

fn url_decode(value: &str) -> String {
    let mut decoded = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                if let (Some(high), Some(low)) =
                    (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
                {
                    decoded.push(high * 16 + low);
                    index += 2;
                } else {
                    decoded.push(bytes[index]);
                }
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&decoded).to_string()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn write_http_response(stream: &mut TcpStream, response: HttpResponse) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        response.status,
        response.reason,
        response.content_type,
        response.body.len()
    )
    .context("failed to write control panel HTTP response headers")?;
    stream
        .write_all(&response.body)
        .context("failed to write control panel HTTP response body")?;
    stream
        .flush()
        .context("failed to flush control panel HTTP response")
}

fn empty_response(status: u16, reason: &'static str) -> HttpResponse {
    HttpResponse {
        status,
        reason,
        content_type: "text/plain; charset=utf-8",
        body: Vec::new(),
    }
}

fn html_response(html: String) -> HttpResponse {
    HttpResponse {
        status: 200,
        reason: "OK",
        content_type: "text/html; charset=utf-8",
        body: html.into_bytes(),
    }
}

fn json_response(status: u16, reason: &'static str, value: Value) -> HttpResponse {
    let body =
        serde_json::to_vec_pretty(&value).unwrap_or_else(|_| b"{\"error\":\"json\"}".to_vec());
    HttpResponse {
        status,
        reason,
        content_type: "application/json; charset=utf-8",
        body,
    }
}

fn generate_control_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")
        .context("failed to open /dev/urandom for control panel token")?
        .read_exact(&mut bytes)
        .context("failed to read control panel token bytes")?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut token, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(token)
}

fn escape_js_string(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| match ch {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '"' => "\\\"".chars().collect(),
            '\n' => "\\n".chars().collect(),
            '\r' => "\\r".chars().collect(),
            '<' => "\\u003c".chars().collect(),
            '>' => "\\u003e".chars().collect(),
            '&' => "\\u0026".chars().collect(),
            _ => vec![ch],
        })
        .collect()
}

fn control_panel_html(control_token: &str, project_id: &str) -> String {
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>browser-connection</title>
<style>
:root {{
  color-scheme: light dark;
  --bg: #f7f8fa;
  --panel: #ffffff;
  --text: #1b1f24;
  --muted: #667085;
  --line: #d0d7de;
  --accent: #0f766e;
  --accent-strong: #115e59;
}}
@media (prefers-color-scheme: dark) {{
  :root {{
    --bg: #101418;
    --panel: #171c22;
    --text: #eef2f6;
    --muted: #a9b4c0;
    --line: #2b333d;
    --accent: #2dd4bf;
    --accent-strong: #5eead4;
  }}
}}
* {{ box-sizing: border-box; }}
[hidden] {{ display: none !important; }}
body {{
  margin: 0;
  min-height: 100vh;
  background: var(--bg);
  color: var(--text);
  font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}}
.shell {{
  display: grid;
  grid-template-columns: minmax(240px, 320px) minmax(0, 1fr);
  min-height: 100vh;
}}
aside {{
  border-right: 1px solid var(--line);
  background: var(--panel);
  padding: 16px;
}}
main {{
  min-width: 0;
  min-height: 100vh;
  display: flex;
  flex-direction: column;
}}
h1 {{
  margin: 0 0 16px;
  font-size: 18px;
  font-weight: 650;
}}
label {{
  display: block;
  margin-bottom: 6px;
  color: var(--muted);
  font-size: 12px;
  font-weight: 650;
  text-transform: uppercase;
}}
select, button {{
  width: 100%;
  min-height: 36px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--panel);
  color: var(--text);
  font: inherit;
}}
select {{ padding: 0 10px; }}
input {{
  width: 100%;
  min-height: 36px;
  margin-bottom: 8px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--panel);
  color: var(--text);
  font: inherit;
  padding: 0 10px;
}}
button {{
  margin-top: 10px;
  cursor: pointer;
  background: var(--accent);
  border-color: var(--accent);
  color: #ffffff;
  font-weight: 650;
}}
button:hover {{ background: var(--accent-strong); }}
.meta {{
  margin-top: 16px;
  display: grid;
  gap: 8px;
  color: var(--muted);
  font-size: 13px;
  overflow-wrap: anywhere;
}}
.share {{
  margin-top: 18px;
  padding-top: 16px;
  border-top: 1px solid var(--line);
}}
.connect {{
  margin-top: 14px;
  padding-top: 14px;
  border-top: 1px solid var(--line);
}}
.connect-status {{
  margin-top: 8px;
  min-height: 34px;
  color: var(--muted);
  font-size: 12px;
  line-height: 1.35;
  overflow-wrap: anywhere;
}}
.meta strong {{ color: var(--text); font-weight: 650; }}
.toolbar {{
  display: flex;
  align-items: center;
  gap: 10px;
  min-height: 44px;
  padding: 8px 12px;
  border-bottom: 1px solid var(--line);
  background: var(--panel);
}}
.toolbar a {{
  color: var(--accent-strong);
  text-decoration: none;
  font-weight: 650;
}}
.frame {{
  flex: 1;
  min-height: 0;
  border: 0;
  background: #000;
}}
.empty {{
  flex: 1;
  display: grid;
  place-items: center;
  color: var(--muted);
}}
.activity {{
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding: 14px;
}}
.activity-grid {{ display: grid; grid-template-columns: minmax(260px, 1fr) minmax(260px, 1fr); gap: 14px; }}
.activity-panel {{ border: 1px solid var(--line); border-radius: 6px; background: var(--panel); padding: 12px; min-width: 0; }}
.activity h2 {{ margin: 0 0 10px; font-size: 14px; }}
.stats {{ display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 8px; margin-bottom: 14px; }}
.stat {{ border: 1px solid var(--line); border-radius: 6px; padding: 8px; background: var(--panel); min-width: 0; }}
.stat strong, .row strong {{ display: block; font-size: 18px; }}
.row {{ border-top: 1px solid var(--line); padding: 8px 0; color: var(--muted); overflow-wrap: anywhere; white-space: pre-wrap; }}
.row:first-child {{ border-top: 0; padding-top: 0; }}
.thumbs {{ display: flex; gap: 8px; overflow-x: auto; margin-top: 8px; }}
.thumbs img {{ width: 96px; height: 64px; object-fit: cover; border: 1px solid var(--line); border-radius: 4px; }}
#latestScreenshot {{ width: 100%; max-height: 46vh; object-fit: contain; background: #000; border-radius: 4px; }}
.tab-button {{ margin: 6px 0 0; min-height: 28px; width: auto; padding: 0 10px; font-size: 12px; }}
.error {{ color: #ef4444; }}
@media (max-width: 760px) {{
  .shell {{ grid-template-columns: 1fr; }}
  aside {{ border-right: 0; border-bottom: 1px solid var(--line); }}
  main {{ min-height: 70vh; }}
  .activity-grid, .stats {{ grid-template-columns: 1fr; }}
}}
</style>
</head>
<body>
<div class="shell">
  <aside>
    <h1>browser-connection</h1>
    <label for="browserSelect">Browser</label>
    <select id="browserSelect"></select>
    <button id="selectButton" type="button">Select</button>
    <div class="connect">
      <button id="connectEdgeButton" type="button">Connect Edge</button>
      <div id="connectStatus" class="connect-status">Checking Edge extension</div>
    </div>
    <div class="share">
      <label for="shareName">Shared Link</label>
      <input id="shareName" value="edge" autocomplete="off" spellcheck="false">
      <input id="shareUrl" placeholder="Paste browser share link" autocomplete="off" spellcheck="false">
      <button id="shareButton" type="button">Add Shared Browser</button>
    </div>
    <div class="meta">
      <div><strong>Active</strong><br><span id="activeName">-</span></div>
      <div><strong>CDP</strong><br><span id="cdpEndpoint">-</span></div>
      <div><strong>noVNC</strong><br><span id="novncUrl">-</span></div>
    </div>
  </aside>
  <main>
    <div class="toolbar"><a id="openNovnc" href="#" target="_blank" rel="noreferrer">Open noVNC</a></div>
    <iframe id="novncFrame" class="frame" title="noVNC"></iframe>
    <div id="emptyState" class="empty" hidden>No noVNC display</div>
    <section id="activityPanel" class="activity" hidden>
      <div class="stats">
        <div class="stat"><label>Mode</label><strong id="activityMode">-</strong></div>
        <div class="stat"><label>Windows</label><strong id="windowCount">0</strong></div>
        <div class="stat"><label>Tabs</label><strong id="tabCount">0</strong></div>
        <div class="stat"><label>Events</label><strong id="eventCount">0</strong></div>
      </div>
      <div class="activity-grid">
        <section class="activity-panel"><h2>Screenshot</h2><img id="latestScreenshot" alt="" hidden><div id="screenshotEmpty" class="row">No screenshots yet</div><div id="screenshotThumbs" class="thumbs"></div></section>
        <section class="activity-panel"><h2>Tabs</h2><div id="activeTab" class="row">-</div><div id="tabsError" class="row error" hidden></div><div id="tabsList"></div></section>
        <section class="activity-panel"><h2>Activity</h2><div id="eventLog"></div></section>
      </div>
    </section>
  </main>
</div>
<script>
const personalName = "{personal}";
const projectId = "{project_id}";
const controlToken = "{control_token}";
const edgeBrowserName = "edge";
let lastActive = "";
let autoConnectStarted = false;
let activityVisible = false;

async function loadInventory() {{
  const response = await fetch("/api/browsers", {{ cache: "no-store" }});
  if (!response.ok) throw new Error(await response.text());
  const inventory = await response.json();
  renderInventory(inventory);
}}

function renderInventory(inventory) {{
  const select = document.getElementById("browserSelect");
  const previous = select.value;
  select.replaceChildren();
  for (const browser of inventory.browsers || []) {{
    const option = document.createElement("option");
    option.value = browser.name;
    option.textContent = browser.name === personalName ? "personal" : browser.name;
    if (browser.active) option.selected = true;
    select.appendChild(option);
  }}
  if (previous && [...select.options].some(option => option.value === previous)) {{
    select.value = previous;
  }}

  const active = (inventory.browsers || []).find(browser => browser.active) || null;
  document.getElementById("activeName").textContent = inventory.active || "-";
  document.getElementById("cdpEndpoint").textContent = active?.cdpEndpoint || "-";
  document.getElementById("novncUrl").textContent = active?.novncUrl || "-";
  setFrame(active?.novncUrl || "", active?.kind === "shared-extension");
}}

function setFrame(url, showActivity) {{
  const frame = document.getElementById("novncFrame");
  const empty = document.getElementById("emptyState");
  const activity = document.getElementById("activityPanel");
  const link = document.getElementById("openNovnc");
  link.href = url || "#";
  link.style.pointerEvents = url ? "auto" : "none";
  link.style.opacity = url ? "1" : "0.45";
  activityVisible = !url && showActivity;
  activity.hidden = !activityVisible;
  if (!url) {{
    frame.hidden = true;
    empty.hidden = activityVisible;
    frame.removeAttribute("src");
    lastActive = "";
    return;
  }}
  empty.hidden = true;
  frame.hidden = false;
  if (url !== lastActive) {{
    frame.src = url;
    lastActive = url;
  }}
}}

async function selectBrowser() {{
  const select = document.getElementById("browserSelect");
  const name = select.value;
  if (!name) return;
  const response = await apiFetch("/api/select?name=" + encodeURIComponent(name), {{ method: "POST" }});
  if (!response.ok) throw new Error(await response.text());
  await loadInventory();
}}

async function connectEdge(auto) {{
  if (!window.browserConnection || typeof window.browserConnection.request !== "function") {{
    setConnectStatus("Edge extension is not available on this page.");
    return;
  }}

  setConnectStatus(auto ? "Requesting Edge connection" : "Opening Edge connection request");
  const result = await window.browserConnection.request({{
    method: "connect",
    params: {{
      protocolVersion: 1,
      relayUrl: window.location.origin,
      workspaceId: projectId,
      poolId: "current-runtime",
      browserName: edgeBrowserName,
      displayName: "Edge"
    }}
  }});
  if (!result || !result.shareUrl) {{
    throw new Error("Edge extension did not return a share URL");
  }}
  await registerShare(edgeBrowserName, result.shareUrl);
  setConnectStatus("Edge connected to this browser pool.");
}}

async function registerShare(name, shareUrl) {{
  const body = new URLSearchParams({{ name, share_url: shareUrl }});
  const response = await apiFetch("/api/share", {{
    method: "POST",
    headers: {{ "Content-Type": "application/x-www-form-urlencoded" }},
    body
  }});
  if (!response.ok) throw new Error(await response.text());
  await loadInventory();
}}

function maybeAutoConnectEdge() {{
  if (autoConnectStarted) return;
  autoConnectStarted = true;
  if (!window.browserConnection || typeof window.browserConnection.request !== "function") {{
    setConnectStatus("Install or enable Edge Share extension to connect this Edge.");
    return;
  }}
  const key = "browserConnectionAutoConnect:" + window.location.origin + window.location.pathname;
  if (sessionStorage.getItem(key) === "1") {{
    setConnectStatus("Edge Share extension detected.");
    return;
  }}
  sessionStorage.setItem(key, "1");
  connectEdge(true).catch(error => setConnectStatus(error.message || String(error)));
}}

async function loadActivity() {{
  if (!activityVisible) return;
  const response = await apiFetch("/api/activity?refresh=1", {{ cache: "no-store" }});
  if (!response.ok) throw new Error(await response.text());
  renderActivity(await response.json());
}}

function renderActivity(activity) {{
  const tabs = activity.tabs || {{}};
  const events = activity.events || [];
  const shots = activity.screenshots || [];
  text("activityMode", activity.mode || "-");
  text("windowCount", tabs.totalWindows || 0);
  text("tabCount", tabs.totalTabs || 0);
  text("eventCount", events.length);
  const active = tabs.activeTab || null;
  document.getElementById("activeTab").textContent = active ? (active.title || "(untitled)") + "\\n" + (active.url || "") : "-";
  const err = document.getElementById("tabsError");
  err.hidden = !activity.tabsError;
  err.textContent = activity.tabsError || "";
  renderScreenshot(activity.latestScreenshot, shots);
  renderTabs(tabs.windows || []);
  renderEvents(events);
}}

function renderScreenshot(latest, shots) {{
  const image = document.getElementById("latestScreenshot");
  const empty = document.getElementById("screenshotEmpty");
  image.hidden = !latest?.dataUrl;
  empty.hidden = !!latest?.dataUrl;
  if (latest?.dataUrl) image.src = latest.dataUrl;
  const thumbs = document.getElementById("screenshotThumbs");
  thumbs.replaceChildren(...shots.slice(0, 8).filter(s => s.dataUrl).map(s => el("img", {{ src: s.dataUrl, title: new Date(s.at).toLocaleTimeString() }})));
}}

function renderTabs(windows) {{
  const rows = [];
  for (const win of windows) {{
    rows.push(el("div", {{ className: "row" }}, "Window " + (win.windowId ?? win.id ?? "-") + " " + (win.profile || (win.incognito ? "incognito" : "regular")) + " " + (win.focused ? "focused " : "") + (win.type || "") + " " + (win.state || "") + "\\n" + (win.tabCount || 0) + " tabs"));
    for (const tab of win.tabs || []) {{
      const row = el("div", {{ className: "row" }}, (tab.active ? "Active: " : "") + (tab.profile || "") + " " + (tab.title || "(untitled)") + "\\n" + (tab.url || ""));
      row.appendChild(el("button", {{ className: "tab-button", onclick: () => activateTab(tab.id) }}, "Activate"));
      rows.push(row);
    }}
  }}
  document.getElementById("tabsList").replaceChildren(...rows);
}}

function renderEvents(events) {{
  document.getElementById("eventLog").replaceChildren(...events.slice(0, 40).map(event => {{
    const status = event.ok ? "ok" : "error";
    const summary = event.error || event.result?.url || event.result?.title || event.result?.text || "";
    return el("div", {{ className: "row" }}, new Date(event.at).toLocaleTimeString() + " " + event.tool + " " + status + " " + event.durationMs + "ms\\n" + String(summary).slice(0, 240));
  }}));
}}

async function activateTab(tabId) {{
  if (!Number.isInteger(tabId)) return;
  await apiFetch("/api/activate-tab?tab_id=" + encodeURIComponent(tabId), {{ method: "POST" }});
  await loadActivity();
}}

function text(id, value) {{ document.getElementById(id).textContent = value; }}
function el(tag, props = {{}}, body = "") {{
  const node = document.createElement(tag);
  Object.assign(node, props);
  if (body) node.textContent = body;
  return node;
}}

function setConnectStatus(message) {{
  document.getElementById("connectStatus").textContent = message;
}}

function apiFetch(url, options = {{}}) {{
  const headers = new Headers(options.headers || {{}});
  headers.set("X-Browser-Control-Token", controlToken);
  return fetch(url, {{ ...options, headers }});
}}

document.getElementById("selectButton").addEventListener("click", () => {{
  selectBrowser().catch(error => console.error(error));
}});
document.getElementById("browserSelect").addEventListener("change", () => {{
  selectBrowser().catch(error => console.error(error));
}});
document.getElementById("connectEdgeButton").addEventListener("click", () => {{
  sessionStorage.removeItem("browserConnectionAutoConnect:" + window.location.origin + window.location.pathname);
  connectEdge(false).catch(error => setConnectStatus(error.message || String(error)));
}});
document.getElementById("shareButton").addEventListener("click", async () => {{
  const name = document.getElementById("shareName").value.trim() || "edge";
  const shareUrl = document.getElementById("shareUrl").value.trim();
  if (!shareUrl) return;
  await registerShare(name, shareUrl);
}});
window.addEventListener("browserConnection#initialized", maybeAutoConnectEdge);
setTimeout(maybeAutoConnectEdge, 300);
loadInventory().catch(error => console.error(error));
setInterval(() => loadInventory().catch(error => console.error(error)), 1000);
setInterval(() => loadActivity().catch(error => console.error(error)), 1000);
</script>
</body>
</html>
"##,
        personal = PERSONAL_BROWSER_NAME,
        project_id = escape_js_string(project_id),
        control_token = control_token
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_param_decodes_names() {
        assert_eq!(
            query_param("/api/select?name=personal+browser", "name").as_deref(),
            Some("personal browser")
        );
        assert_eq!(
            query_param("/api/select?name=work%2Dbrowser", "name").as_deref(),
            Some("work-browser")
        );
    }

    #[test]
    fn validates_control_token_from_header() {
        let request = parse_http_request(
            b"POST /api/share HTTP/1.1\r\nX-Browser-Control-Token: secret\r\nContent-Length: 0\r\n\r\n",
        )
        .expect("request parses");

        assert!(has_valid_control_token(&request, "secret"));
        assert!(!has_valid_control_token(&request, "other"));
    }
}
