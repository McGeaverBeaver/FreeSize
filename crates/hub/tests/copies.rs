//! Connecting a Nextcloud account through Login Flow v2, listing its files
//! over WebDAV, and the Copies page: devices' reports joined with the
//! server's listing, with the "only one copy" report. The Nextcloud server
//! is a stand-in here that answers like Nextcloud 35.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use common::idp::MockUser;
use common::{Browser, blocking, start_hub};
use freesize_agent::{Client, UreqTransport, pair};
use freesize_core::snapshot::FolderSize;
use freesize_proto::sync::{SyncAccount, SyncDir, SyncFile, SyncFolder, SyncReport};
use freesize_proto::unraid::{ContainerMount, NextcloudReport, ShareUse, UnraidReport};
use freesize_proto::{
    ActionKind, DeviceInfo, FileEntry, PairingLink, Platform, Report, ScanReport,
};
use serde_json::{Value, json};

const MB: u64 = 1024 * 1024;
const APP_PASSWORD: &str = "app-secret-0123456789";

#[derive(Clone)]
struct MockNextcloud {
    base: String,
    /// The person has signed in on the flow's page.
    signed_in: Arc<AtomicBool>,
    revoked: Arc<AtomicBool>,
    /// Folders made with MKCOL, as asked.
    made: Arc<Mutex<Vec<String>>>,
}

