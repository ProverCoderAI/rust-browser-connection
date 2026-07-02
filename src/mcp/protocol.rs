use super::{
    tools, McpRuntime, LEGACY_MCP_PROTOCOL_VERSION, MCP_PROTOCOL_VERSION,
    OLDEST_MCP_PROTOCOL_VERSION, PREVIOUS_MCP_PROTOCOL_VERSION, SERVER_NAME,
};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StdioTransport {
    Framed,
    LineDelimited,
}

pub(super) fn read_message<R: BufRead>(
    reader: &mut R,
    transport: &mut Option<StdioTransport>,
) -> Result<Option<String>> {
    if transport.is_none() {
        *transport = detect_transport(reader)?;
    }

    let Some(transport) = transport else {
        return Ok(None);
    };

    match transport {
        StdioTransport::Framed => read_framed_message(reader),
        StdioTransport::LineDelimited => read_line_message(reader),
    }
}

fn detect_transport<R: BufRead>(reader: &mut R) -> Result<Option<StdioTransport>> {
    loop {
        let buffer = reader
            .fill_buf()
            .context("failed to inspect MCP stdin for transport detection")?;
        let Some(first_byte) = buffer.first().copied() else {
            return Ok(None);
        };

        match first_byte {
            b'{' | b'[' => return Ok(Some(StdioTransport::LineDelimited)),
            b'C' | b'c' => return Ok(Some(StdioTransport::Framed)),
            b' ' | b'\t' | b'\r' | b'\n' => reader.consume(1),
            _ => {
                return Err(anyhow!(
                    "MCP stdin transport was not recognized from first byte: {first_byte}"
                ))
            }
        }
    }
}

fn read_framed_message<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let content_length = read_content_length(reader)?;
    let Some(content_length) = content_length else {
        return Ok(None);
    };

    let mut body = vec![0_u8; content_length];
    reader
        .read_exact(&mut body)
        .context("failed to read MCP stdin body")?;

    String::from_utf8(body)
        .map(Some)
        .context("MCP stdin body was not utf8")
}

fn read_line_message<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .context("failed to read MCP stdin line")?;
        if read == 0 {
            return Ok(None);
        }

        let message = line.trim_end_matches(&['\r', '\n'][..]).trim();
        if !message.is_empty() {
            return Ok(Some(message.to_string()));
        }
    }
}

fn read_content_length<R: BufRead>(reader: &mut R) -> Result<Option<usize>> {
    let mut content_length = None;
    let mut saw_header = false;

    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .context("failed to read MCP stdin header")?;
        if read == 0 {
            return if saw_header {
                Err(anyhow!("MCP stdin closed before header terminator"))
            } else {
                Ok(None)
            };
        }

        let header = line.trim_end_matches(&['\r', '\n'][..]);
        if header.is_empty() {
            if saw_header {
                break;
            }
            continue;
        }
        saw_header = true;

        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| anyhow!("MCP stdin header was malformed"))?;

        if name.eq_ignore_ascii_case("Content-Length") {
            if content_length.is_some() {
                return Err(anyhow!("MCP stdin declared Content-Length more than once"));
            }
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .context("MCP stdin Content-Length was not a valid usize")?,
            );
        }
    }

    content_length
        .map(Some)
        .ok_or_else(|| anyhow!("MCP stdin header was missing Content-Length"))
}

pub(super) fn write_message<W: Write>(
    writer: &mut W,
    response: &Value,
    transport: StdioTransport,
) -> Result<()> {
    match transport {
        StdioTransport::Framed => {
            let body = serde_json::to_vec(response).context("failed to encode MCP stdout JSON")?;
            write!(writer, "Content-Length: {}\r\n\r\n", body.len())
                .context("failed to write MCP stdout header")?;
            writer
                .write_all(&body)
                .context("failed to write MCP stdout body")?;
        }
        StdioTransport::LineDelimited => {
            serde_json::to_writer(&mut *writer, response)
                .context("failed to encode MCP stdout JSON")?;
            writer
                .write_all(b"\n")
                .context("failed to write MCP stdout line terminator")?;
        }
    }
    writer.flush().context("failed to flush MCP stdout")?;
    Ok(())
}

pub(super) fn handle_message(runtime: &mut McpRuntime, message: &str) -> Result<Option<Value>> {
    let request: Value = serde_json::from_str(message).context("MCP stdin body was not JSON")?;
    let id = request.get("id").cloned();
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("MCP request did not include method"))?;

    if id.is_none() {
        return Ok(None);
    }
    let id = id.unwrap_or(Value::Null);

    let response = match method {
        "initialize" => match requested_protocol_version(&request) {
            Ok(protocol_version) => success_response(id, initialize_result(protocol_version)),
            Err(error) => error_response(id, -32602, &error.to_string()),
        },
        "tools/list" => success_response(id, json!({ "tools": tools::tool_definitions() })),
        "tools/call" => success_response(id, tools::handle_tool_call(runtime, &request)),
        _ => error_response(id, -32601, &format!("Unknown MCP method: {method}")),
    };

    Ok(Some(response))
}

fn requested_protocol_version(request: &Value) -> Result<&'static str> {
    let requested = request
        .get("params")
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str)
        .unwrap_or(MCP_PROTOCOL_VERSION);

    match requested {
        MCP_PROTOCOL_VERSION => Ok(MCP_PROTOCOL_VERSION),
        PREVIOUS_MCP_PROTOCOL_VERSION => Ok(PREVIOUS_MCP_PROTOCOL_VERSION),
        LEGACY_MCP_PROTOCOL_VERSION => Ok(LEGACY_MCP_PROTOCOL_VERSION),
        OLDEST_MCP_PROTOCOL_VERSION => Ok(OLDEST_MCP_PROTOCOL_VERSION),
        _ => Err(anyhow!("Unsupported MCP protocol version: {requested}")),
    }
}

fn initialize_result(protocol_version: &str) -> Value {
    json!({
        "protocolVersion": protocol_version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
