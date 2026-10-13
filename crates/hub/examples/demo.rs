//! Runs a Hub with a stand-in sign-in provider and a few made-up devices,
//! for trying the dashboard without Authentik or real devices:
//!
//! ```sh
//! cargo run -p freesize-hub --example demo
//! ```
//!
//! Then open http://127.0.0.1:9090 and sign in (the mock provider signs you
//! straight in as an admin). Data goes to a temporary folder.

#[allow(dead_code)]
#[path = "../tests/common/idp.rs"]
mod idp;

use clap::Parser;
use freesize_agent::{Client, UreqTransport};
use freesize_core::snapshot::FolderSize;
use freesize_hub::{AppState, Config};
use freesize_proto::{
    ActionKind, AppEntry, AppsReport, Breakdown, DeviceInfo, Disk, FileEntry, JunkEntry,
    PairingLink, Platform, Report, ScanReport, TypeEntry,
};
use rusqlite::params;

const GB: u64 = 1 << 30;

#[tokio::main]
async fn main() {
    let hub_addr = std::env::var("DEMO_HUB").unwrap_or_else(|_| "127.0.0.1:9090".into());
    let idp_addr = std::env::var("DEMO_IDP").unwrap_or_else(|_| "127.0.0.1:9091".into());
    let data = std::env::temp_dir().join(format!("freesize-hub-demo-{}", std::process::id()));
    let idp_listener = tokio::net::TcpListener::bind(&idp_addr).await.unwrap();
    let idp = idp::MockIdp::serve(
        idp_listener,
        format!("http://{idp_addr}/application/o/freesize/"),
    )
    .await;
    let config = Config::parse_from([
        "freesize-hub",
        "--data",
        data.to_str().unwrap(),
        "--url",
        &format!("http://{hub_addr}"),
        "--name",
        "Home Hub",
        "--tls",
        "off",
        "--oidc-issuer",
        &idp.issuer,
        "--oidc-client-id",
        idp::CLIENT_ID,
        "--oidc-client-secret",
        idp::CLIENT_SECRET,
    ]);
    let state = AppState::open(config).unwrap();
    let listener = tokio::net::TcpListener::bind(&hub_addr).await.unwrap();
    tokio::spawn(freesize_hub::serve(listener, state.clone()));
    let url = format!("http://{hub_addr}");
    let seed_state = state.clone();
    tokio::task::spawn_blocking(move || seed(&url, &seed_state))
        .await
        .unwrap();
    // Alerts for what was just seeded, rather than in a minute.
    freesize_hub::alerts::check_now(&state).await.unwrap();
    println!(
        "FreeSize Hub demo on http://{hub_addr} (data in {})",
        data.display()
    );
    tokio::signal::ctrl_c().await.ok();
    let _ = std::fs::remove_dir_all(&data);
}

fn info(name: &str, platform: Platform, os: &str, attended: bool) -> DeviceInfo {
    DeviceInfo {
        name: name.into(),
        platform,
        os: os.into(),
        hostname: name.to_lowercase().replace(' ', "-"),
        app: if attended {
            format!("FreeSize app {}", env!("CARGO_PKG_VERSION"))
        } else {
            format!("FreeSize agent {}", env!("CARGO_PKG_VERSION"))
        },
        actions: if platform == Platform::Android {
            vec![ActionKind::Delete, ActionKind::Uninstall, ActionKind::Clean]
        } else {
            vec![ActionKind::Delete]
        },
        attended,
        approver: None,
        grants: vec![],
    }
}

fn folders(list: &[(&str, u64)]) -> Vec<FolderSize> {
    list.iter()
        .map(|&(path, gb10)| FolderSize {
            path: path.into(),
            size: gb10 * GB / 10,
            allocated: gb10 * GB / 10,
        })
        .collect()
}

