---
bump: minor
---

### Added
- Add named MCP browser targets so agents can connect to a personal CDP browser and switch between personal, external, and Rust-managed browser sessions at runtime.
- Add optional VNC/noVNC display metadata for browser targets, including a Docker-managed noVNC proxy for host personal browsers.
- Add a local noVNC control panel for choosing the active browser target while one `browser-connection` MCP process keeps running.
- Add link-based shared browser targets through an Edge extension, `browser-connection-relay`, and `--browser-share NAME=URL`.
