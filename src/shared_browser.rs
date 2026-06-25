/*! Bounded WebSocket relay for browser share sessions.

CHANGE: pair browser-extension clients and agent clients through an outbound-friendly relay.
WHY: remote browsers need simple share links without exposing CDP/VNC ports.
PURITY: SHELL
EFFECT: listens for WebSocket clients and forwards bounded JSON messages by request id.
INVARIANT: browser clients register sessions; agent clients must present the matching agent token.
*/

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;
use tungstenite::handshake::server::{Callback, ErrorResponse, Request, Response};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{accept_hdr_with_config, connect, Error as WsError, Message, WebSocket};

pub const DEFAULT_RELAY_BIND: &str = "127.0.0.1:8765";
pub const MAX_SESSION_ID_BYTES: usize = 128;
pub const MAX_TOKEN_BYTES: usize = 512;
pub const MAX_JSON_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SESSIONS: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayConfig {
    pub bind: String,
}

impl RelayConfig {
    pub fn new(bind: impl Into<String>) -> Self {
        Self { bind: bind.into() }
    }
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self::new(DEFAULT_RELAY_BIND)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareAgentUrl {
    pub session: String,
    pub agent_token: String,
    pub websocket_url: String,
}

pub fn parse_share_agent_url(input: &str) -> Result<ShareAgentUrl> {
    let input = input.trim();
    let (scheme, rest) = input
        .split_once("://")
        .ok_or_else(|| anyhow!("share URL must include http:// or https:// scheme"))?;
    let websocket_scheme = match scheme {
        "http" | "ws" => "ws",
        "https" | "wss" => "wss",
        _ => return Err(anyhow!("share URL scheme must be http, https, ws, or wss")),
    };
    if (input.starts_with("ws://") || input.starts_with("wss://")) && input.contains("/ws/agent/") {
        let endpoint =
            RelayEndpoint::parse(rest.split_once('/').map(|(_, path)| path).unwrap_or(rest))?;
        if let RelayEndpoint::Agent {
            session,
            agent_token,
        } = endpoint
        {
            return Ok(ShareAgentUrl {
                session,
                agent_token,
                websocket_url: input.to_string(),
            });
        }
    }

    let (without_fragment, fragment) = input
        .split_once('#')
        .ok_or_else(|| anyhow!("share URL must include #agent=<token> fragment"))?;
    let agent_token = query_param(fragment, "agent")
        .ok_or_else(|| anyhow!("share URL fragment must include agent token"))?;
    let rest = without_fragment
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(rest);
    let (authority, path_and_query) = rest
        .split_once('/')
        .ok_or_else(|| anyhow!("share URL must include /share/<session> path"))?;
    if authority.trim().is_empty() {
        return Err(anyhow!("share URL host is required"));
    }
    let (path, query) = path_and_query
        .split_once('?')
        .unwrap_or((path_and_query, ""));
    let session = parse_share_path(path)?;
    let agent_token = query_param(fragment, "agent")
        .or_else(|| query_param(query, "agent"))
        .unwrap_or(agent_token);
    let agent_token = validate_token("agent token", &agent_token)?;
    let websocket_url =
        format!("{websocket_scheme}://{authority}/ws/agent/{session}?token={agent_token}");

    Ok(ShareAgentUrl {
        session,
        agent_token,
        websocket_url,
    })
}

pub fn agent_ws_url_from_share_url(input: &str) -> Result<String> {
    Ok(parse_share_agent_url(input)?.websocket_url)
}

pub fn agent_websocket_url_from_share_url(input: &str) -> Result<String> {
    agent_ws_url_from_share_url(input)
}

pub fn run_relay(config: RelayConfig) -> Result<()> {
    let listener = TcpListener::bind(&config.bind)
        .with_context(|| format!("failed to bind browser share relay on {}", config.bind))?;
    serve_listener(listener)
}

pub fn serve_listener(listener: TcpListener) -> Result<()> {
    let relay = BrowserShareRelay::new();
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let relay = relay.clone();
                thread::Builder::new()
                    .name("browser-share-relay-client".to_string())
                    .spawn(move || {
                        let _ = relay.handle_stream(stream);
                    })
                    .context("failed to spawn relay client thread")?;
            }
            Err(error) => return Err(error).context("failed to accept relay TCP connection"),
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct BrowserShareRelay {
    state: SharedRelayState,
}

impl BrowserShareRelay {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RelayState::default()),
        }
    }

    pub fn handle_stream(&self, stream: TcpStream) -> Result<()> {
        handle_client(Arc::clone(&self.state), stream)
    }
}

