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
        browser_resource_limit_args(&BrowserResourceLimits::from_values(Some("0.5"), Some("1g"))),
        vec![
            "--cpus".to_string(),
            "0.5".to_string(),
            "--memory".to_string(),
            "1g".to_string()
        ]
    );
}

#[test]
fn novnc_proxy_candidates_include_target_specific_localhost_port() {
    let spec = BrowserTargetDisplaySpec {
        project_id: "dg-test".to_string(),
        browser_name: "personal".to_string(),
        main_container_name: "dg-test".to_string(),
        container_name: "dg-test-browser-novnc-personal".to_string(),
        image_name: "dg-test-browser-novnc-proxy:docker-git-browser".to_string(),
        network_mode: "bridge".to_string(),
        vnc_endpoint: "host.docker.internal:5900".to_string(),
        novnc_port: 6611,
    };

    assert!(novnc_proxy_probe_candidates(&spec).contains(
        &"http://127.0.0.1:6611/vnc.html?autoconnect=true&resize=remote&path=websockify"
            .to_string()
    ));
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
