//! Guessing at the Hub from one address is slowed down, without locking
//! out other addresses.

mod common;

use common::{Browser, blocking, start_hub};
use freesize_agent::{UreqTransport, pair};
use freesize_proto::{ActionKind, DeviceInfo, PairingLink, Platform};

fn info() -> DeviceInfo {
    DeviceInfo {
        name: "Guesser".into(),
        platform: Platform::Linux,
        os: "Debian 13".into(),
        hostname: "guesser".into(),
        app: "FreeSize agent test".into(),
        actions: vec![ActionKind::Delete],
        attended: false,
        approver: None,
        grants: vec![],
        rights: "root".into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_codes_and_sign_ins_are_limited_per_address() {
    let hub = start_hub("limits").await;
    let url = hub.url.clone();
    blocking(move || {
        let transport = UreqTransport::new();
        let wrong = PairingLink {
            hub: url.clone(),
            code: "ZZZZ-ZZZZ-ZZZZ".into(),
            key: None,
        };
        for _ in 0..10 {
            let err = pair(&transport, &wrong, info(), &mut |_| true).unwrap_err();
            assert_eq!(err.status, 403, "{err}");
        }
        let err = pair(&transport, &wrong, info(), &mut |_| true).unwrap_err();
        assert_eq!(err.status, 429, "{err}");
        assert!(err.message.contains("wait a minute"), "{err}");

        // Sign-in starts count too: the 31st from this address waits.
        let mut browser = Browser::new();
        for _ in 0..30 {
            let started = browser.get(&format!("{url}/auth/login"));
            assert_eq!(started.status, 303, "{}", started.body);
        }
        let blocked = browser.get(&format!("{url}/auth/login"));
        assert_eq!(blocked.status, 429);
        assert!(blocked.body.contains("Too many sign-in attempts"));
    })
    .await;
}
