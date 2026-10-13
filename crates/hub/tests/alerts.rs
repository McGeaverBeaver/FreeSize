//! Alerts going out to ntfy and a webhook, the dashboard's alert settings,
//! and the metrics endpoints.

mod common;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::http::HeaderMap;
use axum::routing::post;
use common::idp::MockUser;
use common::{Browser, blocking, start_hub_with};
use freesize_agent::{Client, UreqTransport, pair};
use freesize_proto::{ActionKind, DeviceInfo, Disk, PairingLink, Platform, Report};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// What each receiver got: who, the headers, the body as JSON and as sent.
type Inbox = Arc<Mutex<Vec<(String, HeaderMap, Value, Bytes)>>>;

/// A stand-in for ntfy and a webhook receiver: keeps what it's sent.
async fn receiver() -> (String, Inbox) {
    let inbox: Inbox = Arc::default();
    let keep = |inbox: Inbox, name: &'static str| {
        move |headers: HeaderMap, body: Bytes| async move {
            let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            inbox
                .lock()
                .unwrap()
                .push((name.to_string(), headers, value, body));
            "ok"
        }
    };
    let app = Router::new()
        .route("/", post(keep(inbox.clone(), "ntfy")))
        .route("/hook", post(keep(inbox.clone(), "hook")));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, inbox)
}

