//! Docker shell layer for BrowserConnection (imperative effects via bollard).
//!
//! Separated from core pure functions per AGENTS.md (CORE vs SHELL).
//! All Docker interactions live here.

use anyhow::{Context, Result};
use bollard::container::{Config, CreateContainerOptions, StartContainerOptions};
use bollard::models::{HostConfig, PortBinding};
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

    pub async fn ensure_browser_container(&self, project_id: &str) -> Result<String> {
        let name = format!("dg-{}-browser", project_id);

        // In full impl: list containers first, return existing if matches name (enforce single)
        // For MVP we rely on name uniqueness.

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

        let config = Config {
            image: Some("selenium/standalone-chrome:latest".to_string()),
            host_config: Some(HostConfig {
                port_bindings: Some(port_bindings),
                ..Default::default()
            }),
            ..Default::default()
        };

        let opts = Some(CreateContainerOptions { name: name.clone(), ..Default::default() });

        match self.docker.create_container(opts, config).await {
            Ok(_) => {
                self.docker.start_container(&name, None::<StartContainerOptions<String>>).await?;
            }
            Err(e) if e.to_string().contains("already exists") || e.to_string().contains("409") => {
                // Already running — good, invariant held
                log::info!("Container {} already exists (single session invariant preserved)", name);
            }
            Err(e) => return Err(e.into()),
        }

        Ok(name)
    }
}
