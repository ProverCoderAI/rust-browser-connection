//! Property-based tests for the single-browser invariant
//!
//! INVARIANT (theorem): ∀ project_id, repeated calls to start_browser yield exactly one container.

use docker_git_browser_connection::BrowserConnection;
use proptest::prelude::*;

proptest! {
    #[test]
    fn single_browser_invariant(project_id in "[a-z0-9-]{1,20}") {
        // Note: this is illustrative; real test would require Docker test harness
        // In CI we use #[ignore] or testcontainers
        let conn = BrowserConnection::new().unwrap();
        let url1 = conn.get_cdp_url(&project_id);
        let url2 = conn.get_cdp_url(&project_id);
        prop_assert!(conn.is_single_browser_session(&url1, &url2));
    }
}