fn hmac(key: &[u8], message: &[u8]) -> String {
    let mut block = [0u8; 64];
    block[..key.len()].copy_from_slice(key);
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(message);
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn report(free_gb: u64) -> Report {
    Report {
        disks: Some(vec![Disk {
            name: "disk1".into(),
            path: "/mnt/disk1".into(),
            total: 1000 << 30,
            free: free_gb << 30,
        }]),
        ..Report::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn alerts_go_out_and_metrics_come_in() {
    let hub = start_hub_with("alerts", &["--metrics-token", "scrape-me"]).await;
    let (sink, inbox) = receiver().await;
    let url = hub.url.clone();
    let setup_url = url.clone();
    let setup_sink = sink.clone();
    let (agent, nas) = blocking(move || {
        let url = setup_url;
        let mut admin = Browser::new();
        admin.sign_in(&url, "%2F");
        let code = admin
            .post(
                &format!("{url}/api/pairing-codes"),
                &json!({"group_id": 1}),
                true,
            )
            .json();
        let link = PairingLink::parse(code["link"].as_str().unwrap()).unwrap();
        let info = DeviceInfo {
            name: "NAS".into(),
            platform: Platform::Linux,
            os: "Debian 13".into(),
            hostname: "nas".into(),
            app: "test".into(),
            actions: vec![ActionKind::Delete],
            attended: false,
            approver: None,
            grants: vec![],
        };
        let agent = pair(&UreqTransport::new(), &link, info, &mut |_| false).unwrap();
        Client::new(&agent, &UreqTransport::new())
            .report(&report(50))
            .unwrap();
        let nas = admin.get(&format!("{url}/api/devices")).json()[0]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // Where alerts go; secrets don't come back.
        for (name, target) in [
            (
                "Phone",
                json!({"kind": "ntfy", "server": setup_sink, "topic": "disks", "token": "tk_123"}),
            ),
            (
                "Home Assistant",
                json!({"kind": "webhook", "url": format!("{setup_sink}/hook"), "secret": "s3cret"}),
            ),
        ] {
            let added = admin.post(
                &format!("{url}/api/alerts/channels"),
                &json!({"name": name, "target": target}),
                true,
            );
            assert_eq!(added.status, 200, "{}", added.body);
        }
        let listed = admin.get(&format!("{url}/api/alerts/channels"));
        assert!(!listed.body.contains("tk_123") && !listed.body.contains("s3cret"));
        assert_eq!(listed.json()[0]["has_secret"], true);
        let bad = admin.post(
            &format!("{url}/api/alerts/channels"),
            &json!({"name": "x", "target": {"kind": "ntfy", "server": "ftp://x", "topic": "t"}}),
            true,
        );
        assert_eq!(bad.status, 400);

        // A test message goes out at once.
        let id = listed.json()[0]["id"].as_i64().unwrap();
        let tested = admin.post(
            &format!("{url}/api/alerts/channels/{id}/test"),
            &json!({}),
            true,
        );
        assert_eq!(tested.status, 200, "{}", tested.body);

        // Rules are checked; changes are logged.
        let rules = admin.send(
            "PUT",
            &format!("{url}/api/alerts/rules"),
            &json!({"disk_full_percent": 20}),
        );
        assert_eq!(rules.status, 400);
        let rules = admin.send(
            "PUT",
            &format!("{url}/api/alerts/rules"),
            &json!({"disk_full_percent": 90, "growth_week_percent": 5, "forecast_days": 30,
                    "offline_hours": 24, "requests": true}),
        );
        assert_eq!(rules.status, 200, "{}", rules.body);
        let log = admin.get(&format!("{url}/api/audit")).json();
        let actions: Vec<&str> = log
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["action"].as_str().unwrap())
            .collect();
        assert!(actions.contains(&"alert rules changed"));
        assert!(actions.contains(&"alert channel added"));
        (agent, nas)
    })
    .await;
    {
        let got = inbox.lock().unwrap();
        assert_eq!(got.len(), 1);
        let (to, headers, body, _) = &got[0];
        assert_eq!(to, "ntfy");
        assert_eq!(body["topic"], "disks");
        assert_eq!(body["title"], "FreeSize Hub test");
        assert_eq!(headers["authorization"], "Bearer tk_123");
    }
    inbox.lock().unwrap().clear();

    // The disk is 95% full: both channels hear of it, once.
    let notices = freesize_hub::alerts::check_now(&hub.state).await.unwrap();
    assert_eq!(notices.len(), 1);
    assert!(
        freesize_hub::alerts::check_now(&hub.state)
            .await
            .unwrap()
            .is_empty()
    );
    {
        let got = inbox.lock().unwrap();
        assert_eq!(got.len(), 2, "{got:?}");
        let ntfy = got.iter().find(|(to, _, _, _)| to == "ntfy").unwrap();
        assert_eq!(ntfy.2["title"], "NAS: disk1 is 95% full");
        assert_eq!(ntfy.2["priority"], 4);
        assert_eq!(ntfy.2["click"], format!("{url}/#/device/{nas}").as_str());
        let hook = got.iter().find(|(to, _, _, _)| to == "hook").unwrap();
        assert_eq!(hook.2["event"], "opened");
        assert_eq!(hook.2["kind"], "disk_full");
        assert_eq!(hook.2["device"]["name"], "NAS");
        // The signature covers the body as sent.
        let signature = hook.1["x-disktree-signature"].to_str().unwrap();
        assert_eq!(signature, format!("sha256={}", hmac(b"s3cret", &hook.3)));
    }
    inbox.lock().unwrap().clear();

    let page_url = url.clone();
    let page_nas = nas.clone();
    blocking(move || {
        let url = page_url;
        let mut admin = Browser::new();
        admin.sign_in(&url, "%2F");
        let alerts = admin.get(&format!("{url}/api/alerts")).json();
        assert_eq!(alerts["active"][0]["kind"], "disk_full");
        assert_eq!(alerts["active"][0]["device_name"], "NAS");
        assert_eq!(admin.get(&format!("{url}/api/devices")).json()[0]["alerts"], 1);

        // Metrics need the token.
        let mut scraper = Browser::new();
        assert_eq!(scraper.get(&format!("{url}/metrics")).status, 401);
        assert_eq!(
            scraper.get_bearer(&format!("{url}/metrics"), "wrong").status,
            401
        );
        let metrics = scraper.get_bearer(&format!("{url}/metrics"), "scrape-me");
        assert_eq!(metrics.status, 200);
        assert!(metrics.header("content-type").starts_with("text/plain"));
        assert!(
            metrics.body.contains(&format!(
                "disktree_disk_free_bytes{{device=\"{page_nas}\",name=\"NAS\",disk=\"disk1\",path=\"/mnt/disk1\"}} {}",
                50u64 << 30
            )),
            "{}",
            metrics.body
        );
        assert!(metrics.body.contains(
            "disktree_alerts_active{kind=\"disk_full\",severity=\"warning\"} 1"
        ));
        let summary = scraper
            .get_bearer(&format!("{url}/metrics/json"), "scrape-me")
            .json();
        assert_eq!(summary["alerts"], 1);
        assert_eq!(summary["devices"][0]["disks"][0]["used_percent"], 95.0);
    })
    .await;

    // Space freed: the alert is over, and that's sent too.
    blocking(move || {
        Client::new(&agent, &UreqTransport::new())
            .report(&report(400))
            .unwrap()
    })
    .await;
    let notices = freesize_hub::alerts::check_now(&hub.state).await.unwrap();
    assert!(notices[0].resolved);
    {
        let got = inbox.lock().unwrap();
        let ntfy = got.iter().find(|(to, _, _, _)| to == "ntfy").unwrap();
        assert_eq!(ntfy.2["title"], "Over: NAS: disk1 is 95% full");
    }

    // Viewers see alerts, not where they go.
    hub.idp
        .set_user(MockUser::new("u-viewer", "Val", &["freesize-viewers"]));
    blocking(move || {
        let mut viewer = Browser::new();
        viewer.sign_in(&url, "%2F");
        assert_eq!(viewer.get(&format!("{url}/api/alerts")).status, 200);
        assert_eq!(viewer.get(&format!("{url}/api/alerts/rules")).status, 200);
        assert_eq!(
            viewer.get(&format!("{url}/api/alerts/channels")).status,
            403
        );
        assert_eq!(
            viewer
                .send("PUT", &format!("{url}/api/alerts/rules"), &json!({}))
                .status,
            403
        );
    })
    .await;
    drop(sink);
}
