use super::{McpRuntime, PERSONAL_BROWSER_NAME};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
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
    thread::Builder::new()
        .name("browser-control-panel".to_string())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let runtime = Arc::clone(&runtime);
                thread::Builder::new()
                    .name("browser-control-panel-client".to_string())
                    .spawn(move || {
                        let _ = handle_connection(runtime, stream);
                    })
                    .ok();
            }
        })
        .context("failed to spawn browser control panel thread")?;

    Ok(())
}

fn handle_connection(runtime: Arc<Mutex<McpRuntime>>, mut stream: TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(3))).ok();

    let request = read_http_request(&mut stream)?;
    let response = route_request(runtime, &request);
    write_http_response(&mut stream, response)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpRequest {
    method: String,
    target: String,
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
    let body = String::from_utf8_lossy(&data[header_end + 4..]).to_string();

    Ok(HttpRequest {
        method,
        target,
        body,
    })
}

fn route_request(runtime: Arc<Mutex<McpRuntime>>, request: &HttpRequest) -> HttpResponse {
    match (request.method.as_str(), request_path(&request.target)) {
        ("OPTIONS", _) => empty_response(204, "No Content"),
        ("GET", "/") | ("GET", "/index.html") => html_response(control_panel_html()),
        ("GET", "/api/browsers") => browser_inventory_response(runtime),
        ("POST", "/api/share") | ("GET", "/api/share") => register_share_response(runtime, request),
        ("POST", "/api/select") | ("GET", "/api/select") => {
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
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\n\r\n",
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

fn control_panel_html() -> String {
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
@media (max-width: 760px) {{
  .shell {{ grid-template-columns: 1fr; }}
  aside {{ border-right: 0; border-bottom: 1px solid var(--line); }}
  main {{ min-height: 70vh; }}
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
  </main>
</div>
<script>
const personalName = "{personal}";
let lastActive = "";

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
  setFrame(active?.novncUrl || "");
}}

function setFrame(url) {{
  const frame = document.getElementById("novncFrame");
  const empty = document.getElementById("emptyState");
  const link = document.getElementById("openNovnc");
  link.href = url || "#";
  link.style.pointerEvents = url ? "auto" : "none";
  link.style.opacity = url ? "1" : "0.45";
  if (!url) {{
    frame.hidden = true;
    empty.hidden = false;
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
  const response = await fetch("/api/select?name=" + encodeURIComponent(name), {{ method: "POST" }});
  if (!response.ok) throw new Error(await response.text());
  await loadInventory();
}}

document.getElementById("selectButton").addEventListener("click", () => {{
  selectBrowser().catch(error => console.error(error));
}});
document.getElementById("browserSelect").addEventListener("change", () => {{
  selectBrowser().catch(error => console.error(error));
}});
document.getElementById("shareButton").addEventListener("click", async () => {{
  const name = document.getElementById("shareName").value.trim() || "edge";
  const shareUrl = document.getElementById("shareUrl").value.trim();
  if (!shareUrl) return;
  const body = new URLSearchParams({{ name, share_url: shareUrl }});
  const response = await fetch("/api/share", {{
    method: "POST",
    headers: {{ "Content-Type": "application/x-www-form-urlencoded" }},
    body
  }});
  if (!response.ok) throw new Error(await response.text());
  await loadInventory();
}});
loadInventory().catch(error => console.error(error));
setInterval(() => loadInventory().catch(error => console.error(error)), 1000);
</script>
</body>
</html>
"##,
        personal = PERSONAL_BROWSER_NAME
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
}
