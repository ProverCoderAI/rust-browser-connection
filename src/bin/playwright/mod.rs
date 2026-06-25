use anyhow::{anyhow, Context, Result};
use docker_git_browser_connection::browser_actions::BrowserTarget;
use docker_git_browser_connection::shared_browser::SharedBrowserClient;
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const PLAYWRIGHT_CORE_VERSION: &str = "1.61.1";

pub(super) fn run_playwright_script(
    target: &BrowserTarget,
    source: &str,
    allow_close: bool,
) -> Result<String> {
    match target {
        BrowserTarget::Cdp { endpoint } => run_cdp_playwright_script(endpoint, source, allow_close),
        BrowserTarget::Shared { share_url } => {
            run_shared_playwright_script(share_url, source, allow_close)
        }
    }
}

fn run_cdp_playwright_script(endpoint: &str, source: &str, allow_close: bool) -> Result<String> {
    ensure_node_available()?;
    let playwright_core_path = ensure_playwright_core()?;
    let wrapper = playwright_wrapper_source(endpoint, &playwright_core_path, source, allow_close)?;
    let wrapper_path = write_temp_playwright_wrapper(&wrapper)?;
    let output = run_node_playwright_wrapper(&wrapper_path);
    let _ = fs::remove_file(&wrapper_path);
    output
}

