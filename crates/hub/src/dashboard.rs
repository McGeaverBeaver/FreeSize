//! The dashboard's API for devices, their scans, device groups and pairing.
//! Viewers see only devices in groups they may look at; changes need an
//! admin.

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, Query, State};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use freesize_core::snapshot::FolderSize;
use freesize_proto::pairing::{code_hash, new_code};
use freesize_proto::{AppsReport, DeviceInfo, PairingLink};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::{Admin, User};
use crate::db::now;
use crate::devices::gunzip_json;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, Config, audit};

/// A device counts as online if it was heard from this recently.
const ONLINE_SECONDS: i64 = 120;
/// How long a pairing code works.
const PAIRING_SECONDS: i64 = 10 * 60;
/// Longest grant a device group may offer, in minutes.
pub const MAX_GRANT_MINUTES: u32 = 8 * 60;

#[derive(Clone, Debug, Serialize)]
pub struct Group {
    pub id: i64,
    pub name: String,
    pub grant_minutes: Vec<u32>,
    pub viewer_groups: Vec<String>,
    pub devices: i64,
}

pub fn groups(c: &Connection) -> rusqlite::Result<Vec<Group>> {
    let mut stmt = c.prepare(
        "SELECT g.id, g.name, g.grant_minutes, g.viewer_groups,
                (SELECT count(*) FROM devices d WHERE d.group_id = g.id AND d.removed = 0)
         FROM device_groups g ORDER BY g.name",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Group {
            id: r.get(0)?,
            name: r.get(1)?,
            grant_minutes: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
            viewer_groups: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
            devices: r.get(4)?,
        })
    })?;
    rows.collect()
}

/// Ids of the device groups `user` may look at.
pub(crate) fn visible(
    c: &Connection,
    user: &User,
    config: &Config,
) -> rusqlite::Result<HashSet<i64>> {
    Ok(groups(c)?
        .into_iter()
        .filter(|g| user.sees(&g.viewer_groups, config))
        .map(|g| g.id)
        .collect())
}

