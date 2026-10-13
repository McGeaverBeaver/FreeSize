//! History, search and exports: comparing two scans of a folder, finding
//! files and folders across devices, CSV, JSON and HTML exports, and devices
//! without an agent, whose scans are uploaded as `ncdu -o` files.

use std::collections::HashSet;
use std::io::Read;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{Html, IntoResponse, Response};
use freesize_core::snapshot::{self, ChangeKind, FolderSize, Snapshot};
use freesize_core::{SizeMode, flags, format_size};
use freesize_proto::keys::{KeyPair, random_id};
use freesize_proto::{AppsReport, DeviceInfo, Platform};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::{Admin, User};
use crate::dashboard::{
    DeviceSummary, Item, ScanRow, device_summaries, find_device, find_scan, latest_scans, name_of,
    parent_of, visible,
};
use crate::db::now;
use crate::devices::{gunzip_json, platform_id, store_scan};
use crate::error::{ApiError, ApiResult};
use crate::web::escape;
use crate::{AppState, Config, audit};

const MB: u64 = 1_000_000;
/// Most changes listed when comparing two scans.
const MAX_CHANGES: usize = 500;
/// Most search results.
const MAX_HITS: usize = 300;
/// Saved searches per person.
const MAX_SAVED: i64 = 50;
/// Largest scan file accepted, after unpacking. Reading one takes several
/// times this in memory.
pub const MAX_UPLOAD: usize = 128 * 1024 * 1024;

// ----- Comparing scans ---------------------------------------------------

#[derive(Deserialize)]
pub struct CompareQuery {
    with: i64,
    #[serde(default)]
    format: Option<String>,
}

#[derive(Serialize)]
pub struct ChangeRow {
    pub path: String,
    pub kind: &'static str,
    pub before: u64,
    pub after: u64,
}

#[derive(Serialize)]
pub struct Comparison {
    pub device_id: String,
    pub from: ScanRow,
    pub to: ScanRow,
    /// Smaller changes are left out.
    pub min_change: u64,
    /// What the listed changes add and take away.
    pub grew: u64,
    pub shrank: u64,
    pub changes: Vec<ChangeRow>,
}

fn kind_name(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Grew => "grew",
        ChangeKind::Shrank => "shrank",
        ChangeKind::New => "new",
        ChangeKind::Gone => "gone",
    }
}

fn scan_folders(c: &Connection, scan: i64) -> rusqlite::Result<Vec<FolderSize>> {
    let blob: Vec<u8> = c.query_row("SELECT folders FROM scans WHERE id = ?1", [scan], |r| {
        r.get(0)
    })?;
    Ok(gunzip_json(&blob).unwrap_or_default())
}

/// The same rule as the app's Changes tab: a tenth of a percent of the
/// folder, between 1 MB and 50 MB.
fn min_change(total: u64) -> u64 {
    (total / 1000).clamp(MB, 50 * MB)
}

fn compare_scans(
    c: &Connection,
    user: &User,
    config: &Config,
    a: i64,
    b: i64,
) -> ApiResult<Comparison> {
    let (first, device) = find_scan(c, user, config, a)?;
    let (second, other) = find_scan(c, user, config, b)?;
    if device != other || first.root != second.root {
        return Err(ApiError::bad_request(
            "both scans must be of the same folder on the same device",
        ));
    }
    let (from, to) = if first.taken <= second.taken {
        (first, second)
    } else {
        (second, first)
    };
    let old = Snapshot {
        taken: from.taken,
        folders: scan_folders(c, from.id)?,
    };
    let new = Snapshot {
        taken: to.taken,
        folders: scan_folders(c, to.id)?,
    };
    let total = old
        .total(SizeMode::Allocated)
        .max(new.total(SizeMode::Allocated));
    let min = min_change(total);
    let changes: Vec<ChangeRow> = snapshot::compare(&old, &new, SizeMode::Allocated, min)
        .into_iter()
        .take(MAX_CHANGES)
        .map(|change| ChangeRow {
            kind: kind_name(change.kind),
            before: change.before,
            after: change.after,
            path: change.path,
        })
        .collect();
    let grew = changes
        .iter()
        .map(|c| c.after.saturating_sub(c.before))
        .sum();
    let shrank = changes
        .iter()
        .map(|c| c.before.saturating_sub(c.after))
        .sum();
    Ok(Comparison {
        device_id: device,
        from,
        to,
        min_change: min,
        grew,
        shrank,
        changes,
    })
}

