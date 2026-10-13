//! "Do I have a copy?" (0.12): the devices' reports of their Nextcloud
//! folders joined with each other and with the Hub's own listing of the
//! server, on the server, the account and the path, which are the same on
//! every client. Per folder: how big it is on the server and which devices
//! keep it stored; per big file the same, with the server's file id; and
//! the risks: what one device alone holds, what only the server holds, and
//! big folders no synced folder covers. Nothing here changes anything.

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::Json;
use axum::extract::State;
use freesize_core::snapshot::FolderSize;
use freesize_proto::sync::{BIG_FILE, SyncReport};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::AppState;
use crate::auth::User;
use crate::dashboard::{DeviceSummary, device_summaries, visible};
use crate::devices::gunzip_json;
use crate::error::ApiResult;
use crate::nextcloud::{Account, Entry, account_key, accounts};

/// A scanned folder this big with no synced folder in it is worth a line.
const NOT_SYNCED_FROM: u64 = 1024 * 1024 * 1024;
/// Most such folders named per device.
const NOT_SYNCED_PER_DEVICE: usize = 10;
/// Most folders and files in the answer.
const MAX_ROWS: usize = 2_000;

/// Folders of the system's own that nobody syncs, by name, in any case.
const SYSTEM_FOLDERS: &[&str] = &[
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "$recycle.bin",
    "system volume information",
    "appdata",
    "library",
    "applications",
    "system",
    "usr",
    "var",
    "lib",
    "lib64",
    "proc",
    "sys",
    "dev",
    "snap",
    "opt",
    "bin",
    "sbin",
    "etc",
    "boot",
    "run",
    "tmp",
    ".cache",
    ".local",
    "android",
    "node_modules",
    ".git",
    "steam",
    "steamapps",
    "steamlibrary",
    "target",
];

// ----- Inputs ---------------------------------------------------------------

/// One device's report.
pub struct DeviceSync {
    pub id: String,
    pub name: String,
    pub report: SyncReport,
}

/// One connected account and what the Hub listed on it (empty until the
/// first listing).
pub struct ServerListing {
    pub account: Account,
    pub entries: Vec<Entry>,
}

/// The top folders of one device's latest scan of one root.
pub struct ScanTop {
    pub device_id: String,
    pub device_name: String,
    /// As the device writes it: `C:\` or `/mnt/user`.
    pub root: String,
    /// Depth-one folders, with their sizes.
    pub folders: Vec<(String, u64)>,
}

