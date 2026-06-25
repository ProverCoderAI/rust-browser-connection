use anyhow::{anyhow, Context, Result};
use docker_git_browser_connection::browser_actions::BrowserTarget;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const PLAYWRIGHT_CORE_VERSION: &str = "1.61.1";

pub(super) fn run_playwright_script(
    target: &BrowserTarget,
    source: &str,
    allow_close: bool,
) -> Result<String> {
    let BrowserTarget::Cdp { endpoint } = target else {
        return Err(anyhow!("real Playwright requires a CDP endpoint"));
    };
    ensure_node_available()?;
    let playwright_core_path = ensure_playwright_core()?;
    let wrapper = playwright_wrapper_source(endpoint, &playwright_core_path, source, allow_close)?;
    let wrapper_path = write_temp_playwright_wrapper(&wrapper)?;
    let output = run_node_playwright_wrapper(&wrapper_path);
    let _ = fs::remove_file(&wrapper_path);
    output
}

pub(super) fn parse_playwright_result(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| json!({ "text": text }))
}

fn ensure_node_available() -> Result<()> {
    Command::new("node")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("node is required for rbc pw but was not found")?
        .success()
        .then_some(())
        .ok_or_else(|| anyhow!("node is required for rbc pw but did not run successfully"))
}

fn ensure_playwright_core() -> Result<String> {
    if let Some(path) = node_resolve_playwright_core(None)? {
        return Ok(path);
    };
    let node_modules = install_temp_playwright_core()?;
    node_resolve_playwright_core(Some(&node_modules))?.ok_or_else(|| {
        anyhow!(
            "playwright-core was installed but node could not resolve it from {}",
            node_modules.display()
        )
    })
}

fn run_node_playwright_wrapper(wrapper_path: &Path) -> Result<String> {
    let output = Command::new("node")
        .arg(wrapper_path)
        .stdin(Stdio::null())
        .output()
        .context("failed to run node for rbc pw")?;
    if !output.status.success() {
        return Err(anyhow!(
            "rbc pw failed with status {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_string())
}

fn node_resolve_playwright_core(node_modules: Option<&Path>) -> Result<Option<String>> {
    let mut command = Command::new("node");
    command
        .args(["-p", "require.resolve('playwright-core')"])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if let Some(node_modules) = node_modules {
        command.env("NODE_PATH", node_path_with(node_modules)?);
    }
    let output = command
        .output()
        .context("failed to ask node to resolve playwright-core")?;
    if !output.status.success() {
        return Ok(None);
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!path.is_empty()).then_some(path))
}

fn install_temp_playwright_core() -> Result<PathBuf> {
    let package_dir =
        std::env::temp_dir().join(format!("rbc-playwright-core-{PLAYWRIGHT_CORE_VERSION}"));
    let node_modules = package_dir.join("node_modules");
    let package_json = node_modules.join("playwright-core/package.json");
    if package_json.exists() {
        return Ok(node_modules);
    }
    fs::create_dir_all(&package_dir)
        .with_context(|| format!("failed to create {}", package_dir.display()))?;
    let package = format!("playwright-core@{PLAYWRIGHT_CORE_VERSION}");
    let output = Command::new("npm")
        .args(["install", "--silent", "--no-audit", "--no-fund", "--prefix"])
        .arg(&package_dir)
        .arg(&package)
        .stdin(Stdio::null())
        .output()
        .context("playwright-core is not installed and npm is required for rbc pw fallback")?;
    if !output.status.success() {
        return Err(anyhow!(
            "failed to install {package} for rbc pw fallback with status {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(node_modules)
}

fn node_path_with(path: &Path) -> Result<std::ffi::OsString> {
    let mut paths = vec![path.to_path_buf()];
    if let Some(existing) = std::env::var_os("NODE_PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(paths).map_err(|error| anyhow!("failed to build NODE_PATH: {error}"))
}

fn write_temp_playwright_wrapper(source: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "rbc-playwright-{}-{}.js",
        std::process::id(),
        unix_ms()
    ));
    fs::write(&path, source).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn playwright_wrapper_source(
    cdp_url: &str,
    playwright_core_path: &str,
    user_source: &str,
    allow_close: bool,
) -> Result<String> {
    let cdp_url = serde_json::to_string(cdp_url).context("failed to encode CDP URL")?;
    let playwright_core_path =
        serde_json::to_string(playwright_core_path).context("failed to encode Playwright path")?;
    let user_source =
        serde_json::to_string(user_source).context("failed to encode Playwright script")?;
    Ok(format!(
        r#""use strict";
const cdpUrl = {cdp_url};
const playwrightCorePath = {playwright_core_path};
const userSource = {user_source};
const allowClose = {allow_close};

function renderResult(value) {{
  if (value === undefined) return "";
  if (typeof value === "string") return value;
  return JSON.stringify(value, null, 2);
}}

(async () => {{
  const playwright = require(playwrightCorePath);
  const browser = await playwright.chromium.connectOverCDP(cdpUrl);
  const contexts = browser.contexts();
  const context = contexts[0] || await browser.newContext();
  const pages = context.pages();
  const page = pages[0] || await context.newPage();
  if (!allowClose) {{
    const blocked = async () => {{
      throw new Error("rbc pw keeps shared browser sessions open; pass --allow-close to close them");
    }};
    browser.close = blocked;
    context.close = blocked;
  }}
  const run = new Function(
    "playwright",
    "browser",
    "context",
    "page",
    "pages",
    `"use strict"; return (async () => {{
${{userSource}}
    }})();`
  );
  const result = await run(playwright, browser, context, page, pages);
  const output = renderResult(result);
  if (output) process.stdout.write(String(output));
}})().catch((error) => {{
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
}});
"#
    ))
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
    fn wrapper_uses_cdp_and_guards_close_by_default() {
        let wrapper = playwright_wrapper_source(
            "http://127.0.0.1:9222",
            "/tmp/node_modules/playwright-core/index.js",
            "return await page.title();",
            false,
        )
        .unwrap();
        assert!(wrapper.contains("require(playwrightCorePath)"));
        assert!(wrapper.contains("connectOverCDP(cdpUrl)"));
        assert!(wrapper.contains("browser.close = blocked"));
        assert!(wrapper.contains("context.close = blocked"));
        assert!(wrapper.contains("return await page.title();"));
    }
}
