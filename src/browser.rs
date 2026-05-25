/*! Docker shell layer for BrowserConnection (imperative effects via Docker CLI).

Separated from core pure functions per AGENTS.md (CORE vs SHELL).
All Docker interactions live here.

CHANGE: build and run the browser container from the Rust module itself.
WHY: issue #347 requires noVNC + MCP Playwright to live in one reusable Rust module, not duplicated TS templates.
QUOTE(ТЗ): "Вынести noVNC + MCP Playright в единый модуль."
REF: https://github.com/ProverCoderAI/docker-git/issues/347
SOURCE: n/a
FORMAT THEOREM: ensure(spec) -> running(spec.container_name) and network(spec.container_name)=spec.network_mode
PURITY: SHELL
EFFECT: Docker CLI process execution + temporary filesystem writes.
INVARIANT: repeated ensure_browser_container(spec) reuses exactly spec.container_name.
*/

use anyhow::{anyhow, Context, Result};
use std::fs;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{BrowserResourceLimits, BrowserSpec};

pub(crate) struct BrowserRuntime {
    pub container_name: String,
    pub novnc_url: String,
    pub cdp_url: String,
}

pub(crate) struct BrowserStopRuntime {
    pub container_name: String,
    pub removed: bool,
}

const BROWSER_DOCKERFILE: &str = r#"FROM kechangdev/browser-vnc:latest

# bash/procps keep upstream startup scripts compatible; socat exposes a stable CDP port.
# xwd/imagemagick provide deterministic X11 framebuffer screenshots for noVNC/CDP proof.
RUN apk add --no-cache bash procps socat python3 net-tools curl xwd imagemagick

# CHANGE: patch upstream noVNC/websockify for Python 3.12.
# WHY: old websockify calls array.array.fromstring(), removed in Python 3.12, which closes noVNC after the RFB protocol banner.
# QUOTE(ТЗ): "добиться что бы всё работало и этому были доказательства"
# REF: issue-347
# SOURCE: n/a
# FORMAT THEOREM: websocket_rfb_handshake -> security_types_frame, not websockify_exception(fromstring)
# PURITY: SHELL
# EFFECT: Docker build mutates vendored websockify Python files in the browser image.
# INVARIANT: noVNC connects to the same X11/VNC framebuffer that Chromium renders into.
RUN python3 -c "from pathlib import Path; root=Path('/opt/noVNC/utils/websockify'); [p.write_text(p.read_text().replace('.fromstring(', '.frombytes(').replace('.tostring(', '.tobytes(')) for p in root.rglob('*.py')]"

COPY docker-git-browser-start.sh /usr/local/bin/docker-git-browser-start.sh
RUN chmod +x /usr/local/bin/docker-git-browser-start.sh

ENTRYPOINT ["/usr/local/bin/docker-git-browser-start.sh"]
"#;

const BROWSER_START_SCRIPT: &str = r#"#!/usr/bin/env bash
set -euo pipefail

rm -f /data/SingletonLock /data/SingletonCookie /data/SingletonSocket || true