#[derive(Clone, Debug, Serialize)]
pub struct DiskRow {
    pub name: String,
    pub path: String,
    pub total: i64,
    pub free: i64,
    pub updated: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScanRow {
    pub id: i64,
    pub root: String,
    pub label: String,
    pub taken: i64,
    pub size: i64,
    pub allocated: i64,
    pub files: i64,
    pub dirs: i64,
    pub errors: i64,
    pub complete: bool,
    /// The first folders the scan couldn't read, relative to `root`.
    pub unreadable: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeviceSummary {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub fingerprint: String,
    pub group: GroupRef,
    pub info: DeviceInfo,
    pub paired: i64,
    pub last_seen: i64,
    pub online: bool,
    pub disks: Vec<DiskRow>,
    /// The newest scan of each scanned folder.
    pub scans: Vec<ScanRow>,
    pub waiting: i64,
    /// The name of the approver's device, if it's paired here.
    pub approver_name: Option<String>,
    /// No agent runs on it: its scans are uploaded as files.
    pub agentless: bool,
    /// Alerts on now, not counting waiting requests.
    pub alerts: i64,
    /// The app's version, from its name ("FreeSize app 0.9.0").
    pub version: String,
    /// Runs an older version than the Hub.
    pub behind: bool,
    /// What its junk cleaner found, if it reported.
    pub junk: Option<JunkSummary>,
}

/// A device's junk in one line.
#[derive(Clone, Debug, Serialize)]
pub struct JunkSummary {
    pub total: u64,
    /// Safe and not in use: what one clean would free.
    pub safe: u64,
    pub taken: i64,
}

/// What a device reported as junk: the cleaner's report, or for a phone
/// without one, the junk in its apps report.
pub(crate) fn junk_summary(c: &Connection, device: &str) -> rusqlite::Result<Option<JunkSummary>> {
    let text: Option<String> = c
        .query_row(
            "SELECT report FROM junk WHERE device_id = ?1",
            [device],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(report) =
        text.and_then(|t| serde_json::from_str::<freesize_proto::JunkReport>(&t).ok())
    {
        return Ok(Some(JunkSummary {
            total: report.total,
            safe: report.safe,
            taken: report.taken,
        }));
    }
    let text: Option<String> = c
        .query_row(
            "SELECT report FROM apps WHERE device_id = ?1",
            [device],
            |r| r.get(0),
        )
        .optional()?;
    Ok(text
        .and_then(|t| serde_json::from_str::<AppsReport>(&t).ok())
        .filter(|apps| !apps.junk.is_empty())
        .map(|apps| JunkSummary {
            total: apps.junk.iter().map(|j| j.bytes).sum(),
            safe: 0,
            taken: apps.taken,
        }))
}

/// The version at the end of an app's name, as devices report it.
pub(crate) fn version_of(app: &str) -> &str {
    app.rsplit(' ')
        .next()
        .filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap_or("")
}

/// Whether `version` is older than `than`, comparing numbers part by part
/// ("0.9.10" is newer than "0.9.9"). Unknown versions aren't behind.
pub fn older(version: &str, than: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split(['.', '-'])
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    if version.is_empty() {
        return false;
    }
    let (a, b) = (parts(version), parts(than));
    let len = a.len().max(b.len());
    for i in 0..len {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x < y;
        }
    }
    false
}

#[derive(Clone, Debug, Serialize)]
pub struct GroupRef {
    pub id: i64,
    pub name: String,
}

pub(crate) fn disks(c: &Connection, device: &str) -> rusqlite::Result<Vec<DiskRow>> {
    let mut stmt = c.prepare(
        "SELECT name, path, total, free, updated FROM disks WHERE device_id = ?1 ORDER BY total DESC",
    )?;
    let rows = stmt.query_map([device], |r| {
        Ok(DiskRow {
            name: r.get(0)?,
            path: r.get(1)?,
            total: r.get(2)?,
            free: r.get(3)?,
            updated: r.get(4)?,
        })
    })?;
    rows.collect()
}

pub(crate) fn scan_row(r: &rusqlite::Row) -> rusqlite::Result<ScanRow> {
    Ok(ScanRow {
        id: r.get(0)?,
        root: r.get(1)?,
        label: r.get(2)?,
        taken: r.get(3)?,
        size: r.get(4)?,
        allocated: r.get(5)?,
        files: r.get(6)?,
        dirs: r.get(7)?,
        errors: r.get(8)?,
        complete: r.get(9)?,
        unreadable: serde_json::from_str(&r.get::<_, String>(10)?).unwrap_or_default(),
    })
}

pub(crate) const SCAN_COLUMNS: &str =
    "id, root, label, taken, size, allocated, files, dirs, errors, complete, unreadable";

pub(crate) fn latest_scans(c: &Connection, device: &str) -> rusqlite::Result<Vec<ScanRow>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {SCAN_COLUMNS} FROM scans s WHERE device_id = ?1 AND taken = (
             SELECT max(taken) FROM scans t WHERE t.device_id = s.device_id AND t.root = s.root)
         GROUP BY root ORDER BY allocated DESC"
    ))?;
    let rows = stmt.query_map([device], scan_row)?;
    rows.collect()
}

