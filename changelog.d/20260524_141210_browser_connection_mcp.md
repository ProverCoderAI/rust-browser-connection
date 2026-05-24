---
bump: minor
---

### Added
- Added the `browser-connection` Rust MCP stdio server for Codex/Hermes configs, with direct CDP tools for navigating, snapshotting, evaluating, clicking, typing, key presses, and screenshots against the same noVNC-visible browser session.

### Changed
- Documented `command = "browser-connection"` as the supported product path and made the old external Playwright MCP command shape a forbidden runtime configuration.