/// `GET /api/scans/{id}/compare?with=other[&format=csv|json]`: what changed
/// between two scans of a folder, the older one first whichever is given.
pub async fn compare(
    State(state): State<AppState>,
    user: User,
    Path(id): Path<i64>,
    Query(query): Query<CompareQuery>,
) -> ApiResult<Response> {
    let config = state.config.clone();
    let (comparison, device) = state
        .db
        .call(move |c| -> ApiResult<(Comparison, String)> {
            let comparison = compare_scans(c, &user, &config, id, query.with)?;
            let device: String = c.query_row(
                "SELECT name FROM devices WHERE id = ?1",
                [&comparison.device_id],
                |r| r.get(0),
            )?;
            Ok((comparison, device))
        })
        .await?;
    let name = |ext: &str| {
        file_name(
            &[
                &device,
                label(&comparison.to),
                &iso_date(comparison.from.taken),
                "to",
                &iso_date(comparison.to.taken),
            ],
            ext,
        )
    };
    Ok(match query.format.as_deref() {
        Some("csv") => {
            let mut out = csv_line(&["change", "path", "before", "after", "difference"]);
            for change in &comparison.changes {
                out.push_str(&csv_fields(
                    &[change.kind, &shown_path(&change.path)],
                    &[
                        change.before as i64,
                        change.after as i64,
                        change.after as i64 - change.before as i64,
                    ],
                ));
            }
            download(out, "text/csv; charset=utf-8", &name("csv"))
        }
        Some("json") => download(
            serde_json::to_string_pretty(&comparison).unwrap_or_default(),
            "application/json",
            &name("json"),
        ),
        _ => Json(comparison).into_response(),
    })
}

// ----- Search ------------------------------------------------------------

#[derive(Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
    /// Smallest size listed, in bytes.
    #[serde(default)]
    min: u64,
    /// Only this device.
    #[serde(default)]
    device: Option<String>,
}

#[derive(Serialize)]
pub struct Hit {
    pub device_id: String,
    pub device_name: String,
    /// folder, file or app.
    pub kind: &'static str,
    pub scan_id: Option<i64>,
    pub root: String,
    pub label: String,
    pub path: String,
    pub allocated: u64,
    pub modified: Option<i64>,
}

#[derive(Serialize)]
pub struct SearchResult {
    pub hits: Vec<Hit>,
    /// More matched than are listed.
    pub more: bool,
    pub devices: usize,
}

/// What a search looks for: words anywhere in the path (all of them), and
/// `*.iso` or `.iso` for file types (any of them).
#[derive(Debug, Default, PartialEq)]
struct Terms {
    words: Vec<String>,
    extensions: Vec<String>,
}

impl Terms {
    fn parse(query: &str) -> Terms {
        let mut terms = Terms::default();
        for token in query.split_whitespace() {
            let token = token.to_lowercase();
            let extension = token
                .strip_prefix("*.")
                .or_else(|| token.strip_prefix('.'))
                .filter(|e| !e.is_empty() && !e.contains(['/', '.', '*']));
            match extension {
                Some(ext) => terms.extensions.push(ext.to_string()),
                None => terms.words.push(token.trim_matches('*').to_string()),
            }
        }
        terms.words.retain(|w| !w.is_empty());
        terms
    }

    fn is_empty(&self) -> bool {
        self.words.is_empty() && self.extensions.is_empty()
    }

    fn matches(&self, path: &str) -> bool {
        let path = path.to_lowercase();
        self.words.iter().all(|w| path.contains(w.as_str()))
            && (self.extensions.is_empty()
                || self.extensions.iter().any(|e| {
                    path.strip_suffix(e.as_str())
                        .is_some_and(|rest| rest.ends_with('.'))
                }))
    }

    /// Whether a folder can match: file types only match files.
    fn folders(&self) -> bool {
        self.extensions.is_empty()
    }
}

/// Whether any folder above `path` is in `matched`.
fn below_match(path: &str, matched: &HashSet<&str>) -> bool {
    let mut current = parent_of(path);
    while let Some(parent) = current {
        if matched.contains(parent) {
            return true;
        }
        current = parent_of(parent);
    }
    false
}

