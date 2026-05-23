# docker-git-browser-connection

**Unified single-browser module for docker-git** (noVNC + CDP on one container `dg-{project}-browser`).

Solves GitHub issue #347: when using MCP Playwright, it automatically gets noVNC for the same browser the agent (Hermes) uses.

## Из коробки usage

```bash
# Install
cargo install --git https://github.com/ProverCoderAI/rust-browser-connection

# Start the single browser for a project
docker-git-browser-connection start --project my-project

# Output:
# Browser started for project: my-project
# Container: dg-my-project-browser
# noVNC: http://localhost:6080/...
# CDP (for MCP Playwright / Hermes): http://localhost:9223?project=my-project
```

## For MCP Playwright

Point your MCP Playwright config to the CDP URL returned above.  
The same browser instance will be visible in noVNC — **one browser, zero duplication**.

## Integration in docker-git

The Dockerfile does:

```dockerfile
RUN cargo install --git https://github.com/ProverCoderAI/rust-browser-connection.git docker-git-browser-connection
```

Then call the binary from entrypoints / MCP tools.

Old TypeScript browser code is removed (see AGENTS.md and plan).

## Formal Guarantees

- **Invariant**: Exactly one container per project (`dg-*-browser`)
- Pure functions for URLs
- Typed errors, no panics in core
- See `src/lib.rs` for AGENTS.md-style comments and theorems

## Development

```bash
cargo test
cargo build --release
```

See the plan in `.hermes/plans/` for full implementation roadmap.