pub(crate) fn device_summaries(
    c: &Connection,
    groups: &HashSet<i64>,
    only: Option<&str>,
) -> rusqlite::Result<Vec<DeviceSummary>> {
    let mut stmt = c.prepare(
        "SELECT d.id, d.name, d.platform, d.fingerprint, d.group_id, g.name, d.info, d.paired,
                d.last_seen,
                (SELECT count(*) FROM commands m WHERE m.device_id = d.id
                   AND m.state IN ('queued', 'delivered', 'shown', 'needs_approver')),
                d.agentless,
                (SELECT count(*) FROM alerts a WHERE a.device_id = d.id AND a.resolved IS NULL
                   AND a.kind != 'request')
         FROM devices d JOIN device_groups g ON g.id = d.group_id
         WHERE d.removed = 0 AND (?1 IS NULL OR d.id = ?1)
         ORDER BY d.name COLLATE NOCASE",
    )?;
    let rows: Vec<DeviceSummary> = stmt
        .query_map([only], |r| {
            let last_seen: i64 = r.get(8)?;
            let agentless: bool = r.get(10)?;
            Ok(DeviceSummary {
                id: r.get(0)?,
                name: r.get(1)?,
                platform: r.get(2)?,
                fingerprint: r.get(3)?,
                group: GroupRef {
                    id: r.get(4)?,
                    name: r.get(5)?,
                },
                info: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
                paired: r.get(7)?,
                last_seen,
                online: !agentless && now() - last_seen < ONLINE_SECONDS,
                disks: Vec::new(),
                scans: Vec::new(),
                waiting: r.get(9)?,
                approver_name: None,
                agentless,
                alerts: r.get(11)?,
                version: String::new(),
                behind: false,
                junk: None,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut visible = Vec::new();
    for mut device in rows {
        if !groups.contains(&device.group.id) {
            continue;
        }
        device.version = version_of(&device.info.app).to_string();
        device.behind = !device.agentless && older(&device.version, env!("CARGO_PKG_VERSION"));
        device.disks = disks(c, &device.id)?;
        device.scans = latest_scans(c, &device.id)?;
        device.junk = junk_summary(c, &device.id)?;
        if let Some(approver) = &device.info.approver {
            device.approver_name = c
                .query_row(
                    "SELECT name FROM devices WHERE fingerprint = ?1 AND removed = 0",
                    [approver],
                    |r| r.get(0),
                )
                .optional()?;
        }
        visible.push(device);
    }
    Ok(visible)
}

/// `GET /api/devices`.
pub async fn list(
    State(state): State<AppState>,
    user: User,
) -> ApiResult<Json<Vec<DeviceSummary>>> {
    let config = state.config.clone();
    let devices = state
        .db
        .call(move |c| {
            let groups = visible(c, &user, &config)?;
            device_summaries(c, &groups, None)
        })
        .await?;
    Ok(Json(devices))
}

/// The device if `user` may see it.
pub(crate) fn find_device(
    c: &Connection,
    user: &User,
    config: &Config,
    id: &str,
) -> ApiResult<DeviceSummary> {
    let groups = visible(c, user, config)?;
    device_summaries(c, &groups, Some(id))?
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::not_found("no such device"))
}

#[derive(Serialize)]
pub struct DiskHistory {
    pub path: String,
    /// `[day, total, used]`, oldest first.
    pub samples: Vec<[i64; 3]>,
}

#[derive(Serialize)]
pub struct RootScans {
    pub root: String,
    pub label: String,
    pub scans: Vec<ScanRow>,
}

#[derive(Serialize)]
pub struct DeviceDetail {
    #[serde(flatten)]
    pub summary: DeviceSummary,
    pub history: Vec<DiskHistory>,
    pub roots: Vec<RootScans>,
    pub apps: Option<Value>,
    /// What an Unraid server reported: array, shares, Docker, VMs, …
    pub unraid: Option<Value>,
    /// What the device's quarantine holds.
    pub quarantine: Option<Value>,
    /// What its junk cleaner found, rule by rule.
    pub junk_report: Option<Value>,
    /// What it keeps of its Nextcloud folders.
    pub sync: Option<Value>,
}

/// `GET /api/devices/{id}`.
pub async fn detail(
    State(state): State<AppState>,
    user: User,
    Path(id): Path<String>,
) -> ApiResult<Json<DeviceDetail>> {
    let config = state.config.clone();
    let detail = state
        .db
        .call(move |c| -> ApiResult<DeviceDetail> {
            let summary = find_device(c, &user, &config, &id)?;
            let mut history: Vec<DiskHistory> = Vec::new();
            let mut stmt = c.prepare(
                "SELECT path, day, total, used FROM disk_samples
                 WHERE device_id = ?1 AND day >= ?2 ORDER BY path, day",
            )?;
            let rows = stmt.query_map(params![id, now() / 86_400 - 120], |r| {
                Ok((r.get::<_, String>(0)?, [r.get(1)?, r.get(2)?, r.get(3)?]))
            })?;
            for row in rows {
                let (path, sample) = row?;
                match history.last_mut() {
                    Some(h) if h.path == path => h.samples.push(sample),
                    _ => history.push(DiskHistory {
                        path,
                        samples: vec![sample],
                    }),
                }
            }
            let mut roots: Vec<RootScans> = Vec::new();
            let mut stmt = c.prepare(&format!(
                "SELECT {SCAN_COLUMNS} FROM scans WHERE device_id = ?1 ORDER BY root, taken DESC"
            ))?;
            for scan in stmt.query_map([&id], scan_row)? {
                let scan = scan?;
                match roots.last_mut() {
                    Some(r) if r.root == scan.root => r.scans.push(scan),
                    _ => roots.push(RootScans {
                        root: scan.root.clone(),
                        label: scan.label.clone(),
                        scans: vec![scan],
                    }),
                }
            }
            roots.sort_by_key(|r| std::cmp::Reverse(r.scans[0].allocated));
            let apps: Option<String> = c
                .query_row("SELECT report FROM apps WHERE device_id = ?1", [&id], |r| {
                    r.get(0)
                })
                .optional()?;
            let apps = apps.and_then(|text| serde_json::from_str::<AppsReport>(&text).ok());
            let stored = |sql: &str| -> rusqlite::Result<Option<Value>> {
                let text: Option<String> = c.query_row(sql, [&id], |r| r.get(0)).optional()?;
                Ok(text.and_then(|t| serde_json::from_str(&t).ok()))
            };
            let unraid = stored("SELECT report FROM unraid WHERE device_id = ?1")?;
            let quarantine = stored("SELECT batches FROM quarantine WHERE device_id = ?1")?;
            let junk_report = stored("SELECT report FROM junk WHERE device_id = ?1")?;
            let sync = stored("SELECT report FROM sync WHERE device_id = ?1")?;
            Ok(DeviceDetail {
                summary,
                history,
                roots,
                apps: apps.map(|a| serde_json::to_value(a).unwrap_or(Value::Null)),
                unraid,
                quarantine,
                junk_report,
                sync,
            })
        })
        .await?;
    Ok(Json(detail))
}

#[derive(Deserialize)]
pub struct DeviceChange {
    name: Option<String>,
    group_id: Option<i64>,
}

/// `PATCH /api/devices/{id}`: rename or move to another group.
pub async fn update(
    State(state): State<AppState>,
    Admin(user): Admin,
    Path(id): Path<String>,
    Json(change): Json<DeviceChange>,
) -> ApiResult<Json<Value>> {
    let config = state.config.clone();
    state
        .db
        .call(move |c| -> ApiResult<()> {
            let device = find_device(c, &user, &config, &id)?;
            if let Some(name) = change.name.as_deref().map(str::trim) {
                if name.is_empty() || name.chars().count() > 80 {
                    return Err(ApiError::bad_request("names are 1 to 80 characters"));
                }
                c.execute(
                    "UPDATE devices SET name = ?1 WHERE id = ?2",
                    params![name, id],
                )?;
                audit::append(
                    c,
                    &user.name,
                    "device renamed",
                    Some(&id),
                    &json!({"from": device.name, "to": name}),
                )?;
            }
            if let Some(group) = change.group_id {
                let exists: bool = c.query_row(
                    "SELECT count(*) > 0 FROM device_groups WHERE id = ?1",
                    [group],
                    |r| r.get(0),
                )?;
                if !exists {
                    return Err(ApiError::bad_request("no such group"));
                }
                c.execute(
                    "UPDATE devices SET group_id = ?1 WHERE id = ?2",
                    params![group, id],
                )?;
                audit::append(
                    c,
                    &user.name,
                    "device moved",
                    Some(&id),
                    &json!({"from": device.group.name, "to_group": group}),
                )?;
            }
            Ok(())
        })
        .await?;
    Ok(Json(json!({"ok": true})))
}

/// `DELETE /api/devices/{id}`: forgets a device and what it reported. The
/// device is told the next time it calls, and stops.
pub async fn remove(
    State(state): State<AppState>,
    Admin(user): Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let config = state.config.clone();
    state
        .db
        .call(move |c| -> ApiResult<()> {
            let device = find_device(c, &user, &config, &id)?;
            let tx = c.transaction()?;
            tx.execute("UPDATE devices SET removed = 1 WHERE id = ?1", [&id])?;
            tx.execute("DELETE FROM scans WHERE device_id = ?1", [&id])?;
            tx.execute("DELETE FROM disks WHERE device_id = ?1", [&id])?;
            tx.execute("DELETE FROM disk_samples WHERE device_id = ?1", [&id])?;
            tx.execute("DELETE FROM apps WHERE device_id = ?1", [&id])?;
            tx.execute("DELETE FROM junk WHERE device_id = ?1", [&id])?;
            // Its alerts end quietly.
            tx.execute(
                "UPDATE alerts SET resolved = ?1 WHERE device_id = ?2 AND resolved IS NULL",
                params![now(), id],
            )?;
            tx.execute(
                "UPDATE commands SET state = 'cancelled', updated = ?1
                 WHERE device_id = ?2 AND state IN ('queued', 'delivered', 'shown', 'needs_approver')",
                params![now(), id],
            )?;
            audit::append(
                &tx,
                &user.name,
                "device removed",
                Some(&id),
                &json!({"name": device.name, "fingerprint": device.fingerprint}),
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({"ok": true})))
}

/// `GET /api/devices/{id}/audit`: the log entries about one device.
pub async fn device_audit(
    State(state): State<AppState>,
    user: User,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<audit::Entry>>> {
    let config = state.config.clone();
    let entries = state
        .db
        .call(move |c| -> ApiResult<Vec<audit::Entry>> {
            find_device(c, &user, &config, &id)?;
            Ok(audit::list(c, None, Some(&id), 100)?)
        })
        .await?;
    Ok(Json(entries))
}

/// A scan the user may see, with its device id.
pub(crate) fn find_scan(
    c: &Connection,
    user: &User,
    config: &Config,
    scan: i64,
) -> ApiResult<(ScanRow, String)> {
    let row = c
        .query_row(
            &format!("SELECT {SCAN_COLUMNS}, device_id FROM scans WHERE id = ?1"),
            [scan],
            |r| Ok((scan_row(r)?, r.get::<_, String>(11)?)),
        )
        .optional()?
        .ok_or_else(|| ApiError::not_found("no such scan"))?;
    find_device(c, user, config, &row.1)?;
    Ok(row)
}

pub(crate) fn parent_of(path: &str) -> Option<&str> {
    if path.is_empty() {
        None
    } else {
        Some(path.rsplit_once('/').map_or("", |(parent, _)| parent))
    }
}

pub(crate) fn name_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

#[derive(Deserialize)]
pub struct FolderQuery {
    #[serde(default)]
    path: String,
}

#[derive(Serialize)]
pub struct Item {
    pub name: String,
    pub path: String,
    pub size: i64,
    pub allocated: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<i64>,
}

#[derive(Serialize)]
pub struct Folder {
    pub scan: ScanRow,
    pub device_id: String,
    pub path: String,
    pub size: i64,
    pub allocated: i64,
    pub folders: Vec<Item>,
    pub files: Vec<Item>,
    /// What the listed folders and files don't account for: smaller files
    /// and folders the scan didn't keep.
    pub rest: i64,
}

/// `GET /api/scans/{id}/folders?path=a/b`: one folder of a scan.
pub async fn folder(
    State(state): State<AppState>,
    user: User,
    Path(id): Path<i64>,
    Query(query): Query<FolderQuery>,
) -> ApiResult<Json<Folder>> {
    let config = state.config.clone();
    let folder = state
        .db
        .call(move |c| -> ApiResult<Folder> {
            let (scan, device_id) = find_scan(c, &user, &config, id)?;
            let blob: Vec<u8> =
                c.query_row("SELECT folders FROM scans WHERE id = ?1", [id], |r| {
                    r.get(0)
                })?;
            let folders: Vec<FolderSize> = gunzip_json(&blob).unwrap_or_default();
            let path = query.path.trim_matches('/').to_string();
            let this = folders
                .iter()
                .find(|f| f.path == path)
                .ok_or_else(|| ApiError::not_found("that folder isn't in this scan"))?;
            let mut children: Vec<Item> = folders
                .iter()
                .filter(|f| parent_of(&f.path) == Some(path.as_str()))
                .map(|f| Item {
                    name: name_of(&f.path).to_string(),
                    path: f.path.clone(),
                    size: f.size as i64,
                    allocated: f.allocated as i64,
                    modified: None,
                })
                .collect();
            children.sort_by_key(|i| std::cmp::Reverse(i.allocated));
            let mut stmt = c.prepare(
                "SELECT path, size, allocated, modified FROM scan_files WHERE scan_id = ?1
                 ORDER BY allocated DESC",
            )?;
            let files: Vec<Item> = stmt
                .query_map([id], |r| {
                    let file_path: String = r.get(0)?;
                    Ok(Item {
                        name: name_of(&file_path).to_string(),
                        path: file_path,
                        size: r.get(1)?,
                        allocated: r.get(2)?,
                        modified: Some(r.get(3)?),
                    })
                })?
                .filter_map(Result::ok)
                .filter(|f| parent_of(&f.path) == Some(path.as_str()))
                .collect();
            let listed: i64 = children.iter().chain(&files).map(|i| i.allocated).sum();
            Ok(Folder {
                scan,
                device_id,
                size: this.size as i64,
                allocated: this.allocated as i64,
                rest: (this.allocated as i64 - listed).max(0),
                path,
                folders: children,
                files,
            })
        })
        .await?;
    Ok(Json(folder))
}

#[derive(Deserialize)]
pub struct FilesQuery {
    #[serde(default)]
    under: String,
}

/// `GET /api/scans/{id}/files?under=a/b`: the biggest files kept from a
/// scan, all or below a folder.
pub async fn files(
    State(state): State<AppState>,
    user: User,
    Path(id): Path<i64>,
    Query(query): Query<FilesQuery>,
) -> ApiResult<Json<Value>> {
    let config = state.config.clone();
    let listed = state
        .db
        .call(move |c| -> ApiResult<Value> {
            let (scan, device_id) = find_scan(c, &user, &config, id)?;
            let under = query.under.trim_matches('/').to_string();
            let prefix = format!("{under}/");
            let mut stmt = c.prepare(
                "SELECT path, size, allocated, modified FROM scan_files WHERE scan_id = ?1
                 ORDER BY allocated DESC",
            )?;
            let files: Vec<Item> = stmt
                .query_map([id], |r| {
                    let path: String = r.get(0)?;
                    Ok(Item {
                        name: name_of(&path).to_string(),
                        path,
                        size: r.get(1)?,
                        allocated: r.get(2)?,
                        modified: Some(r.get(3)?),
                    })
                })?
                .filter_map(Result::ok)
                .filter(|f| under.is_empty() || f.path.starts_with(&prefix))
                .take(500)
                .collect();
            Ok(json!({"scan": scan, "device_id": device_id, "under": under, "files": files}))
        })
        .await?;
    Ok(Json(listed))
}

/// `GET /api/groups`.
pub async fn list_groups(State(state): State<AppState>, user: User) -> ApiResult<Json<Vec<Group>>> {
    let config = state.config.clone();
    let groups = state
        .db
        .call(move |c| -> rusqlite::Result<Vec<Group>> {
            Ok(groups(c)?
                .into_iter()
                .filter(|g| user.sees(&g.viewer_groups, &config))
                .collect())
        })
        .await?;
    Ok(Json(groups))
}

#[derive(Deserialize)]
pub struct GroupChange {
    name: Option<String>,
    grant_minutes: Option<Vec<u32>>,
    viewer_groups: Option<Vec<String>>,
}

fn clean_minutes(minutes: Vec<u32>) -> ApiResult<Vec<u32>> {
    let mut minutes: Vec<u32> = minutes.into_iter().filter(|&m| m > 0).collect();
    minutes.sort_unstable();
    minutes.dedup();
    if minutes.len() > 6 || minutes.iter().any(|&m| m > MAX_GRANT_MINUTES) {
        return Err(ApiError::bad_request(format!(
            "up to 6 grant lengths, each at most {MAX_GRANT_MINUTES} minutes"
        )));
    }
    Ok(minutes)
}

fn clean_viewers(groups: Vec<String>) -> ApiResult<Vec<String>> {
    let mut groups: Vec<String> = groups
        .into_iter()
        .map(|g| g.trim().to_string())
        .filter(|g| !g.is_empty())
        .collect();
    groups.sort();
    groups.dedup();
    if groups.len() > 20 || groups.iter().any(|g| g.len() > 100) {
        return Err(ApiError::bad_request("up to 20 viewer groups"));
    }
    Ok(groups)
}

pub(crate) fn clean_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(ApiError::bad_request("names are 1 to 60 characters"));
    }
    Ok(name.to_string())
}

/// `POST /api/groups`.
pub async fn create_group(
    State(state): State<AppState>,
    Admin(user): Admin,
    Json(change): Json<GroupChange>,
) -> ApiResult<Json<Value>> {
    let name = clean_name(change.name.as_deref().unwrap_or_default())?;
    let minutes = clean_minutes(change.grant_minutes.unwrap_or_else(|| vec![10, 30]))?;
    let viewers = clean_viewers(change.viewer_groups.unwrap_or_default())?;
    let id = state
        .db
        .call(move |c| -> ApiResult<i64> {
            let taken: bool = c.query_row(
                "SELECT count(*) > 0 FROM device_groups WHERE name = ?1",
                [&name],
                |r| r.get(0),
            )?;
            if taken {
                return Err(ApiError::conflict("a group with that name exists"));
            }
            c.execute(
                "INSERT INTO device_groups (name, grant_minutes, viewer_groups) VALUES (?1, ?2, ?3)",
                params![name, serde_json::to_string(&minutes).unwrap_or_default(),
                        serde_json::to_string(&viewers).unwrap_or_default()],
            )?;
            let id = c.last_insert_rowid();
            audit::append(
                c,
                &user.name,
                "device group made",
                None,
                &json!({"group": name, "grant_minutes": minutes, "viewer_groups": viewers}),
            )?;
            Ok(id)
        })
        .await?;
    Ok(Json(json!({"id": id})))
}

/// `PATCH /api/groups/{id}`.
pub async fn update_group(
    State(state): State<AppState>,
    Admin(user): Admin,
    Path(id): Path<i64>,
    Json(change): Json<GroupChange>,
) -> ApiResult<Json<Value>> {
    let name = change.name.as_deref().map(clean_name).transpose()?;
    let minutes = change.grant_minutes.map(clean_minutes).transpose()?;
    let viewers = change.viewer_groups.map(clean_viewers).transpose()?;
    state
        .db
        .call(move |c| -> ApiResult<()> {
            let old: String = c
                .query_row("SELECT name FROM device_groups WHERE id = ?1", [id], |r| r.get(0))
                .optional()?
                .ok_or_else(|| ApiError::not_found("no such group"))?;
            if let Some(name) = &name {
                let taken: bool = c.query_row(
                    "SELECT count(*) > 0 FROM device_groups WHERE name = ?1 AND id != ?2",
                    params![name, id],
                    |r| r.get(0),
                )?;
                if taken {
                    return Err(ApiError::conflict("a group with that name exists"));
                }
                c.execute("UPDATE device_groups SET name = ?1 WHERE id = ?2", params![name, id])?;
            }
            if let Some(minutes) = &minutes {
                c.execute(
                    "UPDATE device_groups SET grant_minutes = ?1 WHERE id = ?2",
                    params![serde_json::to_string(minutes).unwrap_or_default(), id],
                )?;
            }
            if let Some(viewers) = &viewers {
                c.execute(
                    "UPDATE device_groups SET viewer_groups = ?1 WHERE id = ?2",
                    params![serde_json::to_string(viewers).unwrap_or_default(), id],
                )?;
            }
            audit::append(
                c,
                &user.name,
                "device group changed",
                None,
                &json!({"group": old, "name": name, "grant_minutes": minutes, "viewer_groups": viewers}),
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({"ok": true})))
}

/// `DELETE /api/groups/{id}`: only empty groups, and never the last one.
pub async fn delete_group(
    State(state): State<AppState>,
    Admin(user): Admin,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    state
        .db
        .call(move |c| -> ApiResult<()> {
            let all = groups(c)?;
            let group = all
                .iter()
                .find(|g| g.id == id)
                .ok_or_else(|| ApiError::not_found("no such group"))?;
            if group.devices > 0 {
                return Err(ApiError::conflict(
                    "move its devices to another group first",
                ));
            }
            if all.len() == 1 {
                return Err(ApiError::conflict("there must be at least one group"));
            }
            c.execute("DELETE FROM pairing_codes WHERE group_id = ?1", [id])?;
            c.execute(
                "DELETE FROM devices WHERE group_id = ?1 AND removed = 1",
                [id],
            )?;
            c.execute("DELETE FROM device_groups WHERE id = ?1", [id])?;
            audit::append(
                c,
                &user.name,
                "device group deleted",
                None,
                &json!({"group": group.name}),
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
pub struct PairingRequest {
    group_id: i64,
}

#[derive(Serialize)]
pub struct PairingCode {
    pub code: String,
    pub link: String,
    /// The link as a QR code, an SVG data URL.
    pub qr: String,
    pub expires: i64,
    pub hub_url: String,
    pub fingerprint: String,
}

/// `POST /api/pairing-codes`: a one-time code for a new device.
pub async fn pairing_code(
    State(state): State<AppState>,
    Admin(user): Admin,
    Json(request): Json<PairingRequest>,
) -> ApiResult<Json<PairingCode>> {
    let hub_url = state.config.public_url().ok_or_else(|| {
        ApiError::conflict(
            "set the Hub's address (FREESIZE_HUB_URL) first: devices need it to reach the Hub",
        )
    })?;
    let code = new_code();
    let hash = code_hash(&code);
    let expires = now() + PAIRING_SECONDS;
    let group = request.group_id;
    state
        .db
        .call(move |c| -> ApiResult<()> {
            let name: String = c
                .query_row(
                    "SELECT name FROM device_groups WHERE id = ?1",
                    [group],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(|| ApiError::bad_request("no such group"))?;
            c.execute(
                "DELETE FROM pairing_codes WHERE expires < ?1",
                [now() - 86_400],
            )?;
            c.execute(
                "INSERT INTO pairing_codes (hash, group_id, created, expires, created_by)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![hash, group, now(), expires, user.name],
            )?;
            audit::append(
                c,
                &user.name,
                "pairing code made",
                None,
                &json!({"group": name}),
            )?;
            Ok(())
        })
        .await?;
    let fingerprint = state.key.public().fingerprint();
    let link = PairingLink {
        hub: hub_url.clone(),
        code: code.clone(),
        key: Some(fingerprint.clone()),
    }
    .to_uri();
    let svg = qrcode::QrCode::new(link.as_bytes())
        .map_err(|e| ApiError::bad_request(e.to_string()))?
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(220, 220)
        .quiet_zone(true)
        .build();
    Ok(Json(PairingCode {
        code,
        qr: format!("data:image/svg+xml;base64,{}", STANDARD.encode(svg)),
        link,
        expires,
        hub_url,
        fingerprint,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert_eq!(version_of("FreeSize app 0.9.0"), "0.9.0");
        assert_eq!(version_of("No agent: scans are uploaded as files"), "");
        assert!(older("0.8.8", "0.9.0"));
        assert!(older("0.9.9", "0.9.10"));
        assert!(!older("0.9.0", "0.9.0"));
        assert!(!older("1.0.0", "0.9.0"));
        assert!(!older("", "0.9.0"));
    }

    #[test]
    fn paths() {
        assert_eq!(parent_of(""), None);
        assert_eq!(parent_of("Movies"), Some(""));
        assert_eq!(parent_of("Movies/2024"), Some("Movies"));
        assert_eq!(name_of("Movies/2024"), "2024");
        assert_eq!(name_of("Movies"), "Movies");
    }

    #[test]
    fn group_settings_are_checked() {
        assert_eq!(clean_minutes(vec![30, 10, 0, 10]).unwrap(), [10, 30]);
        assert!(clean_minutes(vec![600]).is_err());
        assert_eq!(
            clean_viewers(vec![" family ".into(), "".into()]).unwrap(),
            ["family"]
        );
        assert!(clean_name("  ").is_err());
    }
}