fn search_device(
    c: &Connection,
    device: &DeviceSummary,
    terms: &Terms,
    min: u64,
    hits: &mut Vec<Hit>,
) -> rusqlite::Result<()> {
    let hit = |kind, scan: &ScanRow, path: String, allocated: u64, modified| Hit {
        device_id: device.id.clone(),
        device_name: device.name.clone(),
        kind,
        scan_id: Some(scan.id),
        root: scan.root.clone(),
        label: scan.label.clone(),
        path,
        allocated,
        modified,
    };
    for scan in latest_scans(c, &device.id)? {
        let folders = scan_folders(c, scan.id)?;
        // A folder that matches is listed, not everything in it.
        let matched: HashSet<&str> = if terms.folders() {
            folders
                .iter()
                .filter(|f| !f.path.is_empty() && terms.matches(&f.path))
                .map(|f| f.path.as_str())
                .collect()
        } else {
            HashSet::new()
        };
        for folder in &folders {
            if matched.contains(folder.path.as_str())
                && !below_match(&folder.path, &matched)
                && folder.allocated >= min
            {
                hits.push(hit(
                    "folder",
                    &scan,
                    folder.path.clone(),
                    folder.allocated,
                    None,
                ));
            }
        }
        let mut stmt = c.prepare(
            "SELECT path, allocated, modified FROM scan_files
             WHERE scan_id = ?1 AND allocated >= ?2",
        )?;
        let files = stmt.query_map(params![scan.id, min as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for file in files {
            let (path, allocated, modified) = file?;
            if terms.matches(&path) && !below_match(&path, &matched) {
                hits.push(hit("file", &scan, path, allocated as u64, Some(modified)));
            }
        }
    }
    if terms.folders() {
        let report: Option<String> = c
            .query_row(
                "SELECT report FROM apps WHERE device_id = ?1",
                [&device.id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(report) = report.and_then(|t| serde_json::from_str::<AppsReport>(&t).ok()) {
            for app in report.apps {
                let total = app.code + app.data + app.cache;
                if total >= min && terms.matches(&format!("{} {}", app.label, app.package)) {
                    hits.push(Hit {
                        device_id: device.id.clone(),
                        device_name: device.name.clone(),
                        kind: "app",
                        scan_id: None,
                        root: String::new(),
                        label: "Apps".into(),
                        path: app.label,
                        allocated: total,
                        modified: app.last_used,
                    });
                }
            }
        }
    }
    Ok(())
}

/// `GET /api/search?q=iso&min=1000000000&device=id`: files, folders and
/// apps on every device the user may see, from each folder's newest scan.
pub async fn search(
    State(state): State<AppState>,
    user: User,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<SearchResult>> {
    let terms = Terms::parse(&query.q);
    if terms.is_empty() {
        return Err(ApiError::bad_request("type something to look for"));
    }
    if query.q.len() > 200 {
        return Err(ApiError::bad_request("that's too long to search for"));
    }
    let config = state.config.clone();
    let result = state
        .db
        .call(move |c| -> ApiResult<SearchResult> {
            let groups = visible(c, &user, &config)?;
            let devices = device_summaries(c, &groups, query.device.as_deref())?;
            let mut hits = Vec::new();
            for device in &devices {
                search_device(c, device, &terms, query.min, &mut hits)?;
            }
            hits.sort_by_key(|h| std::cmp::Reverse(h.allocated));
            let more = hits.len() > MAX_HITS;
            hits.truncate(MAX_HITS);
            Ok(SearchResult {
                hits,
                more,
                devices: devices.len(),
            })
        })
        .await?;
    Ok(Json(result))
}

#[derive(Serialize)]
pub struct SavedSearch {
    pub id: i64,
    pub name: String,
    pub query: String,
    pub min: i64,
}

/// `GET /api/searches`: the user's saved searches.
pub async fn saved_searches(
    State(state): State<AppState>,
    user: User,
) -> ApiResult<Json<Vec<SavedSearch>>> {
    let list = state
        .db
        .call(move |c| -> rusqlite::Result<Vec<SavedSearch>> {
            let mut stmt = c.prepare(
                "SELECT id, name, query, min_size FROM saved_searches WHERE subject = ?1
                 ORDER BY name COLLATE NOCASE",
            )?;
            let rows = stmt.query_map([&user.subject], |r| {
                Ok(SavedSearch {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    query: r.get(2)?,
                    min: r.get(3)?,
                })
            })?;
            rows.collect()
        })
        .await?;
    Ok(Json(list))
}

#[derive(Deserialize)]
pub struct NewSearch {
    name: String,
    query: String,
    #[serde(default)]
    min: u64,
}

/// `POST /api/searches`: saves a search for the user.
pub async fn save_search(
    State(state): State<AppState>,
    user: User,
    Json(search): Json<NewSearch>,
) -> ApiResult<Json<Value>> {
    let name = search.name.trim().to_string();
    let query = search.query.trim().to_string();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(ApiError::bad_request("names are 1 to 60 characters"));
    }
    if Terms::parse(&query).is_empty() || query.len() > 200 {
        return Err(ApiError::bad_request("that isn't something to search for"));
    }
    let id = state
        .db
        .call(move |c| -> ApiResult<i64> {
            let saved: i64 = c.query_row(
                "SELECT count(*) FROM saved_searches WHERE subject = ?1",
                [&user.subject],
                |r| r.get(0),
            )?;
            if saved >= MAX_SAVED {
                return Err(ApiError::conflict(format!(
                    "you can keep {MAX_SAVED} saved searches; remove one first"
                )));
            }
            c.execute(
                "INSERT INTO saved_searches (subject, name, query, min_size, created)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![user.subject, name, query, search.min as i64, now()],
            )?;
            Ok(c.last_insert_rowid())
        })
        .await?;
    Ok(Json(json!({"id": id})))
}

/// `DELETE /api/searches/{id}`.
pub async fn delete_search(
    State(state): State<AppState>,
    user: User,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let removed = state
        .db
        .call(move |c| {
            c.execute(
                "DELETE FROM saved_searches WHERE id = ?1 AND subject = ?2",
                params![id, user.subject],
            )
        })
        .await?;
    if removed == 0 {
        return Err(ApiError::not_found("no such saved search"));
    }
    Ok(Json(json!({"ok": true})))
}

// ----- Exports -----------------------------------------------------------

/// `yyyy-mm-dd` of a time in seconds since 1970.
pub fn iso_date(secs: i64) -> String {
    // Howard Hinnant's civil-from-days.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The date of a file's change time, or nothing when the scan didn't say.
fn known_date(modified: Option<i64>) -> String {
    modified
        .filter(|&m| m > 0)
        .map(iso_date)
        .unwrap_or_default()
}

/// `yyyy-mm-dd hh:mm` (UTC).
fn iso_time(secs: i64) -> String {
    let in_day = secs.rem_euclid(86_400);
    format!(
        "{} {:02}:{:02}",
        iso_date(secs),
        in_day / 3600,
        in_day % 3600 / 60
    )
}

/// A download name from `parts`, safe for any file system and header.
fn file_name(parts: &[&str], ext: &str) -> String {
    let mut name = String::new();
    for part in parts {
        let clean: String = part
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let clean = clean.trim_matches('-');
        if clean.is_empty() {
            continue;
        }
        if !name.is_empty() {
            name.push('-');
        }
        name.push_str(clean);
    }
    while name.contains("--") {
        name = name.replace("--", "-");
    }
    let name: String = name.chars().take(100).collect();
    format!("{}.{ext}", if name.is_empty() { "freesize" } else { &name })
}

fn download(body: String, content_type: &'static str, name: &str) -> Response {
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
        .unwrap_or(HeaderValue::from_static("attachment"));
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response()
}

/// One CSV cell. Text a spreadsheet would run as a formula gets a `'`.
fn csv_cell(text: &str) -> String {
    let text = if text.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{text}")
    } else {
        text.to_string()
    };
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

fn csv_line(cells: &[&str]) -> String {
    let mut line = cells
        .iter()
        .map(|c| csv_cell(c))
        .collect::<Vec<_>>()
        .join(",");
    line.push_str("\r\n");
    line
}

/// A CSV line of text cells, then numbers.
fn csv_fields(text: &[&str], numbers: &[i64]) -> String {
    let mut cells: Vec<String> = text.iter().map(|t| csv_cell(t)).collect();
    cells.extend(numbers.iter().map(i64::to_string));
    let mut line = cells.join(",");
    line.push_str("\r\n");
    line
}

/// `""` (the scanned folder itself) as `.`.
fn shown_path(path: &str) -> String {
    if path.is_empty() {
        ".".into()
    } else {
        path.to_string()
    }
}

fn label(scan: &ScanRow) -> &str {
    if scan.label.is_empty() {
        &scan.root
    } else {
        &scan.label
    }
}

fn scan_files(c: &Connection, scan: i64) -> rusqlite::Result<Vec<Item>> {
    let mut stmt = c.prepare(
        "SELECT path, size, allocated, modified FROM scan_files WHERE scan_id = ?1
         ORDER BY allocated DESC",
    )?;
    let rows = stmt.query_map([scan], |r| {
        let path: String = r.get(0)?;
        Ok(Item {
            name: name_of(&path).to_string(),
            path,
            size: r.get(1)?,
            allocated: r.get(2)?,
            modified: Some(r.get(3)?),
        })
    })?;
    rows.collect()
}

#[derive(Deserialize)]
pub struct ExportQuery {
    #[serde(default)]
    format: Option<String>,
}

/// `GET /api/scans/{id}/export?format=csv|json`: a scan's folders and files.
pub async fn export_scan(
    State(state): State<AppState>,
    user: User,
    Path(id): Path<i64>,
    Query(query): Query<ExportQuery>,
) -> ApiResult<Response> {
    let config = state.config.clone();
    let (scan, device, folders, files, types) = state
        .db
        .call(move |c| -> ApiResult<_> {
            let (scan, device_id) = find_scan(c, &user, &config, id)?;
            let device = find_device(c, &user, &config, &device_id)?;
            let types: String =
                c.query_row("SELECT types FROM scans WHERE id = ?1", [id], |r| r.get(0))?;
            let types: Value = serde_json::from_str(&types).unwrap_or(Value::Null);
            Ok((
                scan,
                device,
                scan_folders(c, id)?,
                scan_files(c, id)?,
                types,
            ))
        })
        .await?;
    let name = |ext: &str| file_name(&[&device.name, label(&scan), &iso_date(scan.taken)], ext);
    Ok(match query.format.as_deref() {
        Some("json") => {
            let body = json!({
                "device": {"id": device.id, "name": device.name, "platform": device.platform},
                "scan": scan,
                "folders": folders,
                "files": files,
                "types": types,
            });
            download(
                serde_json::to_string_pretty(&body).unwrap_or_default(),
                "application/json",
                &name("json"),
            )
        }
        _ => {
            let mut out = csv_line(&["type", "path", "size", "on disk", "changed"]);
            for folder in &folders {
                out.push_str(&csv_fields(
                    &["folder", &shown_path(&folder.path), ""],
                    &[folder.size as i64, folder.allocated as i64],
                ));
            }
            for file in &files {
                out.push_str(&csv_fields(
                    &["file", &file.path],
                    &[file.size, file.allocated],
                ));
                // The date goes last, as text.
                out.truncate(out.len() - 2);
                out.push(',');
                out.push_str(&known_date(file.modified));
                out.push_str("\r\n");
            }
            download(out, "text/csv; charset=utf-8", &name("csv"))
        }
    })
}

/// `GET /api/devices/export?format=csv|json`: every device and disk the
/// user may see.
pub async fn export_devices(
    State(state): State<AppState>,
    user: User,
    Query(query): Query<ExportQuery>,
) -> ApiResult<Response> {
    let config = state.config.clone();
    let hub = state.config.name.clone();
    let devices = state
        .db
        .call(move |c| -> ApiResult<Vec<DeviceSummary>> {
            let groups = visible(c, &user, &config)?;
            Ok(device_summaries(c, &groups, None)?)
        })
        .await?;
    let name = |ext: &str| file_name(&[&hub, "devices", &iso_date(now())], ext);
    Ok(match query.format.as_deref() {
        Some("json") => download(
            serde_json::to_string_pretty(&devices).unwrap_or_default(),
            "application/json",
            &name("json"),
        ),
        _ => {
            let mut out = csv_line(&[
                "device",
                "group",
                "platform",
                "system",
                "last seen",
                "disk",
                "path",
                "total",
                "used",
                "free",
            ]);
            for device in &devices {
                let seen = iso_time(device.last_seen);
                let base = [
                    device.name.as_str(),
                    device.group.name.as_str(),
                    device.platform.as_str(),
                    device.info.os.as_str(),
                    seen.as_str(),
                ];
                if device.disks.is_empty() {
                    let mut cells = base.to_vec();
                    cells.extend(["", "", "", "", ""]);
                    out.push_str(&csv_line(&cells));
                }
                for disk in &device.disks {
                    let mut cells = base.to_vec();
                    cells.extend([disk.name.as_str(), disk.path.as_str()]);
                    out.push_str(&csv_fields(
                        &cells,
                        &[disk.total, disk.total - disk.free, disk.free],
                    ));
                }
            }
            download(out, "text/csv; charset=utf-8", &name("csv"))
        }
    })
}

const REPORT_STYLE: &str = "\
:root{--fg:#1b1b1f;--dim:#6b6b75;--bar:#3b6fd8;--track:#0000001a;--bg:#fff;--line:#0000001a}\
@media (prefers-color-scheme:dark){:root{--fg:#e8e8ec;--dim:#9a9aa5;--bar:#6b9bf2;\
--track:#ffffff1f;--bg:#1c1c1f;--line:#ffffff1f}}\
body{font:15px/1.45 system-ui,sans-serif;color:var(--fg);background:var(--bg);margin:0 auto;\
max-width:880px;padding:24px 16px}h1{font-size:24px;margin:0}h2{font-size:18px;margin:30px 0 8px}\
h3{font-size:15px;margin:20px 0 4px}.dim{color:var(--dim)}small{color:var(--dim);font-weight:normal}\
table{width:100%;border-collapse:collapse}td,th{padding:5px 6px;border-bottom:1px solid var(--line);\
text-align:left;vertical-align:top}th{font-size:13px;color:var(--dim);font-weight:600}\
.num{text-align:right;white-space:nowrap}.bar{display:block;height:6px;border-radius:3px;\
background:var(--track);min-width:60px}.bar>span{display:block;height:100%;border-radius:3px;\
background:var(--bar)}.path{word-break:break-all}footer{margin-top:36px;font-size:13px}\
@media print{body{max-width:none}}";

fn bar_html(fraction: f64) -> String {
    format!(
        "<span class=\"bar\"><span style=\"width:{:.1}%\"></span></span>",
        (fraction * 100.0).clamp(0.0, 100.0)
    )
}

fn report_html(
    hub: &str,
    device: &DeviceSummary,
    roots: &[(ScanRow, Vec<FolderSize>, Vec<Item>)],
    apps: Option<&AppsReport>,
) -> String {
    let mut out = String::new();
    let e = |text: &str| escape(text);
    out.push_str(&format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{name} · storage report</title><style>{REPORT_STYLE}</style></head><body>\
         <h1>{name}</h1><p class=\"dim\">{system} · group {group} · last heard from {seen} UTC<br>\
         Report from {hub}, {today}</p>",
        name = e(&device.name),
        system = e(if device.info.os.is_empty() {
            device.info.platform.label()
        } else {
            &device.info.os
        }),
        group = e(&device.group.name),
        seen = iso_time(device.last_seen),
        hub = e(hub),
        today = iso_time(now()),
    ));
    if !device.disks.is_empty() {
        out.push_str(
            "<h2>Disks</h2><table><tr><th>Disk</th><th></th><th class=\"num\">Used</th>\
             <th class=\"num\">Free</th><th class=\"num\">Size</th></tr>",
        );
        for disk in &device.disks {
            let used = (disk.total - disk.free).max(0);
            let fraction = if disk.total > 0 {
                used as f64 / disk.total as f64
            } else {
                0.0
            };
            out.push_str(&format!(
                "<tr><td>{}<br><small class=\"path\">{}</small></td><td>{}</td>\
                 <td class=\"num\">{} ({:.0}%)</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                e(&disk.name),
                e(&disk.path),
                bar_html(fraction),
                format_size(used as u64),
                fraction * 100.0,
                format_size(disk.free.max(0) as u64),
                format_size(disk.total.max(0) as u64),
            ));
        }
        out.push_str("</table>");
    }
    if !roots.is_empty() {
        out.push_str("<h2>Scanned folders</h2>");
    }
    for (scan, folders, files) in roots {
        out.push_str(&format!(
            "<h3>{} <small>{} · {} on disk in {} files · scanned {} UTC{}</small></h3>",
            e(label(scan)),
            e(&scan.root),
            format_size(scan.allocated as u64),
            scan.files,
            iso_time(scan.taken),
            if scan.complete { "" } else { " · partial" },
        ));
        let total = scan.allocated.max(1) as f64;
        let mut top: Vec<&FolderSize> = folders
            .iter()
            .filter(|f| parent_of(&f.path) == Some(""))
            .collect();
        top.sort_by_key(|f| std::cmp::Reverse(f.allocated));
        if !top.is_empty() {
            out.push_str("<table><tr><th>Folder</th><th></th><th class=\"num\">Size</th></tr>");
            for folder in top.iter().take(15) {
                out.push_str(&format!(
                    "<tr><td class=\"path\">{}</td><td>{}</td><td class=\"num\">{}</td></tr>",
                    e(&folder.path),
                    bar_html(folder.allocated as f64 / total),
                    format_size(folder.allocated),
                ));
            }
            out.push_str("</table>");
        }
        if !files.is_empty() {
            out.push_str(
                "<table><tr><th>Largest files</th><th class=\"num\">Changed</th>\
                 <th class=\"num\">Size</th></tr>",
            );
            for file in files.iter().take(10) {
                out.push_str(&format!(
                    "<tr><td class=\"path\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                    e(&file.path),
                    known_date(file.modified),
                    format_size(file.allocated.max(0) as u64),
                ));
            }
            out.push_str("</table>");
        }
    }
    if let Some(apps) = apps.filter(|a| !a.apps.is_empty()) {
        let mut list: Vec<_> = apps.apps.iter().collect();
        list.sort_by_key(|a| std::cmp::Reverse(a.code + a.data + a.cache));
        out.push_str(
            "<h2>Apps</h2><table><tr><th>App</th><th class=\"num\">App</th>\
             <th class=\"num\">Data</th><th class=\"num\">Cache</th><th class=\"num\">Total</th></tr>",
        );
        for app in list.iter().take(20) {
            out.push_str(&format!(
                "<tr><td>{}<br><small>{}</small></td><td class=\"num\">{}</td><td class=\"num\">{}</td>\
                 <td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                e(&app.label),
                e(&app.package),
                format_size(app.code),
                format_size(app.data),
                format_size(app.cache),
                format_size(app.code + app.data + app.cache),
            ));
        }
        out.push_str("</table>");
    }
    out.push_str(&format!(
        "<footer class=\"dim\">Made by FreeSize Hub {}. Sizes are space on disk.</footer></body></html>",
        env!("CARGO_PKG_VERSION")
    ));
    out
}

#[derive(Deserialize)]
pub struct ReportQuery {
    #[serde(default)]
    download: bool,
}

/// `GET /api/devices/{id}/report[?download=true]`: a page about one device
/// to print or keep: disks, scanned folders, largest files and apps.
pub async fn device_report(
    State(state): State<AppState>,
    user: User,
    Path(id): Path<String>,
    Query(query): Query<ReportQuery>,
) -> ApiResult<Response> {
    let config = state.config.clone();
    let hub = state.config.name.clone();
    let (device, roots, apps) = state
        .db
        .call(move |c| -> ApiResult<_> {
            let device = find_device(c, &user, &config, &id)?;
            let mut roots = Vec::new();
            for scan in latest_scans(c, &id)? {
                let folders = scan_folders(c, scan.id)?;
                let files = scan_files(c, scan.id)?;
                roots.push((scan, folders, files));
            }
            let apps: Option<String> = c
                .query_row("SELECT report FROM apps WHERE device_id = ?1", [&id], |r| {
                    r.get(0)
                })
                .optional()?;
            let apps = apps.and_then(|t| serde_json::from_str::<AppsReport>(&t).ok());
            Ok((device, roots, apps))
        })
        .await?;
    let html = report_html(&hub, &device, &roots, apps.as_ref());
    let mut response = Html(html).into_response();
    let headers = response.headers_mut();
    // A page that stands alone, also when saved: its own styles, nothing
    // loaded from anywhere.
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; img-src data:; \
             frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        ),
    );
    if query.download {
        let name = file_name(&[&device.name, "report", &iso_date(now())], "html");
        if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
            headers.insert(header::CONTENT_DISPOSITION, value);
        }
    }
    Ok(response)
}

// ----- Devices without an agent --------------------------------------------

#[derive(Deserialize)]
pub struct NewDevice {
    name: String,
    group_id: i64,
    #[serde(default)]
    platform: Option<Platform>,
}

/// `POST /api/devices`: a device without an agent, such as a router or a
/// VPS, whose scans are uploaded as `ncdu -o` files. It has no key anyone
/// holds, so it can't call the Hub or be asked to do anything.
pub async fn create_device(
    State(state): State<AppState>,
    Admin(user): Admin,
    Json(new): Json<NewDevice>,
) -> ApiResult<Json<Value>> {
    let name = new.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(ApiError::bad_request("names are 1 to 80 characters"));
    }
    let platform = new.platform.unwrap_or(Platform::Other);
    // A key whose private half is thrown away at once.
    let key = KeyPair::generate().public();
    let info = DeviceInfo {
        name: name.clone(),
        platform,
        os: String::new(),
        hostname: String::new(),
        app: "No agent: scans are uploaded as files".into(),
        actions: Vec::new(),
        attended: false,
        approver: None,
        grants: Vec::new(),
    };
    let id = state
        .db
        .call(move |c| -> ApiResult<String> {
            let exists: bool = c.query_row(
                "SELECT count(*) > 0 FROM device_groups WHERE id = ?1",
                [new.group_id],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(ApiError::bad_request("no such group"));
            }
            let id = random_id::<10>();
            c.execute(
                "INSERT INTO devices (id, name, platform, key, fingerprint, group_id, info,
                                      paired, last_seen, agentless)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, 1)",
                params![
                    id,
                    name,
                    platform_id(platform),
                    key.to_base64(),
                    key.fingerprint(),
                    new.group_id,
                    serde_json::to_string(&info).unwrap_or_default(),
                    now(),
                ],
            )?;
            audit::append(
                c,
                &user.name,
                "device added without an agent",
                Some(&id),
                &json!({"name": name, "platform": platform.label()}),
            )?;
            Ok(id)
        })
        .await?;
    Ok(Json(json!({"id": id})))
}

