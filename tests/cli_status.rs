use docker_git_browser_connection::{
    compute_browser_ports, render_cdp_url_for_ports, render_novnc_url_for_ports,
};
use std::process::Command;

#[test]
fn status_command_prints_single_browser_urls_without_docker() {
    let project = "docker-git-issue-347";
    let ports = compute_browser_ports(project);
    let output = Command::new(env!("CARGO_BIN_EXE_docker-git-browser-connection"))
        .args(["status", "--project", project])
        .output()
        .expect("Failed to execute binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&format!("noVNC: {}", render_novnc_url_for_ports(ports))));
    assert!(stdout.contains(&format!("CDP: {}", render_cdp_url_for_ports(ports))));
    assert!(stdout.contains("Invariant check: true"));
}

#[test]
fn start_help_exposes_resource_limit_flags_without_docker() {
    let output = Command::new(env!("CARGO_BIN_EXE_docker-git-browser-connection"))
        .args(["start", "--help"])
        .output()
        .expect("Failed to execute binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--cpu-limit"));
    assert!(stdout.contains("--ram-limit"));
}

#[test]
fn root_help_exposes_stop_command_without_docker() {
    let output = Command::new(env!("CARGO_BIN_EXE_docker-git-browser-connection"))
        .args(["--help"])
        .output()
        .expect("Failed to execute binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("stop"));
}

#[test]
fn browserctl_help_exposes_browser_actions_without_mcp() {
    let output = Command::new(env!("CARGO_BIN_EXE_browserctl"))
        .args(["--help"])
        .output()
        .expect("Failed to execute browserctl");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("snapshot"));
    assert!(stdout.contains("navigate"));
    assert!(stdout.contains("--share-url"));
    assert!(stdout.contains("--cdp-url"));
}

#[test]
fn browserctl_eval_requires_expression_or_file_before_network() {
    let output = Command::new(env!("CARGO_BIN_EXE_browserctl"))
        .args(["--cdp-url", "http://127.0.0.1:1", "eval"])
        .output()
        .expect("Failed to execute browserctl");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("eval requires --expression or --file"));
}
