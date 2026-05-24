use docker_git_browser_connection::{browser_spec_from_env, render_cdp_url, render_novnc_url};

fn main() {
    let spec = browser_spec_from_env("dg-example", None);
    println!("container={}", spec.container_name);
    println!("noVNC={}", render_novnc_url());
    println!("CDP={}", render_cdp_url());
}
