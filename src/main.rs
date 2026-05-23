//! CLI entrypoint for docker-git-browser-connection
//!
//! Usage (из коробки):
//!   docker-git-browser-connection start --project myproj
//!
//! This binary is what MCP Playwright / Hermes / docker-git entrypoints invoke
//! to guarantee a single unified browser (noVNC + CDP).

use clap::{Parser, Subcommand};
use docker_git_browser_connection::{BrowserConnection, BrowserInfo};

#[derive(Parser)]
#[command(name = "docker-git-browser-connection", version, about = "Unified noVNC + CDP browser for docker-git, MCP Playwright and Hermes (per #347)")]
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
    },
    /// Show status / URLs for the project's browser
    Status {
        #[arg(long)]
        project: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    let cli = Cli::parse();

    let conn = BrowserConnection::new()?;

    match cli.command {
        Commands::Start { project } => {
            let info: BrowserInfo = conn.start_browser(&project).await?;
            println!("Browser started for project: {}", info.project_id);
            println!("Container: {}", info.container_name);
            println!("noVNC: {}", info.novnc_url);
            println!("CDP (for MCP Playwright / Hermes): {}", info.cdp_url);
            println!("Use the CDP URL in your MCP Playwright config to get automatic noVNC.");
        }
        Commands::Status { project } => {
            let novnc = conn.get_novnc_url(&project);
            let cdp = conn.get_cdp_url(&project);
            println!("noVNC: {}", novnc);
            println!("CDP: {}", cdp);
            println!("Invariant check: {}", conn.is_single_browser_session(&cdp, &novnc));
        }
    }

    Ok(())
}
