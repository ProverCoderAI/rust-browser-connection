//! CLI browser automation without MCP.

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use docker_git_browser_connection::browser_actions::{
    command_result_json, dispatch_browser_command, BrowserCommand, BrowserTarget,
};
use docker_git_browser_connection::{
    compute_browser_control_panel_port, compute_browser_ports, render_cdp_url_for_ports,
};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Parser)]
#[command(
    name = "rbc",
    version,
    about = "Rust browser-connection CLI automation without MCP"
)]
struct Cli {
    /// Shared browser extension URL from Edge Share.
    #[arg(long, global = true, conflicts_with = "cdp_url")]
    share_url: Option<String>,

    /// CDP endpoint, e.g. http://127.0.0.1:9223.
    #[arg(long, global = true, conflicts_with = "share_url")]
    cdp_url: Option<String>,

    /// Control panel port. Defaults to the deterministic port for PROJECT.
    #[arg(long, global = true)]
    control_port: Option<u16>,

    /// Emit machine-readable JSON.
    #[arg(long = "json", global = true)]
    json_output: bool,

    /// Append JSONL action traces under this directory.
    #[arg(long, global = true, value_name = "DIR")]
    trace_dir: Option<PathBuf>,

    /// docker-git/browser-connection project id for control-panel discovery.
    project: String,

    #[command(subcommand)]
    command: TopCommand,
}

#[derive(Debug, Subcommand)]
enum TopCommand {
    /// Optional explicit namespace for tool commands: rbc PROJECT tools snapshot.
    Tools {
        #[command(subcommand)]
        command: ToolCommand,
    },
    /// Navigate the current page to a URL.
    Navigate { url: String },
    /// Return page title, URL, visible text, and simple selectors.
    Snapshot,
    /// Evaluate JavaScript in the current page.
    Eval {
        /// JavaScript expression to evaluate.
        #[arg(value_name = "JS", conflicts_with_all = ["expression", "file"])]
        code: Option<String>,
        /// JavaScript expression to evaluate.
        #[arg(long, conflicts_with_all = ["code", "file"])]
        expression: Option<String>,
        /// Read JavaScript expression from a file.
        #[arg(long, value_name = "PATH", conflicts_with_all = ["code", "expression"])]
        file: Option<PathBuf>,
    },
    /// Click an element by CSS selector.
    Click { selector: String },
    /// Set text in an input-like element.
    Type { selector: String, text: String },
    /// Press a keyboard key.
    Key { key: String },
    /// Capture a PNG screenshot.
    Screenshot {
        /// Capture beyond the viewport.
        #[arg(long)]
        full_page: bool,
        /// Decode the screenshot and write PNG bytes to this path.
        #[arg(long, value_name = "PNG")]
        output: Option<PathBuf>,
    },
    /// List tabs/windows for a shared-extension browser.
    Tabs,
    /// Activate a tab by browser tab id.
    ActivateTab { tab_id: i64 },
}

#[derive(Debug, Clone, Subcommand)]
enum ToolCommand {
    /// Navigate the current page to a URL.
    Navigate { url: String },
    /// Return page title, URL, visible text, and simple selectors.
    Snapshot,
    /// Evaluate JavaScript in the current page.
    Eval {
        /// JavaScript expression to evaluate.
        #[arg(value_name = "JS", conflicts_with_all = ["expression", "file"])]
        code: Option<String>,
        /// JavaScript expression to evaluate.
        #[arg(long, conflicts_with_all = ["code", "file"])]
        expression: Option<String>,
        /// Read JavaScript expression from a file.
        #[arg(long, value_name = "PATH", conflicts_with_all = ["code", "expression"])]
        file: Option<PathBuf>,
    },
    /// Click an element by CSS selector.
    Click { selector: String },
    /// Set text in an input-like element.
    Type { selector: String, text: String },
    /// Press a keyboard key.
    Key { key: String },
    /// Capture a PNG screenshot.
    Screenshot {
        /// Capture beyond the viewport.
        #[arg(long)]
        full_page: bool,
        /// Decode the screenshot and write PNG bytes to this path.
        #[arg(long, value_name = "PNG")]
        output: Option<PathBuf>,
    },
    /// List tabs/windows for a shared-extension browser.
    Tabs,
    /// Activate a tab by browser tab id.
    ActivateTab { tab_id: i64 },
}

