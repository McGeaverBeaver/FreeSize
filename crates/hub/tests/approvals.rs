//! Requests from the dashboard, approved on the device or by the designated
//! approver's phone, carried out and reported.

mod common;

use common::{Browser, blocking, start_hub};
use disktree_agent::commands::{self, Choice, FsExecutor, Policy, Verdict};
use disktree_agent::{AgentState, Approver, Client, UreqTransport, pair};
use disktree_core::{ScanOptions, ScanProgress, scan};
use disktree_proto::{
    ActionKind, ApprovedBy, AppsReport, CommandState, CommandUpdate, Decision, DeviceInfo,
    JunkEntry, PairingLink, Platform, Purpose, Report,
};
use serde_json::{Value, json};

fn info(
    name: &str,
    attended: bool,
    actions: Vec<ActionKind>,
    approver: Option<String>,
) -> DeviceInfo {
    DeviceInfo {
        name: name.into(),
        platform: if attended {
            Platform::Android
        } else {
            Platform::Linux
        },
        attended,
        actions,
        approver,
        ..DeviceInfo::default()
    }
}

fn paired(admin: &mut Browser, url: &str, info: DeviceInfo) -> AgentState {
    let code = admin
        .post(
            &format!("{url}/api/pairing-codes"),
            &json!({"group_id": 1}),
            true,
        )
        .json();
    let link = PairingLink::parse(code["link"].as_str().unwrap()).unwrap();
    pair(&UreqTransport::new(), &link, info, &mut |_| false).unwrap()
}

