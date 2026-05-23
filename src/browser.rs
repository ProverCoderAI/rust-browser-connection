/*! Docker shell layer for BrowserConnection (imperative effects via bollard).

Separated from core pure functions per AGENTS.md (CORE vs SHELL).
All Docker interactions live here.

INVARIANT: ∀ project_id, repeated ensure_browser_container(project_id) → exactly one container named "dg-{project_id}-browser"
The container provides noVNC (6080), VNC (5900), CDP (9223).
*/

use anyhow::{Context, Result};
use bollard::container::{Config, CreateContainerOptions, StartContainerOptions, NetworkingConfig};
use bollard::secret::{EndpointSettings, HostConfig, PortBinding};
use bollard::Docker;
use std::collections::HashMap;

pub struct DockerBrowserShell {
    docker: Docker,
}

impl DockerBrowserShell {
    pub fn new() -> Result<Self> {
        let docker = Docker::connect_with_local_defaults()
            .context("Docker connection failed")?;
        Ok(Self { docker })
    }

    /// Ensures a single browser container for the project.
    /// If network is Some(name), the container is created on that network (so main container can reach dg-*-browser:9223 by name).
    pub async fn ensure_browser_container(&self, project_id: &str, network: Option<&str>) -> Result<String> {
        let name = format!("dg-{}-browser", project_id);

        let mut port_bindings = HashMap::new();
        for (container_port, host_port) in [("5900", "5900"), ("6080", "6080"), ("9223", "9223")] {
            port_bindings.insert(
                format!("{}/tcp", container_port),
                Some(vec![PortBinding {
                    host_ip: Some("0.0.0.0".to_string()),
                    host_port: Some(host_port.to_string()),
                }]),
            );
        }

        let mut host_config = HostConfig {
            port_bindings: Some(port_bindings),
            ..Default::default()
        };

        let mut networking_config = None;
        if let Some(net) = network {
            let mut endpoints = HashMap::new();
            endpoints.insert(net.to_string(), EndpointSettings::default());
            networking_config = Some(NetworkingConfig {
                endpoints_config: endpoints,
            });
        }

        let config = Config {
            image: Some("selenium/standalone-chrome:latest".to_string()),
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
                // Already running — single session invariant preserved
                log::info!("Container {} already exists (single session invariant preserved)", name);
            }
            Err(e) => return Err(e.into()),
        }

        Ok(name)
    }
}
