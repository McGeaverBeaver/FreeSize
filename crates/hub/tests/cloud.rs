//! The FreeSize Nextcloud app's reports (0.20): stored, turned into
//! suggestions on the Copies page, deletes checked against them, and each
//! person seeing only their own Nextcloud account.

mod common;

use common::idp::MockUser;
use common::{Browser, blocking, start_hub};
use freesize_agent::{Client, UreqTransport, pair};
use freesize_proto::cloud::{CloudFile, CloudFolder, CloudReport, CloudUser, DuplicateSet};
use freesize_proto::{ActionKind, DeviceInfo, PairingLink, Platform, Report};
use serde_json::{Value, json};

const MB: u64 = 1024 * 1024;

fn file(path: &str, size: u64, modified: i64, fileid: i64) -> CloudFile {
    CloudFile {
        path: path.into(),
        size,
        modified,
        fileid,
    }
}

fn account(id: &str, name: &str) -> CloudUser {
    CloudUser {
        id: id.into(),
        name: name.into(),
        email: format!("{id}@example.lan"),
        quota: -3,
        used: 10 * MB,
        files: 3,
        trash: 200 * MB,
        trash_files: 4,
        versions: 0,
        versions_files: 0,
        folders: vec![CloudFolder {
            path: "Photos".into(),
            size: 6 * MB,
            files: 2,
        }],
        biggest: vec![file("Photos/a.jpg", 3 * MB, 1_500_000_000, 1)],
        old: vec![file("Photos/a.jpg", 3 * MB, 1_500_000_000, 1)],
        old_files: 1,
        old_bytes: 3 * MB,
        duplicates: vec![DuplicateSet {
            size: 3 * MB,
            sha256: "ab".repeat(32),
            files: vec![
                file("Photos/a.jpg", 3 * MB, 1_500_000_000, 1),
                file("Backup/a.jpg", 3 * MB, 1_600_000_000, 2),
            ],
        }],
        hashed: 2,
        to_hash: 2,
    }
}

fn cloud_names(value: &Value) -> Vec<String> {
    value["cloud"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|v| v["users"].as_array().unwrap().clone())
        .map(|u| u["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_person_sees_their_own_account() {
    let hub = start_hub("cloud").await;
    let url = hub.url.clone();
    let idp = hub.idp.clone();
    blocking(move || {
        let mut admin = Browser::new();
        admin.sign_in(&url, "%2F");
        let code = admin.post(
            &format!("{url}/api/pairing-codes"),
            &json!({"group_id": 1}),
            true,
        );
        let link = PairingLink::parse(code.json()["link"].as_str().unwrap()).unwrap();
        let info = DeviceInfo {
            name: "Nextcloud".into(),
            platform: Platform::Nextcloud,
            os: "Nextcloud 34.0.5".into(),
            hostname: "cloud".into(),
            app: "FreeSize Nextcloud app 0.20.0".into(),
            actions: vec![ActionKind::Delete],
            attended: false,
            approver: Some("ABCD-EFGH-JKMN-PQRS".into()),
            grants: vec![],
            rights: String::new(),
        };
        let device = pair(&UreqTransport::new(), &link, info, &mut |_| true).unwrap();
        Client::new(&device, &UreqTransport::new())
            .report(&Report {
                cloud: Some(CloudReport {
                    taken: 1_791_800_000,
                    server: "https://cloud.example.lan".into(),
                    version: "34.0.5".into(),
                    trash_bin: true,
                    users: vec![account("alice", "Alice"), account("carol", "Carol")],
                }),
                ..Report::default()
            })
            .unwrap();

        // The admin sees both accounts and what to do about them.
        let copies = admin.get(&format!("{url}/api/copies")).json();
        assert_eq!(cloud_names(&copies), ["alice", "carol"]);
        let extra: Vec<&Value> = copies["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["kind"] == "cloud_duplicates")
            .collect();
        assert_eq!(extra.len(), 2);
        assert_eq!(extra[0]["bytes"], 3 * MB);
        // The oldest copy stays.
        assert_eq!(extra[0]["action"]["paths"], json!(["Backup/a.jpg"]));
        assert!(
            copies["suggestions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["kind"] == "cloud_trash")
        );

        // Deletes name only what the app listed, for a user it covers.
        let ask = |admin: &mut Browser, root: &str, path: &str| {
            admin.post(
                &format!("{url}/api/devices/{}/commands", device.device),
                &json!({"action": {"type": "delete", "root": root, "paths": [path], "bytes": 0}}),
                true,
            )
        };
        let asked = ask(&mut admin, "alice", "Backup/a.jpg");
        assert_eq!(asked.status, 200, "{}", asked.body);
        let missing = ask(&mut admin, "alice", "Secret/plans.pdf");
        assert_eq!(missing.status, 400, "{}", missing.body);
        let stranger = ask(&mut admin, "dave", "Backup/a.jpg");
        assert_eq!(stranger.status, 400, "{}", stranger.body);
        let carols = ask(&mut admin, "carol", "Photos/a.jpg");
        assert_eq!(carols.status, 200, "{}", carols.body);
        let commands = admin.get(&format!("{url}/api/commands")).json();
        let alices = commands
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["action"]["root"] == "alice")
            .unwrap();
        assert_eq!(alices["action"]["bytes"], 3 * MB);

        // Alice, signed in with her email, sees her account and her
        // requests only.
        idp.set_user(MockUser::new("alice", "Alice", &["freesize-viewers"]));
        let mut alice = Browser::new();
        alice.sign_in(&url, "%2F");
        let copies = alice.get(&format!("{url}/api/copies")).json();
        assert_eq!(cloud_names(&copies), ["alice"]);
        assert!(
            copies["suggestions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| !s["title"].as_str().unwrap().contains("Carol"))
        );
        // Not an admin: nothing to send from here.
        assert_eq!(copies["cloud"][0]["can_delete"], false);
        let detail = alice
            .get(&format!("{url}/api/devices/{}", device.device))
            .json();
        let users: Vec<&str> = detail["cloud"]["users"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u["id"].as_str().unwrap())
            .collect();
        assert_eq!(users, ["alice"]);
        let commands = alice.get(&format!("{url}/api/commands")).json();
        let roots: Vec<&str> = commands
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["action"]["root"].as_str().unwrap())
            .collect();
        assert_eq!(roots, ["alice"]);
        // She can't ask for deletes herself.
        assert_eq!(ask(&mut alice, "alice", "Backup/a.jpg").status, 403);

        // Someone with no account there sees none.
        idp.set_user(MockUser::new("erin", "Erin", &["freesize-viewers"]));
        let mut erin = Browser::new();
        erin.sign_in(&url, "%2F");
        let copies = erin.get(&format!("{url}/api/copies")).json();
        assert!(cloud_names(&copies).is_empty());
        assert!(
            copies["suggestions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| !s["kind"].as_str().unwrap().starts_with("cloud"))
        );
        let detail = erin
            .get(&format!("{url}/api/devices/{}", device.device))
            .json();
        assert!(detail["cloud"].is_null());
        let commands = erin.get(&format!("{url}/api/commands")).json();
        assert!(commands.as_array().unwrap().is_empty());
    })
    .await;
}
