//! The FreeSize Nextcloud app's reports (0.20): stored, turned into
//! suggestions on the Copies page, deletes checked against them, and each
//! person seeing only their own Nextcloud account.

mod common;

use common::idp::MockUser;
use common::{Browser, blocking, start_hub};
use freesize_agent::{Client, UreqTransport, pair};
use freesize_proto::cloud::{CloudFile, CloudFolder, CloudReport, CloudUser, DuplicateSet};
use freesize_proto::{
    ActionKind, CommandState, CommandUpdate, Decision, DeviceInfo, PairingLink, Platform, Purpose,
    Report,
};
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

/// Nextcloud runs the app every five minutes at best, so its approver is
/// asked as soon as the request is made, may answer before the app looks,
/// and is told when a request it was shown ends without its answer.
#[tokio::test(flavor = "multi_thread")]
async fn the_approver_is_asked_straight_away() {
    let hub = start_hub("cloud-approver").await;
    let url = hub.url.clone();
    blocking(move || {
        let transport = UreqTransport::new();
        let mut admin = Browser::new();
        admin.sign_in(&url, "%2F");
        let pair_as = |admin: &mut Browser, info: DeviceInfo| {
            let code = admin.post(
                &format!("{url}/api/pairing-codes"),
                &json!({"group_id": 1}),
                true,
            );
            let link = PairingLink::parse(code.json()["link"].as_str().unwrap()).unwrap();
            pair(&transport, &link, info, &mut |_| true).unwrap()
        };
        let phone = pair_as(
            &mut admin,
            DeviceInfo {
                name: "Pixel".into(),
                platform: Platform::Android,
                attended: true,
                actions: vec![ActionKind::Delete],
                ..DeviceInfo::default()
            },
        );
        let info = DeviceInfo {
            name: "Nextcloud".into(),
            platform: Platform::Nextcloud,
            os: "Nextcloud 35.0.2.2".into(),
            hostname: "cloud".into(),
            app: "FreeSize Nextcloud app 0.20.5".into(),
            actions: vec![ActionKind::Delete],
            attended: false,
            approver: Some(phone.fingerprint()),
            grants: vec![],
            rights: String::new(),
        };
        let cloud = pair_as(&mut admin, info.clone());
        Client::new(&cloud, &transport)
            .report(&Report {
                info: Some(info),
                cloud: Some(CloudReport {
                    taken: 1_791_800_000,
                    server: "https://cloud.example.lan".into(),
                    version: "35.0.2.2".into(),
                    trash_bin: true,
                    users: vec![account("alice", "Alice")],
                }),
                ..Report::default()
            })
            .unwrap();
        let ask = |admin: &mut Browser, path: &str| {
            let made = admin.post(
                &format!("{url}/api/devices/{}/commands", cloud.device),
                &json!({"action": {"type": "delete", "root": "alice", "paths": [path], "bytes": 0}}),
                true,
            );
            assert_eq!(made.status, 200, "{}", made.body);
            made.json()["id"].as_str().unwrap().to_string()
        };

        // The phone sees it before the app has looked.
        let first = ask(&mut admin, "Backup/a.jpg");
        let at_phone = Client::new(&phone, &transport).poll(1).unwrap();
        assert_eq!(at_phone.approvals.len(), 1);
        assert!(at_phone.approvals[0].note.contains("Alice's trash bin"));
        let shown = &at_phone.approvals[0].command;
        let command = shown.open(&phone.hub_key, Purpose::Command).unwrap();
        assert_eq!(command.id, first);
        Client::new(&phone, &transport)
            .decide(&Decision {
                command: first.clone(),
                command_hash: shown.hash(),
                device: cloud.device.clone(),
                allow: true,
                minutes: 0,
                time: freesize_agent::now(),
            })
            .unwrap();

        // The app gets the request and the answer together, asks as it
        // always does, and the answer comes again.
        let at_cloud = Client::new(&cloud, &transport).poll(0).unwrap();
        assert_eq!(at_cloud.commands.len(), 1);
        assert_eq!(at_cloud.decisions.len(), 1);
        let update = |state: CommandState, detail: &str| CommandUpdate {
            state,
            detail: detail.into(),
            approval: None,
            freed: 0,
            failed: vec![],
        };
        Client::new(&cloud, &transport)
            .update(&first, &update(CommandState::NeedsApprover, "Moves the files."))
            .unwrap();
        let again = Client::new(&cloud, &transport).poll(0).unwrap();
        assert_eq!(again.decisions.len(), 1);
        Client::new(&cloud, &transport)
            .update(&first, &update(CommandState::Done, "1 moved to the trash bin"))
            .unwrap();
        // Shown once, and answered: nothing more for the phone.
        let at_phone = Client::new(&phone, &transport).poll(0).unwrap();
        assert!(at_phone.approvals.is_empty());
        assert!(at_phone.cancelled.is_empty());

        // A request withdrawn on the dashboard comes down on the phone too.
        let second = ask(&mut admin, "Photos/a.jpg");
        let at_phone = Client::new(&phone, &transport).poll(1).unwrap();
        assert_eq!(at_phone.approvals.len(), 1);
        let withdrawn = admin.post(&format!("{url}/api/commands/{second}/cancel"), &json!({}), true);
        assert_eq!(withdrawn.status, 200, "{}", withdrawn.body);
        let at_phone = Client::new(&phone, &transport).poll(0).unwrap();
        assert_eq!(at_phone.cancelled, [second]);
        let at_phone = Client::new(&phone, &transport).poll(0).unwrap();
        assert!(at_phone.cancelled.is_empty());
    })
    .await;
}
