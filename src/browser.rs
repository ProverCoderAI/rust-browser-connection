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
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::BrowserSpec;

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

    pub fn ensure_browser_container(&self, spec: &BrowserSpec) -> Result<String> {
        ensure_docker_available()?;
        ensure_browser_image(spec)?;

        let state = inspect_container_state(&spec.container_name)?;
        match state.as_deref() {
            Some("running") => return Ok(spec.container_name.clone()),
            Some(_) => {
                docker(["rm", "-f", &spec.container_name], "docker rm browser")?;
            }
            None => {}
        }

        ensure_volume(&spec.volume_name)?;
        run_browser_container(spec)?;
        wait_for_cdp(spec)?;
        Ok(spec.container_name.clone())
    }
}

fn docker<const N: usize>(args: [&str; N], label: &str) -> Result<String> {
    let output = docker_command()
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to execute {label}"))?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }

    Err(anyhow!(
        "{label} failed with status {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    ))
}

// CHANGE: route Docker CLI through docker-git's configured host daemon when present.
// WHY: generated docker-git project containers expose Docker via DOCKER_GIT_PROJECT_DOCKER_HOST,
//      not necessarily via /var/run/docker.sock; the Rust module must work out-of-the-box there.
// QUOTE(ТЗ): "к агенту подключить возможность работать с нашим браузером который запущен в докере"
// REF: https://github.com/ProverCoderAI/docker-git/issues/347
// SOURCE: n/a
// FORMAT THEOREM: DOCKER_HOST ∨ DOCKER_GIT_PROJECT_DOCKER_HOST -> docker_cli_can_reach_daemon
// PURITY: SHELL
// EFFECT: configures a child process environment only.
// INVARIANT: an explicit DOCKER_HOST always wins over docker-git's fallback variable.
// COMPLEXITY: O(n)/O(n), n = env string length.
fn docker_command() -> Command {
    let mut command = Command::new("docker");
    if std::env::var_os("DOCKER_HOST").is_none() {
        if let Some(host) = std::env::var_os("DOCKER_GIT_PROJECT_DOCKER_HOST") {
            if !host.is_empty() {
                command.env("DOCKER_HOST", host);
            }
        }
    }
    command
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

fn run_browser_container(spec: &BrowserSpec) -> Result<()> {
    docker(
        [
            "run",
            "-d",
            "--name",
            &spec.container_name,
            "--label",
            "docker-git.browser=1",
            "--label",
            &format!("docker-git.project-container={}", spec.main_container_name),
            "--network",
            &spec.network_mode,
            "--shm-size",
            "2g",
            "-e",
            "VNC_NOPW=1",
            "-v",
            &format!("{}:/data", spec.volume_name),
            &spec.image_name,
        ],
        "docker run browser",
    )
    .map(|_| ())
}

fn cdp_probe_candidates(spec: &BrowserSpec) -> Vec<String> {
    let mut candidates = vec![format!(
        "http://127.0.0.1:{}/json/version",
        crate::BROWSER_CDP_PORT
    )];

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
fn wait_for_cdp(spec: &BrowserSpec) -> Result<()> {
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
                return Ok(());
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Err(anyhow!(
        "browser CDP endpoint did not become ready; tried: {}",
        last_candidates.join(", ")
    ))
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