// ----- Output ---------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize)]
pub struct Copies {
    pub accounts: Vec<AccountView>,
    /// Accounts devices sync with that the Hub isn't connected to.
    pub unmatched: Vec<UnmatchedAccount>,
    pub folders: Vec<FolderCopies>,
    pub files: Vec<FileCopies>,
    pub risks: Vec<Risk>,
    /// Devices that reported their Nextcloud folders.
    pub reported: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct AccountView {
    #[serde(flatten)]
    pub account: Account,
    /// Names of the devices syncing with it.
    pub devices: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct UnmatchedAccount {
    pub server: String,
    pub user: String,
    pub devices: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct FolderCopies {
    pub server: String,
    pub user: String,
    /// On the server: `/Photos/2019`.
    pub path: String,
    /// What the server holds there; none when the Hub hasn't listed it.
    pub on_server: Option<u64>,
    /// The best-known size: the server's, else the most a device knows.
    pub size: u64,
    pub devices: Vec<DeviceCopy>,
    /// Devices with any of it stored.
    pub kept_on: usize,
    /// Bytes stored across the devices, all told.
    pub stored: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DeviceCopy {
    pub id: String,
    pub name: String,
    /// Where it is on the device.
    pub local: String,
    pub size: u64,
    pub on_disk: u64,
    pub files: u64,
    pub on_disk_files: u64,
    /// Every file the device knows of is stored on it.
    pub complete: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct FileCopies {
    pub server: String,
    pub user: String,
    pub path: String,
    pub size: u64,
    pub fileid: Option<i64>,
    pub checksums: String,
    /// None when the Hub hasn't listed the account.
    pub on_server: Option<bool>,
    pub devices: Vec<FileCopy>,
    pub kept_on: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct FileCopy {
    pub id: String,
    pub name: String,
    pub local: String,
    pub on_disk: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Risk {
    /// "only_on_device", "only_on_server" or "not_synced".
    pub kind: String,
    /// "warning" or "info".
    pub severity: String,
    pub title: String,
    pub detail: String,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    /// On the server, or on the device for what isn't synced.
    pub path: String,
    pub size: u64,
}

// ----- The join -------------------------------------------------------------

type Key = (String, String);

#[derive(Default)]
struct FolderSide {
    size: u64,
    devices: Vec<DeviceCopy>,
}

#[derive(Default)]
struct FileSide {
    size: u64,
    devices: Vec<FileCopy>,
}

/// Joins everything. Pure, so it can be tested without a Hub.
pub fn join(devices: &[DeviceSync], listings: &[ServerListing], scans: &[ScanTop]) -> Copies {
    // What the Hub listed, per account.
    let mut servers: HashMap<Key, &ServerListing> = HashMap::new();
    for listing in listings {
        servers.insert(
            account_key(&listing.account.server, &listing.account.user),
            listing,
        );
    }
    let mut server_entries: HashMap<Key, HashMap<&str, &Entry>> = HashMap::new();
    for (key, listing) in &servers {
        server_entries.insert(
            key.clone(),
            listing
                .entries
                .iter()
                .map(|e| (e.path.as_str(), e))
                .collect(),
        );
    }

    // What the devices keep, per account and path.
    let mut folders: BTreeMap<(Key, String), FolderSide> = BTreeMap::new();
    let mut files: BTreeMap<(Key, String), FileSide> = BTreeMap::new();
    let mut account_devices: BTreeMap<Key, (String, String, Vec<String>)> = BTreeMap::new();
    for device in devices {
        for account in &device.report.accounts {
            let key = account_key(&account.server, &account.user);
            let known = account_devices
                .entry(key.clone())
                .or_insert_with(|| (account.server.clone(), account.user.clone(), Vec::new()));
            if !known.2.contains(&device.name) {
                known.2.push(device.name.clone());
            }
            for folder in &account.folders {
                let root = folder.remote_path("");
                let mut add_dir =
                    |path: String, local: String, size, on_disk, count, on_disk_count| {
                        if depth(&path) > 2 {
                            return;
                        }
                        let side = folders.entry((key.clone(), path)).or_default();
                        side.size = side.size.max(size);
                        side.devices.push(DeviceCopy {
                            id: device.id.clone(),
                            name: device.name.clone(),
                            local,
                            size,
                            on_disk,
                            files: count,
                            on_disk_files: on_disk_count,
                            complete: count > 0 && on_disk_count >= count,
                        });
                    };
                add_dir(
                    root.clone(),
                    folder.local.clone(),
                    folder.size,
                    folder.on_disk,
                    folder.files,
                    folder.on_disk_files,
                );
                for dir in &folder.dirs {
                    add_dir(
                        folder.remote_path(&dir.path),
                        local_path(&folder.local, &dir.path),
                        dir.size,
                        dir.on_disk,
                        dir.files,
                        dir.on_disk_files,
                    );
                }
                for file in &folder.big {
                    let side = files
                        .entry((key.clone(), folder.remote_path(&file.path)))
                        .or_default();
                    side.size = side.size.max(file.size);
                    side.devices.push(FileCopy {
                        id: device.id.clone(),
                        name: device.name.clone(),
                        local: local_path(&folder.local, &file.path),
                        on_disk: file.on_disk,
                    });
                }
            }
        }
    }

    // Folders and big files the server has that no device reported.
    for (key, listing) in &servers {
        for entry in &listing.entries {
            if entry.is_dir {
                if depth(&entry.path) <= 2 {
                    folders
                        .entry((key.clone(), entry.path.clone()))
                        .or_default();
                }
            } else if entry.size >= BIG_FILE {
                files
                    .entry((key.clone(), entry.path.clone()))
                    .or_default()
                    .size = entry.size;
            }
        }
    }

    let name_of = |key: &Key| -> (String, String) {
        if let Some(listing) = servers.get(key) {
            (listing.account.server.clone(), listing.account.user.clone())
        } else if let Some((server, user, _)) = account_devices.get(key) {
            (server.clone(), user.clone())
        } else {
            (key.0.clone(), key.1.clone())
        }
    };

    let mut out = Copies {
        reported: devices
            .iter()
            .filter(|d| !d.report.accounts.is_empty())
            .count(),
        ..Copies::default()
    };
    for listing in listings {
        let key = account_key(&listing.account.server, &listing.account.user);
        out.accounts.push(AccountView {
            account: listing.account.clone(),
            devices: account_devices
                .get(&key)
                .map(|a| a.2.clone())
                .unwrap_or_default(),
        });
    }
    for (key, (server, user, names)) in &account_devices {
        if !servers.contains_key(key) {
            out.unmatched.push(UnmatchedAccount {
                server: server.clone(),
                user: user.clone(),
                devices: names.clone(),
            });
        }
    }

    for ((key, path), mut side) in folders {
        let on_server = server_entries
            .get(&key)
            .and_then(|e| e.get(path.as_str()))
            .map(|e| e.size);
        side.devices.sort_by(|a, b| a.name.cmp(&b.name));
        let (server, user) = name_of(&key);
        out.folders.push(FolderCopies {
            server,
            user,
            size: on_server.unwrap_or(side.size),
            on_server,
            kept_on: side.devices.iter().filter(|d| d.on_disk > 0).count(),
            stored: side.devices.iter().map(|d| d.on_disk).sum(),
            devices: side.devices,
            path,
        });
    }
    out.folders
        .sort_by(|a, b| b.stored.cmp(&a.stored).then_with(|| a.path.cmp(&b.path)));
    out.folders.truncate(MAX_ROWS);

    for ((key, path), mut side) in files {
        let listed = server_entries.get(&key);
        let entry = listed.and_then(|e| e.get(path.as_str()));
        side.devices.sort_by(|a, b| a.name.cmp(&b.name));
        let (server, user) = name_of(&key);
        out.files.push(FileCopies {
            server,
            user,
            size: entry.map_or(side.size, |e| e.size),
            fileid: entry.map(|e| e.fileid).filter(|&id| id > 0),
            checksums: entry.map(|e| e.checksums.join(" ")).unwrap_or_default(),
            on_server: listed.map(|_| entry.is_some()),
            kept_on: side.devices.iter().filter(|d| d.on_disk).count(),
            devices: side.devices,
            path,
        });
    }
    out.files
        .sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
    out.files.truncate(MAX_ROWS);

    out.risks = risks(&out, devices, scans);
    out
}

fn depth(path: &str) -> usize {
    path.trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .count()
}

/// A path on the device, with the device's own separator.
fn local_path(local: &str, relative: &str) -> String {
    if relative.is_empty() {
        return local.to_string();
    }
    let sep = if local.contains('\\') { "\\" } else { "/" };
    format!(
        "{}{sep}{}",
        local.trim_end_matches(['/', '\\']),
        relative.replace('/', sep)
    )
}

fn risks(copies: &Copies, devices: &[DeviceSync], scans: &[ScanTop]) -> Vec<Risk> {
    let mut risks = Vec::new();
    // Listed accounts: the server's side is known.
    let listed: HashSet<Key> = copies
        .accounts
        .iter()
        .filter(|a| a.account.synced.is_some())
        .map(|a| account_key(&a.account.server, &a.account.user))
        .collect();

    // Big files one device alone holds, which the server lacks.
    for file in &copies.files {
        let key = account_key(&file.server, &file.user);
        if !listed.contains(&key) || file.on_server != Some(false) {
            continue;
        }
        let holders: Vec<&FileCopy> = file.devices.iter().filter(|d| d.on_disk).collect();
        if holders.len() == 1 {
            let only = holders[0];
            risks.push(Risk {
                kind: "only_on_device".into(),
                severity: "warning".into(),
                title: format!("{} is only on {}", file.path, only.name),
                detail: format!(
                    "Nextcloud doesn't have it yet and no other device keeps it. On {}: {}",
                    only.name, only.local
                ),
                device_id: Some(only.id.clone()),
                device_name: Some(only.name.clone()),
                path: file.path.clone(),
                size: file.size,
            });
        }
    }
    // Folders a device holds that the server doesn't know.
    for folder in &copies.folders {
        let key = account_key(&folder.server, &folder.user);
        if !listed.contains(&key) || folder.on_server.is_some() || folder.path == "/" {
            continue;
        }
        let holders: Vec<&DeviceCopy> = folder.devices.iter().filter(|d| d.on_disk > 0).collect();
        if holders.len() == 1 && holders[0].on_disk > 0 {
            let only = holders[0];
            risks.push(Risk {
                kind: "only_on_device".into(),
                severity: "warning".into(),
                title: format!("{} is only on {}", folder.path, only.name),
                detail: format!(
                    "The folder isn't on Nextcloud and no other device keeps it. On {}: {}",
                    only.name, only.local
                ),
                device_id: Some(only.id.clone()),
                device_name: Some(only.name.clone()),
                path: folder.path.clone(),
                size: only.on_disk,
            });
        }
    }
    // Top folders only the server holds.
    for folder in &copies.folders {
        let key = account_key(&folder.server, &folder.user);
        if !listed.contains(&key) || depth(&folder.path) != 1 || folder.kept_on > 0 {
            continue;
        }
        let Some(size) = folder.on_server.filter(|&s| s > 0) else {
            continue;
        };
        risks.push(Risk {
            kind: "only_on_server".into(),
            severity: "info".into(),
            title: format!("{} is only on Nextcloud", folder.path),
            detail: if folder.devices.is_empty() {
                "No device syncs it.".to_string()
            } else {
                format!(
                    "{} syncs it, but keeps it online-only.",
                    folder
                        .devices
                        .iter()
                        .map(|d| d.name.as_str())
                        .collect::<Vec<_>>()
                        .join(" and ")
                )
            },
            device_id: None,
            device_name: None,
            path: folder.path.clone(),
            size,
        });
    }
    // Big scanned folders that no synced folder covers.
    let mut synced_locals: HashMap<&str, Vec<String>> = HashMap::new();
    for device in devices {
        for account in &device.report.accounts {
            for folder in &account.folders {
                synced_locals
                    .entry(device.id.as_str())
                    .or_default()
                    .push(normalize(&folder.local));
            }
        }
    }
    for scan in scans {
        let locals = synced_locals
            .get(scan.device_id.as_str())
            .cloned()
            .unwrap_or_default();
        let sep = if scan.root.contains('\\') { "\\" } else { "/" };
        let mut found: Vec<(String, u64)> = scan
            .folders
            .iter()
            .filter(|(path, size)| {
                *size >= NOT_SYNCED_FROM
                    && !path.contains('/')
                    && !SYSTEM_FOLDERS.contains(&path.to_lowercase().as_str())
            })
            .map(|(path, size)| {
                (
                    format!(
                        "{}{sep}{}",
                        scan.root.trim_end_matches(['/', '\\']),
                        path.replace('/', sep)
                    ),
                    *size,
                )
            })
            .filter(|(full, _)| {
                let full = normalize(full);
                !locals.iter().any(|local| {
                    full == *local
                        || full.starts_with(&format!("{local}/"))
                        || local.starts_with(&format!("{full}/"))
                })
            })
            .collect();
        found.sort_by_key(|f| std::cmp::Reverse(f.1));
        for (path, size) in found.into_iter().take(NOT_SYNCED_PER_DEVICE) {
            risks.push(Risk {
                kind: "not_synced".into(),
                severity: "info".into(),
                title: format!("{path} on {} isn't in a synced folder", scan.device_name),
                detail: "FreeSize knows of no copy elsewhere. If there is one, nothing is \
                         wrong; if not, this is what a lost or broken device would take."
                    .into(),
                device_id: Some(scan.device_id.clone()),
                device_name: Some(scan.device_name.clone()),
                path,
                size,
            });
        }
    }
    risks.sort_by(|a, b| {
        severity_rank(&a.severity)
            .cmp(&severity_rank(&b.severity))
            .then_with(|| b.size.cmp(&a.size))
    });
    risks
}

fn severity_rank(severity: &str) -> u8 {
    match severity {
        "warning" => 0,
        _ => 1,
    }
}

/// A device path for comparing: `/` between names, no trailing one, and
/// without case, since Windows and macOS don't mind it.
fn normalize(path: &str) -> String {
    path.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

// ----- From the database ----------------------------------------------------

pub(crate) fn device_syncs(
    c: &Connection,
    devices: &[DeviceSummary],
) -> rusqlite::Result<Vec<DeviceSync>> {
    let mut stmt = c.prepare("SELECT report FROM sync WHERE device_id = ?1")?;
    let mut out = Vec::new();
    for device in devices {
        let report: Option<String> = stmt.query_row([&device.id], |r| r.get(0)).optional_row()?;
        if let Some(report) = report.and_then(|t| serde_json::from_str::<SyncReport>(&t).ok()) {
            out.push(DeviceSync {
                id: device.id.clone(),
                name: device.name.clone(),
                report,
            });
        }
    }
    Ok(out)
}

trait OptionalRow<T> {
    fn optional_row(self) -> rusqlite::Result<Option<T>>;
}

impl<T> OptionalRow<T> for rusqlite::Result<T> {
    fn optional_row(self) -> rusqlite::Result<Option<T>> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err),
        }
    }
}

pub(crate) fn listings(c: &Connection) -> rusqlite::Result<Vec<ServerListing>> {
    let mut out = Vec::new();
    let mut stmt = c.prepare(
        "SELECT path, fileid, size, is_dir, modified, checksums FROM nextcloud_files
         WHERE account_id = ?1 ORDER BY path",
    )?;
    for account in accounts(c)? {
        let entries = stmt
            .query_map([account.id], |r| {
                Ok(Entry {
                    path: r.get(0)?,
                    fileid: r.get(1)?,
                    size: r.get::<_, i64>(2)?.max(0) as u64,
                    is_dir: r.get(3)?,
                    modified: r.get(4)?,
                    checksums: r
                        .get::<_, String>(5)?
                        .split_whitespace()
                        .map(str::to_string)
                        .collect(),
                })
            })?
            .collect::<rusqlite::Result<Vec<Entry>>>()?;
        out.push(ServerListing { account, entries });
    }
    Ok(out)
}

pub(crate) fn scan_tops(
    c: &Connection,
    devices: &[DeviceSummary],
) -> rusqlite::Result<Vec<ScanTop>> {
    let mut out = Vec::new();
    for device in devices {
        for scan in &device.scans {
            let blob: Vec<u8> = c.query_row(
                "SELECT folders FROM scans WHERE id = ?1",
                params![scan.id],
                |r| r.get(0),
            )?;
            let folders: Vec<FolderSize> = gunzip_json(&blob).unwrap_or_default();
            out.push(ScanTop {
                device_id: device.id.clone(),
                device_name: device.name.clone(),
                root: scan.root.clone(),
                folders: folders
                    .into_iter()
                    .filter(|f| !f.path.is_empty() && !f.path.contains('/'))
                    .map(|f| (f.path, f.allocated.max(f.size)))
                    .collect(),
            });
        }
    }
    Ok(out)
}

/// `GET /api/copies`: the join, for the devices the person may see.
pub async fn copies(State(state): State<AppState>, user: User) -> ApiResult<Json<Copies>> {
    let config = state.config.clone();
    let mut copies = state
        .db
        .call(move |c| -> rusqlite::Result<Copies> {
            let groups = visible(c, &user, &config)?;
            let devices = device_summaries(c, &groups, None)?;
            let syncs = device_syncs(c, &devices)?;
            let listings = listings(c)?;
            let scans = scan_tops(c, &devices)?;
            Ok(join(&syncs, &listings, &scans))
        })
        .await?;
    for account in &mut copies.accounts {
        account.account.syncing = state.nextcloud.is_syncing(account.account.id);
    }
    Ok(Json(copies))
}

#[cfg(test)]
mod tests {
    use freesize_proto::sync::{SyncAccount, SyncDir, SyncFile, SyncFolder, join_remote};

    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    fn account(id: i64, synced: Option<i64>) -> Account {
        Account {
            id,
            server: "https://cloud.example.com".into(),
            user: "bob".into(),
            added: 1,
            added_by: "Admin".into(),
            synced,
            error: None,
            files: 0,
            folders: 0,
            bytes: 0,
            trash: 0,
            quota_used: 0,
            syncing: false,
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

    fn entry(path: &str, fileid: i64, size: u64, is_dir: bool) -> Entry {
        Entry {
            path: path.into(),
            fileid,
            size,
            is_dir,
            modified: 0,
            checksums: if is_dir {
                vec![]
            } else {
                vec!["SHA1:x".into()]
            },
        }
    }

    fn device(id: &str, name: &str, server: &str, folders: Vec<SyncFolder>) -> DeviceSync {
        DeviceSync {
            id: id.into(),
            name: name.into(),
            report: SyncReport {
                taken: 1,
                accounts: vec![SyncAccount {
                    server: server.into(),
                    user: "Bob".into(),
                    client: "desktop".into(),
                    folders,
                }],
            },
        }
    }

    #[test]
    fn counts_devices_per_folder_and_file() {
        // The desktop syncs everything and keeps it; the laptop syncs
        // Photos online-only but keeps 2019; the phone has the app folder.
        let desktop = device(
            "d1",
            "Desktop",
            "https://cloud.example.com/",
            vec![SyncFolder {
                local: "C:\\Users\\bob\\Nextcloud".into(),
                remote: "/".into(),
                size: 10 * GB,
                on_disk: 10 * GB,
                files: 3,
                on_disk_files: 3,
                virtual_files: "off".into(),
                dirs: vec![
                    dir("Photos", 8 * GB, 8 * GB, 2, 2),
                    dir("Photos/2019", 5 * GB, 5 * GB, 1, 1),
                    dir("Photos/2020", 3 * GB, 3 * GB, 1, 1),
                    dir("Work", 2 * GB, 2 * GB, 1, 1),
                ],
                big: vec![
                    SyncFile {
                        path: "Photos/2019/trip.mp4".into(),
                        size: 5 * GB,
                        modified: 0,
                        on_disk: true,
                    },
                    SyncFile {
                        path: "Work/draft.iso".into(),
                        size: 2 * GB,
                        modified: 0,
                        on_disk: true,
                    },
                ],
            }],
        );
        let laptop = device(
            "d2",
            "Laptop",
            "HTTPS://Cloud.example.com",
            vec![SyncFolder {
                local: "/home/bob/Photos".into(),
                remote: "/Photos".into(),
                size: 8 * GB,
                on_disk: 5 * GB,
                files: 2,
                on_disk_files: 1,
                virtual_files: "suffix".into(),
                dirs: vec![
                    dir("2019", 5 * GB, 5 * GB, 1, 1),
                    dir("2020", 3 * GB, 0, 1, 0),
                ],
                big: vec![SyncFile {
                    path: "2019/trip.mp4".into(),
                    size: 5 * GB,
                    modified: 0,
                    on_disk: true,
                }],
            }],
        );
        let listing = ServerListing {
            account: account(1, Some(5)),
            entries: vec![
                entry("/", 1, 9 * GB, true),
                entry("/Photos", 2, 8 * GB, true),
                entry("/Photos/2019", 3, 5 * GB, true),
                entry("/Photos/2019/trip.mp4", 4, 5 * GB, false),
                entry("/Photos/2020", 5, 3 * GB, true),
                entry("/Archive", 6, GB, true),
            ],
        };
        let scans = vec![ScanTop {
            device_id: "d1".into(),
            device_name: "Desktop".into(),
            root: "C:\\".into(),
            folders: vec![
                ("Users".into(), 40 * GB),
                ("Windows".into(), 30 * GB),
                ("Games".into(), 90 * GB),
                ("Small".into(), 5),
            ],
        }];
        let copies = join(&[desktop, laptop], &[listing], &scans);
        assert_eq!(copies.reported, 2);
        assert_eq!(copies.accounts[0].devices, ["Desktop", "Laptop"]);
        assert!(copies.unmatched.is_empty());

        let folder = |path: &str| copies.folders.iter().find(|f| f.path == path).unwrap();
        let photos = folder("/Photos");
        assert_eq!(photos.on_server, Some(8 * GB));
        assert_eq!(photos.kept_on, 2);
        assert_eq!(photos.stored, 13 * GB);
        assert_eq!(photos.devices[0].local, "C:\\Users\\bob\\Nextcloud\\Photos");
        assert_eq!(photos.devices[1].local, "/home/bob/Photos");
        assert!(photos.devices[0].complete);
        assert!(!photos.devices[1].complete);
        let y2020 = folder("/Photos/2020");
        assert_eq!(y2020.kept_on, 1);
        assert_eq!(y2020.devices.len(), 2);
        // The server's Archive folder, kept by nobody.
        let archive = folder("/Archive");
        assert_eq!(archive.kept_on, 0);
        assert_eq!(archive.size, GB);
        // Work isn't on the server (not uploaded yet).
        let work = folder("/Work");
        assert_eq!(work.on_server, None);
        assert_eq!(work.size, 2 * GB);
        // The root and the laptop's deeper folders fall out of the two levels.
        assert!(copies.folders.iter().any(|f| f.path == "/"));
        // The most stored across devices comes first.
        assert_eq!(copies.folders[0].path, "/Photos");

        let trip = copies
            .files
            .iter()
            .find(|f| f.path == "/Photos/2019/trip.mp4")
            .unwrap();
        assert_eq!(trip.fileid, Some(4));
        assert_eq!(trip.on_server, Some(true));
        assert_eq!(trip.kept_on, 2);
        assert_eq!(trip.checksums, "SHA1:x");
        let draft = copies
            .files
            .iter()
            .find(|f| f.path == "/Work/draft.iso")
            .unwrap();
        assert_eq!(draft.on_server, Some(false));
        assert_eq!(draft.kept_on, 1);

        let kinds: Vec<(&str, &str)> = copies
            .risks
            .iter()
            .map(|r| (r.kind.as_str(), r.path.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("only_on_device", "/Work/draft.iso"),
                ("only_on_device", "/Work"),
                ("not_synced", "C:\\Games"),
                ("only_on_server", "/Archive"),
            ]
        );
        assert_eq!(copies.risks[0].device_name.as_deref(), Some("Desktop"));
        assert!(
            copies.risks[0]
                .detail
                .contains("C:\\Users\\bob\\Nextcloud\\Work\\draft.iso")
        );
        assert_eq!(copies.risks[2].size, 90 * GB);
    }

    #[test]
    fn an_unlisted_account_raises_no_alarm() {
        let phone = device(
            "p1",
            "Phone",
            "https://cloud.example.com",
            vec![SyncFolder {
                local: "/storage/emulated/0/Android/media/com.nextcloud.client/nextcloud/bob@cloud.example.com".into(),
                remote: "/".into(),
                size: GB,
                on_disk: GB,
                files: 1,
                on_disk_files: 1,
                virtual_files: "android".into(),
                dirs: vec![dir("Camera", GB, GB, 1, 1)],
                big: vec![SyncFile {
                    path: "Camera/clip.mp4".into(),
                    size: GB,
                    modified: 0,
                    on_disk: true,
                }],
            }],
        );
        // Not connected on the Hub at all.
        let copies = join(&[phone], &[], &[]);
        assert_eq!(copies.unmatched.len(), 1);
        assert_eq!(copies.unmatched[0].devices, ["Phone"]);
        assert_eq!(copies.folders.len(), 2);
        assert_eq!(copies.files[0].on_server, None);
        assert!(copies.risks.is_empty());
        // Connected but not listed yet: still nothing to say.
        let phone = DeviceSync {
            id: "p1".into(),
            name: "Phone".into(),
            report: copies_report(),
        };
        let listing = ServerListing {
            account: account(1, None),
            entries: vec![],
        };
        let copies = join(&[phone], &[listing], &[]);
        assert!(copies.unmatched.is_empty());
        assert_eq!(copies.files[0].on_server, Some(false));
        assert!(copies.risks.is_empty());
    }

    fn copies_report() -> SyncReport {
        SyncReport {
            taken: 1,
            accounts: vec![SyncAccount {
                server: "https://cloud.example.com".into(),
                user: "bob".into(),
                client: "android".into(),
                folders: vec![SyncFolder {
                    local: "/x".into(),
                    remote: "/".into(),
                    size: GB,
                    on_disk: GB,
                    files: 1,
                    on_disk_files: 1,
                    virtual_files: String::new(),
                    dirs: vec![],
                    big: vec![SyncFile {
                        path: "clip.mp4".into(),
                        size: GB,
                        modified: 0,
                        on_disk: true,
                    }],
                }],
            }],
        }
    }

    #[test]
    fn paths_join_and_compare() {
        assert_eq!(
            local_path("C:\\Users\\bob\\Nextcloud\\", "Photos/2019"),
            "C:\\Users\\bob\\Nextcloud\\Photos\\2019"
        );
        assert_eq!(local_path("/home/bob/Nextcloud", ""), "/home/bob/Nextcloud");
        assert_eq!(normalize("C:\\Users\\Bob\\"), "c:/users/bob");
        assert_eq!(depth("/"), 0);
        assert_eq!(depth("/a/b"), 2);
        assert_eq!(join_remote("/Photos", "2019"), "/Photos/2019");
    }
}
