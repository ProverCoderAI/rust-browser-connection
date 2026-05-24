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