impl MockNextcloud {
    async fn start() -> MockNextcloud {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let nextcloud = MockNextcloud {
            base: format!("http://{}", listener.local_addr().unwrap()),
            signed_in: Arc::default(),
            revoked: Arc::default(),
            made: Arc::default(),
        };
        let app = Router::new()
            .route("/index.php/login/v2", post(login_start))
            .route("/login/v2/poll", post(login_poll))
            .fallback(dav)
            .with_state(nextcloud.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        nextcloud
    }
}

async fn login_start(State(nc): State<MockNextcloud>, headers: HeaderMap) -> Response {
    let agent = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(agent.starts_with("FreeSize Hub/"), "user agent {agent}");
    axum::Json(json!({
        "poll": {"token": "poll-token-xyz", "endpoint": format!("{}/login/v2/poll", nc.base)},
        "login": format!("{}/login/v2/flow/abc", nc.base),
    }))
    .into_response()
}

async fn login_poll(State(nc): State<MockNextcloud>, body: String) -> Response {
    assert_eq!(body, "token=poll-token-xyz");
    if !nc.signed_in.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    axum::Json(json!({
        "server": format!("{}/", nc.base),
        "loginName": "bob",
        "appPassword": APP_PASSWORD,
    }))
    .into_response()
}

fn entry(href: &str, fileid: u32, size: u64, dir: bool, checksum: &str) -> String {
    let kind = if dir {
        "<d:resourcetype><d:collection/></d:resourcetype>"
    } else {
        "<d:resourcetype/>"
    };
    let length = if dir {
        String::new()
    } else {
        format!("<d:getcontentlength>{size}</d:getcontentlength>")
    };
    let sums = if checksum.is_empty() {
        "<oc:checksums/>".to_string()
    } else {
        format!("<oc:checksums><oc:checksum>{checksum}</oc:checksum></oc:checksums>")
    };
    format!(
        "<d:response><d:href>{href}</d:href><d:propstat><d:prop>{kind}{length}\
         <d:getlastmodified>Mon, 12 Oct 2026 10:00:00 GMT</d:getlastmodified>\
         <oc:fileid>{fileid}</oc:fileid><oc:size>{size}</oc:size>{sums}\
         <d:quota-used-bytes>{size}</d:quota-used-bytes></d:prop>\
         <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
    )
}

/// The account's files: Photos (with 2019 and a big trip video), Work
/// (small), Archive (nobody keeps it), and a big ISO in the root.
async fn dav(State(nc): State<MockNextcloud>, request: Request) -> Response {
    let method = request.method().as_str().to_string();
    let path = request.uri().path().to_string();
    let headers = request.headers().clone();
    let expected = format!(
        "Basic {}",
        base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            format!("bob:{APP_PASSWORD}")
        )
    );
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if method == "DELETE" && path == "/ocs/v2.php/core/apppassword" {
        assert_eq!(
            headers.get("ocs-apirequest").and_then(|v| v.to_str().ok()),
            Some("true")
        );
        nc.revoked.store(true, Ordering::SeqCst);
        return StatusCode::OK.into_response();
    }
    if method == "MKCOL" {
        let mut made = nc.made.lock().unwrap();
        if made.contains(&path) {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        made.push(path);
        return StatusCode::CREATED.into_response();
    }
    if method != "PROPFIND" {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    assert_eq!(
        headers.get("depth").and_then(|v| v.to_str().ok()),
        Some("1")
    );
    let body = axum::body::to_bytes(request.into_body(), 1 << 20)
        .await
        .unwrap_or_else(|_| Bytes::new());
    assert!(String::from_utf8_lossy(&body).contains("<oc:fileid/>"));
    let files = "/remote.php/dav/files/bob";
    let entries: Vec<String> = match path.trim_end_matches('/') {
        p if p == files => vec![
            entry(&format!("{files}/"), 1, 900 * MB, true, ""),
            entry(&format!("{files}/Photos/"), 2, 500 * MB, true, ""),
            entry(&format!("{files}/Work/"), 3, 10 * MB, true, ""),
            entry(&format!("{files}/Archive/"), 4, 190 * MB, true, ""),
            entry(
                &format!("{files}/big.iso"),
                5,
                200 * MB,
                false,
                "SHA1:aaa MD5:bbb",
            ),
        ],
        p if p == format!("{files}/Photos") => vec![
            entry(&format!("{files}/Photos/"), 2, 500 * MB, true, ""),
            entry(&format!("{files}/Photos/2019/"), 6, 350 * MB, true, ""),
            entry(
                &format!("{files}/Photos/trip%20home.mp4"),
                7,
                150 * MB,
                false,
                "SHA256:ccc",
            ),
        ],
        p if p == format!("{files}/Photos/2019") => vec![
            entry(&format!("{files}/Photos/2019/"), 6, 350 * MB, true, ""),
            entry(
                &format!("{files}/Photos/2019/a.jpg"),
                8,
                350 * MB,
                false,
                "",
            ),
        ],
        p if p == format!("{files}/Work") => vec![
            entry(&format!("{files}/Work/"), 3, 10 * MB, true, ""),
            entry(&format!("{files}/Work/doc.txt"), 9, 10 * MB, false, ""),
        ],
        p if p == format!("{files}/Archive") => vec![
            entry(&format!("{files}/Archive/"), 4, 190 * MB, true, ""),
            entry(&format!("{files}/Archive/old.tar"), 10, 190 * MB, false, ""),
        ],
        "/remote.php/dav/trashbin/bob/trash" => vec![
            entry("/remote.php/dav/trashbin/bob/trash/", 11, 0, true, ""),
            entry(
                "/remote.php/dav/trashbin/bob/trash/gone.mp4.d1700000000",
                12,
                50 * MB,
                false,
                "",
            ),
        ],
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let xml = format!(
        "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\" xmlns:s=\"http://sabredav.org/ns\" \
         xmlns:oc=\"http://owncloud.org/ns\" xmlns:nc=\"http://nextcloud.org/ns\">{}</d:multistatus>",
        entries.join("")
    );
    (
        StatusCode::MULTI_STATUS,
        [("content-type", "application/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

fn info(name: &str, platform: Platform) -> DeviceInfo {
    DeviceInfo {
        name: name.into(),
        platform,
        os: "Test OS".into(),
        hostname: name.to_lowercase(),
        app: "FreeSize app test".into(),
        actions: match platform {
            Platform::Windows => vec![
                ActionKind::Delete,
                ActionKind::MoveIn,
                ActionKind::OnlineOnly,
            ],
            Platform::Linux => vec![ActionKind::Delete, ActionKind::Mount],
            _ => vec![ActionKind::Delete],
        },
        attended: true,
        approver: None,
        grants: vec![],
        rights: "user".into(),
    }
}

fn dir(path: &str, size: u64, on_disk: u64, files: u64, on_disk_files: u64) -> SyncDir {
    SyncDir {
        path: path.into(),
        size,
        on_disk,
        files,
        on_disk_files,
    }
}

fn file(path: &str, size: u64, on_disk: bool) -> SyncFile {
    SyncFile {
        path: path.into(),
        size,
        modified: 1_791_799_200,
        on_disk,
    }
}

fn sync(server: &str, folders: Vec<SyncFolder>) -> SyncReport {
    SyncReport {
        taken: 1_791_800_000,
        accounts: vec![SyncAccount {
            server: server.into(),
            user: "Bob".into(),
            client: "desktop".into(),
            folders,
        }],
    }
}

fn pair_device(url: &str, admin: &mut Browser, info: DeviceInfo) -> freesize_agent::AgentState {
    let code = admin.post(
        &format!("{url}/api/pairing-codes"),
        &json!({"group_id": 1}),
        true,
    );
    assert_eq!(code.status, 200, "{}", code.body);
    let link = PairingLink::parse(code.json()["link"].as_str().unwrap()).unwrap();
    pair(&UreqTransport::new(), &link, info, &mut |_| true).unwrap()
}

/// Polls `check` until it answers; `check` returns what it last saw on
/// the right, for the failure message.
fn wait_for(what: &str, mut check: impl FnMut() -> Result<Value, Value>) -> Value {
    let started = Instant::now();
    loop {
        let seen = match check() {
            Ok(value) => return value,
            Err(seen) => seen,
        };
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "waited too long for {what}; last seen {seen}"
        );
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_list_and_count_copies() {
    let hub = start_hub("copies").await;
    let nextcloud = MockNextcloud::start().await;
    let url = hub.url.clone();
    let idp = hub.idp.clone();
    let nc = nextcloud.clone();
    blocking(move || {
        let mut admin = Browser::new();
        admin.sign_in(&url, "%2F");
        let transport = UreqTransport::new();

        // The desktop syncs everything and keeps it all, plus a big file
        // Nextcloud doesn't have yet; the laptop syncs Photos online-only
        // except 2019.
        let desktop = pair_device(&url, &mut admin, info("Desktop", Platform::Windows));
        Client::new(&desktop, &transport)
            .report(&Report {
                sync: Some(sync(
                    &format!("{}/", nc.base.to_uppercase()),
                    vec![SyncFolder {
                        local: "C:\\Users\\bob\\Nextcloud".into(),
                        remote: "/".into(),
                        size: 1200 * MB,
                        on_disk: 1200 * MB,
                        files: 6,
                        on_disk_files: 6,
                        virtual_files: "wincfapi".into(),
                        dirs: vec![
                            dir("Photos", 500 * MB, 500 * MB, 2, 2),
                            dir("Photos/2019", 350 * MB, 350 * MB, 1, 1),
                            dir("Work", 310 * MB, 310 * MB, 2, 2),
                            dir("Archive", 190 * MB, 0, 1, 0),
                        ],
                        big: vec![
                            file("big.iso", 200 * MB, true),
                            file("Photos/trip home.mp4", 150 * MB, true),
                            file("Work/local.iso", 300 * MB, true),
                            file("Archive/old.tar", 190 * MB, false),
                        ],
                    }],
                )),
                ..Report::default()
            })
            .unwrap();
        let laptop = pair_device(&url, &mut admin, info("Laptop", Platform::Linux));
        Client::new(&laptop, &transport)
            .report(&Report {
                sync: Some(sync(
                    &nc.base,
                    vec![SyncFolder {
                        local: "/home/bob/Photos".into(),
                        remote: "/Photos".into(),
                        size: 500 * MB,
                        on_disk: 350 * MB,
                        files: 2,
                        on_disk_files: 1,
                        virtual_files: "suffix".into(),
                        dirs: vec![dir("2019", 350 * MB, 350 * MB, 1, 1)],
                        big: vec![file("trip home.mp4", 150 * MB, false)],
                    }],
                )),
                ..Report::default()
            })
            .unwrap();

        // Before the Hub is connected: the devices' side only.
        let copies = admin.get(&format!("{url}/api/copies")).json();
        assert_eq!(copies["reported"], 2);
        assert_eq!(copies["accounts"].as_array().unwrap().len(), 0);
        assert_eq!(
            copies["unmatched"][0]["devices"],
            json!(["Desktop", "Laptop"])
        );
        assert_eq!(copies["risks"].as_array().unwrap().len(), 0);
        let detail = admin
            .get(&format!("{url}/api/devices/{}", desktop.device))
            .json();
        assert_eq!(
            detail["sync"]["accounts"][0]["folders"][0]["on_disk"],
            1200 * MB
        );

        // A viewer can look but not connect.
        idp.set_user(MockUser::new("u-viewer", "Viewer", &["freesize-viewers"]));
        let mut viewer = Browser::new();
        viewer.sign_in(&url, "%2F");
        let refused = viewer.post(
            &format!("{url}/api/nextcloud/connect"),
            &json!({"server": nc.base}),
            true,
        );
        assert_eq!(refused.status, 403);
        assert_eq!(viewer.get(&format!("{url}/api/copies")).status, 200);

        // Connecting: the Hub starts the flow, the person signs in, the Hub
        // picks up the app password and lists the files.
        let bad = admin.post(
            &format!("{url}/api/nextcloud/connect"),
            &json!({"server": ""}),
            true,
        );
        assert_eq!(bad.status, 400);
        let started = admin.post(
            &format!("{url}/api/nextcloud/connect"),
            &json!({"server": nc.base}),
            true,
        );
        assert_eq!(started.status, 200, "{}", started.body);
        let started = started.json();
        assert_eq!(
            started["login_url"],
            format!("{}/login/v2/flow/abc", nc.base)
        );
        let flow = started["flow"].as_str().unwrap().to_string();
        let waiting = admin
            .get(&format!("{url}/api/nextcloud/connect/{flow}"))
            .json();
        assert_eq!(waiting["state"], "waiting");
        nc.signed_in.store(true, Ordering::SeqCst);
        let done = wait_for("the sign-in", || {
            let answer = admin
                .get(&format!("{url}/api/nextcloud/connect/{flow}"))
                .json();
            if answer["state"] == "done" {
                Ok(answer)
            } else {
                Err(answer)
            }
        });
        assert_eq!(done["user"], "bob");
        let account = wait_for("the listing", || {
            let accounts = admin.get(&format!("{url}/api/nextcloud")).json();
            let account = accounts[0].clone();
            if account["synced"].is_null() {
                Err(account)
            } else {
                Ok(account)
            }
        });
        assert_eq!(account["server"], nc.base);
        assert_eq!(account["user"], "bob");
        assert!(account["error"].is_null(), "{account}");
        assert_eq!(account["bytes"], 900 * MB);
        assert_eq!(account["trash"], 50 * MB);
        assert_eq!(account["quota_used"], 900 * MB);
        assert_eq!(account["files"], 5);
        assert_eq!(account["folders"], 4);
        assert!(account.get("password").is_none());
        let id = account["id"].as_i64().unwrap();

        // The join.
        let copies = admin.get(&format!("{url}/api/copies")).json();
        assert_eq!(
            copies["accounts"][0]["devices"],
            json!(["Desktop", "Laptop"])
        );
        assert_eq!(copies["unmatched"].as_array().unwrap().len(), 0);
        let folder = |path: &str| {
            copies["folders"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["path"] == path)
                .unwrap_or_else(|| panic!("no folder {path} in {}", copies["folders"]))
                .clone()
        };
        let photos = folder("/Photos");
        assert_eq!(photos["on_server"], 500 * MB);
        assert_eq!(photos["kept_on"], 2);
        assert_eq!(photos["stored"], 850 * MB);
        assert_eq!(photos["devices"][0]["name"], "Desktop");
        assert_eq!(photos["devices"][0]["complete"], true);
        assert_eq!(photos["devices"][1]["local"], "/home/bob/Photos");
        assert_eq!(photos["devices"][1]["complete"], false);
        let y2019 = folder("/Photos/2019");
        assert_eq!(y2019["kept_on"], 2);
        assert_eq!(y2019["devices"][1]["local"], "/home/bob/Photos/2019");
        let archive = folder("/Archive");
        assert_eq!(archive["kept_on"], 0);
        assert_eq!(archive["on_server"], 190 * MB);
        let work = folder("/Work");
        assert_eq!(work["on_server"], 10 * MB);
        assert_eq!(work["size"], 10 * MB);

        let big = |path: &str| {
            copies["files"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["path"] == path)
                .unwrap_or_else(|| panic!("no file {path} in {}", copies["files"]))
                .clone()
        };
        let trip = big("/Photos/trip home.mp4");
        assert_eq!(trip["fileid"], 7);
        assert_eq!(trip["checksums"], "SHA256:ccc");
        assert_eq!(trip["on_server"], true);
        assert_eq!(trip["kept_on"], 1);
        assert_eq!(trip["devices"][1]["on_disk"], false);
        let local = big("/Work/local.iso");
        assert_eq!(local["on_server"], false);
        assert!(local["fileid"].is_null());
        assert_eq!(local["kept_on"], 1);
        // Files only the server lists are there too.
        let old = big("/Archive/old.tar");
        assert_eq!(old["fileid"], 10);
        assert_eq!(old["kept_on"], 0);

        let risks: Vec<(String, String)> = copies["risks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["kind"].as_str().unwrap().to_string(),
                    r["path"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            risks,
            [
                ("only_on_device".to_string(), "/Work/local.iso".to_string()),
                ("only_on_server".to_string(), "/Archive".to_string()),
            ],
            "{}",
            copies["risks"]
        );
        assert_eq!(copies["risks"][0]["device_name"], "Desktop");
        assert!(
            copies["risks"][0]["detail"]
                .as_str()
                .unwrap()
                .contains("C:\\Users\\bob\\Nextcloud\\Work\\local.iso")
        );
        assert!(
            copies["risks"][1]["detail"]
                .as_str()
                .unwrap()
                .contains("Desktop syncs it, but keeps it online-only")
        );

        // Suggestions (0.13): a big folder outside the desktop's sync
        // folder, camera files a phone still holds after they uploaded,
        // and a share an Unraid Nextcloud container mounts.
        Client::new(&desktop, &transport)
            .report(&Report {
                scan: Some(ScanReport {
                    root: "C:\\".into(),
                    label: "C:".into(),
                    taken: 1_791_800_100,
                    size: 60_000 * MB,
                    allocated: 60_000 * MB,
                    files: 3,
                    dirs: 3,
                    errors: 0,
                    complete: true,
                    unreadable: vec![],
                    folders: vec![
                        FolderSize {
                            path: String::new(),
                            size: 60_000 * MB,
                            allocated: 60_000 * MB,
                        },
                        FolderSize {
                            path: "Windows".into(),
                            size: 30_000 * MB,
                            allocated: 30_000 * MB,
                        },
                        FolderSize {
                            path: "GoPro".into(),
                            size: 5_000 * MB,
                            allocated: 5_000 * MB,
                        },
                        FolderSize {
                            path: "Users".into(),
                            size: 25_000 * MB,
                            allocated: 25_000 * MB,
                        },
                    ],
                    largest: vec![],
                    types: vec![],
                }),
                ..Report::default()
            })
            .unwrap();
        let phone = pair_device(&url, &mut admin, info("Phone", Platform::Android));
        Client::new(&phone, &transport)
            .report(&Report {
                sync: Some(SyncReport {
                    taken: 1_791_800_000,
                    accounts: vec![SyncAccount {
                        server: nc.base.clone(),
                        user: "bob".into(),
                        client: "android".into(),
                        folders: vec![],
                    }],
                }),
                scan: Some(ScanReport {
                    root: "/storage/emulated/0".into(),
                    label: "Internal storage".into(),
                    taken: 1_791_800_100,
                    size: 400 * MB,
                    allocated: 400 * MB,
                    files: 3,
                    dirs: 2,
                    errors: 0,
                    complete: true,
                    unreadable: vec![],
                    folders: vec![FolderSize {
                        path: "DCIM".into(),
                        size: 400 * MB,
                        allocated: 400 * MB,
                    }],
                    largest: vec![
                        FileEntry {
                            path: "DCIM/Camera/trip home.mp4".into(),
                            size: 150 * MB,
                            allocated: 150 * MB,
                            modified: 1_791_799_200,
                        },
                        FileEntry {
                            path: "DCIM/Camera/VID_other.mp4".into(),
                            size: 120 * MB,
                            allocated: 120 * MB,
                            modified: 1_791_799_200,
                        },
                        FileEntry {
                            path: "Download/a.jpg".into(),
                            size: 350 * MB,
                            allocated: 350 * MB,
                            modified: 1_791_799_200,
                        },
                    ],
                    types: vec![],
                }),
                ..Report::default()
            })
            .unwrap();
        let tower = pair_device(&url, &mut admin, info("Tower", Platform::Linux));
        Client::new(&tower, &transport)
            .report(&Report {
                unraid: Some(UnraidReport {
                    started: true,
                    shares: vec![
                        ShareUse {
                            name: "Media".into(),
                            total: 9_000 * MB,
                            ..ShareUse::default()
                        },
                        ShareUse {
                            name: "Photos".into(),
                            total: 2_000 * MB,
                            ..ShareUse::default()
                        },
                    ],
                    nextcloud: vec![NextcloudReport {
                        data: "/mnt/user/appdata/nextcloud/data".into(),
                        container: "nextcloud".into(),
                        mounts: vec![
                            ContainerMount {
                                host: "/mnt/user/Media".into(),
                                inside: "/media".into(),
                            },
                            ContainerMount {
                                host: "/mnt/user/Photos".into(),
                                inside: "/photos".into(),
                            },
                        ],
                        external: vec!["/Photos".into()],
                        ..NextcloudReport::default()
                    }],
                    ..UnraidReport::default()
                }),
                ..Report::default()
            })
            .unwrap();
        let copies = admin.get(&format!("{url}/api/copies")).json();
        let suggestion = |kind: &str| {
            copies["suggestions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["kind"] == kind)
                .unwrap_or_else(|| panic!("no {kind} suggestion in {}", copies["suggestions"]))
                .clone()
        };
        assert_eq!(
            copies["suggestions"].as_array().unwrap().len(),
            4,
            "{}",
            copies["suggestions"]
        );
        // The laptop keeps Photos, which the server has whole and the
        // desktop keeps too; Linux frees it in the client, not from here.
        let free = suggestion("online_only");
        assert_eq!(free["device_name"], "Laptop");
        assert_eq!(free["bytes"], 350 * MB);
        assert!(free["action"].is_null());
        assert!(free["pointer"].as_str().unwrap().contains("Free up local space"));
        let move_in = suggestion("move_in");
        assert_eq!(move_in["device_name"], "Desktop");
        assert_eq!(move_in["bytes"], 5_000 * MB);
        assert_eq!(move_in["action"]["path"], "GoPro");
        assert_eq!(move_in["action"]["target"], "Archive/Desktop/GoPro");
        let camera = suggestion("camera_uploads");
        assert_eq!(camera["device_name"], "Phone");
        assert_eq!(camera["bytes"], 150 * MB);
        assert_eq!(
            camera["action"]["paths"],
            json!(["DCIM/Camera/trip home.mp4"])
        );
        let external = suggestion("external_storage");
        assert_eq!(external["device_name"], "Tower");
        assert_eq!(external["action"]["share"], "Media");
        assert_eq!(external["action"]["mount"], "/Media");
        assert_eq!(external["action"]["inside"], "/media");

        // The requests the suggestions make: each checked against what the
        // device reported. The move makes its folder on Nextcloud first.
        let mut commands = |action: &Value| {
            admin.post(
                &format!(
                    "{url}/api/devices/{}/commands",
                    action["device"].as_str().unwrap()
                ),
                &json!({"action": action["action"]}),
                true,
            )
        };
        let sent = commands(&json!({"device": desktop.device, "action": move_in["action"]}));
        assert_eq!(sent.status, 200, "{}", sent.body);
        wait_for("the folders on Nextcloud", || {
            let made = nc.made.lock().unwrap().clone();
            if made.len() >= 3 {
                Ok(json!(made))
            } else {
                Err(json!(made))
            }
        });
        assert_eq!(
            nc.made.lock().unwrap().clone(),
            [
                "/remote.php/dav/files/bob/Archive",
                "/remote.php/dav/files/bob/Archive/Desktop",
                "/remote.php/dav/files/bob/Archive/Desktop/GoPro",
            ]
        );
        let sent = commands(&json!({"device": phone.device, "action": camera["action"]}));
        assert_eq!(sent.status, 200, "{}", sent.body);
        let sent = commands(&json!({"device": tower.device, "action": external["action"]}));
        assert_eq!(sent.status, 200, "{}", sent.body);
        let refused = commands(&json!({"device": tower.device, "action": {
            "type": "external_storage", "share": "Secrets", "container": "nextcloud", "mount": "/Secrets", "inside": "/x"}}));
        assert_eq!(refused.status, 400, "{}", refused.body);
        let sent = commands(&json!({"device": desktop.device, "action": {
            "type": "online_only", "root": "C:\\Users\\bob\\Nextcloud", "paths": ["Photos", "Archive/old.tar"], "bytes": 0}}));
        assert_eq!(sent.status, 200, "{}", sent.body);
        let refused = commands(&json!({"device": desktop.device, "action": {
            "type": "online_only", "root": "C:\\Users\\bob\\Nextcloud", "paths": ["../Secrets"], "bytes": 0}}));
        assert_eq!(refused.status, 400, "{}", refused.body);
        let refused = commands(&json!({"device": laptop.device, "action": {
            "type": "online_only", "root": "/home/bob/Photos", "paths": [], "bytes": 0}}));
        assert_eq!(refused.status, 400, "{}", refused.body);
        let listed = admin.get(&format!("{url}/api/commands")).json();
        let summaries: Vec<String> = listed
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["summary"].as_str().unwrap().to_string())
            .collect();
        assert!(
            summaries
                .iter()
                .any(|s| s.starts_with("Make online-only 2 items in C:\\Users\\bob\\Nextcloud (500 MB)")),
            "{summaries:?}"
        );
        assert!(
            summaries.iter().any(|s| s.contains("GoPro") && s.contains("Archive/Desktop/GoPro")),
            "{summaries:?}"
        );
        assert!(
            summaries.iter().any(|s| s.contains("Media")),
            "{summaries:?}"
        );

        // Listing again on request, and the audit trail.
        let again = admin.post(&format!("{url}/api/nextcloud/{id}/sync"), &json!({}), true);
        assert_eq!(again.status, 200);
        let audit = admin.get(&format!("{url}/api/audit")).json();
        assert!(
            audit
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["action"] == "Nextcloud connected" && e["details"]["user"] == "bob"),
            "{audit}"
        );

        // Disconnecting forgets the listing and revokes the app password.
        assert_eq!(
            viewer.delete(&format!("{url}/api/nextcloud/{id}")).status,
            403
        );
        let removed = wait_for("the second listing", || {
            let accounts = admin.get(&format!("{url}/api/nextcloud")).json();
            if accounts[0]["syncing"] == false {
                Ok(accounts)
            } else {
                Err(accounts)
            }
        });
        assert_eq!(removed[0]["id"], id);
        let gone = admin.delete(&format!("{url}/api/nextcloud/{id}"));
        assert_eq!(gone.status, 200, "{}", gone.body);
        assert_eq!(gone.json()["revoked"], true);
        assert!(nc.revoked.load(Ordering::SeqCst));
        assert_eq!(admin.get(&format!("{url}/api/nextcloud")).json(), json!([]));
        let copies = admin.get(&format!("{url}/api/copies")).json();
        assert_eq!(copies["unmatched"].as_array().unwrap().len(), 1);
    })
    .await;
}
