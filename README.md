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
browser-connection-relay       # WebSocket relay for browser share links
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
It also starts a local browser/noVNC control panel by default; `browser_list` reports its
`controlPanelUrl`.

Optional panel controls:

```toml
[mcp_servers.playwright]
command = "browser-connection"
args = ["--project", "dg-my-project", "--control-port", "6888"]
```

Use `--no-control-panel` for headless tests or when another process owns the chosen port.

## Share a remote Edge by link

For a remote Microsoft Edge where you do not want SSH, VPN, or an exposed CDP port, use the Edge
share extension plus the relay:

```bash
browser-connection-relay --bind 127.0.0.1:8765
```

In Edge, load `extension/edge-share` as an unpacked extension, open the extension popup, keep the
relay URL as `http://127.0.0.1:8765` for local testing or set your hosted relay URL, then click
`Share` and copy the link.

Open the `browser-connection` control panel (`controlPanelUrl` from `browser_list`), paste the link
into `Shared Link`, and add it as `edge`. The target appears as `kind=shared-extension`; selecting
it makes MCP tools route through the extension session instead of a CDP port:

```toml
[mcp_servers.playwright]
command = "browser-connection"
args = [
  "--project", "dg-my-project",
  "--browser-share", "edge=https://relay.example/share/session#agent=token",
  "--active-browser", "managed",
]
```

The share link is a bearer credential. Anyone with the link can control that shared session until
the user clicks `Stop` in the extension or the relay session is removed. Extension sharing supports
common browser tools such as navigate, snapshot, evaluate, click, type, key press, and screenshot,
but it is not full CDP/VNC parity and protected Edge pages may reject actions.

## Personal browser

You can attach an already running desktop Chrome/Chromium browser if it exposes a CDP port.
When `browser-connection` runs inside Docker, use a host-reachable address such as
`host.docker.internal`, not `127.0.0.1`.

```bash
google-chrome \
  --remote-debugging-address=0.0.0.0 \
  --remote-debugging-port=9222 \
  --user-data-dir="$HOME/.browser-connection-personal" \
  --no-first-run
```

To also view that desktop browser through noVNC, expose the host desktop/browser through VNC:

```bash
x11vnc -display :0 -rfbport 5900 -listen 0.0.0.0 -nopw -forever -shared
```

Then register both the CDP endpoint and the VNC display endpoint:

```toml
[mcp_servers.playwright]
command = "browser-connection"
args = [
  "--project", "dg-my-project",
  "--personal-browser", "http://host.docker.internal:9222",
  "--personal-vnc", "host.docker.internal:5900",
]
```

Multiple browser targets can be configured and switched at runtime. This example starts on the
Docker-managed noVNC browser and keeps the personal browser ready in the noVNC control panel:

```toml
[mcp_servers.playwright]
command = "browser-connection"
args = [
  "--project", "dg-my-project",
  "--browser", "personal=http://host.docker.internal:9222",
  "--browser-vnc", "personal=host.docker.internal:5900",
  "--browser", "work=http://127.0.0.1:9333",
  "--active-browser", "managed",
]
```

Environment alternatives:

```bash
export BROWSER_CONNECTION_PERSONAL_CDP_ENDPOINT=http://host.docker.internal:9222
export BROWSER_CONNECTION_PERSONAL_VNC_ENDPOINT=host.docker.internal:5900
export BROWSER_CONNECTION_BROWSERS=work=http://127.0.0.1:9333
export BROWSER_CONNECTION_BROWSER_VNCS=work=host.docker.internal:5901
export BROWSER_CONNECTION_BROWSER_SHARES=edge=https://relay.example/share/session#agent=token
export BROWSER_CONNECTION_ACTIVE_BROWSER=managed
```

`browser_list` reports each target's `cdpEndpoint`, `vncEndpoint`, `novncUrl`, and `shareUrl`. If a
target has a VNC endpoint but no explicit noVNC URL, `browser_select` starts a lightweight noVNC
proxy for that target unless `--no-start-browser` is set.

Open the reported `controlPanelUrl` to choose `managed`, `personal`, or any configured browser from
the noVNC UI. The same `browser-connection` process keeps running, and MCP browser tools use the new
active target on the next tool call.

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
browser_select(name, cdp_endpoint?, vnc_endpoint?, novnc_url?, share_url?)
```

The noVNC control panel is the interactive switcher. Use `browser_select` with `name=managed` to
return to the Rust-managed noVNC/CDP browser, or with
`name=personal`, `cdp_endpoint=http://host.docker.internal:9222`, and
`vnc_endpoint=host.docker.internal:5900` to connect a personal browser without restarting the MCP
server.

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
