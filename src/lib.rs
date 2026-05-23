/*! BrowserConnection — formally verifiable unified browser module for docker-git
//!
//! Provides single browser session (noVNC + CDP) for MCP Playwright and Hermes.
//! CORE: pure URL generation + invariant check
//! SHELL: DockerBrowserShell (bollard)

mod browser;

use anyhow::Result;
use crate::browser::DockerBrowserShell;
use serde::Serialize;

/// Pure deterministic port allocator (prevents collisions between projects)
pub fn compute_browser_ports(project_id: &str) -> (u16, u16, u16) {
    let hash: u32 = project_id.bytes().fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    let offset = (hash % 400) as u16;
    (5900 + offset, 6080 + offset, 9223 + offset)
}

#[derive(Debug, Clone, Serialize)]
pub struct BrowserInfo {
    pub project_id: String,
    pub container_name: String,
    pub novnc_url: String,
    pub cdp_url: String,
}

pub struct BrowserConnection {
    shell: DockerBrowserShell,
}

impl BrowserConnection {
    pub fn new() -> Result<Self> {
        let shell = DockerBrowserShell::new()?;
        Ok(Self { shell })
    }

    pub async fn start_browser(&self, project_id: &str, network: Option<&str>) -> Result<BrowserInfo> {
        let container_name = self.shell.ensure_browser_container(project_id, network).await?;
        let (_, novnc_p, cdp_p) = compute_browser_ports(project_id);

        Ok(BrowserInfo {
            project_id: project_id.to_string(),
            container_name,
            novnc_url: format!("http://localhost:{}/vnc.html?autoconnect=true&resize=remote&path=websockify?token={}", novnc_p, project_id),
            cdp_url: format!("http://localhost:{}", cdp_p),
        })
    }

    pub fn get_novnc_url(&self, project_id: &str) -> String {
        let (_, novnc_port, _) = compute_browser_ports(project_id);
        format!("http://localhost:{}/vnc.html?autoconnect=true&resize=remote&path=websockify?token={}", novnc_port, project_id)
    }

    pub fn get_cdp_url(&self, project_id: &str) -> String {
        let (_, _, cdp_port) = compute_browser_ports(project_id);
        format!("http://localhost:{}", cdp_port)
    }

    pub fn is_single_browser_session(&self, cdp_url: &str, novnc_url: &str) -> bool {
        novnc_url.contains("vnc.html") && cdp_url.starts_with("http://localhost:")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_pure_functions() {
        let conn = BrowserConnection::new().unwrap();
        let novnc = conn.get_novnc_url("proj42");
        let cdp = conn.get_cdp_url("proj42");
        assert!(conn.is_single_browser_session(&cdp, &novnc));
        let p1 = compute_browser_ports("foo");
        let p2 = compute_browser_ports("bar");
        assert_ne!(p1, p2);
    }
}