fn main() {
    env_logger::init();
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let target = resolve_target(&cli)?;
    let tool = tool_command(&cli.command);
    let command = browser_command(&tool)?;
    let started = Instant::now();
    let result = dispatch_browser_command(&target, &command);
    let duration_ms = started.elapsed().as_millis() as u64;

    match result {
        Ok(text) => {
            let output = command_output(&command, &text, &cli)?;
            write_trace(
                &cli,
                &target,
                &command,
                duration_ms,
                Ok(&output.trace_result),
            )?;
            print_output(&cli, &target, &command, &output)?;
            Ok(())
        }
        Err(error) => {
            write_trace(&cli, &target, &command, duration_ms, Err(&error))?;
            Err(error)
        }
    }
}

#[derive(Debug)]
struct CommandOutput {
    text: String,
    json_result: Value,
    trace_result: Value,
}

fn resolve_target(cli: &Cli) -> Result<BrowserTarget> {
    if let Some(url) = cli.share_url.as_ref() {
        return Ok(BrowserTarget::shared(url));
    }
    if let Some(url) = cli.cdp_url.as_ref() {
        return Ok(BrowserTarget::cdp(url));
    }

    let project = cli.project.trim();
    if project.is_empty() {
        return Err(anyhow!("PROJECT must not be empty"));
    }
    let port = cli
        .control_port
        .unwrap_or_else(|| compute_browser_control_panel_port(project));
    match load_control_panel_inventory(port) {
        Ok(inventory) => target_from_inventory(&inventory).with_context(|| {
            format!("control panel on port {port} did not expose an active browser")
        }),
        Err(_) => Ok(BrowserTarget::cdp(render_cdp_url_for_ports(
            compute_browser_ports(project),
        ))),
    }
}

fn load_control_panel_inventory(port: u16) -> Result<Value> {
    let url = format!("http://127.0.0.1:{port}/api/browsers");
    let output = Command::new("curl")
        .args(["-fsS", &url])
        .output()
        .with_context(|| format!("failed to run curl for {url}"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "control panel request failed with status {}",
            output.status
        ));
    }
    serde_json::from_slice(&output.stdout).context("control panel inventory was not JSON")
}

fn target_from_inventory(inventory: &Value) -> Result<BrowserTarget> {
    let active = inventory
        .get("active")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("inventory did not include active browser"))?;
    let browsers = inventory
        .get("browsers")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("inventory did not include browsers"))?;
    let browser = browsers
        .iter()
        .find(|browser| browser.get("active").and_then(Value::as_bool) == Some(true))
        .or_else(|| {
            browsers
                .iter()
                .find(|browser| browser.get("name").and_then(Value::as_str) == Some(active))
        })
        .ok_or_else(|| anyhow!("active browser `{active}` was not found in inventory"))?;
    if let Some(share_url) = browser
        .get("shareUrl")
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
    {
        return Ok(BrowserTarget::shared(share_url));
    }
    if let Some(endpoint) = browser
        .get("cdpEndpoint")
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
    {
        return Ok(BrowserTarget::cdp(endpoint));
    }
    Err(anyhow!(
        "active browser `{active}` has neither shareUrl nor cdpEndpoint"
    ))
}

fn tool_command(command: &TopCommand) -> ToolCommand {
    match command {
        TopCommand::Tools { command } => command.clone(),
        TopCommand::Navigate { url } => ToolCommand::Navigate { url: url.clone() },
        TopCommand::Snapshot => ToolCommand::Snapshot,
        TopCommand::Eval {
            code,
            expression,
            file,
        } => ToolCommand::Eval {
            code: code.clone(),
            expression: expression.clone(),
            file: file.clone(),
        },
        TopCommand::Click { selector } => ToolCommand::Click {
            selector: selector.clone(),
        },
        TopCommand::Type { selector, text } => ToolCommand::Type {
            selector: selector.clone(),
            text: text.clone(),
        },
        TopCommand::Key { key } => ToolCommand::Key { key: key.clone() },
        TopCommand::Screenshot { full_page, output } => ToolCommand::Screenshot {
            full_page: *full_page,
            output: output.clone(),
        },
        TopCommand::Tabs => ToolCommand::Tabs,
        TopCommand::ActivateTab { tab_id } => ToolCommand::ActivateTab { tab_id: *tab_id },
    }
}

