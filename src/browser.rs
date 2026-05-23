/*! Docker shell layer for BrowserConnection (imperative effects via bollard).

Separated from core pure functions per AGENTS.md (CORE vs SHELL).
All Docker interactions live here.

INVARIANT: ∀ project_id, repeated ensure_browser_container(project_id) → exactly one container named "dg-{project_id}-browser"
*/

use anyhow::{Context, Result};
use bollard::container::{Config, CreateContainerOptions, StartContainerOptions, NetworkingConfig};
use bollard::secret::{EndpointSettings, HostConfig, PortBinding};
use bollard::Docker;
use std::collections::HashMap;

use crate::compute_browser_ports;

pub struct DockerBrowserShell {
    docker: Docker,
}

impl DockerBrowserShell {
    pub fn new() -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()
            .context("Docker connection failed")?;
        Ok(Self { docker })
    }

    pub async fn ensure_browser_container(&self, project_id: &str, network: Option<&str>) -> Result<String> {
        let name = format!("dg-{}-browser", project_id);

        let (vnc_host, novnc_host, cdp_host) = compute_browser_ports(project_id);

        let mut port_bindings = HashMap::new();
        port_bindings.insert(
            "5900/tcp".to_string(),
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some(vnc_host.to_string()),
            }]),
        );
        port_bindings.insert(
            "6080/tcp".to_string(),
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some(novnc_host.to_string()),
            }]),
        );
        port_bindings.insert(
            "9223/tcp".to_string(),
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some(cdp_host.to_string()),
            }]),
        );

        let host_config = HostConfig {
            port_bindings: Some(port_bindings),
            ..Default::default()
        };

        let mut networking_config = None;
        if let Some(net) = network {
            let mut endpoints = HashMap::new();
            endpoints.insert(net.to_string(), EndpointSettings::default());
            networking_config = Some(NetworkingConfig { endpoints_config: endpoints });
        }

        let config = Config {
            image: Some("dg-docker-git-issue-347-browser:docker-git-browser".to_string()),
            host_config: Some(host_config),
            networking_config,
            ..Default::default()
        };

        let opts = Some(CreateContainerOptions { name: name.clone(), ..Default::default() });

        match self.docker.create_container(opts, config).await {
            Ok(_) => {
                self.docker.start_container(&name, None::<StartContainerOptions<String>>).await?;
            }
            Err(e) if e.to_string().contains("already exists") || e.to_string().contains("409") => {
                log::info!("Container {} already exists (single session invariant preserved)", name);
            }
            Err(e) => return Err(e.into()),
        }

        Ok(name)
    }
}
