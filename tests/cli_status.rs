use std::process::Command;

#[test]
fn status_command_prints_single_browser_urls_without_docker() {
    let output = Command::new(env!("CARGO_BIN_EXE_docker-git-browser-connection"))
        .args(["status", "--project", "docker-git-issue-347"])
        .output()
        .expect("Failed to execute binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("noVNC: http://127.0.0.1:6080/vnc.html"));
    assert!(stdout.contains("CDP: http://127.0.0.1:9223"));
    assert!(stdout.contains("Invariant check: true"));
}
