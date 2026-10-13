//! HTTPS with the Hub's own authority: devices pair over it and trust it for
//! the Hub's address only; the dashboard is sent from plain http to https;
//! devices paired over plain http are told to move, signed.

mod common;

use common::{Browser, blocking, start_hub_tls};
use freesize_agent::{Client, Trust, UreqTransport, hello, move_to, pair};
use freesize_proto::{ActionKind, DeviceInfo, PairingLink, Platform};
use serde_json::json;

fn info(name: &str) -> DeviceInfo {
    DeviceInfo {
        name: name.into(),
        platform: Platform::Linux,
        os: "Debian 13".into(),
        hostname: "nas".into(),
        app: "FreeSize agent test".into(),
        actions: vec![ActionKind::Delete],
        attended: false,
        approver: None,
        grants: vec![],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn https_with_the_hubs_own_authority() {
    let hub = start_hub_tls("tls").await;
    let other = start_hub_tls("tls-other").await;
    let url = hub.url.clone();
    let other_url = other.url.clone();
    let ca = hub
        .state
        .tls
        .as_ref()
        .unwrap()
        .ca_pem()
        .unwrap()
        .to_string();
    assert!(ca.starts_with("-----BEGIN CERTIFICATE-----"));
    blocking(move || {
        let plain = url.replacen("https://", "http://", 1);
        let transport = UreqTransport::new();

        // Plain http on the same port still answers a device's hello, and
        // names the authority; the dashboard is sent to https there.
        let first = hello(&transport, &plain, &Trust::System).unwrap();
        assert_eq!(first.ca.as_deref(), Some(ca.as_str()));
        let mut plain_browser = Browser::new();
        let sent = plain_browser.get(&format!("{plain}/?x=1"));
        assert_eq!(sent.status, 308);
        assert_eq!(sent.location.as_deref(), Some(&*format!("{url}/?x=1")));
        assert_eq!(
            plain_browser.get(&format!("{plain}/api/status")).status,
            200
        );
        // Without the authority, the certificate is refused.
        assert!(plain_browser.get_result(&url).is_err());

        // With it, the dashboard works and hands the authority out.
        let mut admin = Browser::trusting(&ca);
        admin.sign_in(&url, "%2F");
        let me = admin.get(&format!("{url}/api/me")).json();
        assert_eq!(me["hub"]["tls"]["own"], true);
        assert_eq!(me["hub"]["tls"]["names"][0], "127.0.0.1");
        let authority = admin.get(&format!("{url}/ca.pem"));
        assert_eq!(authority.status, 200);
        assert_eq!(authority.body, ca);
        assert_eq!(authority.header("content-type"), "application/x-pem-file");

        // Pairing: the link is https; a link written before the Hub served
        // HTTPS (plain http) pairs over it too.
        let code = admin.post(
            &format!("{url}/api/pairing-codes"),
            &json!({"group_id": 1}),
            true,
        );
        assert_eq!(code.status, 200, "{}", code.body);
        let link = PairingLink::parse(code.json()["link"].as_str().unwrap()).unwrap();
        assert_eq!(link.hub, url);
        let mut old_link = link.clone();
        old_link.hub = plain.clone();
        let state = pair(&transport, &old_link, info("NAS"), &mut |_| false).unwrap();
        assert_eq!(state.hub, url);
        assert_eq!(state.ca.as_deref(), Some(ca.as_str()));
        let polled = Client::new(&state, &transport).poll(1).unwrap();
        assert!(polled.move_to.is_none());

        // The Hub's certificate is refused against the system's roots...
        let mut untrusted = state.clone();
        untrusted.ca = None;
        let err = Client::new(&untrusted, &transport).poll(1).unwrap_err();
        assert_eq!(err.status, 0, "{err}");
        // ...the authority isn't trusted for any other name...
        let mut elsewhere = state.clone();
        elsewhere.hub = url.replacen("127.0.0.1", "localhost", 1);
        assert!(Client::new(&elsewhere, &transport).poll(1).is_err());
        // ...and another Hub's certificate is refused for this address.
        let mut impostor = state.clone();
        impostor.hub = other_url;
        let err = Client::new(&impostor, &transport).poll(1).unwrap_err();
        assert_eq!(err.status, 0, "{err}");

        // A device paired over plain http is told where the Hub is now, in
        // a signed answer, and moves there.
        let mut older = state.clone();
        older.hub = plain;
        older.ca = None;
        let response = Client::new(&older, &transport).poll(1).unwrap();
        let signed = response.move_to.expect("told to move");
        assert_eq!(move_to(&transport, &mut older, &signed), Ok(true));
        assert_eq!(older.hub, url);
        assert_eq!(older.ca, state.ca);
        assert!(
            Client::new(&older, &transport)
                .poll(1)
                .unwrap()
                .move_to
                .is_none()
        );
    })
    .await;
}