impl Default for BrowserShareRelay {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedBrowserClient {
    share_url: String,
}

impl SharedBrowserClient {
    pub fn new(share_url: impl Into<String>) -> Self {
        Self {
            share_url: share_url.into(),
        }
    }

    pub fn navigate(&self, url: &str) -> Result<String> {
        self.call_text("navigate", json!({ "url": url }))
    }

    pub fn snapshot(&self) -> Result<String> {
        self.call_text("snapshot", json!({}))
    }

    pub fn evaluate(&self, expression: &str) -> Result<String> {
        self.call_text("evaluate", json!({ "expression": expression }))
    }

    pub fn click(&self, selector: &str) -> Result<String> {
        self.call_text("click", json!({ "selector": selector }))
    }

    pub fn type_text(&self, selector: &str, text: &str) -> Result<String> {
        self.call_text("type", json!({ "selector": selector, "text": text }))
    }

    pub fn press_key(&self, key: &str) -> Result<String> {
        self.call_text("press_key", json!({ "key": key }))
    }

    pub fn screenshot(&self, full_page: bool) -> Result<String> {
        self.call_text("screenshot", json!({ "fullPage": full_page }))
    }

    pub fn list_tabs(&self) -> Result<Value> {
        self.call("list_tabs", json!({}))
    }

    pub fn activate_tab(&self, tab_id: i64) -> Result<String> {
        self.call_text("activate_tab", json!({ "tabId": tab_id }))
    }

    pub fn call_command(&self, command: &str, params: Value) -> Result<Value> {
        self.call(command, params)
    }

    fn call_text(&self, command: &str, params: Value) -> Result<String> {
        let result = self.call(command, params)?;
        if let Some(text) = result.as_str() {
            return Ok(text.to_string());
        }
        serde_json::to_string_pretty(&result).context("failed to render shared browser response")
    }

    fn call(&self, command: &str, params: Value) -> Result<Value> {
        let websocket_url = agent_ws_url_from_share_url(&self.share_url)?;
        let (mut socket, _) = connect(&websocket_url).with_context(|| {
            format!("failed to connect to shared browser relay {websocket_url}")
        })?;
        let request = json!({ "id": 1, "command": command, "params": params });
        socket
            .send(Message::Text(request.to_string()))
            .with_context(|| format!("failed to send shared browser command {command}"))?;

        loop {
            match socket
                .read()
                .with_context(|| format!("failed to read shared browser response for {command}"))?
            {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(&text).with_context(|| {
                        format!("shared browser response for {command} was not JSON")
                    })?;
                    if value.get("id").and_then(Value::as_i64) != Some(1) {
                        continue;
                    }
                    if value.get("ok").and_then(Value::as_bool) == Some(false) {
                        let error = value
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("shared browser command failed");
                        return Err(anyhow!("{error}"));
                    }
                    return Ok(value.get("result").cloned().unwrap_or(Value::Null));
                }
                Message::Ping(payload) => {
                    socket
                        .send(Message::Pong(payload))
                        .context("failed to answer shared browser relay ping")?;
                }
                Message::Close(_) => {
                    return Err(anyhow!(
                        "shared browser relay closed before {command} response"
                    ))
                }
                _ => {}
            }
        }
    }
}

type SharedRelayState = Arc<RelayState>;

#[derive(Debug, Default)]
struct RelayState {
    sessions: Mutex<HashMap<String, RelaySession>>,
    next_connection_id: AtomicU64,
}

#[derive(Debug)]
struct RelaySession {
    browser_token: String,
    agent_token: String,
    browser: Option<PeerHandle>,
    agent: Option<PeerHandle>,
}