#[derive(Deserialize)]
pub struct UploadQuery {
    #[serde(default)]
    label: String,
}

/// Unpacks a gzipped upload, refusing anything that unpacks too big.
fn unpack(bytes: &[u8]) -> ApiResult<Vec<u8>> {
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return Ok(bytes.to_vec());
    }
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .take(MAX_UPLOAD as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| ApiError::bad_request(format!("couldn't unpack the file: {e}")))?;
    if out.len() > MAX_UPLOAD {
        return Err(ApiError::bad_request(format!(
            "the scan file is over {} unpacked; run the FreeSize agent on that machine instead",
            format_size(MAX_UPLOAD as u64)
        )));
    }
    Ok(out)
}

/// `POST /api/devices/{id}/scans?label=Name`: a scan file for a device
/// without an agent: `ncdu -o` output (ncdu 1.x, or ncdu 2's JSON export)
/// or FreeSize's own export, plain or gzipped.
pub async fn upload_scan(
    State(state): State<AppState>,
    Admin(user): Admin,
    Path(id): Path<String>,
    Query(query): Query<UploadQuery>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    if body.is_empty() {
        return Err(ApiError::bad_request("choose a scan file to upload"));
    }
    let label = query.label.trim().chars().take(80).collect::<String>();
    // Reading a big scan takes a while: off the async threads.
    let report = tokio::task::spawn_blocking(move || -> ApiResult<_> {
        let text = unpack(&body)?;
        let (tree, taken) = freesize_core::ncdu::import_with_time(text.as_slice())
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        let now = now();
        let taken = taken.filter(|&t| t <= now + 86_400).unwrap_or(now);
        let errors = tree.nodes().iter().filter(|n| n.has(flags::ERROR)).count() as u64;
        let root = tree.path(tree.root()).to_string_lossy().into_owned();
        let label = if label.is_empty() {
            root.trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|n| !n.is_empty())
                .unwrap_or(&root)
                .to_string()
        } else {
            label
        };
        Ok(freesize_agent::report::scan_report(
            &tree, &label, taken, errors, true,
        ))
    })
    .await
    .map_err(|_| ApiError::internal("reading the scan file failed"))??;
    let config = state.config.clone();
    let stored = state
        .db
        .call(move |c| -> ApiResult<Value> {
            let device = find_device(c, &user, &config, &id)?;
            if !device.agentless {
                return Err(ApiError::bad_request(
                    "this device has an agent, which sends its own scans",
                ));
            }
            let summary = json!({
                "root": report.root,
                "label": report.label,
                "size": report.allocated,
                "files": report.files,
                "taken": report.taken,
            });
            let scan = store_scan(c, &id, report)?;
            c.execute(
                "UPDATE devices SET last_seen = max(last_seen, ?1) WHERE id = ?2",
                params![now(), id],
            )?;
            audit::append(c, &user.name, "scan file uploaded", Some(&id), &summary)?;
            let mut answer = summary;
            answer["scan"] = json!(scan);
            Ok(answer)
        })
        .await?;
    Ok(Json(stored))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_search_terms() {
        let terms = Terms::parse("Ubuntu *.ISO .img");
        assert_eq!(terms.words, ["ubuntu"]);
        assert_eq!(terms.extensions, ["iso", "img"]);
        assert!(terms.matches("Downloads/ubuntu-24.04.iso"));
        assert!(!terms.matches("Downloads/ubuntu-24.04.zip"));
        assert!(!terms.matches("Downloads/debian.iso"));
        assert!(!terms.matches("Downloads/ubuntu-iso"));
        assert!(Terms::parse("  * ").is_empty());
        let plain = Terms::parse("plex");
        assert!(plain.folders());
        assert!(plain.matches("appdata/Plex/Library"));
    }

    #[test]
    fn lists_only_the_top_match() {
        let matched: HashSet<&str> = ["appdata/plex", "appdata/plex/Library"].into();
        assert!(!below_match("appdata/plex", &matched));
        assert!(below_match("appdata/plex/Library", &matched));
        assert!(below_match("appdata/plex/Library/db.blob", &matched));
        assert!(!below_match("isos/plex.iso", &matched));
    }

    #[test]
    fn csv_is_safe_for_spreadsheets() {
        assert_eq!(csv_cell("plain"), "plain");
        assert_eq!(csv_cell("a,b"), "\"a,b\"");
        assert_eq!(csv_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_cell("=cmd|' /C calc'!A0"), "'=cmd|' /C calc'!A0");
        assert_eq!(csv_fields(&["x"], &[-5, 3]), "x,-5,3\r\n");
    }

    #[test]
    fn names_downloads_safely() {
        assert_eq!(
            file_name(&["Robert's Pixel", "Internal storage", "2026-10-12"], "csv"),
            "Robert-s-Pixel-Internal-storage-2026-10-12.csv"
        );
        assert_eq!(file_name(&["../../etc", ""], "json"), "etc.json");
        assert_eq!(file_name(&["ÄÖÜ"], "csv"), "freesize.csv");
    }

    #[test]
    fn dates() {
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(iso_date(1_791_763_200), "2026-10-12");
        assert_eq!(
            iso_time(1_791_763_200 + 3 * 3600 + 25 * 60),
            "2026-10-12 03:25"
        );
    }

    #[test]
    fn refuses_zip_bombs() {
        use std::io::Write;
        let mut packed = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        let zeros = vec![b' '; 1024 * 1024];
        for _ in 0..(MAX_UPLOAD / zeros.len() + 1) {
            packed.write_all(&zeros).unwrap();
        }
        let packed = packed.finish().unwrap();
        assert!(packed.len() < 1024 * 1024);
        assert!(unpack(&packed).is_err());
        assert_eq!(unpack(b"[1,2]").unwrap(), b"[1,2]");
    }
}
