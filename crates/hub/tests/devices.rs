//! Pairing devices, signed device requests, reports, and what the dashboard
//! shows of them.

mod common;

use common::idp::MockUser;
use common::{Browser, blocking, start_hub};
use disktree_agent::{Client, Transport, UreqTransport, pair};
use disktree_core::{ScanOptions, ScanProgress, scan};
use disktree_proto::signing::sign_request;
use disktree_proto::{ActionKind, DeviceInfo, Disk, PairingLink, Platform, Report};
use serde_json::{Value, json};

fn info(name: &str) -> DeviceInfo {
    DeviceInfo {
        name: name.into(),
        platform: Platform::Linux,
        os: "Debian 13".into(),
        hostname: "nas".into(),
        app: "DiskTree agent test".into(),
        actions: vec![ActionKind::Delete],
        attended: false,
        approver: None,
        grants: vec![],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_report_and_look() {
    let hub = start_hub("devices").await;
    let url = hub.url.clone();
    blocking(move || {
        let mut admin = Browser::new();
        admin.sign_in(&url, "%2F");
        let code = admin.post(
            &format!("{url}/api/pairing-codes"),
            &json!({"group_id": 1}),
            true,
        );
        assert_eq!(code.status, 200, "{}", code.body);
        let code = code.json();
        let link = PairingLink::parse(code["link"].as_str().unwrap()).unwrap();
        assert_eq!(link.hub, url);
        assert!(
            code["qr"]
                .as_str()
                .unwrap()
                .starts_with("data:image/svg+xml;base64,")
        );

        let transport = UreqTransport::new();
        // A link naming another key is refused before anything is sent.
        let mut wrong = link.clone();
        wrong.key = Some("AAAA-AAAA-AAAA-AAAA".into());
        let err = pair(&transport, &wrong, info("NAS"), &mut |_| true).unwrap_err();
        assert!(err.message.contains("pretending"), "{err}");

        let state = pair(&transport, &link, info("NAS"), &mut |_| false).unwrap();
        assert_eq!(
            state.hub_key.fingerprint(),
            code["fingerprint"].as_str().unwrap()
        );
        // Codes work once.
        let again = pair(&transport, &link, info("NAS 2"), &mut |_| false).unwrap_err();
        assert_eq!(again.status, 403);

        // A real scan, reported.
        let dir = std::env::temp_dir().join(format!("disktree-hub-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Movies/2024")).unwrap();
        std::fs::write(dir.join("Movies/2024/trip.mp4"), vec![1u8; 400_000]).unwrap();
        std::fs::write(dir.join("Movies/old.mkv"), vec![1u8; 200_000]).unwrap();
        std::fs::write(dir.join("notes.txt"), b"hello").unwrap();
        let tree = scan(&dir, &ScanOptions::default(), &ScanProgress::new())
            .unwrap()
            .tree;
        let scan_report =
            disktree_agent::report::scan_report(&tree, "Shares", 1_790_000_000, 0, true);
        let client = Client::new(&state, &transport);
        client
            .report(&Report {
                info: Some(DeviceInfo {
                    name: "Big NAS".into(),
                    ..info("NAS")
                }),
                disks: Some(vec![Disk {
                    name: "disk1".into(),
                    path: "/mnt/disk1".into(),
                    total: 8 << 40,
                    free: 1 << 40,
                }]),
                scan: Some(scan_report),
                apps: None,
            })
            .unwrap();
        assert!(client.poll(1).unwrap().commands.is_empty());

        let devices = admin.get(&format!("{url}/api/devices")).json();
        assert_eq!(devices.as_array().unwrap().len(), 1);
        let device = &devices[0];
        assert_eq!(device["name"], "Big NAS");
        assert_eq!(device["online"], true);
        assert_eq!(device["disks"][0]["total"], 8u64 << 40);
        assert_eq!(device["scans"][0]["label"], "Shares");
        let id = device["id"].as_str().unwrap().to_string();
        let scan_id = device["scans"][0]["id"].as_i64().unwrap();

        let detail = admin.get(&format!("{url}/api/devices/{id}")).json();
        assert_eq!(detail["roots"][0]["scans"].as_array().unwrap().len(), 1);
        assert_eq!(detail["history"][0]["samples"].as_array().unwrap().len(), 1);

        let top = admin
            .get(&format!("{url}/api/scans/{scan_id}/folders?path="))
            .json();
        assert_eq!(top["folders"][0]["name"], "Movies");
        assert_eq!(top["files"][0]["name"], "notes.txt");
        let movies = admin
            .get(&format!("{url}/api/scans/{scan_id}/folders?path=Movies"))
            .json();
        assert_eq!(movies["folders"][0]["path"], "Movies/2024");
        assert_eq!(movies["files"][0]["name"], "old.mkv");
        let files = admin
            .get(&format!("{url}/api/scans/{scan_id}/files?under=Movies"))
            .json();
        assert_eq!(files["files"][0]["path"], "Movies/2024/trip.mp4");
        assert_eq!(files["files"].as_array().unwrap().len(), 2);

        let log = admin.get(&format!("{url}/api/devices/{id}/audit")).json();
        assert_eq!(log[0]["action"], "paired");

        // Removing the device: its next request is told so.
        assert_eq!(admin.delete(&format!("{url}/api/devices/{id}")).status, 200);
        let removed = client.report(&Report::default()).unwrap_err();
        assert!(removed.removed, "{removed:?}");
        assert!(
            admin
                .get(&format!("{url}/api/devices"))
                .json()
                .as_array()
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&dir);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_requests_cant_be_replayed_or_changed() {
    let hub = start_hub("replay").await;
    let url = hub.url.clone();
    blocking(move || {
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
        let transport = UreqTransport::new();
        let state = pair(&transport, &link, info("PC"), &mut |_| false).unwrap();

        let body = br#"{"wait":0}"#;
        let sig = sign_request(
            &state.key(),
            &state.device,
            "POST",
            "/api/v1/poll",
            body,
            disktree_agent::now(),
        );
        let headers = vec![
            ("x-disktree-device", sig.device.clone()),
            ("x-disktree-time", sig.time.to_string()),
            ("x-disktree-nonce", sig.nonce.clone()),
            ("x-disktree-signature", sig.signature.clone()),
            ("Content-Type", "application/json".to_string()),
        ];
        let send = |body: &[u8]| {
            transport
                .request(
                    "POST",
                    &format!("{url}/api/v1/poll"),
                    &headers,
                    Some(body),
                    10,
                )
                .unwrap()
        };
        assert_eq!(send(body).0, 200);
        let (status, answer) = send(body);
        assert_eq!(status, 401);
        assert!(String::from_utf8_lossy(&answer).contains("already sent"));

        let fresh = sign_request(
            &state.key(),
            &state.device,
            "POST",
            "/api/v1/poll",
            body,
            disktree_agent::now(),
        );
        let changed = vec![
            ("x-disktree-device", fresh.device),
            ("x-disktree-time", fresh.time.to_string()),
            ("x-disktree-nonce", fresh.nonce),
            ("x-disktree-signature", fresh.signature),
            ("Content-Type", "application/json".to_string()),
        ];
        let (status, _) = transport
            .request(
                "POST",
                &format!("{url}/api/v1/poll"),
                &changed,
                Some(br#"{"wait":1}"#),
                10,
            )
            .unwrap();
        assert_eq!(status, 401);

        // Old timestamps are refused too.
        let old = sign_request(
            &state.key(),
            &state.device,
            "POST",
            "/api/v1/poll",
            body,
            disktree_agent::now() - 3600,
        );
        let stale = vec![
            ("x-disktree-device", old.device),
            ("x-disktree-time", old.time.to_string()),
            ("x-disktree-nonce", old.nonce),
            ("x-disktree-signature", old.signature),
        ];
        let (status, _) = transport
            .request(
                "POST",
                &format!("{url}/api/v1/poll"),
                &stale,
                Some(body),
                10,
            )
            .unwrap();
        assert_eq!(status, 401);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn viewers_see_only_their_groups() {
    let hub = start_hub("scoped").await;
    let url = hub.url.clone();
    let idp = hub.idp.clone();
    let setup_url = url.clone();
    blocking(move || {
        let mut admin = Browser::new();
        admin.sign_in(&setup_url, "%2F");
        let family = admin
            .post(
                &format!("{setup_url}/api/groups"),
                &json!({"name": "Family", "grant_minutes": [5, 15], "viewer_groups": ["family"]}),
                true,
            )
            .json();
        let family_id = family["id"].as_i64().unwrap();
        let transport = UreqTransport::new();
        for (name, group) in [("Kid's phone", family_id), ("Server", 1)] {
            let code = admin
                .post(
                    &format!("{setup_url}/api/pairing-codes"),
                    &json!({"group_id": group}),
                    true,
                )
                .json();
            let link = PairingLink::parse(code["link"].as_str().unwrap()).unwrap();
            pair(&transport, &link, info(name), &mut |_| false).unwrap();
        }
        let groups = admin.get(&format!("{setup_url}/api/groups")).json();
        let family = groups
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["name"] == "Family")
            .unwrap()
            .clone();
        assert_eq!(family["grant_minutes"], json!([5, 15]));
        assert_eq!(family["devices"], 1);
        // Groups with devices can't be deleted.
        assert_eq!(
            admin
                .delete(&format!("{setup_url}/api/groups/{family_id}"))
                .status,
            409
        );
    })
    .await;

    idp.set_user(MockUser::new("u-parent", "Pat", &["family"]));
    blocking(move || {
        let mut parent = Browser::new();
        assert_eq!(parent.sign_in(&url, "%2F").status, 303);
        let devices = parent.get(&format!("{url}/api/devices")).json();
        let names: Vec<&str> = devices
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["Kid's phone"]);
        let groups: Vec<Value> = parent
            .get(&format!("{url}/api/groups"))
            .json()
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(groups.len(), 1);
        // Viewers can't make pairing codes or change devices.
        assert_eq!(
            parent
                .post(
                    &format!("{url}/api/pairing-codes"),
                    &json!({"group_id": 1}),
                    true
                )
                .status,
            403
        );
        let id = devices[0]["id"].as_str().unwrap();
        assert_eq!(
            parent.delete(&format!("{url}/api/devices/{id}")).status,
            403
        );
    })
    .await;
}
