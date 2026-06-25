use docker_git_browser_connection::{
    compute_browser_ports, render_cdp_url_for_ports, render_novnc_url_for_ports,
};
use std::fs;
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
fn rbc_help_exposes_browser_actions_without_mcp() {
    let output = Command::new(env!("CARGO_BIN_EXE_rbc"))
        .args(["--help"])
        .output()
        .expect("Failed to execute rbc");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("snapshot"));
    assert!(stdout.contains("navigate"));
    assert!(stdout.contains("--share-url"));
    assert!(stdout.contains("--cdp-url"));
    assert!(!stdout.contains("--project"));
}

#[test]
fn rbc_eval_requires_expression_or_file_before_network() {
    let output = Command::new(env!("CARGO_BIN_EXE_rbc"))
        .args(["--cdp-url", "http://127.0.0.1:1", "dg-test", "eval"])
        .output()
        .expect("Failed to execute rbc");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("eval requires JS, --expression, or --file"));
}

#[test]
fn rbc_supports_tools_namespace_for_project_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_rbc"))
        .args(["dg-test", "tools", "--help"])
        .output()
        .expect("Failed to execute rbc");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("snapshot"));
    assert!(stdout.contains("activate-tab"));
}

#[test]
fn rbc_pw_requires_script_or_code_before_network() {
    let output = Command::new(env!("CARGO_BIN_EXE_rbc"))
        .args(["--cdp-url", "http://127.0.0.1:1", "dg-test", "pw"])
        .output()
        .expect("Failed to execute rbc");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("pw requires SCRIPT or --code"));
}

#[test]
fn rbc_pw_rejects_share_url_without_cdp() {
    let output = Command::new(env!("CARGO_BIN_EXE_rbc"))
        .args([
            "--share-url",
            "https://relay.example/share/browser#agent=token",
            "dg-test",
            "pw",
            "--code",
            "return 1;",
        ])
        .output()
        .expect("Failed to execute rbc");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("real Playwright requires a CDP endpoint"));
}

#[test]
fn rbc_pw_runs_code_with_fake_playwright_core() {
    let temp = tempfile::tempdir().expect("tempdir is created");
    let module_dir = temp.path().join("node_modules/playwright-core");
    fs::create_dir_all(&module_dir).expect("fake module dir is created");
    fs::write(
        module_dir.join("index.js"),
        r#"
exports.chromium = {
  connectOverCDP: async (url) => ({
    contexts: () => [{
      pages: () => [{
        title: async () => "Fake Title",
        url: () => url
      }],
      newPage: async () => ({
        title: async () => "New Page",
        url: () => url
      }),
      close: async () => {}
    }],
    newContext: async () => ({
      pages: () => [],
      newPage: async () => ({
        title: async () => "New Context Page",
        url: () => url
      }),
      close: async () => {}
    }),
    close: async () => {}
  })
};
"#,
    )
    .expect("fake playwright-core is written");

    let output = Command::new(env!("CARGO_BIN_EXE_rbc"))
        .env("NODE_PATH", temp.path().join("node_modules"))
        .args([
            "--cdp-url",
            "http://fake-cdp:9222",
            "--json",
            "dg-test",
            "pw",
            "--code",
            "return { title: await page.title(), url: page.url() };",
        ])
        .output()
        .expect("run rbc pw");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(value["tool"], "browser_playwright");
    assert_eq!(value["result"]["title"], "Fake Title");
    assert_eq!(value["result"]["url"], "http://fake-cdp:9222");
}
