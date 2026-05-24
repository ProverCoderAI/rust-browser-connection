# docker-git-browser-connection

**Rust-only single-browser module for docker-git**: one project browser container with VNC, noVNC and CDP, plus a first-party MCP stdio command named `browser-connection`.

Solves GitHub issue #347: agents control the same Chromium that the user sees through noVNC. There is no separate Playwright browser session and no runtime dependency on `npx @playwright/mcp`.

## Binaries

Installing this crate provides two commands:

```text
docker-git-browser-connection  # browser container lifecycle: start/status
browser-connection             # MCP stdio server for Codex/Hermes configs
```

Install:

```bash
cargo install --git https://github.com/ProverCoderAI/rust-browser-connection --bins
```

Verify:

```bash
which docker-git-browser-connection
which browser-connection
docker-git-browser-connection --help
browser-connection --help
```

## Start/reuse the single browser container

```bash
# Inside docker-git project containers DOCKER_GIT_PROJECT_DOCKER_HOST is used
# automatically when /var/run/docker.sock is not mounted.
docker-git-browser-connection start --project dg-my-project --network container:dg-my-project

# Output:
# Browser started for project: dg-my-project
# Container: dg-my-project-browser
# noVNC: http://127.0.0.1:6080/vnc.html?autoconnect=true&resize=remote&path=websockify
# CDP (for MCP Playwright / Hermes): http://127.0.0.1:9223
```

Health check:

```bash
curl http://127.0.0.1:9223/json/version
open http://127.0.0.1:6080/vnc.html?autoconnect=true\&resize=remote\&path=websockify
```

## MCP config: use `browser-connection`, not `npx @playwright/mcp`

Do **not** configure docker-git projects like this:

```toml
[mcp_servers.playwright]
command = "npx"
args = ["-y", "@playwright/mcp@latest", "--cdp-endpoint", "http://127.0.0.1:9223"]
```

That uses the external upstream Playwright MCP command. The docker-git product path is the Rust MCP stdio server from this repo:

```toml
[mcp_servers.playwright]
command = "browser-connection"
args = ["--project", "dg-my-project"]
```

If the Cargo bin directory is not on PATH:

```toml
[mcp_servers.playwright]
command = "/root/.cargo/bin/browser-connection"
args = ["--project", "dg-my-project"]
```

Hermes YAML equivalent:

```yaml
mcp_servers:
  playwright:
    command: browser-connection
    args:
      - --project
      - dg-my-project
    timeout: 120
    connect_timeout: 60
```

The MCP command auto-starts/reuses the Rust-managed browser by default. For protocol smoke tests without Docker, pass `--no-start-browser`.

## MCP smoke test

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
| browser-connection --project dg-my-project --no-start-browser
```

Expected output includes:

```text
"name":"browser-connection"
"browser_navigate"
"browser_snapshot"
"browser_evaluate"
"browser_click"
"browser_type"
"browser_press_key"
"browser_take_screenshot"
```

## Runtime flow

```text
Codex/Hermes MCP client
        ↓ command = "browser-connection"
Rust MCP stdio server from this crate
        ↓ CDP http://127.0.0.1:9223
Rust-managed Chromium container dg-*-browser
        ↓ same framebuffer
noVNC http://127.0.0.1:6080/vnc.html?autoconnect=true&resize=remote&path=websockify
```

## Supported MCP tools

The first-party MCP server exposes a compact browser automation subset:

```text
browser_navigate(url)
browser_snapshot()
browser_evaluate(expression)
browser_click(selector)
browser_type(selector, text)
browser_press_key(key)
browser_take_screenshot(full_page?)
```

Selectors are CSS selectors. `browser_snapshot` returns page title, URL, visible text and a simple list of interactive elements with selectors.

## Integration in docker-git

Project images should install this repo and configure agent MCP as `browser-connection`:

```dockerfile
RUN cargo install --git https://github.com/ProverCoderAI/rust-browser-connection.git --bins
```

Generated Codex config should contain:

```toml
[mcp_servers.playwright]
command = "browser-connection"
args = ["--project", "${PROJECT_ID}"]
```

Generated Hermes config should contain the YAML equivalent. Old TypeScript browser code and external `npx @playwright/mcp` config paths should not be generated.

## Formal guarantees

- **Invariant**: exactly one browser container per project (`dg-*-browser`).
- **Invariant**: MCP tools target the configured CDP endpoint; they do not start a second browser.
- **Invariant**: noVNC and MCP observe/control the same Chromium framebuffer/session.
- Pure helpers render URLs/specs without Docker.
- Shell effects are isolated in `src/browser.rs`, `src/cdp.rs`, and `src/mcp.rs` with AGENTS.md-style comments.

## Development

```bash
source ~/.cargo/env || true
cargo fmt --check
cargo check --locked --bins
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Negative verification that the product path does not depend on external Playwright MCP:

```bash
grep -R '@playwright/mcp\|command = "npx"\|--cdp-endpoint' -n README.md Cargo.toml src tests .github || true
```

Any match must be either a warning about the old forbidden config or test/documentation text proving it is not used as the runtime command.
