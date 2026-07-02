//! WebSocket relay for browser-connection shared browser extension sessions.

use clap::Parser;
use docker_git_browser_connection::shared_browser::{run_relay, RelayConfig, DEFAULT_RELAY_BIND};

#[derive(Debug, Parser)]
#[command(
    name = "browser-connection-relay",
    version,
    about = "WebSocket relay for browser-connection shared browser links"
)]
struct Cli {
    /// Address to bind, e.g. 127.0.0.1:8765 or 0.0.0.0:8765.
    #[arg(long, default_value = DEFAULT_RELAY_BIND)]
    bind: String,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let cli = Cli::parse();
    run_relay(RelayConfig::new(cli.bind))
}