# CHANGE: force no-password shared VNC for automatic noVNC proof/control.
# WHY: docker-git's browser URL is opened by agents and users without a manual VNC password prompt.
# QUOTE(ТЗ): "автоматически для него поднимает noVNC что бы управлять единым браузером с агентом"
# REF: issue-347
# SOURCE: n/a
# FORMAT THEOREM: start_browser -> noVNC autoconnect reaches Chromium framebuffer
# PURITY: SHELL
# EFFECT: rewrites upstream supervisor config before /start.sh starts supervisord.
# INVARIANT: x11vnc remains shared, so noVNC viewing does not disconnect the agent-controlled browser.
for supervisor_file in /etc/supervisor.d/*.ini /etc/supervisor/conf.d/*.conf; do
  if [[ -f "$supervisor_file" ]]; then
    sed -i \
      -e 's|-forever -usepw -display :99 -rfbport 5900|-forever -nopw -shared -display :99 -rfbport 5900|g' \
      -e 's|x11vnc -forever -usepw|x11vnc -forever -nopw -shared|g' \
      "$supervisor_file"
  fi
done

# kechangdev/browser-vnc binds Chromium CDP on 127.0.0.1:9222.  MCP/Hermes use :9223.
# The proxy keeps Host checks stable and makes the endpoint reachable from the project namespace.
socat TCP-LISTEN:9223,fork,reuseaddr TCP:127.0.0.1:9222 &

exec /start.sh
"#;

pub struct DockerBrowserShell;

impl DockerBrowserShell {
    pub fn new() -> Self {
        Self
    }

    pub fn ensure_browser_container(
        &self,
        spec: &BrowserSpec,
        limits: &BrowserResourceLimits,
    ) -> Result<BrowserRuntime> {
        ensure_docker_available()?;
        ensure_browser_image(spec)?;

        let state = inspect_container_state(&spec.container_name)?;
        match state.as_deref() {
            Some("running") => {
                let mut runtime_spec = spec.clone();
                runtime_spec.network_mode = inspect_container_network_mode(&spec.container_name)?
                    .unwrap_or_else(|| spec.network_mode.clone());
                let (cdp_url, novnc_url) = runtime_urls(&runtime_spec)?;
                return Ok(BrowserRuntime {
                    container_name: spec.container_name.clone(),
                    novnc_url,
                    cdp_url,
                });
            }
            Some(_) => {
                docker(["rm", "-f", &spec.container_name], "docker rm browser")?;
            }
            None => {}
        }

        let mut runtime_spec = spec.clone();
        runtime_spec.network_mode = effective_network_mode(
            &spec.network_mode,
            referenced_container_state(&spec.network_mode)?.as_deref(),
        );

        ensure_volume(&runtime_spec.volume_name)?;
        run_browser_container(&runtime_spec, limits)?;
        let (cdp_url, novnc_url) = runtime_urls(&runtime_spec)?;
        Ok(BrowserRuntime {
            container_name: runtime_spec.container_name,
            novnc_url,
            cdp_url,
        })
    }

    pub fn stop_browser_container(&self, spec: &BrowserSpec) -> Result<BrowserStopRuntime> {
        ensure_docker_available()?;
        let removed = match inspect_container_state(&spec.container_name)? {
            Some(_) => {
                docker(["rm", "-f", &spec.container_name], "docker rm browser")?;
                true
            }
            None => false,
        };

        Ok(BrowserStopRuntime {
            container_name: spec.container_name.clone(),
            removed,
        })
    }
}

fn docker<const N: usize>(args: [&str; N], label: &str) -> Result<String> {
    let output = docker_command()
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to execute {label}"))?;

    docker_output(output, label)
}

fn docker_dynamic(args: &[String], label: &str) -> Result<String> {
    let output = docker_command()
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to execute {label}"))?;

    docker_output(output, label)
}

fn docker_output(output: std::process::Output, label: &str) -> Result<String> {
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }

    Err(anyhow!(
        "{label} failed with status {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    ))
}

const HOST_DOCKER_INTERNAL_DOCKER_HOST: &str = "tcp://host.docker.internal:2375";

// CHANGE: route Docker CLI through docker-git's configured or discoverable host daemon.
// WHY: MCP clients may scrub DOCKER_HOST from stdio server environments, while generated docker-git
//      containers still expose Docker through host.docker.internal:2375 rather than /var/run/docker.sock.
// QUOTE(ТЗ): "а почему он сам не находит эту ссылку?"
// REF: https://github.com/ProverCoderAI/docker-git/issues/347
// SOURCE: n/a
// FORMAT THEOREM: explicit(DOCKER_HOST) ∨ project_env ∨ unix_socket ∨ host_internal_tcp -> docker_cli_can_reach_daemon
// PURITY: SHELL
// EFFECT: may probe host.docker.internal:2375 and configures a child process environment only.
// INVARIANT: explicit DOCKER_HOST is never overridden; project-scoped fallback wins over autodetection.
// COMPLEXITY: O(a)/O(a), a = number of resolved host.docker.internal addresses.
fn docker_command() -> Command {
    let mut command = Command::new("docker");
    if let Some(host) = docker_host_override() {
        command.env("DOCKER_HOST", host);
    }
    command
}

fn docker_host_override() -> Option<String> {
    selected_docker_host_override(
        nonempty_env("DOCKER_HOST").as_deref(),
        nonempty_env("DOCKER_GIT_PROJECT_DOCKER_HOST").as_deref(),
        Path::new("/var/run/docker.sock").exists(),
        host_docker_internal_docker_api_available(),
    )
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn selected_docker_host_override(
    docker_host: Option<&str>,
    docker_git_project_docker_host: Option<&str>,
    unix_socket_exists: bool,
    host_docker_internal_available: bool,
) -> Option<String> {
    if docker_host.is_some_and(|host| !host.trim().is_empty()) {
        return None;
    }

    if let Some(host) = docker_git_project_docker_host
        .map(str::trim)
        .filter(|host| !host.is_empty())
    {
        return Some(host.to_string());
    }

    if unix_socket_exists {
        return None;
    }

    if host_docker_internal_available {
        return Some(HOST_DOCKER_INTERNAL_DOCKER_HOST.to_string());
    }

    None
}

fn host_docker_internal_docker_api_available() -> bool {
    let Ok(addrs) = ("host.docker.internal", 2375).to_socket_addrs() else {
        return false;
    };

    addrs
        .into_iter()
        .any(|addr| TcpStream::connect_timeout(&addr, Duration::from_millis(150)).is_ok())
}

fn ensure_docker_available() -> Result<()> {
    docker(["info"], "docker info").map(|_| ())
}

fn inspect_container_state(container_name: &str) -> Result<Option<String>> {
    let output = docker_command()
        .args(["inspect", "-f", "{{.State.Status}}", container_name])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .context("failed to inspect browser container")?;

    if !output.status.success() {
        return Ok(None);
    }

    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
    ))
}

fn inspect_container_network_mode(container_name: &str) -> Result<Option<String>> {
    let output = docker_command()
        .args([
            "inspect",
            "-f",
            "{{.HostConfig.NetworkMode}}",
            container_name,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("failed to inspect browser network mode for {container_name}"))?;

    if !output.status.success() {
        return Ok(None);
    }

    let network_mode = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if network_mode.is_empty() {
        Ok(None)
    } else {
        Ok(Some(network_mode))
    }
}

// CHANGE: avoid `docker run --network container:<missing>` hard failure by falling back to bridge.
// WHY: MCP stdio startup must work when launched before/without a docker-git project container.
// QUOTE(ТЗ): "добить задачу ... поднятие MCP Playright вместе с noVNC"
// REF: user report: Docker status 125, "No such container: dg-my-project"
// SOURCE: n/a
// FORMAT THEOREM: container:<name> ∧ state(name) != running -> bridge; otherwise preserve requested network.
// PURITY: CORE
// INVARIANT: explicit usable container namespace is kept; unusable default namespace cannot abort startup.
// COMPLEXITY: O(n)/O(n), n = |network_mode|.
fn effective_network_mode(network_mode: &str, referenced_state: Option<&str>) -> String {
    if network_mode.starts_with("container:") && referenced_state != Some("running") {
        "bridge".to_string()
    } else {
        network_mode.to_string()
    }
}

fn referenced_container_state(network_mode: &str) -> Result<Option<String>> {
    match network_mode.strip_prefix("container:") {
        Some(container_name) => inspect_container_state(container_name),
        None => Ok(None),
    }
}

fn should_publish_ports(network_mode: &str) -> bool {
    !network_mode.starts_with("container:") && network_mode != "host"
}

fn image_exists(image_name: &str) -> Result<bool> {
    let status = docker_command()
        .args(["image", "inspect", image_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to inspect browser image")?;
    Ok(status.success())
}

fn ensure_browser_image(spec: &BrowserSpec) -> Result<()> {
    if image_exists(&spec.image_name)? {
        return Ok(());
    }

    let context = BrowserBuildContext::create()?;
    docker(
        [
            "build",
            "-t",
            &spec.image_name,
            "-f",
            context.dockerfile_path_str(),
            context.path_str(),
        ],
        "docker build browser image",
    )?;
    Ok(())
}

fn ensure_volume(volume_name: &str) -> Result<()> {
    docker(["volume", "create", volume_name], "docker volume create").map(|_| ())
}

fn browser_resource_limit_args(limits: &BrowserResourceLimits) -> Vec<String> {
    let mut args = Vec::new();

    if let Some(cpu_limit) = &limits.cpu_limit {
        args.extend(["--cpus".to_string(), cpu_limit.clone()]);
    }

    if let Some(ram_limit) = &limits.ram_limit {
        args.extend(["--memory".to_string(), ram_limit.clone()]);
    }

    args
}

fn run_browser_container(spec: &BrowserSpec, limits: &BrowserResourceLimits) -> Result<()> {
    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        spec.container_name.clone(),
        "--label".to_string(),
        "docker-git.browser=1".to_string(),
        "--label".to_string(),
        format!("docker-git.project-container={}", spec.main_container_name),
        "--network".to_string(),
        spec.network_mode.clone(),
        "--shm-size".to_string(),
        "2g".to_string(),
    ];

    args.extend(browser_resource_limit_args(limits));

    if should_publish_ports(&spec.network_mode) {
        args.extend([
            "-p".to_string(),
            format!("127.0.0.1:{}:{}", spec.ports.vnc, crate::BROWSER_VNC_PORT),
            "-p".to_string(),
            format!(
                "127.0.0.1:{}:{}",
                spec.ports.novnc,
                crate::BROWSER_NOVNC_PORT
            ),
            "-p".to_string(),
            format!("127.0.0.1:{}:{}", spec.ports.cdp, crate::BROWSER_CDP_PORT),
        ]);
    }

    args.extend([
        "-e".to_string(),
        "VNC_NOPW=1".to_string(),
        "-v".to_string(),
        format!("{}:/data", spec.volume_name),
        spec.image_name.clone(),
    ]);

    docker_dynamic(&args, "docker run browser").map(|_| ())
}

fn cdp_probe_candidates(spec: &BrowserSpec) -> Vec<String> {
    let mut candidates = if should_publish_ports(&spec.network_mode) {
        vec![format!("http://127.0.0.1:{}/json/version", spec.ports.cdp)]
    } else {
        vec![format!(
            "http://127.0.0.1:{}/json/version",
            crate::BROWSER_CDP_PORT
        )]
    };

    if let Some(container_name) = spec.network_mode.strip_prefix("container:") {
        if let Ok(Some(ip)) = inspect_container_ip(container_name) {
            candidates.push(format!(
                "http://{}:{}/json/version",
                ip,
                crate::BROWSER_CDP_PORT
            ));
        }
    }

    if let Ok(Some(ip)) = inspect_container_ip(&spec.container_name) {
        candidates.push(format!(
            "http://{}:{}/json/version",
            ip,
            crate::BROWSER_CDP_PORT
        ));
    }

    candidates.sort();
    candidates.dedup();
    candidates
}

fn novnc_probe_candidates(spec: &BrowserSpec) -> Vec<String> {
    let mut candidates = if should_publish_ports(&spec.network_mode) {
        vec![crate::render_novnc_url_for_ports(spec.ports)]
    } else {
        vec![crate::render_novnc_url()]
    };

    if let Some(container_name) = spec.network_mode.strip_prefix("container:") {
        if let Ok(Some(ip)) = inspect_container_ip(container_name) {
            candidates.push(format!(
                "http://{}:{}/vnc.html?autoconnect=true&resize=remote&path=websockify",
                ip,
                crate::BROWSER_NOVNC_PORT
            ));
        }
    }

    if let Ok(Some(ip)) = inspect_container_ip(&spec.container_name) {
        candidates.push(format!(
            "http://{}:{}/vnc.html?autoconnect=true&resize=remote&path=websockify",
            ip,
            crate::BROWSER_NOVNC_PORT
        ));
    }

    candidates.sort();
    candidates.dedup();
    candidates
}

fn runtime_urls(spec: &BrowserSpec) -> Result<(String, String)> {
    let cdp_url = wait_for_cdp(spec)?;
    let novnc_url = wait_for_novnc(spec)?;
    Ok((cdp_url, novnc_url))
}

fn cdp_version_url_to_base(url: &str) -> String {
    url.trim_end_matches("/json/version").to_string()
}

// CHANGE: probe CDP through localhost first, then Docker bridge/container IP fallbacks.
// WHY: host-side verification often cannot reach localhost:9223 when the Rust-created browser shares a project network namespace.
// QUOTE(ТЗ): "Если localhost:9223 не работает, найди bridge IP через docker inspect"
// REF: issue-347
// SOURCE: n/a
// FORMAT THEOREM: reachable(cdp, localhost ∪ bridge_ips) -> wait_for_cdp succeeds
// PURITY: SHELL
// EFFECT: curl subprocesses observe network readiness.
// INVARIANT: success proves the CDP endpoint for spec.container_name is answering /json/version.
// COMPLEXITY: O(a*b), a = attempts, b = candidate endpoints.
fn wait_for_cdp(spec: &BrowserSpec) -> Result<String> {
    ensure_curl_available()?;
    let mut last_candidates = Vec::new();
    for _ in 0..60 {
        last_candidates = cdp_probe_candidates(spec);
        for url in &last_candidates {
            let status = Command::new("curl")
                .args([
                    "-sSf",
                    "--connect-timeout",
                    "2",
                    "--max-time",
                    "5",
                    "-H",
                    "Host: 127.0.0.1:9222",
                    url,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if matches!(status, Ok(exit) if exit.success()) {
                return Ok(cdp_version_url_to_base(url));
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Err(anyhow!(
        "browser CDP endpoint did not become ready; tried: {}",
        last_candidates.join(", ")
    ))
}

fn wait_for_novnc(spec: &BrowserSpec) -> Result<String> {
    ensure_curl_available()?;
    let mut last_candidates = Vec::new();
    for _ in 0..30 {
        last_candidates = novnc_probe_candidates(spec);
        for url in &last_candidates {
            let status = Command::new("curl")
                .args(["-sSf", "--connect-timeout", "2", "--max-time", "5", url])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if matches!(status, Ok(exit) if exit.success()) {
                return Ok(url.to_string());
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Err(anyhow!(
        "browser noVNC endpoint did not become ready; tried: {}",
        last_candidates.join(", ")
    ))
}

// CHANGE: fail fast when the host shell lacks curl for CDP readiness checks.
// WHY: otherwise a missing binary is indistinguishable from a slow browser and produces a misleading timeout.
// QUOTE(ТЗ): "добиться что бы всё работало и этому были доказательства"
// REF: issue-347, CodeRabbit review 3294618470
// SOURCE: n/a
// FORMAT THEOREM: ¬exists(curl) -> wait_for_cdp errors before network polling.
// PURITY: SHELL
// EFFECT: executes `curl --version` to validate the external probe dependency.
// INVARIANT: CDP timeout errors now only represent attempted network probes, not missing curl.
// COMPLEXITY: O(1)
fn ensure_curl_available() -> Result<()> {
    let status = Command::new("curl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("curl is required for CDP readiness probing but was not found")?;

    if !status.success() {
        return Err(anyhow!(
            "curl is required for CDP readiness probing but exited with status {status}"
        ));
    }

    Ok(())
}

fn inspect_container_ip(container_name: &str) -> Result<Option<String>> {
    let output = docker_command()
        .args([
            "inspect",
            "-f",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
            container_name,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("failed to inspect container IP for {container_name}"))?;

    if !output.status.success() {
        return Ok(None);
    }

    let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ip.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ip))
    }
}

struct BrowserBuildContext {
    path: PathBuf,
    dockerfile: PathBuf,
}

impl BrowserBuildContext {
    fn create() -> Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock before unix epoch")?
            .as_nanos();
        let path = std::env::temp_dir().join(format!("docker-git-browser-build-{nonce}"));
        fs::create_dir_all(&path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        let dockerfile = path.join("Dockerfile.browser");
        fs::write(&dockerfile, BROWSER_DOCKERFILE).context("failed to write browser Dockerfile")?;
        fs::write(
            path.join("docker-git-browser-start.sh"),
            BROWSER_START_SCRIPT,
        )
        .context("failed to write browser start script")?;
        Ok(Self { path, dockerfile })
    }

    fn path_str(&self) -> &str {
        path_to_str(&self.path)
    }

    fn dockerfile_path_str(&self) -> &str {
        path_to_str(&self.dockerfile)
    }
}

impl Drop for BrowserBuildContext {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn path_to_str(path: &Path) -> &str {
    path.to_str()
        .expect("temporary Docker build path must be valid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_project_container_falls_back_to_bridge_network() {
        assert_eq!(
            effective_network_mode("container:dg-missing", None),
            "bridge"
        );
        assert_eq!(
            effective_network_mode("container:dg-project", Some("running")),
            "container:dg-project"
        );
        assert_eq!(effective_network_mode("bridge", None), "bridge");
    }

    #[test]
    fn browser_resource_limit_args_are_omitted_when_limits_are_empty() {
        assert!(browser_resource_limit_args(&BrowserResourceLimits::none()).is_empty());
    }

    #[test]
    fn browser_resource_limit_args_render_docker_run_limits() {
        assert_eq!(
            browser_resource_limit_args(&BrowserResourceLimits::from_values(
                Some("0.5"),
                Some("1g")
            )),
            vec![
                "--cpus".to_string(),
                "0.5".to_string(),
                "--memory".to_string(),
                "1g".to_string()
            ]
        );
    }

    #[test]
    fn docker_host_autodetect_keeps_explicit_env_and_project_env_precedence() {
        assert_eq!(
            selected_docker_host_override(
                Some("tcp://explicit.example:2375"),
                Some("tcp://project.example:2375"),
                false,
                true,
            ),
            None
        );
        assert_eq!(
            selected_docker_host_override(None, Some("tcp://project.example:2375"), false, true),
            Some("tcp://project.example:2375".to_string())
        );
    }

    #[test]
    fn docker_host_autodetect_falls_back_to_host_docker_internal_when_socket_missing() {
        assert_eq!(
            selected_docker_host_override(None, None, false, true),
            Some("tcp://host.docker.internal:2375".to_string())
        );
        assert_eq!(selected_docker_host_override(None, None, true, true), None);
        assert_eq!(
            selected_docker_host_override(None, None, false, false),
            None
        );
    }
}