fn files(list: &[(&str, u64)]) -> Vec<FileEntry> {
    list.iter()
        .map(|&(path, mb)| FileEntry {
            path: path.into(),
            size: mb << 20,
            allocated: mb << 20,
            modified: freesize_hub::db::now() - (mb as i64 % 900) * 86_400,
        })
        .collect()
}

fn scan(
    root: &str,
    label: &str,
    days_ago: i64,
    folders: Vec<FolderSize>,
    files: Vec<FileEntry>,
) -> ScanReport {
    let total = folders
        .iter()
        .find(|f| f.path.is_empty())
        .cloned()
        .unwrap_or_default();
    ScanReport {
        root: root.into(),
        label: label.into(),
        taken: freesize_hub::db::now() - days_ago * 86_400 - 3600,
        size: total.size,
        allocated: total.allocated,
        files: 48_213,
        dirs: 6_120,
        errors: 0,
        complete: true,
        folders,
        largest: files,
        types: vec![
            TypeEntry {
                extension: "mkv".into(),
                files: 210,
                size: 900 * GB,
            },
            TypeEntry {
                extension: "jpg".into(),
                files: 31_000,
                size: 120 * GB,
            },
        ],
    }
}

/// Pairs the made-up devices through the real API and reports for them.
fn seed(url: &str, state: &AppState) {
    let transport = UreqTransport::new();
    // Each device: what it says, what it reports, and its disks' (path,
    // total GB, used GB) for two months of history.
    type Demo = (DeviceInfo, Report, Vec<(&'static str, u64, u64)>);
    let devices: Vec<Demo> = vec![
        (
            info("Tower", Platform::Linux, "Unraid 7.2", false),
            Report {
                disks: Some(vec![
                    Disk {
                        name: "Array".into(),
                        path: "/mnt/user".into(),
                        total: 36_000 * GB,
                        free: 7_400 * GB,
                    },
                    Disk {
                        name: "cache".into(),
                        path: "/mnt/cache".into(),
                        total: 1_900 * GB,
                        free: 410 * GB,
                    },
                ]),
                scan: Some(scan(
                    "/mnt/user",
                    "User shares",
                    0,
                    folders(&[
                        ("", 285_000),
                        ("media", 214_000),
                        ("media/movies", 121_000),
                        ("media/tv", 83_000),
                        ("media/music", 10_000),
                        ("backups", 41_000),
                        ("backups/pc", 29_000),
                        ("backups/phones", 12_000),
                        ("appdata", 9_800),
                        ("appdata/plex", 6_100),
                        ("appdata/immich", 2_900),
                        ("isos", 3_200),
                        ("domains", 17_000),
                    ]),
                    files(&[
                        ("domains/win11/vdisk1.img", 102_400),
                        ("backups/pc/2024-full.tar", 88_000),
                        ("media/movies/Dune Part Two (2024)/dune2.mkv", 61_000),
                        ("isos/ubuntu-24.04.iso", 6_100),
                        ("appdata/plex/Library/db.blobs", 3_900),
                    ]),
                )),
                ..Report::default()
            },
            vec![("/mnt/user", 36_000, 28_600), ("/mnt/cache", 1_900, 1_490)],
        ),
        (
            info(
                "Robert's Pixel 9 Pro",
                Platform::Android,
                "Android 16",
                true,
            ),
            Report {
                disks: Some(vec![Disk {
                    name: "Internal storage".into(),
                    path: "/storage/emulated/0".into(),
                    total: 238 * GB,
                    free: 31 * GB,
                }]),
                scan: Some(scan(
                    "/storage/emulated/0",
                    "Internal storage",
                    1,
                    folders(&[
                        ("", 1_240),
                        ("DCIM", 640),
                        ("DCIM/Camera", 610),
                        ("Android", 310),
                        ("Android/media", 290),
                        ("Movies", 160),
                        ("Download", 72),
                        ("Pictures", 58),
                    ]),
                    files(&[
                        ("DCIM/Camera/PXL_20260812_video.mp4", 4_200),
                        ("Movies/Insta360/VID_20260704.insv", 3_800),
                        ("Download/old-phone-backup.zip", 2_900),
                    ]),
                )),
                apps: Some(AppsReport {
                    taken: freesize_hub::db::now() - 3600,
                    breakdown: Some(Breakdown {
                        total: 238 * GB,
                        free: 31 * GB,
                        apps: 71 * GB,
                        files: 124 * GB,
                        system: 12 * GB,
                    }),
                    apps: vec![
                        app("com.whatsapp", "WhatsApp", 9, 3, Some(1)),
                        app("com.google.android.youtube", "YouTube", 6, 2, Some(0)),
                        app("com.spotify.music", "Spotify", 11, 1, Some(2)),
                        app("com.arashivision.insta360akiko", "Insta360", 3, 4, Some(40)),
                        app("com.example.game", "Galaxy Raiders", 14, 0, Some(200)),
                    ],
                    junk: vec![
                        JunkEntry {
                            kind: "trash".into(),
                            label: "Trash".into(),
                            bytes: 2 * GB,
                            items: 41,
                        },
                        JunkEntry {
                            kind: "installers".into(),
                            label: "Old installers".into(),
                            bytes: GB,
                            items: 3,
                        },
                    ],
                }),
                ..Report::default()
            },
            vec![("/storage/emulated/0", 238, 207)],
        ),
        (
            info("Gaming PC", Platform::Windows, "Windows 11", true),
            Report {
                disks: Some(vec![
                    Disk {
                        name: "C:".into(),
                        path: "C:\\".into(),
                        total: 931 * GB,
                        free: 62 * GB,
                    },
                    Disk {
                        name: "D:".into(),
                        path: "D:\\".into(),
                        total: 1_863 * GB,
                        free: 1_100 * GB,
                    },
                ]),
                scan: Some(scan(
                    "C:\\",
                    "C:",
                    2,
                    folders(&[
                        ("", 8_690),
                        ("Program Files (x86)", 4_100),
                        ("Program Files (x86)/Steam", 3_700),
                        ("Users", 2_600),
                        ("Users/Robert", 2_550),
                        ("Windows", 380),
                        ("Windows.old", 290),
                    ]),
                    files(&[
                        ("hiberfil.sys", 25_000),
                        ("Users/Robert/Videos/clip.mp4", 9_000),
                    ]),
                )),
                ..Report::default()
            },
            vec![("C:\\", 931, 869), ("D:\\", 1_863, 763)],
        ),
        (
            info("MacBook Air", Platform::Macos, "macOS 26", true),
            Report {
                disks: Some(vec![Disk {
                    name: "Macintosh HD".into(),
                    path: "/".into(),
                    total: 494 * GB,
                    free: 188 * GB,
                }]),
                ..Report::default()
            },
            vec![("/", 494, 306)],
        ),
    ];

    let group = 1;
    let mut agents = Vec::new();
    for (i, (device, report, history)) in devices.into_iter().enumerate() {
        let code = make_code(state, url, group);
        let link = PairingLink::parse(&code).unwrap();
        let agent =
            freesize_agent::pair(&transport, &link, device.clone(), &mut |_| false).unwrap();
        Client::new(&agent, &transport).report(&report).unwrap();
        // Two months of daily disk samples, growing towards today's.
        let id = agent.device.clone();
        agents.push((device.clone(), agent));
        state.db.now(|c| {
            for (path, total, used_now) in &history {
                for day in 1..60i64 {
                    let used = (*used_now as f64 * (1.0 - day as f64 * 0.0025)) as u64;
                    c.execute(
                        "INSERT OR REPLACE INTO disk_samples (device_id, path, day, total, used)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            id,
                            path,
                            freesize_hub::db::now() / 86_400 - day,
                            (total * GB) as i64,
                            (used * GB) as i64
                        ],
                    )
                    .unwrap();
                }
            }
            // Only the first devices were heard from recently.
            if i >= 2 {
                c.execute(
                    "UPDATE devices SET last_seen = ?1 WHERE id = ?2",
                    params![freesize_hub::db::now() - 3 * 3600 * i as i64, id],
                )
                .unwrap();
            }
        });
    }
    tower_history(&agents);
    agentless(state);
    approvals_and_requests(state, &agents);
}

/// Older scans of the tower's shares, for comparing.
fn tower_history(agents: &[(DeviceInfo, freesize_agent::AgentState)]) {
    let transport = UreqTransport::new();
    let (_, tower) = agents.iter().find(|(i, _)| i.name == "Tower").unwrap();
    let older = [
        (
            45,
            folders(&[
                ("", 251_000),
                ("media", 171_000),
                ("media/movies", 112_000),
                ("media/tv", 50_000),
                ("media/music", 9_000),
                ("backups", 59_000),
                ("backups/pc", 47_000),
                ("backups/phones", 12_000),
                ("appdata", 8_900),
                ("appdata/plex", 5_400),
                ("appdata/immich", 2_700),
                ("isos", 9_100),
                ("isos/old", 5_900),
            ]),
        ),
        (
            14,
            folders(&[
                ("", 279_000),
                ("media", 205_000),
                ("media/movies", 118_000),
                ("media/tv", 77_000),
                ("media/music", 10_000),
                ("backups", 41_000),
                ("backups/pc", 29_000),
                ("backups/phones", 12_000),
                ("appdata", 9_500),
                ("appdata/plex", 5_900),
                ("appdata/immich", 2_800),
                ("isos", 3_200),
                ("domains", 14_000),
            ]),
        ),
    ];
    for (days, list) in older {
        Client::new(tower, &transport)
            .report(&Report {
                scan: Some(scan("/mnt/user", "User shares", days, list, Vec::new())),
                ..Report::default()
            })
            .unwrap();
    }
}

/// A Proxmox host without an agent, with a scan uploaded as an ncdu file.
fn agentless(state: &AppState) {
    let ncdu = br#"[1,2,{"progname":"ncdu","progver":"1.19","timestamp":0},
        [{"name":"/var/lib/vz","asize":4096,"dsize":4096},
         [{"name":"images","asize":4096,"dsize":4096},
          [{"name":"100","asize":4096,"dsize":4096},
           {"name":"vm-100-disk-0.qcow2","asize":68719476736,"dsize":41231686041}],
          [{"name":"101","asize":4096,"dsize":4096},
           {"name":"vm-101-disk-0.raw","asize":34359738368,"dsize":34359738368}]],
         [{"name":"template","asize":4096,"dsize":4096},
          [{"name":"iso","asize":4096,"dsize":4096},
           {"name":"debian-13.1.0-amd64-netinst.iso","asize":786432000,"dsize":786436096},
           {"name":"ubuntu-24.04.3-live-server-amd64.iso","asize":3213064192,"dsize":3213066240}]],
         [{"name":"dump","asize":4096,"dsize":4096},
          {"name":"vzdump-qemu-100-2026_09_01.vma.zst","asize":21474836480,"dsize":21474836480},
          {"name":"vzdump-qemu-100-2026_10_01.vma.zst","asize":22548578304,"dsize":22548578304}]]]"#;
    let tree = freesize_core::ncdu::import(&ncdu[..]).unwrap();
    let report = freesize_agent::report::scan_report(
        &tree,
        "VM storage",
        freesize_hub::db::now() - 2 * 86_400,
        0,
        true,
    );
    let key = freesize_proto::keys::KeyPair::generate().public();
    let mut info = info("Proxmox host", Platform::Linux, "", false);
    info.app = "No agent: scans are uploaded as files".into();
    info.actions = Vec::new();
    state.db.now(|c| {
        let now = freesize_hub::db::now();
        c.execute(
            "INSERT INTO devices (id, name, platform, key, fingerprint, group_id, info, paired,
                                  last_seen, agentless)
             VALUES ('DEMOPROXMOX', 'Proxmox host', 'linux', ?1, ?2, 1, ?3, ?4, ?4, 1)",
            params![
                key.to_base64(),
                key.fingerprint(),
                serde_json::to_string(&info).unwrap(),
                now - 2 * 86_400
            ],
        )
        .unwrap();
        freesize_hub::devices::store_scan(c, "DEMOPROXMOX", report).unwrap();
        // Where alerts would go; off, so the demo sends nothing.
        c.execute(
            "INSERT INTO alert_channels (name, target, enabled, created)
             VALUES ('Robert''s phone', ?1, 0, ?2)",
            params![
                r#"{"kind":"ntfy","server":"https://ntfy.sh","topic":"freesize-demo-7f3k","token":""}"#,
                now
            ],
        )
        .unwrap();
    });
}