fn requests(admin: &mut Browser, url: &str) -> Vec<Value> {
    admin
        .get(&format!("{url}/api/commands"))
        .json()
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_approver_decides_for_a_server() {
    let hub = start_hub("approver").await;
    let url = hub.url.clone();
    let state = hub.state.clone();
    blocking(move || {
        let transport = UreqTransport::new();
        let mut admin = Browser::new();
        admin.sign_in(&url, "%2F");
        let phone = paired(&mut admin, &url, info("Phone", true, vec![ActionKind::Delete], None));
        let mut server = paired(&mut admin, &url, info("Tower", false, vec![ActionKind::Delete], None));

        // On the server: choose the phone as approver by its fingerprint.
        let found = Client::new(&server, &transport).device_key(&phone.fingerprint()).unwrap();
        assert_eq!(found.name, "Phone");
        server.approver = Some(Approver { device: found.id, name: found.name, key: found.key });

        let dir = std::env::temp_dir().join(format!("disktree-hub-approve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("share/isos")).unwrap();
        std::fs::write(dir.join("share/isos/old.iso"), vec![1u8; 70_000]).unwrap();
        std::fs::write(dir.join("share/isos/older.iso"), vec![1u8; 50_000]).unwrap();
        let root = dir.join("share");
        let tree = scan(&root, &ScanOptions::default(), &ScanProgress::new()).unwrap().tree;
        Client::new(&server, &transport)
            .report(&Report {
                info: Some(info("Tower", false, vec![ActionKind::Delete], Some(phone.fingerprint()))),
                scan: Some(disktree_agent::report::scan_report(&tree, "share", 1_790_000_000, 0, true)),
                ..Report::default()
            })
            .unwrap();
        let policy = Policy {
            attended: false,
            actions: vec![ActionKind::Delete],
            roots: vec![root.clone()],
            keep: vec![],
            guards: Default::default(),
        };

        let devices = admin.get(&format!("{url}/api/devices")).json();
        let server_id = devices.as_array().unwrap().iter().find(|d| d["name"] == "Tower").unwrap()["id"]
            .as_str().unwrap().to_string();
        let ask = |admin: &mut Browser, path: &str| {
            admin.post(
                &format!("{url}/api/devices/{server_id}/commands"),
                &json!({"action": {"type": "delete", "root": root.to_string_lossy(), "paths": [path], "bytes": 0},
                        "reason": "old installers"}),
                true,
            )
        };
        let made = ask(&mut admin, "isos/old.iso");
        assert_eq!(made.status, 200, "{}", made.body);

        // The server gets it and asks its approver.
        let poll = Client::new(&server, &transport).poll(1).unwrap();
        assert_eq!(poll.commands.len(), 1);
        let Verdict::AskApprover(request) = commands::consider(&server, &poll.commands[0], &policy, disktree_agent::now()) else {
            panic!("the server should ask its approver");
        };
        assert!(request.command.action.summary().contains("old.iso"));
        Client::new(&server, &transport)
            .update(&request.command.id, &CommandUpdate {
                state: CommandState::NeedsApprover,
                detail: String::new(),
                approval: None,
                freed: 0,
                failed: vec![],
            })
            .unwrap();

        // A device that isn't the approver can't decide.
        let decision = Decision {
            command: request.command.id.clone(),
            command_hash: request.signed.hash(),
            device: server.device.clone(),
            allow: true,
            minutes: 10,
            time: disktree_agent::now(),
        };
        let stranger = paired(&mut admin, &url, info("Laptop", true, vec![], None));
        let refused = Client::new(&stranger, &transport).decide(&decision).unwrap_err();
        assert_eq!(refused.status, 403);

        // The phone sees the request, checks the Hub signed it, and allows
        // it for 10 minutes.
        let at_phone = Client::new(&phone, &transport).poll(1).unwrap();
        assert_eq!(at_phone.approvals.len(), 1);
        let shown = at_phone.approvals[0].command.open(&phone.hub_key, Purpose::Command).unwrap();
        assert_eq!(at_phone.approvals[0].device_name, "Tower");
        assert!(commands::describe(&shown, Some("Tower"))[0].contains("permanently delete 1 item"));
        Client::new(&phone, &transport).decide(&decision).unwrap();

        // The server checks the decision is the phone's and carries it out.
        let poll = Client::new(&server, &transport).poll(1).unwrap();
        assert_eq!(poll.decisions.len(), 1);
        let choice = commands::check_decision(&server, &request, &poll.decisions[0]).unwrap();
        assert_eq!(choice, Choice::For(10));
        let approval = commands::decide(
            &mut server,
            &request,
            choice,
            ApprovedBy::Approver { fingerprint: phone.fingerprint() },
            disktree_agent::now(),
        )
        .unwrap();
        let update = commands::run(&request, &FsExecutor, approval);
        assert_eq!(update.state, CommandState::Done);
        server.mark_handled(&request.command.id);
        Client::new(&server, &transport).update(&request.command.id, &update).unwrap();
        assert!(!dir.join("share/isos/old.iso").exists());

        let listed = requests(&mut admin, &url);
        let done = listed.iter().find(|r| r["id"] == request.command.id.as_str()).unwrap();
        assert_eq!(done["state"], "done");
        assert!(done["freed"].as_i64().unwrap() >= 70_000);
        assert_eq!(done["approval"]["minutes"], 10);

        // The grant covers the next delete by the same admin.
        assert_eq!(ask(&mut admin, "isos/older.iso").status, 200);
        let poll = Client::new(&server, &transport).poll(1).unwrap();
        let Verdict::Run(next, approval) = commands::consider(&server, &poll.commands[0], &policy, disktree_agent::now()) else {
            panic!("the grant should cover it");
        };
        let update = commands::run(&next, &FsExecutor, approval);
        Client::new(&server, &transport).update(&next.command.id, &update).unwrap();
        assert!(!dir.join("share/isos/older.iso").exists());

        // Every step is in the audit log, and the chain holds.
        let log = admin.get(&format!("{url}/api/audit")).json();
        let actions: Vec<&str> = log.as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect();
        for expected in ["request made", "waiting for the approver", "approver allowed", "request done"] {
            assert!(actions.contains(&expected), "{expected} missing from {actions:?}");
        }
        let check = state.db.now(|c| disktree_hub::audit::verify(c).unwrap());
        assert_eq!(check.broken_at, None);
        let _ = std::fs::remove_dir_all(&dir);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn devices_deny_and_admins_withdraw() {
    let hub = start_hub("deny").await;
    let url = hub.url.clone();
    blocking(move || {
        let transport = UreqTransport::new();
        let mut admin = Browser::new();
        admin.sign_in(&url, "%2F");
        let mut phone = paired(
            &mut admin,
            &url,
            info(
                "Phone",
                true,
                vec![ActionKind::Clean, ActionKind::Uninstall],
                None,
            ),
        );
        let lonely = paired(
            &mut admin,
            &url,
            info("Headless", false, vec![ActionKind::Delete], None),
        );
        Client::new(&phone, &transport)
            .report(&Report {
                apps: Some(AppsReport {
                    taken: 1,
                    junk: vec![JunkEntry {
                        kind: "trash".into(),
                        label: "Trash".into(),
                        bytes: 1 << 30,
                        items: 4,
                    }],
                    ..AppsReport::default()
                }),
                ..Report::default()
            })
            .unwrap();
        let devices = admin.get(&format!("{url}/api/devices")).json();
        let id_of = |name: &str| {
            devices
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["name"] == name)
                .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let (phone_id, lonely_id) = (id_of("Phone"), id_of("Headless"));
        let clean =
            json!({"action": {"type": "clean_junk", "kind": "trash", "label": "", "bytes": 0}});

        // Checked against what the devices reported.
        let unknown = admin.post(
            &format!("{url}/api/devices/{phone_id}/commands"),
            &json!({"action": {"type": "uninstall", "package": "com.nope", "label": ""}}),
            true,
        );
        assert_eq!(unknown.status, 400);
        let no_approver = admin.post(
            &format!("{url}/api/devices/{lonely_id}/commands"),
            &json!({"action": {"type": "delete", "root": "/srv", "paths": ["x"], "bytes": 0}}),
            true,
        );
        assert_eq!(no_approver.status, 409);
        assert!(no_approver.body.contains("approver"));
        drop(lonely);

        // Asked on the phone, denied there.
        assert_eq!(
            admin
                .post(
                    &format!("{url}/api/devices/{phone_id}/commands"),
                    &clean,
                    true
                )
                .status,
            200
        );
        let poll = Client::new(&phone, &transport).poll(1).unwrap();
        let policy = Policy {
            attended: true,
            actions: vec![ActionKind::Clean],
            ..Policy::default()
        };
        let Verdict::Ask(request) =
            commands::consider(&phone, &poll.commands[0], &policy, disktree_agent::now())
        else {
            panic!("should ask");
        };
        assert_eq!(request.command.action.summary(), "Trash (1.00 GB)");
        assert!(
            commands::decide(&mut phone, &request, Choice::Deny, ApprovedBy::Device, 0).is_none()
        );
        Client::new(&phone, &transport)
            .update(
                &request.command.id,
                &CommandUpdate {
                    state: CommandState::Denied,
                    detail: "denied on the device".into(),
                    approval: None,
                    freed: 0,
                    failed: vec![],
                },
            )
            .unwrap();
        assert_eq!(requests(&mut admin, &url)[0]["state"], "denied");

        // Withdrawn before delivery: never reaches the phone.
        let made = admin
            .post(
                &format!("{url}/api/devices/{phone_id}/commands"),
                &clean,
                true,
            )
            .json();
        let id = made["id"].as_str().unwrap();
        assert_eq!(
            admin
                .post(&format!("{url}/api/commands/{id}/cancel"), &json!({}), true)
                .status,
            200
        );
        let poll = Client::new(&phone, &transport).poll(0).unwrap();
        assert!(poll.commands.is_empty() && poll.cancelled.is_empty());
        // Withdrawn after delivery: the phone is told.
        let made = admin
            .post(
                &format!("{url}/api/devices/{phone_id}/commands"),
                &clean,
                true,
            )
            .json();
        let id = made["id"].as_str().unwrap().to_string();
        assert_eq!(
            Client::new(&phone, &transport)
                .poll(1)
                .unwrap()
                .commands
                .len(),
            1
        );
        assert_eq!(
            admin
                .post(&format!("{url}/api/commands/{id}/cancel"), &json!({}), true)
                .status,
            200
        );
        assert_eq!(
            Client::new(&phone, &transport).poll(1).unwrap().cancelled,
            std::slice::from_ref(&id)
        );
        // Finished requests can't be withdrawn or changed.
        assert_eq!(
            admin
                .post(&format!("{url}/api/commands/{id}/cancel"), &json!({}), true)
                .status,
            409
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn polls_wake_up_for_new_requests() {
    let hub = start_hub("wake").await;
    let url = hub.url.clone();
    blocking(move || {
        let transport = UreqTransport::new();
        let mut admin = Browser::new();
        admin.sign_in(&url, "%2F");
        let phone = paired(
            &mut admin,
            &url,
            info("Phone", true, vec![ActionKind::Clean], None),
        );
        Client::new(&phone, &transport)
            .report(&Report {
                apps: Some(AppsReport {
                    taken: 1,
                    junk: vec![JunkEntry {
                        kind: "trash".into(),
                        label: "Trash".into(),
                        bytes: 5,
                        items: 1,
                    }],
                    ..AppsReport::default()
                }),
                ..Report::default()
            })
            .unwrap();
        let phone_id = admin.get(&format!("{url}/api/devices")).json()[0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let started = std::time::Instant::now();
        let waiter = std::thread::spawn(move || {
            Client::new(&phone, &UreqTransport::new()).poll(30).unwrap()
        });
        std::thread::sleep(std::time::Duration::from_millis(500));
        admin.post(
            &format!("{url}/api/devices/{phone_id}/commands"),
            &json!({"action": {"type": "clean_junk", "kind": "trash", "label": "", "bytes": 0}}),
            true,
        );
        let poll = waiter.join().unwrap();
        assert_eq!(poll.commands.len(), 1);
        assert!(
            started.elapsed().as_secs() < 10,
            "the poll should wake up right away"
        );
    })
    .await;
}