#[derive(Debug, Clone)]
struct PeerHandle {
    connection_id: u64,
    tx: mpsc::Sender<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerRole {
    Browser,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RelayEndpoint {
    Browser {
        session: String,
        browser_token: String,
        agent_token: String,
    },
    Agent {
        session: String,
        agent_token: String,
    },
}

impl RelayState {
    fn register(
        &self,
        endpoint: &RelayEndpoint,
        tx: mpsc::Sender<String>,
    ) -> Result<(String, PeerRole, u64)> {
        let connection_id = self.next_connection_id.fetch_add(1, Ordering::Relaxed) + 1;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| anyhow!("relay session lock was poisoned"))?;
        match endpoint {
            RelayEndpoint::Browser {
                session,
                browser_token,
                agent_token,
            } => {
                if !sessions.contains_key(session) && sessions.len() >= MAX_SESSIONS {
                    return Err(anyhow!("relay session limit reached"));
                }
                let session_entry =
                    sessions
                        .entry(session.clone())
                        .or_insert_with(|| RelaySession {
                            browser_token: browser_token.clone(),
                            agent_token: agent_token.clone(),
                            browser: None,
                            agent: None,
                        });
                if session_entry.browser_token != *browser_token {
                    return Err(anyhow!("browser token did not match session"));
                }
                if session_entry.agent_token != *agent_token {
                    return Err(anyhow!("agent token did not match session"));
                }
                session_entry.browser = Some(PeerHandle { connection_id, tx });
                Ok((session.clone(), PeerRole::Browser, connection_id))
            }
            RelayEndpoint::Agent {
                session,
                agent_token,
            } => {
                let session_entry = sessions
                    .get_mut(session)
                    .ok_or_else(|| anyhow!("relay session is not registered"))?;
                if session_entry.agent_token != *agent_token {
                    return Err(anyhow!("agent token did not match session"));
                }
                session_entry.agent = Some(PeerHandle { connection_id, tx });
                Ok((session.clone(), PeerRole::Agent, connection_id))
            }
        }
    }

    fn unregister(&self, session: &str, role: PeerRole, connection_id: u64) {
        let Ok(mut sessions) = self.sessions.lock() else {
            return;
        };
        let Some(session_entry) = sessions.get_mut(session) else {
            return;
        };
        match role {
            PeerRole::Browser => {
                if session_entry
                    .browser
                    .as_ref()
                    .is_some_and(|peer| peer.connection_id == connection_id)
                {
                    sessions.remove(session);
                }
            }
            PeerRole::Agent => {
                if session_entry
                    .agent
                    .as_ref()
                    .is_some_and(|peer| peer.connection_id == connection_id)
                {
                    session_entry.agent = None;
                }
            }
        }
    }

    fn route_message(
        &self,
        session: &str,
        from: PeerRole,
        connection_id: u64,
        raw_message: &str,
    ) -> Result<()> {
        let message = match validate_relay_json_message(from, raw_message)? {
            Some(message) => message,
            None => return Ok(()),
        };
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| anyhow!("relay session lock was poisoned"))?;
        let session_entry = sessions
            .get(session)
            .ok_or_else(|| anyhow!("relay session is not registered"))?;
        let sender = match from {
            PeerRole::Browser => session_entry.browser.as_ref(),
            PeerRole::Agent => session_entry.agent.as_ref(),
        }
        .ok_or_else(|| anyhow!("relay sender is not connected"))?;
        if sender.connection_id != connection_id {
            return Err(anyhow!("relay sender connection is stale"));
        }
        let target = match from {
            PeerRole::Browser => session_entry.agent.as_ref(),
            PeerRole::Agent => session_entry.browser.as_ref(),
        }
        .ok_or_else(|| anyhow!("relay peer is not connected"))?;
        target
            .tx
            .send(message)
            .context("failed to forward relay message")
    }
}

impl RelayEndpoint {
    fn parse(target: &str) -> Result<Self> {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let segments = path
            .trim_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        match segments.as_slice() {
            ["ws", "browser", session] => Ok(Self::Browser {
                session: validate_session_id(session)?,
                browser_token: validate_token(
                    "browser token",
                    &query_param(query, "token")
                        .ok_or_else(|| anyhow!("browser token is required"))?,
                )?,
                agent_token: validate_token(
                    "agent token",
                    &query_param(query, "agent_token")
                        .ok_or_else(|| anyhow!("agent token is required"))?,
                )?,
            }),
            ["ws", "agent", session] => Ok(Self::Agent {
                session: validate_session_id(session)?,
                agent_token: validate_token(
                    "agent token",
                    &query_param(query, "token")
                        .ok_or_else(|| anyhow!("agent token is required"))?,
                )?,
            }),
            _ => Err(anyhow!(
                "relay endpoint must be /ws/browser/<session> or /ws/agent/<session>"
            )),
        }
    }

