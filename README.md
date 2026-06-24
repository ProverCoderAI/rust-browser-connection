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

## Personal browser

You can attach an already running desktop Chrome/Chromium browser if it exposes a CDP port:

```bash
google-chrome --remote-debugging-port=9222 --user-data-dir="$HOME/.browser-connection-personal"
```

Then point the MCP server at that browser:

```toml
[mcp_servers.playwright]
command = "browser-connection"
args = ["--project", "dg-my-project", "--personal-browser", "http://127.0.0.1:9222"]
```

Multiple browser targets can be configured and switched at runtime:

```toml
[mcp_servers.playwright]
command = "browser-connection"
args = [
  "--project", "dg-my-project",
  "--browser", "personal=http://127.0.0.1:9222",
  "--browser", "work=http://127.0.0.1:9333",
  "--active-browser", "personal",
]
```

Environment alternatives:

```bash
export BROWSER_CONNECTION_PERSONAL_CDP_ENDPOINT=http://127.0.0.1:9222
export BROWSER_CONNECTION_BROWSERS=work=http://127.0.0.1:9333
export BROWSER_CONNECTION_ACTIVE_BROWSER=personal
```

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
browser_list()
browser_select(name, cdp_endpoint?)
```

Use `browser_select` with `name=managed` to return to the Rust-managed noVNC/CDP browser, or with `name=personal` and `cdp_endpoint=http://127.0.0.1:9222` to connect a personal browser without restarting the MCP server.

## Smoke test

```bash
python3 - <<'PY' | browser-connection --project dg-my-project --no-start-browser | python3 - <<'PY'
import json
import sys

messages = [
    {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "probe", "version": "0"},
        },
    },
    {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}},
    {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
]

for message in messages:
    body = json.dumps(message, separators=(",", ":")).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode())
    sys.stdout.buffer.write(body)
PY
import json
import sys

stream = sys.stdin.buffer
while True:
    header = {}
    while True:
        line = stream.readline()
        if not line:
            raise SystemExit(0)
        stripped = line.strip()
        if not stripped:
            break
        name, value = line.decode().split(":", 1)
        header[name.lower()] = value.strip()

    length = int(header["content-length"])
    body = stream.read(length)
    print(json.dumps(json.loads(body), indent=2))
PY
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
