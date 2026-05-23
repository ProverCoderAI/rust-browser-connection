# Per-project host ports for browser containers

**Date**: 2026-05-23

**Type**: fix / improvement

## Summary
Replaced hardcoded host ports (5900/6080/9223) with deterministic project-specific ports.

This eliminates "port already allocated" and "connection refused" errors when multiple `dg-*-browser` containers run at the same time (common in development with several issues/PRs).

## Changes
- Added pure `compute_browser_ports(project_id)` in CORE (lib.rs)
- SHELL now uses the computed ports for Docker PortBindings
- URLs returned by `start` / `status` / `get_*_url` are now correct for the project
- Unit test added for the property "different projects → different ports"
- Updated formal invariants and AGENTS.md comments

## Impact
- `docker-git-browser-connection start --project foo` now binds unique ports
- Multiple projects can safely coexist
- MCP Playwright / Hermes agent gets a working localhost CDP URL for each project
- Backward compatible for single-project usage (just the port numbers change)

## Verification
- Property test in `cargo test`
- Manual verification via docker rust image (ports bound and reachable)
- Existing CDP guard + noVNC continue to work

See commits 40ba5132 and 51beb4a5.
