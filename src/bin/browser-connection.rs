//! MCP stdio binary for docker-git's Rust browser connection.
//!
//! Usage:
//!   browser-connection --project dg-my-project
//!
//! This command intentionally replaces external upstream Playwright MCP configs.

use clap::Parser;
use docker_git_browser_connection::compute_browser_control_panel_port;
use docker_git_browser_connection::mcp::{
    active_browser_from_env, browser_endpoints_from_env, parse_named_browser_endpoint,
    parse_named_browser_novnc_url, parse_named_browser_share_url, parse_named_browser_vnc_endpoint,
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

    /// Register a VNC display endpoint for a browser target as NAME=HOST:PORT. Can be repeated.
    #[arg(long = "browser-vnc", value_name = "NAME=HOST:PORT")]
    browser_vnc: Vec<String>,

    /// Register a pre-existing noVNC URL for a browser target as NAME=URL. Can be repeated.
    #[arg(long = "browser-novnc", value_name = "NAME=URL")]
    browser_novnc: Vec<String>,

    /// Register a shared browser extension link as NAME=URL. Can be repeated.
    #[arg(long = "browser-share", value_name = "NAME=URL")]
    browser_share: Vec<String>,

    /// Register and select a personal browser CDP endpoint, e.g. http://127.0.0.1:9222.
    #[arg(long)]
    personal_browser: Option<String>,

    /// Register a VNC display endpoint for the personal browser, e.g. host.docker.internal:5900.
    #[arg(long)]
    personal_vnc: Option<String>,

    /// Register a pre-existing noVNC URL for the personal browser.
    #[arg(long)]
    personal_novnc: Option<String>,

    /// Browser target to use at startup: managed, explicit, personal, or a --browser name.
    #[arg(long)]
    active_browser: Option<String>,

    /// Host port for the local browser/noVNC control panel.
    #[arg(long)]
    control_port: Option<u16>,

    /// Disable the local browser/noVNC control panel.
    #[arg(long)]
    no_control_panel: bool,

    /// Do not start/reuse Docker browser on startup; useful for MCP handshake tests.
    #[arg(long)]
    no_start_browser: bool,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let cli = Cli::parse();
    let mut browser_endpoints = browser_endpoints_from_env()?;
    let mut active_browser = active_browser_from_env();
    let project_id = project_id_from_env_or_default(cli.project);
    let control_port = if cli.no_control_panel {
        None
    } else {
        Some(
            cli.control_port
                .unwrap_or_else(|| compute_browser_control_panel_port(&project_id)),
        )
    };

    for browser in &cli.browser {
        browser_endpoints.push(parse_named_browser_endpoint(browser)?);
    }

    let mut config = McpServerConfig::new(
        project_id,
        cli.network,
        cli.cdp_endpoint,
        !cli.no_start_browser,
    )
    .with_browser_endpoints(browser_endpoints)
    .with_control_port(control_port);

    for browser_vnc in &cli.browser_vnc {
        let (name, endpoint) = parse_named_browser_vnc_endpoint(browser_vnc)?;
        config = config.with_browser_vnc_endpoint(name, endpoint)?;
    }

    for browser_novnc in &cli.browser_novnc {
        let (name, url) = parse_named_browser_novnc_url(browser_novnc)?;
        config = config.with_browser_novnc_url(name, url)?;
    }

    for browser_share in &cli.browser_share {
        let (name, url) = parse_named_browser_share_url(browser_share)?;
        config = config.with_browser_share_url(name, url)?;
    }

    if let Some(endpoint) = cli.personal_browser {
        config = config.with_browser_endpoints(vec![NamedBrowserEndpoint::new(
            PERSONAL_BROWSER_NAME,
            endpoint,
        )?]);
        active_browser = Some(PERSONAL_BROWSER_NAME.to_string());
    }

    if let Some(endpoint) = cli.personal_vnc {
        config = config.with_browser_vnc_endpoint(PERSONAL_BROWSER_NAME, endpoint)?;
    }

    if let Some(url) = cli.personal_novnc {
        config = config.with_browser_novnc_url(PERSONAL_BROWSER_NAME, url)?;
    }

    if cli.active_browser.is_some() {
        active_browser = cli.active_browser;
    }

    let config = config.with_active_browser(active_browser)?;

    let stdin = io::stdin();
    let stdout = io::stdout();
    run_stdio(config, stdin.lock(), stdout.lock())
}