    fn role(&self) -> PeerRole {
        match self {
            Self::Browser { .. } => PeerRole::Browser,
            Self::Agent { .. } => PeerRole::Agent,
        }
    }

    fn session(&self) -> &str {
        match self {
            Self::Browser { session, .. } | Self::Agent { session, .. } => session,
        }
    }
}

fn handle_client(state: SharedRelayState, stream: TcpStream) -> Result<()> {
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
    let target = Arc::new(Mutex::new(String::new()));
    let callback = TargetCapture {
        target: Arc::clone(&target),
    };
    let mut websocket = accept_hdr_with_config(stream, callback, Some(websocket_config()))
        .context("failed to accept relay websocket")?;
    let target = target
        .lock()
        .map_err(|_| anyhow!("relay request target lock was poisoned"))?
        .clone();
    let endpoint = RelayEndpoint::parse(&target)?;
    let (tx, rx) = mpsc::channel();
    let (session, role, connection_id) = state.register(&endpoint, tx)?;
    let result = relay_client_loop(
        &state,
        endpoint.session(),
        endpoint.role(),
        connection_id,
        &mut websocket,
        rx,
    );
    state.unregister(&session, role, connection_id);
    result
}

struct TargetCapture {
    target: Arc<Mutex<String>>,
}

impl Callback for TargetCapture {
    #[allow(clippy::result_large_err)]
    fn on_request(
        self,
        request: &Request,
        response: Response,
    ) -> std::result::Result<Response, ErrorResponse> {
        if let Ok(mut target) = self.target.lock() {
            *target = request.uri().to_string();
        }
        Ok(response)
    }
}

fn relay_client_loop(
    state: &SharedRelayState,
    session: &str,
    role: PeerRole,
    connection_id: u64,
    websocket: &mut WebSocket<TcpStream>,
    rx: mpsc::Receiver<String>,
) -> Result<()> {
    loop {
        drain_outbound(websocket, &rx)?;
        match websocket.read() {
            Ok(message) => {
                if message.is_close() {
                    return Ok(());
                }
                if message.is_ping() || message.is_pong() {
                    continue;
                }
                let text = message
                    .to_text()
                    .context("relay message was not valid utf8")?;
                state.route_message(session, role, connection_id, text)?;
            }
            Err(WsError::Io(error))
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => return Ok(()),
            Err(error) => return Err(error).context("relay websocket read failed"),
        }
    }
}

fn drain_outbound(websocket: &mut WebSocket<TcpStream>, rx: &mpsc::Receiver<String>) -> Result<()> {
    loop {
        match rx.try_recv() {
            Ok(message) => websocket
                .send(Message::Text(message))
                .context("failed to send relay websocket message")?,
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
        }
    }
}

fn websocket_config() -> WebSocketConfig {
    WebSocketConfig {
        max_message_size: Some(MAX_JSON_MESSAGE_BYTES),
        max_frame_size: Some(MAX_JSON_MESSAGE_BYTES),
        max_write_buffer_size: MAX_JSON_MESSAGE_BYTES * 2,
        ..WebSocketConfig::default()
    }
}

fn parse_share_path(path: &str) -> Result<String> {
    let segments = path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    match segments.as_slice() {
        ["share", session] => validate_session_id(session),
        _ => Err(anyhow!("share URL path must be /share/<session>")),
    }
}

fn validate_relay_json_message(from: PeerRole, raw_message: &str) -> Result<Option<String>> {
    if raw_message.len() > MAX_JSON_MESSAGE_BYTES {
        return Err(anyhow!("relay JSON message exceeded size limit"));
    }
    let value: Value = serde_json::from_str(raw_message).context("relay message was not JSON")?;
    let Some(id) = value.get("id") else {
        if from == PeerRole::Browser {
            return Ok(None);
        }
        return Err(anyhow!("relay JSON message must include request id"));
    };
    if id.is_null() || id.is_array() || id.is_object() {
        return Err(anyhow!(
            "relay JSON message id must be a string, number, or boolean"
        ));
    }
    serde_json::to_string(&value)
        .map(Some)
        .context("failed to encode relay JSON message")
}

