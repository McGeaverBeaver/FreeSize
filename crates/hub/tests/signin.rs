//! Signing in through the (mock) identity provider, sessions, roles and the
//! CSRF check.

mod common;

use common::idp::MockUser;
use common::{Browser, blocking, start_hub};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn admins_sign_in_and_out() {
    let hub = start_hub("signin").await;
    let url = hub.url.clone();
    blocking(move || {
        let mut browser = Browser::new();
        assert_eq!(browser.get(&format!("{url}/api/me")).status, 401);
        let status = browser.get(&format!("{url}/api/status")).json();
        assert_eq!(status["sign_in"], true);

        let back = browser.sign_in(&url, "%2F%23%2Faudit");
        assert_eq!(back.status, 303);
        assert_eq!(back.location.as_deref(), Some("/#/audit"));
        let me = browser.get(&format!("{url}/api/me")).json();
        assert_eq!(me["name"], "Robert");
        assert_eq!(me["admin"], true);
        assert_eq!(me["hub"]["fingerprint"].as_str().unwrap().len(), 19);

        let log = browser.get(&format!("{url}/api/audit")).json();
        assert_eq!(log[0]["action"], "signed in");
        assert_eq!(log[0]["details"]["role"], "admin");

        // Changes need the CSRF token.
        assert_eq!(
            browser
                .post(&format!("{url}/auth/logout"), &json!({}), false)
                .status,
            403
        );
        let out = browser.post(&format!("{url}/auth/logout"), &json!({}), true);
        assert_eq!(out.status, 200);
        assert!(
            out.json()["redirect"]
                .as_str()
                .unwrap()
                .ends_with("/end-session/")
        );
        assert_eq!(browser.cookie, None);
        assert_eq!(browser.get(&format!("{url}/api/me")).status, 401);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn roles_come_from_groups() {
    let hub = start_hub("roles").await;
    let url = hub.url.clone();
    let idp = hub.idp.clone();

    idp.set_user(MockUser::new(
        "u-viewer",
        "Vera",
        &["disktree-viewers", "family"],
    ));
    let viewer_url = url.clone();
    blocking(move || {
        let mut browser = Browser::new();
        assert_eq!(browser.sign_in(&viewer_url, "%2F").status, 303);
        let me = browser.get(&format!("{viewer_url}/api/me")).json();
        assert_eq!(me["admin"], false);
        // Viewers can't read the audit log.
        assert_eq!(browser.get(&format!("{viewer_url}/api/audit")).status, 403);
    })
    .await;

    idp.set_user(MockUser::new("u-guest", "Gus", &["family"]));
    let state = hub.state.clone();
    blocking(move || {
        let mut browser = Browser::new();
        let refused = browser.sign_in(&url, "%2F");
        assert_eq!(refused.status, 403);
        assert!(refused.body.contains("No access to DiskTree"));
        assert_eq!(browser.cookie, None);
        let check = state.db.now(|c| disktree_hub::audit::verify(c).unwrap());
        assert_eq!(check.broken_at, None);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_links_work_once() {
    let hub = start_hub("once").await;
    let url = hub.url.clone();
    blocking(move || {
        let mut browser = Browser::new();
        let start = browser.get(&format!("{url}/auth/login?next=//evil.example"));
        let at_provider = browser.get(start.location.as_deref().unwrap());
        let callback = at_provider.location.unwrap();
        let back = browser.get(&callback);
        assert_eq!(back.location.as_deref(), Some("/"), "no open redirect");
        // The same answer again is refused.
        let mut other = Browser::new();
        assert_eq!(other.get(&callback).status, 403);
        // A made-up state too.
        assert_eq!(
            other
                .get(&format!("{url}/auth/callback?code=x&state=y"))
                .status,
            403
        );
    })
    .await;
}