fn run_shared_playwright_script(
    share_url: &str,
    source: &str,
    allow_close: bool,
) -> Result<String> {
    ensure_node_available()?;
    let wrapper = shared_playwright_wrapper_source(source, allow_close)?;
    let wrapper_path = write_temp_playwright_wrapper(&wrapper)?;
    let output = run_node_shared_playwright_wrapper(share_url, &wrapper_path);
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

fn run_node_shared_playwright_wrapper(share_url: &str, wrapper_path: &Path) -> Result<String> {
    let mut child = Command::new("node")
        .arg(wrapper_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to run node for shared-extension rbc pw")?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("failed to open node stdin for shared-extension rbc pw"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to open node stdout for shared-extension rbc pw"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to open node stderr for shared-extension rbc pw"))?;
    let client = SharedBrowserClient::new(share_url);
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let mut final_result = None;
    let mut final_error = None;
    let mut plain_output = String::new();

    loop {
        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .context("failed to read shared-extension rbc pw bridge output")?;
        if bytes == 0 {
            break;
        }
        let text = line.trim_end_matches(['\r', '\n']);
        if text.is_empty() {
            continue;
        }

        let Ok(message) = serde_json::from_str::<Value>(text) else {
            plain_output.push_str(text);
            plain_output.push('\n');
            continue;
        };
        match message.get("type").and_then(Value::as_str) {
            Some("rbc-command") => {
                let response = shared_bridge_response(&client, &message);
                writeln!(stdin, "{}", serde_json::to_string(&response)?)
                    .context("failed to write shared-extension rbc pw bridge response")?;
            }
            Some("rbc-result") => {
                final_result = Some(message.get("result").cloned().unwrap_or(Value::Null));
                break;
            }
            Some("rbc-error") => {
                let error = message
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("shared-extension rbc pw failed");
                final_error = Some(error.to_string());
                break;
            }
            _ => {
                plain_output.push_str(text);
                plain_output.push('\n');
            }
        }
    }

    drop(stdin);
    let status = child
        .wait()
        .context("failed to wait for shared-extension rbc pw")?;
    let stderr_text = read_stderr(&mut stderr)?;
    if let Some(error) = final_error {
        return Err(anyhow!("{error}"));
    }
    if !status.success() {
        return Err(anyhow!(
            "shared-extension rbc pw failed with status {status}\n{}",
            stderr_text.trim()
        ));
    }
    if let Some(result) = final_result {
        return render_shared_result(&result);
    }
    let plain_output = plain_output.trim_end_matches(['\r', '\n']).to_string();
    if !plain_output.is_empty() {
        return Ok(plain_output);
    }
    if !stderr_text.trim().is_empty() {
        return Err(anyhow!("{}", stderr_text.trim()));
    }
    Ok(String::new())
}

fn shared_bridge_response(client: &SharedBrowserClient, message: &Value) -> Value {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let command = match message.get("command").and_then(Value::as_str) {
        Some(command) if !command.trim().is_empty() => command,
        _ => {
            return json!({
                "id": id,
                "ok": false,
                "error": "shared-extension rbc pw bridge command is required"
            })
        }
    };
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    match client.call_command(command, params) {
        Ok(result) => json!({ "id": id, "ok": true, "result": result }),
        Err(error) => json!({ "id": id, "ok": false, "error": error.to_string() }),
    }
}

fn render_shared_result(value: &Value) -> Result<String> {
    if value.is_null() {
        return Ok(String::new());
    }
    if let Some(text) = value.as_str() {
        return Ok(text.to_string());
    }
    serde_json::to_string_pretty(value).context("failed to render shared-extension rbc pw result")
}

fn read_stderr(stderr: &mut ChildStderr) -> Result<String> {
    let mut text = String::new();
    stderr
        .read_to_string(&mut text)
        .context("failed to read shared-extension rbc pw stderr")?;
    Ok(text)
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

fn shared_playwright_wrapper_source(user_source: &str, allow_close: bool) -> Result<String> {
    let user_source =
        serde_json::to_string(user_source).context("failed to encode Playwright script")?;
    let mut source = String::new();
    source.push_str(
        r#""use strict";
const fs = require("fs");
const readline = require("readline");
"#,
    );
    source.push_str("const userSource = ");
    source.push_str(&user_source);
    source.push_str(";\nconst allowClose = ");
    source.push_str(if allow_close { "true" } else { "false" });
    source.push_str(
        r#";

const availableSubset = "page.goto, page.title, page.url, page.evaluate, page.click, page.fill, page.type, page.press, page.screenshot, page.content, page.textContent, page.innerText, page.inputValue, page.locator, page.getByText, page.getByRole, page.waitForSelector, page.waitForTimeout, page.waitForLoadState";
let nextId = 1;
const pending = new Map();
const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });

input.on("line", (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch (error) {
    return;
  }
  const waiter = pending.get(message.id);
  if (!waiter) return;
  pending.delete(message.id);
  if (message.ok === false) {
    waiter.reject(new Error(message.error || "shared-extension command failed"));
  } else {
    waiter.resolve(message.result);
  }
});

function hostCommand(command, params) {
  const id = nextId++;
  process.stdout.write(JSON.stringify({ type: "rbc-command", id, command, params: params || {} }) + "\n");
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
  });
}

function unsupported(name) {
  return async () => {
    throw new Error(`Playwright method ${name} is not available for shared-extension targets. Available subset: ${availableSubset}`);
  };
}

function commandValue(result) {
  if (result && typeof result === "object" && Object.prototype.hasOwnProperty.call(result, "value")) {
    return result.value;
  }
  return result;
}

function matcher(value, options) {
  if (value instanceof RegExp) {
    return { kind: "regex", source: value.source, flags: value.flags };
  }
  return {
    kind: "text",
    value: String(value ?? ""),
    exact: options && options.exact === true
  };
}

function locatorExpression(locator, action, args) {
  return `(() => {
    const locator = ${JSON.stringify(locator)};
    const action = ${JSON.stringify(action)};
    const args = ${JSON.stringify(args || [])};

    function visible(element) {
      if (!element || !element.isConnected) return false;
      const style = window.getComputedStyle(element);
      if (style.visibility === "hidden" || style.display === "none") return false;
      const rect = element.getBoundingClientRect();
      return rect.width > 0 && rect.height > 0;
    }

    function textOf(element) {
      return String(element && (element.innerText || element.textContent || element.value || "") || "").replace(/\\s+/g, " ").trim();
    }

    function nameOf(element) {
      return String(
        element.getAttribute("aria-label") ||
        element.getAttribute("alt") ||
        element.getAttribute("title") ||
        element.value ||
        element.innerText ||
        element.textContent ||
        ""
      ).replace(/\\s+/g, " ").trim();
    }

    function implicitRole(element) {
      const tag = element.tagName.toLowerCase();
      const type = String(element.getAttribute("type") || "").toLowerCase();
      if (tag === "button") return "button";
      if (tag === "a" && element.hasAttribute("href")) return "link";
      if (tag === "select") return "combobox";
      if (tag === "textarea") return "textbox";
      if (tag === "input") {
        if (["button", "submit", "reset"].includes(type)) return "button";
        if (["checkbox"].includes(type)) return "checkbox";
        if (["radio"].includes(type)) return "radio";
        return "textbox";
      }
      if (/^h[1-6]$/.test(tag)) return "heading";
      if (tag === "img") return "img";
      return "";
    }

    function matchesMatcher(text, expected) {
      if (!expected) return true;
      const value = String(text || "");
      if (expected.kind === "regex") {
        return new RegExp(expected.source, expected.flags || "").test(value);
      }
      return expected.exact ? value === expected.value : value.toLowerCase().includes(String(expected.value).toLowerCase());
    }

    function allElements() {
      return Array.from(document.querySelectorAll("body *"));
    }

    function findAll() {
      if (locator.kind === "css") {
        return Array.from(document.querySelectorAll(locator.selector));
      }
      if (locator.kind === "text") {
        return allElements().filter((element) => visible(element) && matchesMatcher(textOf(element), locator.text));
      }
      if (locator.kind === "role") {
        return allElements().filter((element) => {
          const role = element.getAttribute("role") || implicitRole(element);
          return role === locator.role && visible(element) && matchesMatcher(nameOf(element), locator.name);
        });
      }
      throw new Error("Unsupported locator kind: " + locator.kind);
    }

    function one() {
      const matches = findAll();
      const element = matches[locator.index || 0];
      if (!element) throw new Error("No element matches locator: " + locator.description);
      return element;
    }

    function fillElement(element, text, replace) {
      element.scrollIntoView({ block: "center", inline: "center", behavior: "auto" });
      element.focus();
      if (element.isContentEditable) {
        if (replace) element.innerText = "";
        document.execCommand("insertText", false, text);
      } else if ("value" in element) {
        if (replace) {
          element.value = text;
        } else {
          element.value = String(element.value || "") + text;
        }
        element.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: text }));
        element.dispatchEvent(new Event("change", { bubbles: true }));
      } else {
        throw new Error("Target element is not editable");
      }
    }

    if (action === "count") return findAll().length;
    const element = one();
    if (action === "visible") return visible(element);
    if (action === "textContent") return element.textContent || "";
    if (action === "innerText") return element.innerText || "";
    if (action === "inputValue") return element.value || "";
    if (action === "click") {
      element.scrollIntoView({ block: "center", inline: "center", behavior: "auto" });
      const rect = element.getBoundingClientRect();
      const x = rect.left + rect.width / 2;
      const y = rect.top + rect.height / 2;
      element.dispatchEvent(new MouseEvent("mouseover", { bubbles: true, clientX: x, clientY: y }));
      element.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, clientX: x, clientY: y }));
      element.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, clientX: x, clientY: y }));
      element.click();
      return { text: textOf(element), rect: { x: Math.round(rect.x), y: Math.round(rect.y), width: Math.round(rect.width), height: Math.round(rect.height) } };
    }
    if (action === "fill") {
      fillElement(element, String(args[0] ?? ""), true);
      return { textLength: String(args[0] ?? "").length };
    }
    if (action === "type") {
      fillElement(element, String(args[0] ?? ""), false);
      return { textLength: String(args[0] ?? "").length };
    }
    if (action === "focus") {
      element.scrollIntoView({ block: "center", inline: "center", behavior: "auto" });
      element.focus();
      return true;
    }
    throw new Error("Unsupported locator action: " + action);
  })()`;
}

async function evaluateExpression(expression) {
  return commandValue(await hostCommand("evaluate", { expression }));
}

function createLocator(spec) {
  async function action(name, args) {
    return evaluateExpression(locatorExpression(spec, name, args || []));
  }
  return {
    click: () => action("click"),
    fill: (text) => action("fill", [String(text ?? "")]),
    type: (text) => action("type", [String(text ?? "")]),
    press: async (key) => {
      await action("focus");
      return hostCommand("press_key", { key: String(key) });
    },
    textContent: () => action("textContent"),
    innerText: () => action("innerText"),
    inputValue: () => action("inputValue"),
    count: () => action("count"),
    isVisible: () => action("visible"),
    waitFor: async (options = {}) => waitForLocator(spec, options),
    first: () => createLocator({ ...spec, index: 0, description: `${spec.description}.first()` }),
    nth: (index) => createLocator({ ...spec, index, description: `${spec.description}.nth(${index})` })
  };
}

async function waitForLocator(spec, options = {}) {
  const timeout = Number(options.timeout ?? 30000);
  const state = options.state || "visible";
  const started = Date.now();
  while (Date.now() - started <= timeout) {
    const locator = createLocator(spec);
    const count = await locator.count();
    const visible = count > 0 ? await locator.isVisible() : false;
    if (state === "attached" && count > 0) return locator;
    if (state === "hidden" && (count === 0 || !visible)) return locator;
    if ((state === "visible" || !state) && visible) return locator;
    await page.waitForTimeout(100);
  }
  throw new Error(`Timed out waiting for ${spec.description} to be ${state}`);
}

const page = {
  goto: (url) => hostCommand("navigate", { url: String(url) }),
  title: () => evaluateExpression("document.title"),
  url: () => evaluateExpression("location.href"),
  evaluate: (fnOrExpression, arg) => {
    const expression = typeof fnOrExpression === "function"
      ? `(${fnOrExpression.toString()})(${JSON.stringify(arg)})`
      : String(fnOrExpression);
    return evaluateExpression(expression);
  },
  click: (selector) => createLocator({ kind: "css", selector: String(selector), description: String(selector) }).click(),
  fill: (selector, text) => createLocator({ kind: "css", selector: String(selector), description: String(selector) }).fill(text),
  type: (selector, text) => createLocator({ kind: "css", selector: String(selector), description: String(selector) }).type(text),
  press: async (selectorOrKey, key) => {
    if (key === undefined) {
      return hostCommand("press_key", { key: String(selectorOrKey) });
    }
    await createLocator({ kind: "css", selector: String(selectorOrKey), description: String(selectorOrKey) }).press(key);
    return null;
  },
  screenshot: async (options = {}) => {
    const result = await hostCommand("screenshot", {
      fullPage: options.fullPage === true,
      format: options.type === "jpeg" ? "jpeg" : "png",
      quality: options.quality
    });
    const dataUrl = result.dataUrl || "";
    const base64 = result.data || (dataUrl.includes(",") ? dataUrl.split(",").slice(1).join(",") : "");
    const buffer = Buffer.from(base64, "base64");
    if (options.path) fs.writeFileSync(options.path, buffer);
    return buffer;
  },
  content: () => evaluateExpression("document.documentElement ? document.documentElement.outerHTML : ''"),
  textContent: (selector) => createLocator({ kind: "css", selector: String(selector), description: String(selector) }).textContent(),
  innerText: (selector) => createLocator({ kind: "css", selector: String(selector), description: String(selector) }).innerText(),
  inputValue: (selector) => createLocator({ kind: "css", selector: String(selector), description: String(selector) }).inputValue(),
  locator: (selector) => createLocator({ kind: "css", selector: String(selector), description: String(selector) }),
  getByText: (text, options) => createLocator({ kind: "text", text: matcher(text, options), description: `text=${String(text)}` }),
  getByRole: (role, options = {}) => createLocator({ kind: "role", role: String(role), name: options.name === undefined ? null : matcher(options.name, options), description: `role=${String(role)}` }),
  waitForSelector: (selector, options = {}) => waitForLocator({ kind: "css", selector: String(selector), description: String(selector) }, options),
  waitForTimeout: (ms) => new Promise((resolve) => setTimeout(resolve, Number(ms) || 0)),
  waitForLoadState: async (state = "load", options = {}) => {
    const timeout = Number(options.timeout ?? 30000);
    const started = Date.now();
    while (Date.now() - started <= timeout) {
      const readyState = await evaluateExpression("document.readyState");
      if (state === "domcontentloaded" && (readyState === "interactive" || readyState === "complete")) return;
      if ((state === "load" || state === "networkidle") && readyState === "complete") return;
      await page.waitForTimeout(100);
    }
    throw new Error(`Timed out waiting for load state ${state}`);
  },
  keyboard: {
    press: (key) => hostCommand("press_key", { key: String(key) }),
    type: (text) => hostCommand("type", { text: String(text) })
  },
  mouse: {
    click: unsupported("page.mouse.click")
  }
};

const context = {
  pages: () => [page],
  newPage: unsupported("context.newPage"),
  close: allowClose ? async () => {} : unsupported("context.close"),
  storageState: unsupported("context.storageState"),
  route: unsupported("context.route")
};
const browser = {
  contexts: () => [context],
  newContext: unsupported("browser.newContext"),
  close: allowClose ? async () => {} : unsupported("browser.close")
};
const pages = [page];
const playwright = {
  chromium: {
    connectOverCDP: unsupported("playwright.chromium.connectOverCDP")
  }
};

function normalizeResult(value) {
  if (value === undefined) return null;
  if (Buffer.isBuffer(value)) {
    return { type: "Buffer", data: value.toString("base64") };
  }
  return value;
}

(async () => {
  const run = new Function(
    "playwright",
    "browser",
    "context",
    "page",
    "pages",
    `"use strict"; return (async () => {
${userSource}
    })();`
  );
  const result = await run(playwright, browser, context, page, pages);
  process.stdout.write(JSON.stringify({ type: "rbc-result", result: normalizeResult(result) }) + "\n");
})().catch((error) => {
  process.stdout.write(JSON.stringify({ type: "rbc-error", error: error && error.stack ? error.stack : String(error) }) + "\n");
  process.exitCode = 1;
});
"#,
    );
    Ok(source)
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

    #[test]
    fn shared_wrapper_exposes_subset_and_bridge_commands() {
        let wrapper = shared_playwright_wrapper_source(
            "return await page.getByText(/more/i).count();",
            false,
        )
        .unwrap();
        assert!(wrapper.contains("rbc-command"));
        assert!(wrapper.contains("hostCommand(\"navigate\""));
        assert!(wrapper.contains("getByRole"));
        assert!(wrapper.contains("Available subset"));
        assert!(wrapper.contains("return await page.getByText(/more/i).count();"));
    }
}
