/*! BrowserConnection — formally verifiable unified browser module for docker-git
//!
//! Provides one browser session (noVNC + CDP) for MCP Playwright and Hermes.
//! CORE: pure URL/name/env resolution + invariant checks.
//! SHELL: DockerBrowserShell in `browser.rs`.
//!
//! CHANGE: separate pure BrowserSpec from Docker side effects.
//! WHY: issue #347 requires a reusable Rust-only module that docker-git can install and call.
//! QUOTE(ТЗ): "Вынести noVNC + MCP Playright в единый модуль."
//! REF: https://github.com/ProverCoderAI/docker-git/issues/347
//! SOURCE: n/a
//! FORMAT THEOREM: forall p: ProjectId, spec(p).container = normalize(p) + "-browser" unless env overrides it
//! PURITY: CORE
//! INVARIANT: pure helpers never require Docker and are deterministic for the same inputs.
*/

mod browser;
pub mod cdp;
pub mod mcp;

use crate::browser::DockerBrowserShell;
use anyhow::Result;
use serde::Serialize;
use std::env;

pub const BROWSER_VNC_PORT: u16 = 5900;
pub const BROWSER_NOVNC_PORT: u16 = 6080;
pub const BROWSER_CDP_PORT: u16 = 9223;
const DOCKER_GIT_CONTAINER_PREFIX: &str = "dg-";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrowserSpec {
    pub project_id: String,
    pub main_container_name: String,
    pub container_name: String,
    pub image_name: String,
    pub volume_name: String,
    pub network_mode: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BrowserInfo {
    pub project_id: String,
    pub container_name: String,
    pub novnc_url: String,
    pub cdp_url: String,
}

fn resolved_or<F>(resolve: &F, name: &str, fallback: String) -> String
where
    F: Fn(&str) -> Option<String>,
{
    resolve(name)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
}

fn env_value(name: &str) -> Option<String> {
    env::var(name).ok()
}

// CHANGE: split BrowserSpec construction from process environment reads.
// WHY: tests and downstream callers need deterministic default specs even when DOCKER_GIT_* env vars exist.
// QUOTE(ТЗ): "чистый Rust-only модуль, без дублей и с формальными инвариантами"
// REF: issue-347, CodeRabbit review 3294618472
// SOURCE: n/a
// FORMAT THEOREM: resolver = ∅ -> spec(project_id) is deterministic and Docker-free.
// PURITY: CORE
// INVARIANT: BrowserSpec default derivation is independent from ambient CI/developer env unless browser_spec_from_env is used.
// COMPLEXITY: O(n)/O(n), n = total resolved string length.
fn browser_spec_from_resolver<F>(project_id: &str, network: Option<&str>, resolve: F) -> BrowserSpec
where
    F: Fn(&str) -> Option<String>,
{
    let project = project_id.trim();
    let project = if project.is_empty() {
        "default"
    } else {
        project
    };
    let normalized_container = normalize_project_container_name(project);
    let main_container_name = resolved_or(
        &resolve,
        "DOCKER_GIT_PROJECT_CONTAINER_NAME",
        normalized_container,
    );
    let container_name = resolved_or(
        &resolve,
        "DOCKER_GIT_BROWSER_CONTAINER_NAME",
        format!("{}-browser", main_container_name),
    );
    let image_name = resolved_or(
        &resolve,
        "DOCKER_GIT_BROWSER_IMAGE_NAME",
        format!("{}:docker-git-browser", container_name),
    );
    let volume_name = resolved_or(
        &resolve,
        "DOCKER_GIT_BROWSER_VOLUME_NAME",
        format!("{}-data", container_name),
    );
    let network_mode = network
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("container:{}", main_container_name));

    BrowserSpec {
        project_id: project.to_string(),
        main_container_name,
        container_name,
        image_name,
        volume_name,
        network_mode,
    }
}

pub fn browser_spec_from_defaults(project_id: &str, network: Option<&str>) -> BrowserSpec {
    browser_spec_from_resolver(project_id, network, |_| None)
}

