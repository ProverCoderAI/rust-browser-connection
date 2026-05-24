//! MCP stdio binary for docker-git's Rust browser connection.
//!
//! Usage:
//!   browser-connection --project dg-my-project
//!
//! This command intentionally replaces external upstream Playwright MCP configs.

use clap::Parser;
use docker_git_browser_connection::mcp::{
    project_id_from_env_or_default, run_stdio, McpServerConfig, SERVER_NAME,
};
use std::io;

#[derive(Debug, Parser)]
#[command(
    name = SERVER_NAME,
    version,
    about = "Rust MCP stdio server for docker-git's single noVNC/CDP browser"
)]
struct Cli {
    /// docker-git project id/container namespace, e.g. dg-my-project.
    #[arg(long)]
    project: Option<String>,

    /// Docker network mode for auto-started browser container.
    #[arg(long)]
    network: Option<String>,

    /// Explicit CDP endpoint override, e.g. http://127.0.0.1:9223.
    #[arg(long)]
    cdp_endpoint: Option<String>,

    /// Do not start/reuse Docker browser on startup; useful for MCP handshake tests.
    #[arg(long)]
    no_start_browser: bool,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let cli = Cli::parse();
    let config = McpServerConfig::new(
        project_id_from_env_or_default(cli.project),
        cli.network,
        cli.cdp_endpoint,
        !cli.no_start_browser,
    );

    let stdin = io::stdin();
    let stdout = io::stdout();
    run_stdio(config, stdin.lock(), stdout.lock())
}