fn browser_command(command: &ToolCommand) -> Result<BrowserCommand> {
    match command {
        ToolCommand::Navigate { url } => Ok(BrowserCommand::Navigate { url: url.clone() }),
        ToolCommand::Snapshot => Ok(BrowserCommand::Snapshot),
        ToolCommand::Eval {
            code,
            expression,
            file,
        } => {
            let expression = match (code, expression, file) {
                (Some(expression), None, None) | (None, Some(expression), None) => {
                    expression.clone()
                }
                (None, None, Some(path)) => fs::read_to_string(path)
                    .with_context(|| format!("failed to read {}", path.display()))?,
                (None, None, None) => {
                    return Err(anyhow!("eval requires JS, --expression, or --file"))
                }
                _ => {
                    return Err(anyhow!(
                        "eval accepts only one of JS, --expression, or --file"
                    ))
                }
            };
            Ok(BrowserCommand::Evaluate { expression })
        }
        ToolCommand::Click { selector } => Ok(BrowserCommand::Click {
            selector: selector.clone(),
        }),
        ToolCommand::Type { selector, text } => Ok(BrowserCommand::TypeText {
            selector: selector.clone(),
            text: text.clone(),
        }),
        ToolCommand::Key { key } => Ok(BrowserCommand::PressKey { key: key.clone() }),
        ToolCommand::Screenshot { full_page, .. } => Ok(BrowserCommand::Screenshot {
            full_page: *full_page,
        }),
        ToolCommand::Tabs => Ok(BrowserCommand::ListTabs),
        ToolCommand::ActivateTab { tab_id } => Ok(BrowserCommand::ActivateTab { tab_id: *tab_id }),
    }
}

fn command_output(command: &BrowserCommand, text: &str, cli: &Cli) -> Result<CommandOutput> {
    if let BrowserCommand::Screenshot { .. } = command {
        if let ToolCommand::Screenshot {
            output: Some(path), ..
        } = tool_command(&cli.command)
        {
            let written = write_screenshot(text, &path)?;
            let result = json!({
                "path": path,
                "mimeType": written.mime_type,
                "bytes": written.bytes
            });
            return Ok(CommandOutput {
                text: path.display().to_string(),
                json_result: result.clone(),
                trace_result: result,
            });
        }
    }

    let result = command_result_json(command, text);
    Ok(CommandOutput {
        text: text.to_string(),
        json_result: result.clone(),
        trace_result: result,
    })
}

fn print_output(
    cli: &Cli,
    target: &BrowserTarget,
    command: &BrowserCommand,
    output: &CommandOutput,
) -> Result<()> {
    if cli.json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": true,
                "tool": command.tool_name(),
                "target": {
                    "kind": target.kind(),
                    "label": target.safe_label()
                },
                "result": output.json_result
            }))?
        );
    } else {
        println!("{}", output.text);
    }
    Ok(())
}

