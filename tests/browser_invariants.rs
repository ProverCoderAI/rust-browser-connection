//! Property-based tests for the single-browser invariant
//!
//! INVARIANT (theorem): ∀ project_id, status URL rendering is pure and Docker-free.

use docker_git_browser_connection::{
    browser_spec_from_defaults, compute_browser_ports, is_single_browser_session,
    normalize_project_container_name, render_cdp_url_for_ports, render_novnc_url_for_ports,
};
use proptest::prelude::*;

proptest! {
    #[test]
    fn single_browser_invariant(project_id in "[a-z0-9-]{1,20}") {
        let spec = browser_spec_from_defaults(&project_id, None);
        let normalized = normalize_project_container_name(&project_id);
        prop_assert_eq!(spec.main_container_name, normalized.clone());
        prop_assert_eq!(spec.container_name, format!("{}-browser", normalized));
        prop_assert!(is_single_browser_session(
            &render_cdp_url_for_ports(spec.ports),
            &render_novnc_url_for_ports(spec.ports)
        ));
    }

    #[test]
    fn normalized_project_container_name_is_idempotent(project_id in "[a-z0-9-]{1,20}") {
        let once = normalize_project_container_name(&project_id);
        let twice = normalize_project_container_name(&once);
        prop_assert_eq!(once, twice.clone());
        prop_assert!(twice.starts_with("dg-"));
    }
}

#[test]
fn project_specific_ports_render_distinct_localhost_urls() {
    let alpha = compute_browser_ports("dg-alpha");
    let beta = compute_browser_ports("dg-beta");
    assert_ne!(alpha, beta);
    assert!(render_novnc_url_for_ports(alpha).contains(&format!(":{}", alpha.novnc)));
    assert_eq!(
        render_cdp_url_for_ports(alpha),
        format!("http://127.0.0.1:{}", alpha.cdp)
    );
}