/// The tower's approver is the phone; a few requests in different states.
fn approvals_and_requests(state: &AppState, agents: &[(DeviceInfo, freesize_agent::AgentState)]) {
    let transport = UreqTransport::new();
    let find = |name: &str| agents.iter().find(|(i, _)| i.name.contains(name)).unwrap();
    let (phone_info, phone) = find("Pixel");
    let (tower_info, tower) = find("Tower");
    let mut info = tower_info.clone();
    info.approver = Some(phone.fingerprint());
    Client::new(tower, &transport)
        .report(&Report {
            info: Some(info),
            ..Report::default()
        })
        .unwrap();
    let now = freesize_hub::db::now();
    let requests = [
        (
            &tower.device,
            "Delete backups/pc/2024-full.tar in /mnt/user (85.9 GB)",
            "done",
            -7200,
            "deleted 1 of 1, freeing 85.9 GB",
            Some(r#"{"by":{"type":"approver","fingerprint":"x"},"minutes":10}"#),
        ),
        (
            &tower.device,
            "Delete isos/ubuntu-24.04.iso in /mnt/user (5.96 GB)",
            "needs_approver",
            -300,
            "",
            None,
        ),
        (
            &phone.device,
            "Uninstall Galaxy Raiders (com.example.game)",
            "denied",
            -86_000,
            "denied on the device",
            None,
        ),
    ];
    let _ = phone_info;
    state.db.now(|c| {
        for (i, (device, summary, st, ago, detail, approval)) in requests.iter().enumerate() {
            c.execute(
                "INSERT INTO commands (id, device_id, kind, summary, admin, admin_name, created, expires,
                                       signed, state, detail, approval, freed, updated)
                 VALUES (?1, ?2, 'delete', ?3, 'u-admin', 'Robert', ?4, ?5, '{}', ?6, ?7, ?8, ?9, ?4)",
                params![format!("DEMO{i}"), device, summary, now + ago, now + ago + 86_400, st, detail,
                        approval, if *st == "done" { 92_200_000_000i64 } else { 0 }],
            )
            .unwrap();
        }
    });
}

fn app(package: &str, label: &str, gb: u64, cache: u64, days: Option<i64>) -> AppEntry {
    AppEntry {
        package: package.into(),
        label: label.into(),
        code: GB / 4,
        data: gb * GB,
        cache: cache * GB,
        last_used: days.map(|d| freesize_hub::db::now() - d * 86_400),
        installed: None,
        source: "Play Store".into(),
        removable: true,
    }
}

/// A pairing code made the way the dashboard makes one.
fn make_code(state: &AppState, url: &str, group: i64) -> String {
    let code = freesize_proto::pairing::new_code();
    state.db.now(|c| {
        c.execute(
            "INSERT INTO pairing_codes (hash, group_id, created, expires, created_by)
             VALUES (?1, ?2, ?3, ?4, 'demo')",
            params![
                freesize_proto::pairing::code_hash(&code),
                group,
                freesize_hub::db::now(),
                freesize_hub::db::now() + 600
            ],
        )
        .unwrap();
    });
    PairingLink {
        hub: url.into(),
        code,
        key: Some(state.key.public().fingerprint()),
    }
    .to_uri()
}
