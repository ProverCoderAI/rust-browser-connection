//! MCP stdio binary for docker-git's Rust browser connection.
//!
//! Usage:
//!   browser-connection --project dg-my-project
//!
//! This command intentionally replaces external upstream Playwright MCP configs.

use clap::Parser;
use docker_git_browser_connection::mcp::{
    active_browser_from_env, browser_endpoints_from_env, parse_named_browser_endpoint,
    project_id_from_env_or_default, run_stdio, McpServerConfig, NamedBrowserEndpoint,
    PERSONAL_BROWSER_NAME, SERVER_NAME,
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

    /// Register an additional browser target as NAME=CDP_ENDPOINT. Can be repeated.
    #[arg(long = "browser", value_name = "NAME=CDP_ENDPOINT")]
    browser: Vec<String>,

    /// Register and select a personal browser CDP endpoint, e.g. http://127.0.0.1:9222.
    #[arg(long)]
    personal_browser: Option<String>,

    /// Browser target to use at startup: managed, explicit, personal, or a --browser name.
    #[arg(long)]
    active_browser: Option<String>,

    /// Do not start/reuse Docker browser on startup; useful for MCP handshake tests.
    #[arg(long)]
    no_start_browser: bool,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let cli = Cli::parse();
    let mut browser_endpoints = browser_endpoints_from_env()?;
    let mut active_browser = active_browser_from_env();

    for browser in &cli.browser {
        browser_endpoints.push(parse_named_browser_endpoint(browser)?);
    }

    if let Some(endpoint) = cli.personal_browser {
        browser_endpoints.push(NamedBrowserEndpoint::new(PERSONAL_BROWSER_NAME, endpoint)?);
        active_browser = Some(PERSONAL_BROWSER_NAME.to_string());
    }

    if cli.active_browser.is_some() {
        active_browser = cli.active_browser;
    }

    let config = McpServerConfig::new(
        project_id_from_env_or_default(cli.project),
        cli.network,
        cli.cdp_endpoint,
        !cli.no_start_browser,
    )
    .with_browser_endpoints(browser_endpoints)
    .with_active_browser(active_browser)?;

    let stdin = io::stdin();
    let stdout = io::stdout();
    run_stdio(config, stdin.lock(), stdout.lock())
}