// CHANGE: normalize external project ids to docker-git's concrete container namespace.
// WHY: issue #347 proof names the logical project as "docker-git-issue-347" while Docker resources are `dg-docker-git-issue-347-*`.
// QUOTE(ТЗ): "релевантный контейнер: dg-docker-git-issue-347-browser"
// REF: issue-347
// SOURCE: n/a
// FORMAT THEOREM: ∀p ∈ ProjectId: normalize(p).starts_with("dg-") ∧ normalize(normalize(p)) = normalize(p)
// PURITY: CORE
// INVARIANT: project ids are idempotently mapped to the single docker-git namespace.
// COMPLEXITY: O(n)/O(n), n = |project_id|.
pub fn normalize_project_container_name(project_id: &str) -> String {
    let project = project_id.trim();
    let project = if project.is_empty() {
        "default"
    } else {
        project
    };
    if project.starts_with(DOCKER_GIT_CONTAINER_PREFIX) {
        project.to_string()
    } else {
        format!("{DOCKER_GIT_CONTAINER_PREFIX}{project}")
    }
}

// CHANGE: derive Docker runtime names from docker-git env first, then from normalized project id.
// WHY: docker-git already writes DOCKER_GIT_BROWSER_* env into compose; respecting it prevents name drift.
// QUOTE(ТЗ): "браузер поднимается внутри докера"
// REF: issue-347
// SOURCE: n/a
// FORMAT THEOREM: env.container_name != empty -> spec.container_name = env.container_name
// PURITY: CORE (except environment boundary read isolated in this constructor)
// INVARIANT: BrowserSpec has non-empty Docker object names and exactly one browser container name.
// COMPLEXITY: O(n)/O(n), n = total env string length.
pub fn browser_spec_from_env(project_id: &str, network: Option<&str>) -> BrowserSpec {
    browser_spec_from_resolver(project_id, network, env_value)
}

pub fn render_novnc_url() -> String {
    format!(
        "http://127.0.0.1:{}/vnc.html?autoconnect=true&resize=remote&path=websockify",
        BROWSER_NOVNC_PORT
    )
}

pub fn render_cdp_url() -> String {
    format!("http://127.0.0.1:{}", BROWSER_CDP_PORT)
}

pub fn is_single_browser_session(cdp_url: &str, novnc_url: &str) -> bool {
    novnc_url.contains("/vnc.html") && cdp_url == render_cdp_url()
}

pub struct BrowserConnection {
    shell: DockerBrowserShell,
}

impl BrowserConnection {
    pub fn new() -> Result<Self> {
        let shell = DockerBrowserShell::new();
        Ok(Self { shell })
    }

    pub fn start_browser(&self, project_id: &str, network: Option<&str>) -> Result<BrowserInfo> {
        let spec = browser_spec_from_env(project_id, network);
        let container_name = self.shell.ensure_browser_container(&spec)?;

        Ok(BrowserInfo {
            project_id: spec.project_id,
            container_name,
            novnc_url: render_novnc_url(),
            cdp_url: render_cdp_url(),
        })
    }

    pub fn get_novnc_url(&self, _project_id: &str) -> String {
        render_novnc_url()
    }

    pub fn get_cdp_url(&self, _project_id: &str) -> String {
        render_cdp_url()
    }

    pub fn is_single_browser_session(&self, cdp_url: &str, novnc_url: &str) -> bool {
        is_single_browser_session(cdp_url, novnc_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_urls_do_not_require_docker() {
        let novnc = render_novnc_url();
        let cdp = render_cdp_url();
        assert_eq!(cdp, "http://127.0.0.1:9223");
        assert!(novnc.contains("/vnc.html"));
        assert!(is_single_browser_session(&cdp, &novnc));
    }

    #[test]
    fn default_spec_uses_normalized_project_container_namespace() {
        let spec = browser_spec_from_defaults("docker-git-issue-347", None);
        assert_eq!(spec.project_id, "docker-git-issue-347");
        assert_eq!(spec.main_container_name, "dg-docker-git-issue-347");
        assert_eq!(spec.container_name, "dg-docker-git-issue-347-browser");
        assert_eq!(
            spec.image_name,
            "dg-docker-git-issue-347-browser:docker-git-browser"
        );
        assert_eq!(spec.network_mode, "container:dg-docker-git-issue-347");
    }
}
