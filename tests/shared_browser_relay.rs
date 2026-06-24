use std::io::Write;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use tungstenite::{connect, stream::MaybeTlsStream, Message, WebSocket};

fn unused_local_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind an unused local port");
    listener
        .local_addr()
        .expect("read local listener address")
        .port()
}

fn encode_message(value: Value) -> Vec<u8> {
    let body = serde_json::to_vec(&value).expect("message body serializes");
    let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    framed.extend_from_slice(&body);
    framed
}

fn decode_messages(mut stdout: &[u8]) -> Vec<Value> {
    let mut responses = Vec::new();

    while !stdout.is_empty() {
        let split = stdout
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("frame separator present");
        let (header, rest) = stdout.split_at(split);
        let body_start = &rest[4..];
        let header_text = String::from_utf8(header.to_vec()).expect("header is utf8");
        let content_length = header_text
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("Content-Length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
            })
            .expect("Content-Length header present");
        let (body, remaining) = body_start.split_at(content_length);
        responses.push(serde_json::from_slice(body).expect("frame body is JSON"));
        stdout = remaining;
    }

    responses
}

fn retry_browser_connect(url: &str) -> WebSocket<MaybeTlsStream<std::net::TcpStream>> {
    let mut last_error = None;
    for _ in 0..100 {
        match connect(url) {
            Ok((socket, _)) => return socket,
            Err(error) => {
                last_error = Some(error);
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
    panic!("browser websocket did not connect: {last_error:?}");
}

fn stop_child(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn mcp_navigates_shared_browser_through_relay_link() {
    let relay_port = unused_local_port();
    let relay_bind = format!("127.0.0.1:{relay_port}");
    let relay = Command::new(env!("CARGO_BIN_EXE_browser-connection-relay"))
        .args(["--bind", &relay_bind])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn browser-connection-relay");

    let session = "session-test";
    let agent_token = "agent-token";
    let browser_url = format!(
        "ws://127.0.0.1:{relay_port}/ws/browser/{session}?token=browser-token&agent_token={agent_token}"
    );
    let (ready_tx, ready_rx) = mpsc::channel();
    let browser_thread = thread::spawn(move || {
        let mut socket = retry_browser_connect(&browser_url);
        ready_tx.send(()).expect("signal browser websocket ready");
        loop {
            let message = socket.read().expect("read relayed browser command");
            if let Message::Text(text) = message {
                let request: Value = serde_json::from_str(&text).expect("request is JSON");
                assert_eq!(request["command"], "navigate");
                assert_eq!(request["params"]["url"], "https://example.com/");
                socket
                    .send(Message::Text(
                        json!({
                            "id": request["id"].clone(),
                            "ok": true,
                            "result": "Navigated remote Edge"
                        })
                        .to_string(),
                    ))
                    .expect("send relayed browser response");
                break;
            }
        }
    });
    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("browser websocket connects to relay");

    let share_url = format!("http://127.0.0.1:{relay_port}/share/{session}#agent={agent_token}");
    let mut child = Command::new(env!("CARGO_BIN_EXE_browser-connection"))
        .args([
            "--project",
            "dg-shared-browser-test",
            "--no-start-browser",
            "--no-control-panel",
            "--browser-share",
            &format!("edge={share_url}"),
            "--active-browser",
            "edge",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn browser-connection MCP server");

    {
        let stdin = child.stdin.as_mut().expect("stdin is piped");
        let mut input = Vec::new();
        input.extend(encode_message(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "probe", "version": "0" }
            }
        })));
        input.extend(encode_message(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "browser_navigate",
                "arguments": { "url": "https://example.com/" }
            }
        })));
        stdin.write_all(&input).expect("write MCP requests");
    }
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("browser-connection process exits after stdin EOF");
    stop_child(relay);
    browser_thread.join().expect("browser thread exits");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let responses = decode_messages(&output.stdout);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[1]["result"]["isError"], false);
    assert_eq!(
        responses[1]["result"]["content"][0]["text"],
        "Navigated remote Edge"
    );
}
