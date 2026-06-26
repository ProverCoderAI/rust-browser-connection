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
        ("GET", "/api/recording") => {
            if !has_valid_control_token(request, control_token) {
                return json_response(
                    403,
                    "Forbidden",
                    json!({ "error": "invalid control token" }),
                );
            }
            recording_response(runtime, "state")
        }
        ("POST", "/api/recording/start") | ("GET", "/api/recording/start") => {
            if !has_valid_control_token(request, control_token) {
                return json_response(
                    403,
                    "Forbidden",
                    json!({ "error": "invalid control token" }),
                );
            }
            recording_response(runtime, "start")
        }
        ("POST", "/api/recording/record") | ("GET", "/api/recording/record") => {
            if !has_valid_control_token(request, control_token) {
                return json_response(
                    403,
                    "Forbidden",
                    json!({ "error": "invalid control token" }),
                );
            }
            recording_response(runtime, "record")
        }
        ("POST", "/api/recording/inspect") | ("GET", "/api/recording/inspect") => {
            if !has_valid_control_token(request, control_token) {
                return json_response(
                    403,
                    "Forbidden",
                    json!({ "error": "invalid control token" }),
                );
            }
            recording_response(runtime, "inspect")
        }
        ("POST", "/api/recording/stop") | ("GET", "/api/recording/stop") => {
            if !has_valid_control_token(request, control_token) {
                return json_response(
                    403,
                    "Forbidden",
                    json!({ "error": "invalid control token" }),
                );
            }
            recording_response(runtime, "stop")
        }
        ("POST", "/api/recording/clear") | ("GET", "/api/recording/clear") => {
            if !has_valid_control_token(request, control_token) {
                return json_response(
                    403,
                    "Forbidden",
                    json!({ "error": "invalid control token" }),
                );
            }
            recording_response(runtime, "clear")
        }
        ("POST", "/api/recording/play") | ("GET", "/api/recording/play") => {
            if !has_valid_control_token(request, control_token) {
                return json_response(
                    403,
                    "Forbidden",
                    json!({ "error": "invalid control token" }),
                );
            }
            recording_response(runtime, "play")
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

fn recording_response(runtime: Arc<Mutex<McpRuntime>>, action: &str) -> HttpResponse {
    let result = runtime
        .lock()
        .map_err(|_| anyhow!("MCP runtime lock was poisoned"))
        .and_then(|runtime| match action {
            "state" => runtime.shared_recording_state_from_panel(),
            "start" => runtime.start_shared_recording_from_panel(),
            "record" => runtime.set_shared_recording_mode_from_panel("record"),
            "inspect" => runtime.set_shared_recording_mode_from_panel("inspect"),
            "stop" => runtime.stop_shared_recording_from_panel(),
            "clear" => runtime.clear_shared_recording_from_panel(),
            "play" => runtime.play_shared_recording_from_panel(),
            _ => Err(anyhow!("unknown recording action")),
        });
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
    include_str!("control_panel.html")
        .replace(
            "__PERSONAL_BROWSER_NAME__",
            &escape_js_string(PERSONAL_BROWSER_NAME),
        )
        .replace("__PROJECT_ID__", &escape_js_string(project_id))
        .replace("__CONTROL_TOKEN__", &escape_js_string(control_token))
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