fn write_trace(
    cli: &Cli,
    target: &BrowserTarget,
    command: &BrowserCommand,
    duration_ms: u64,
    result: Result<&Value, &anyhow::Error>,
) -> Result<()> {
    let Some(dir) = cli.trace_dir.as_ref() else {
        return Ok(());
    };
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let path = dir.join("rbc.jsonl");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let event = match result {
        Ok(value) => json!({
            "at": unix_ms(),
            "tool": command.tool_name(),
            "target": { "kind": target.kind(), "label": target.safe_label() },
            "durationMs": duration_ms,
            "ok": true,
            "result": value
        }),
        Err(error) => json!({
            "at": unix_ms(),
            "tool": command.tool_name(),
            "target": { "kind": target.kind(), "label": target.safe_label() },
            "durationMs": duration_ms,
            "ok": false,
            "error": error.to_string()
        }),
    };
    writeln!(file, "{}", serde_json::to_string(&event)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

struct WrittenScreenshot {
    mime_type: String,
    bytes: usize,
}

fn write_screenshot(text: &str, path: &Path) -> Result<WrittenScreenshot> {
    let (mime_type, data) = screenshot_payload(text)?;
    let bytes = decode_base64(&data)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, &bytes).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(WrittenScreenshot {
        mime_type,
        bytes: bytes.len(),
    })
}

fn screenshot_payload(text: &str) -> Result<(String, String)> {
    let trimmed = text.trim();
    if trimmed.starts_with("data:") {
        return parse_data_url(trimmed);
    }
    let value: Value =
        serde_json::from_str(trimmed).context("screenshot result was not JSON or data URL")?;
    if let Some(data_url) = value.get("dataUrl").and_then(Value::as_str) {
        return parse_data_url(data_url);
    }
    if let Some(data) = value.get("data").and_then(Value::as_str) {
        let mime_type = value
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("image/png")
            .to_string();
        return Ok((mime_type, data.to_string()));
    }
    Err(anyhow!("screenshot result did not include dataUrl or data"))
}

fn parse_data_url(data_url: &str) -> Result<(String, String)> {
    let (header, data) = data_url
        .split_once(',')
        .ok_or_else(|| anyhow!("invalid data URL screenshot"))?;
    let mime_type = header
        .strip_prefix("data:")
        .and_then(|value| value.strip_suffix(";base64"))
        .unwrap_or("image/png")
        .to_string();
    Ok((mime_type, data.to_string()))
}

fn decode_base64(input: &str) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u8;
    for ch in input.chars().filter(|ch| !ch.is_whitespace()) {
        if ch == '=' {
            break;
        }
        let value = match ch {
            'A'..='Z' => ch as u32 - 'A' as u32,
            'a'..='z' => ch as u32 - 'a' as u32 + 26,
            '0'..='9' => ch as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            _ => return Err(anyhow!("invalid base64 character `{ch}`")),
        };
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Ok(output)
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64_with_padding() {
        assert_eq!(decode_base64("TWE=").unwrap(), b"Ma");
        assert_eq!(decode_base64("TQ==").unwrap(), b"M");
    }

    #[test]
    fn parses_shared_screenshot_json_payload() {
        let (mime, data) = screenshot_payload(r#"{"mimeType":"image/png","data":"TQ=="}"#).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(data, "TQ==");
    }

    #[test]
    fn eval_command_reads_expression_from_file() {
        let path = std::env::temp_dir().join(format!("rbc-eval-{}.js", unix_ms()));
        fs::write(&path, "document.title").unwrap();
        let command = browser_command(&ToolCommand::Eval {
            code: None,
            expression: None,
            file: Some(path.clone()),
        })
        .unwrap();
        let _ = fs::remove_file(path);
        assert_eq!(
            command,
            BrowserCommand::Evaluate {
                expression: "document.title".to_string()
            }
        );
    }

    #[test]
    fn eval_command_accepts_direct_javascript() {
        let command = browser_command(&ToolCommand::Eval {
            code: Some("document.title".to_string()),
            expression: None,
            file: None,
        })
        .unwrap();
        assert_eq!(
            command,
            BrowserCommand::Evaluate {
                expression: "document.title".to_string()
            }
        );
    }

    #[test]
    fn writes_screenshot_png_from_data_url() {
        let path = std::env::temp_dir().join(format!("rbc-shot-{}.png", unix_ms()));
        let written = write_screenshot("data:image/png;base64,TQ==", &path).unwrap();
        assert_eq!(written.mime_type, "image/png");
        assert_eq!(written.bytes, 1);
        assert_eq!(fs::read(&path).unwrap(), b"M");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn chooses_share_target_before_cdp_from_inventory() {
        let inventory = json!({
            "active": "edge",
            "browsers": [{
                "name": "edge",
                "active": true,
                "shareUrl": "https://relay.example/share/s#agent=a",
                "cdpEndpoint": "http://127.0.0.1:1"
            }]
        });
        let target = target_from_inventory(&inventory).unwrap();
        assert!(matches!(target, BrowserTarget::Shared { .. }));
    }
}
