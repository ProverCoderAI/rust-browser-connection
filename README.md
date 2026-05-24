# rust-browser-connection

Rust MCP/noVNC bridge for docker-git: one project = one Chromium container visible in noVNC and controlled through MCP/CDP.

## Install

```bash
cargo install --git https://github.com/ProverCoderAI/rust-browser-connection --branch main --locked --bins
```

Installs two binaries:

```text
docker-git-browser-connection  # start/status browser container
browser-connection             # MCP stdio server for Codex/Hermes
```

## Start browser manually

```bash
docker-git-browser-connection start --project dg-my-project
```

Output contains:

```text
Container: dg-my-project-browser
noVNC: http://...
CDP: http://...
```

Check status:

```bash
docker-git-browser-connection status --project dg-my-project
```

## Codex MCP config

`~/.codex/config.toml`:

```toml
[mcp_servers.playwright]
command = "browser-connection"
args = ["--project", "dg-my-project"]
```

Use `browser-connection`, not `npx @playwright/mcp`. The MCP server starts/reuses the same Rust-managed browser container automatically.

## Hermes MCP config

`~/.hermes/config.yaml`:

```yaml
mcp_servers:
  playwright:
    command: browser-connection
    args: ["--project", "dg-my-project"]
    timeout: 120
    connect_timeout: 60
```

## MCP tools

```text
browser_navigate(url)
browser_snapshot()
browser_evaluate(expression)
browser_click(selector)
browser_type(selector, text)
browser_press_key(key)
browser_take_screenshot(full_page?)
```

## Smoke test

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
| browser-connection --project dg-my-project --no-start-browser
```

Expected: server `browser-connection` and tools like `browser_navigate`, `browser_snapshot`, `browser_evaluate`.

## Notes

- Container name: `<project>-browser`, e.g. `dg-my-project-browser`.
- The binary auto-detects Docker via `/var/run/docker.sock` or `tcp://host.docker.internal:2375`.
- If `container:<project>` network is unavailable, it falls back to bridge mode and prints reachable noVNC/CDP URLs.
- Invariant: MCP and noVNC operate on the same Chromium session; no second Playwright browser is started.

## Development

```bash
cargo fmt --check
cargo check --locked --bins
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```