fn validate_session_id(value: &str) -> Result<String> {
    let value = decode_url_component(value).trim().to_string();
    if value.is_empty() {
        return Err(anyhow!("session id is required"));
    }
    if value.len() > MAX_SESSION_ID_BYTES {
        return Err(anyhow!("session id is too long"));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(anyhow!("session id contains unsupported characters"));
    }
    Ok(value)
}

fn validate_token(name: &str, value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow!("{name} is required"));
    }
    if value.len() > MAX_TOKEN_BYTES {
        return Err(anyhow!("{name} is too long"));
    }
    if value
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(anyhow!(
            "{name} contains unsupported whitespace/control characters"
        ));
    }
    Ok(value.to_string())
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        (decode_url_component(key) == name).then(|| decode_url_component(value))
    })
}

fn decode_url_component(value: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn parses_share_url_into_agent_websocket_url() {
        let parsed =
            parse_share_agent_url("http://relay.local/share/edge-1#agent=agent-token").unwrap();

        assert_eq!(parsed.session, "edge-1");
        assert_eq!(parsed.agent_token, "agent-token");
        assert_eq!(
            parsed.websocket_url,
            "ws://relay.local/ws/agent/edge-1?token=agent-token"
        );
    }

    #[test]
    fn parses_https_share_url_into_wss_url() {
        let ws_url =
            agent_ws_url_from_share_url("https://relay.example.com/share/s_1#agent=a%2Db").unwrap();

        assert_eq!(ws_url, "wss://relay.example.com/ws/agent/s_1?token=a-b");
    }

    #[test]
    fn preserves_direct_agent_websocket_url() {
        let url = agent_websocket_url_from_share_url("ws://127.0.0.1:8787/ws/agent/abc?token=tok")
            .unwrap();

        assert_eq!(url, "ws://127.0.0.1:8787/ws/agent/abc?token=tok");
    }

    #[test]
    fn rejects_share_url_without_agent_fragment() {
        let error = parse_share_agent_url("http://relay.local/share/edge-1").unwrap_err();

        assert!(error.to_string().contains("#agent"));
    }

    #[test]
    fn parses_browser_and_agent_ws_endpoints() {
        let browser =
            RelayEndpoint::parse("/ws/browser/edge-1?token=browser-token&agent_token=agent-token")
                .unwrap();
        let agent = RelayEndpoint::parse("/ws/agent/edge-1?token=agent-token").unwrap();

        assert_eq!(
            browser,
            RelayEndpoint::Browser {
                session: "edge-1".to_string(),
                browser_token: "browser-token".to_string(),
                agent_token: "agent-token".to_string()
            }
        );
        assert_eq!(
            agent,
            RelayEndpoint::Agent {
                session: "edge-1".to_string(),
                agent_token: "agent-token".to_string()
            }
        );
    }

    #[test]
    fn rejects_messages_without_request_id() {
        let error =
            validate_relay_json_message(PeerRole::Agent, r#"{"method":"ping"}"#).unwrap_err();

        assert!(error.to_string().contains("request id"));
    }

    #[test]
    fn ignores_browser_events_without_request_id() {
        let result = validate_relay_json_message(PeerRole::Browser, r#"{"type":"hello"}"#).unwrap();

        assert_eq!(result, None);
    }

    #[test]
    fn forwards_json_messages_between_browser_and_agent() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let _ = serve_listener(listener);
        });

        let browser_url = format!(
            "ws://127.0.0.1:{port}/ws/browser/session-1?token=browser-token&agent_token=agent-token"
        );
        let agent_url = format!("ws://127.0.0.1:{port}/ws/agent/session-1?token=agent-token");
        let (mut browser, _) = connect(browser_url).unwrap();
        let (mut agent, _) = connect(agent_url).unwrap();

        agent
            .send(Message::Text(r#"{"id":"1","method":"ping"}"#.to_string()))
            .unwrap();
        let forwarded_to_browser = browser.read().unwrap().to_text().unwrap().to_string();
        assert_eq!(forwarded_to_browser, r#"{"id":"1","method":"ping"}"#);

        browser
            .send(Message::Text(r#"{"id":"1","result":"pong"}"#.to_string()))
            .unwrap();
        let forwarded_to_agent = agent.read().unwrap().to_text().unwrap().to_string();
        assert_eq!(forwarded_to_agent, r#"{"id":"1","result":"pong"}"#);
    }
}
