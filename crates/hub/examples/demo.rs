//! Runs a Hub with a stand-in sign-in provider and a few made-up devices,
//! for trying the dashboard without Authentik or real devices:
//!
//! ```sh
//! cargo run -p disktree-hub --example demo
//! ```
//!
//! Then open http://127.0.0.1:9090 and sign in (the mock provider signs you
//! straight in as an admin). Data goes to a temporary folder.

#[allow(dead_code)]
#[path = "../tests/common/idp.rs"]
mod idp;

use clap::Parser;
use disktree_agent::{Client, UreqTransport};
use disktree_core::snapshot::FolderSize;
use disktree_hub::{AppState, Config};
use disktree_proto::{
    ActionKind, AppEntry, AppsReport, Breakdown, DeviceInfo, Disk, FileEntry, JunkEntry,
    PairingLink, Platform, Report, ScanReport, TypeEntry,
};
use rusqlite::params;

const GB: u64 = 1 << 30;

#[tokio::main]
async fn main() {
    let hub_addr = std::env::var("DEMO_HUB").unwrap_or_else(|_| "127.0.0.1:9090".into());
    let idp_addr = std::env::var("DEMO_IDP").unwrap_or_else(|_| "127.0.0.1:9091".into());
    let data = std::env::temp_dir().join(format!("disktree-hub-demo-{}", std::process::id()));
    let idp_listener = tokio::net::TcpListener::bind(&idp_addr).await.unwrap();
    let idp = idp::MockIdp::serve(
        idp_listener,
        format!("http://{idp_addr}/application/o/disktree/"),
    )
    .await;
    let config = Config::parse_from([
        "disktree-hub",
        "--data",
        data.to_str().unwrap(),
        "--url",
        &format!("http://{hub_addr}"),
        "--name",
        "Home Hub",
        "--oidc-issuer",
        &idp.issuer,
        "--oidc-client-id",
        idp::CLIENT_ID,
        "--oidc-client-secret",
        idp::CLIENT_SECRET,
    ]);
    let state = AppState::open(config).unwrap();
    let listener = tokio::net::TcpListener::bind(&hub_addr).await.unwrap();
    tokio::spawn(disktree_hub::serve(listener, state.clone()));
    let url = format!("http://{hub_addr}");
    let seed_state = state.clone();
    tokio::task::spawn_blocking(move || seed(&url, &seed_state))
        .await
        .unwrap();
    println!(
        "DiskTree Hub demo on http://{hub_addr} (data in {})",
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
            "DiskTree app 0.7.0".into()
        } else {
            "DiskTree agent 0.7.0".into()
        },
        actions: vec![ActionKind::Delete],
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
            modified: disktree_hub::db::now() - (mb as i64 % 900) * 86_400,
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
        taken: disktree_hub::db::now() - days_ago * 86_400 - 3600,
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
                    taken: disktree_hub::db::now() - 3600,
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
    for (i, (device, report, history)) in devices.into_iter().enumerate() {
        let code = make_code(state, url, group);
        let link = PairingLink::parse(&code).unwrap();
        let agent =
            disktree_agent::pair(&transport, &link, device.clone(), &mut |_| false).unwrap();
        Client::new(&agent, &transport).report(&report).unwrap();
        // Two months of daily disk samples, growing towards today's.
        let id = agent.device.clone();
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
                            disktree_hub::db::now() / 86_400 - day,
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
                    params![disktree_hub::db::now() - 3 * 3600 * i as i64, id],
                )
                .unwrap();
            }
        });
    }
}

fn app(package: &str, label: &str, gb: u64, cache: u64, days: Option<i64>) -> AppEntry {
    AppEntry {
        package: package.into(),
        label: label.into(),
        code: GB / 4,
        data: gb * GB,
        cache: cache * GB,
        last_used: days.map(|d| disktree_hub::db::now() - d * 86_400),
        installed: None,
        source: "Play Store".into(),
        removable: true,
    }
}

/// A pairing code made the way the dashboard makes one.
fn make_code(state: &AppState, url: &str, group: i64) -> String {
    let code = disktree_proto::pairing::new_code();
    state.db.now(|c| {
        c.execute(
            "INSERT INTO pairing_codes (hash, group_id, created, expires, created_by)
             VALUES (?1, ?2, ?3, ?4, 'demo')",
            params![
                disktree_proto::pairing::code_hash(&code),
                group,
                disktree_hub::db::now(),
                disktree_hub::db::now() + 600
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
