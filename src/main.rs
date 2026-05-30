//! CLI entrypoint for docker-git-browser-connection
//!
//! Usage (из коробки):
//!   docker-git-browser-connection start --project dg-myproj
//!
//! This binary is what MCP Playwright / Hermes / docker-git entrypoints invoke
//! to guarantee a single unified browser (noVNC + CDP).

use clap::{Parser, Subcommand};
use docker_git_browser_connection::{
    browser_resource_limits_from_env, BrowserConnection, BrowserInfo,
};

#[derive(Parser)]
#[command(
    name = "docker-git-browser-connection",
    version,
    about = "Unified noVNC + CDP browser for docker-git, MCP Playwright and Hermes (per #347)"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start (or reuse) the single browser container for a project
    Start {
        #[arg(long)]
        project: String,
        /// Docker network mode for the browser container. Defaults to container:<project container>.
        #[arg(long)]
        network: Option<String>,
        /// Docker --cpus limit for the browser container. Defaults to DOCKER_GIT_BROWSER_CPU_LIMIT.
        #[arg(long)]
        cpu_limit: Option<String>,
        /// Docker --memory limit for the browser container. Defaults to DOCKER_GIT_BROWSER_RAM_LIMIT.
        #[arg(long)]
        ram_limit: Option<String>,
    },
    /// Stop and remove the single browser container for a project
    Stop {
        #[arg(long)]
        project: String,
    },
    /// Show status / URLs for the project's browser
    Status {
        #[arg(long)]
        project: String,
    },
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let cli = Cli::parse();
    let conn = BrowserConnection::new()?;

    match cli.command {
        Commands::Start {
            project,
            network,
            cpu_limit,
            ram_limit,
        } => {
            let net_ref = network.as_deref();
            let limits = browser_resource_limits_from_env()
                .with_cli_overrides(cpu_limit.as_deref(), ram_limit.as_deref());
            let info: BrowserInfo = conn.start_browser_with_limits(&project, net_ref, &limits)?;
            println!("Browser started for project: {}", info.project_id);
            println!("Container: {}", info.container_name);
            println!("noVNC: {}", info.novnc_url);
            println!("CDP (for MCP Playwright / Hermes): {}", info.cdp_url);
            println!("Use the CDP URL in your MCP Playwright config to get automatic noVNC.");
        }
        Commands::Stop { project } => {
            let info = conn.stop_browser(&project)?;
            if info.removed {
                println!("Browser stopped for project: {}", info.project_id);
            } else {
                println!("Browser already absent for project: {}", info.project_id);
            }
            println!("Container: {}", info.container_name);
        }
        Commands::Status { project } => {
            let novnc = conn.get_novnc_url(&project);
            let cdp = conn.get_cdp_url(&project);
            println!("noVNC: {}", novnc);
            println!("CDP: {}", cdp);
            println!(
                "Invariant check: {}",
                conn.is_single_browser_session(&cdp, &novnc)
            );
        }
    }

    Ok(())
}
