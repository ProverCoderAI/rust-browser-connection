---
type: feature
category: core
---
feat: implement unified noVNC + CDP browser module in Rust (docker-git-browser-connection) per #347

- Single `dg-{project}-browser` container with ports 5900/6080/9223
- CLI `docker-git-browser-connection start --project <id>` — "из коробки"
- Formal invariants in code (single session, pure URL functions)
- MCP Playwright can attach via CDP URL, automatically gets noVNC
- Old TS duplication removed in favor of this separate Rust crate
