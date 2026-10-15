//! The API devices talk to (`/api/v1/...`). Apart from `hello` and `pair`,
//! every request must be signed by a paired device's key, within a few
//! minutes of the Hub's clock and with a nonce never seen before.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{FromRequest, Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::{Extension, Json};
use freesize_proto::keys::random_id;
use freesize_proto::pairing::code_hash;
use freesize_proto::signing::{
    DEVICE_HEADER, MAX_SKEW, NONCE_HEADER, SIGNATURE_HEADER, TIME_HEADER, verify_request,
};
use freesize_proto::{
    DeviceInfo, DeviceKey, Hello, HubAddress, PROTOCOL, PairRequest, PairResponse, Platform,
    PollRequest, PollResponse, PublicKey, Purpose, Report, RequestSignature, ScanReport, Signed,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, Via, audit};

/// Wakes a device's waiting poll when something arrives for it.
#[derive(Default)]
pub struct Wakers(Mutex<HashMap<String, Arc<Notify>>>);

impl Wakers {
    pub fn get(&self, device: &str) -> Arc<Notify> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(device.to_string())
            .or_default()
            .clone()
    }

    pub fn wake(&self, device: &str) {
        self.get(device).notify_one();
    }
}

/// `GET /api/v1/hello`.
pub async fn hello(State(state): State<AppState>) -> Json<Hello> {
    Json(hello_of(&state))
}

fn hello_of(state: &AppState) -> Hello {
    Hello {
        name: state.config.name.clone(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol: PROTOCOL,
        key: state.key.public(),
        ca: state
            .tls
            .as_ref()
            .and_then(|t| t.ca_pem())
            .map(String::from),
    }
}

/// For a device still on plain http while the Hub serves HTTPS: where to
/// go, signed. None without a public address to send it to.
fn move_for(state: &AppState, via: Option<&Via>) -> Option<Signed<HubAddress>> {
    let via = via?;
    if via.tls || state.tls.is_none() {
        return None;
    }
    let url = state.config.public_url()?;
    let address = HubAddress {
        url,
        ca: hello_of(state).ca,
        time: now(),
    };
    Some(Signed::sign(&address, &state.key, Purpose::Move))
}

/// `POST /api/v1/pair`: a new device with a one-time code.
pub async fn pair(
    State(state): State<AppState>,
    via: Option<Extension<Via>>,
    Json(request): Json<Signed<PairRequest>>,
) -> ApiResult<Json<Signed<PairResponse>>> {
    let peer = via.map(|v| v.peer.ip());
    if state.pair_limiter.blocked(peer) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too many wrong pairing codes; wait a minute",
        ));
    }
    let claimed = request
        .peek()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    // The device proves it holds the key it registers.
    let pairing = request
        .open(&claimed.key, Purpose::PairRequest)
        .map_err(|e| ApiError::bad_request(format!("pairing request: {e}")))?;
    if (now() - pairing.time).abs() > MAX_SKEW {
        return Err(ApiError::bad_request(
            "the device's clock is off by more than five minutes",
        ));
    }
    let info = clean_info(pairing.info.clone());
    let hash = code_hash(&pairing.code);
    let key = pairing.key;
    let result = state
        .db
        .call(move |c| -> rusqlite::Result<Result<String, &'static str>> {
            let tx = c.transaction()?;
            let group: Option<i64> = tx
                .query_row(
                    "SELECT group_id FROM pairing_codes
                     WHERE hash = ?1 AND used_by IS NULL AND expires > ?2",
                    params![hash, now()],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(group) = group else {
                return Ok(Err("wrong or expired pairing code"));
            };
            let known: Option<i64> = tx
                .query_row(
                    "SELECT removed FROM devices WHERE key = ?1",
                    [key.to_base64()],
                    |r| r.get(0),
                )
                .optional()?;
            if known == Some(0) {
                return Ok(Err("this device is already paired"));
            }
            // A device removed earlier pairs again under a new id.
            tx.execute("DELETE FROM devices WHERE key = ?1", [key.to_base64()])?;
            let id = random_id::<10>();
            tx.execute(
                "INSERT INTO devices (id, name, platform, key, fingerprint, group_id, info,
                                      paired, last_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                params![
                    id,
                    info.name,
                    platform_id(info.platform),
                    key.to_base64(),
                    key.fingerprint(),
                    group,
                    serde_json::to_string(&info).unwrap_or_default(),
                    now(),
                ],
            )?;
            tx.execute(
                "UPDATE pairing_codes SET used_by = ?1 WHERE hash = ?2",
                params![id, hash],
            )?;
            audit::append(
                &tx,
                &info.name,
                "paired",
                Some(&id),
                &json!({"platform": info.platform.label(), "fingerprint": key.fingerprint(),
                        "app": info.app}),
            )?;
            tx.commit()?;
            Ok(Ok(id))
        })
        .await?;
    let id = match result {
        Ok(id) => id,
        Err(why) => {
            state.pair_limiter.record(peer);
            return Err(ApiError::forbidden(why));
        }
    };
    tracing::info!("paired {} as {id}", pairing.info.name);
    let response = PairResponse {
        device: id,
        hub: hello_of(&state),
        device_key: key,
    };
    Ok(Json(Signed::sign(
        &response,
        &state.key,
        Purpose::PairResponse,
    )))
}

/// Keeps what a device says about itself within sane lengths.
fn clean_info(mut info: DeviceInfo) -> DeviceInfo {
    let clip = |s: &mut String, max: usize| {
        if s.chars().count() > max {
            *s = s.chars().take(max).collect();
        }
    };
    info.name = info.name.trim().to_string();
    if info.name.is_empty() {
        info.name = "Unnamed device".into();
    }
    clip(&mut info.name, 80);
    clip(&mut info.os, 80);
    clip(&mut info.hostname, 120);
    clip(&mut info.app, 80);
    clip(&mut info.rights, 20);
    info.grants.truncate(50);
    info
}

pub fn platform_id(platform: Platform) -> &'static str {
    match platform {
        Platform::Android => "android",
        Platform::Windows => "windows",
        Platform::Macos => "macos",
        Platform::Linux => "linux",
        Platform::Nextcloud => "nextcloud",
        Platform::Other => "other",
    }
}

/// A paired device, as a signed request found it.
#[derive(Clone, Debug)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub fingerprint: String,
    pub group_id: i64,
    pub info: DeviceInfo,
}

/// A request signed by a paired device, with its JSON body.
pub struct DeviceRequest<T> {
    pub device: Device,
    pub body: T,
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(String::from)
}

impl<T: DeserializeOwned + Send> FromRequest<AppState> for DeviceRequest<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &AppState) -> Result<Self, ApiError> {
        let (parts, body) = request.into_parts();
        let unsigned = || ApiError::unauthorized("missing request signature");
        let signature = RequestSignature {
            device: header(&parts.headers, DEVICE_HEADER).ok_or_else(unsigned)?,
            time: header(&parts.headers, TIME_HEADER)
                .and_then(|t| t.parse().ok())
                .ok_or_else(unsigned)?,
            nonce: header(&parts.headers, NONCE_HEADER).ok_or_else(unsigned)?,
            signature: header(&parts.headers, SIGNATURE_HEADER).ok_or_else(unsigned)?,
        };
        if (now() - signature.time).abs() > MAX_SKEW {
            return Err(ApiError::unauthorized(
                "the device's clock is off by more than five minutes",
            ));
        }
        if signature.nonce.len() > 64 {
            return Err(unsigned());
        }
        let bytes = Bytes::from_request(Request::from_parts(parts.clone(), body), state)
            .await
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        let path = parts
            .uri
            .path_and_query()
            .map_or_else(|| parts.uri.path().to_string(), |p| p.as_str().to_string());
        let method = parts.method.as_str().to_string();
        let sig = signature.clone();
        let signed_bytes = bytes.clone();
        let device = state
            .db
            .call(move |c| -> ApiResult<Device> {
                let row = c
                    .query_row(
                        "SELECT id, name, key, fingerprint, group_id, info, removed
                         FROM devices WHERE id = ?1 AND agentless = 0",
                        [&sig.device],
                        |r| {
                            Ok((
                                Device {
                                    id: r.get(0)?,
                                    name: r.get(1)?,
                                    fingerprint: r.get(3)?,
                                    group_id: r.get(4)?,
                                    info: serde_json::from_str(&r.get::<_, String>(5)?)
                                        .unwrap_or_default(),
                                },
                                r.get::<_, String>(2)?,
                                r.get::<_, bool>(6)?,
                            ))
                        },
                    )
                    .optional()?;
                let Some((device, key, removed)) = row else {
                    return Err(ApiError::unauthorized("unknown device"));
                };
                let key = PublicKey::from_base64(&key)
                    .ok_or_else(|| ApiError::unauthorized("unknown device"))?;
                if !verify_request(&key, &method, &path, &signed_bytes, &sig) {
                    return Err(ApiError::unauthorized("bad request signature"));
                }
                if removed {
                    let mut err = ApiError::forbidden("this device was removed from the Hub");
                    err.removed = true;
                    return Err(err);
                }
                c.execute("DELETE FROM nonces WHERE time < ?1", [now() - 2 * MAX_SKEW])?;
                let fresh = c.execute(
                    "INSERT OR IGNORE INTO nonces (device_id, nonce, time) VALUES (?1, ?2, ?3)",
                    params![device.id, sig.nonce, sig.time],
                )?;
                if fresh == 0 {
                    return Err(ApiError::unauthorized("this request was already sent once"));
                }
                c.execute(
                    "UPDATE devices SET last_seen = ?1 WHERE id = ?2",
                    params![now(), device.id],
                )?;
                Ok(device)
            })
            .await?;
        // No body (a GET) reads as null.
        let text: &[u8] = if bytes.is_empty() { b"null" } else { &bytes };
        let body =
            serde_json::from_slice(text).map_err(|e| ApiError::bad_request(e.to_string()))?;
        Ok(DeviceRequest { device, body })
    }
}

/// Most files kept per scan.
const MAX_FILES: usize = 1000;
/// Most folders kept per scan.
const MAX_FOLDERS: usize = 20_000;
/// Most unreadable folders named per scan.
const MAX_UNREADABLE: usize = 100;

/// `POST /api/v1/report`.
pub async fn report(
    State(state): State<AppState>,
    request: DeviceRequest<Report>,
) -> ApiResult<Json<Value>> {
    let DeviceRequest { device, body } = request;
    state
        .db
        .call(move |c| store_report(c, &device, body))
        .await?;
    Ok(Json(json!({"ok": true})))
}

pub fn store_report(c: &mut Connection, device: &Device, report: Report) -> rusqlite::Result<()> {
    let tx = c.transaction()?;
    let time = now();
    if let Some(info) = report.info {
        // The name stays as paired, or as an admin renamed it.
        let info = clean_info(info);
        if info.approver != device.info.approver {
            audit::append(
                &tx,
                &device.name,
                if info.approver.is_some() {
                    "approver set on the device"
                } else {
                    "approver removed on the device"
                },
                Some(&device.id),
                &json!({"approver": info.approver}),
            )?;
        }
        tx.execute(
            "UPDATE devices SET platform = ?1, info = ?2 WHERE id = ?3",
            params![
                platform_id(info.platform),
                serde_json::to_string(&info).unwrap_or_default(),
                device.id
            ],
        )?;
    }
    if let Some(disks) = report.disks {
        tx.execute("DELETE FROM disks WHERE device_id = ?1", [&device.id])?;
        for disk in disks.iter().take(64) {
            tx.execute(
                "INSERT OR REPLACE INTO disks (device_id, path, name, total, free, updated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    device.id,
                    disk.path,
                    disk.name,
                    disk.total as i64,
                    disk.free as i64,
                    time
                ],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO disk_samples (device_id, path, day, total, used)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    device.id,
                    disk.path,
                    time / 86_400,
                    disk.total as i64,
                    disk.total.saturating_sub(disk.free) as i64
                ],
            )?;
        }
    }
    if let Some(scan) = report.scan {
        store_scan(&tx, &device.id, scan)?;
    }
    if let Some(apps) = report.apps {
        tx.execute(
            "INSERT OR REPLACE INTO apps (device_id, taken, report) VALUES (?1, ?2, ?3)",
            params![
                device.id,
                apps.taken,
                serde_json::to_string(&apps).unwrap_or_default()
            ],
        )?;
    }
    if let Some(batches) = report.quarantine {
        tx.execute(
            "INSERT OR REPLACE INTO quarantine (device_id, updated, batches) VALUES (?1, ?2, ?3)",
            params![
                device.id,
                time,
                serde_json::to_string(&batches).unwrap_or_default()
            ],
        )?;
    }
    if let Some(junk) = report.junk {
        tx.execute(
            "INSERT OR REPLACE INTO junk (device_id, taken, report) VALUES (?1, ?2, ?3)",
            params![
                device.id,
                junk.taken,
                serde_json::to_string(&junk).unwrap_or_default()
            ],
        )?;
    }
    if let Some(sync) = report.sync {
        tx.execute(
            "INSERT OR REPLACE INTO sync (device_id, taken, report) VALUES (?1, ?2, ?3)",
            params![
                device.id,
                sync.taken,
                serde_json::to_string(&sync).unwrap_or_default()
            ],
        )?;
    }
    if let Some(cloud) = report.cloud {
        tx.execute(
            "INSERT OR REPLACE INTO cloud (device_id, taken, report) VALUES (?1, ?2, ?3)",
            params![
                device.id,
                cloud.taken,
                serde_json::to_string(&cloud).unwrap_or_default()
            ],
        )?;
    }
    if let Some(unraid) = report.unraid {
        tx.execute(
            "INSERT OR REPLACE INTO unraid (device_id, taken, report) VALUES (?1, ?2, ?3)",
            params![
                device.id,
                unraid.taken,
                serde_json::to_string(&unraid).unwrap_or_default()
            ],
        )?;
    }
    tx.commit()
}

pub fn store_scan(c: &Connection, device: &str, mut scan: ScanReport) -> rusqlite::Result<i64> {
    scan.folders.truncate(MAX_FOLDERS);
    scan.largest.truncate(MAX_FILES);
    scan.unreadable.truncate(MAX_UNREADABLE);
    c.execute(
        "INSERT INTO scans (device_id, root, label, taken, size, allocated, files, dirs, errors,
                            complete, folders, types, received, unreadable)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            device,
            scan.root,
            scan.label,
            scan.taken,
            scan.size as i64,
            scan.allocated as i64,
            scan.files as i64,
            scan.dirs as i64,
            scan.errors as i64,
            scan.complete,
            gzip_json(&scan.folders),
            serde_json::to_string(&scan.types).unwrap_or_default(),
            now(),
            serde_json::to_string(&scan.unreadable).unwrap_or_default(),
        ],
    )?;
    let id = c.last_insert_rowid();
    let mut insert = c.prepare(
        "INSERT INTO scan_files (scan_id, path, size, allocated, modified) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for file in &scan.largest {
        insert.execute(params![
            id,
            file.path,
            file.size as i64,
            file.allocated as i64,
            file.modified
        ])?;
    }
    crate::history::prune(c, device, &scan.root)?;
    Ok(id)
}

pub fn gzip_json<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = serde_json::to_writer(&mut encoder, value);
    encoder.finish().unwrap_or_default()
}

pub fn gunzip_json<T: DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    let mut text = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut text)
        .ok()?;
    serde_json::from_slice(&text).ok()
}

/// Longest a poll is held open.
const MAX_WAIT: u32 = 60;

/// `POST /api/v1/poll`: what's waiting for the device, held open up to
/// `wait` seconds until something is.
pub async fn poll(
    State(state): State<AppState>,
    via: Option<Extension<Via>>,
    request: DeviceRequest<PollRequest>,
) -> ApiResult<Json<PollResponse>> {
    let move_to = move_for(&state, via.as_ref().map(|v| &v.0));
    let device = request.device;
    let wait = Duration::from_secs(u64::from(request.body.wait.min(MAX_WAIT)));
    let deadline = tokio::time::Instant::now() + wait;
    let waker = state.wakers.get(&device.id);
    let mut resend = request.body.resend;
    loop {
        let notified = waker.notified();
        let mut pending = crate::commands::pending_for(&state, &device, resend).await?;
        resend = false;
        let empty = pending.commands.is_empty()
            && pending.approvals.is_empty()
            && pending.decisions.is_empty()
            && pending.cancelled.is_empty();
        if !empty || tokio::time::Instant::now() >= deadline || move_to.is_some() {
            pending.move_to = move_to;
            pending.latest = state.latest.for_version(&device.info.app);
            return Ok(Json(pending));
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return Ok(Json(PollResponse {
                latest: state.latest.for_version(&device.info.app),
                ..PollResponse::default()
            }));
        }
    }
}

/// `GET /api/v1/devices/{fingerprint}`: another device's key, for choosing
/// it as approver. The device checks the fingerprint matches the key.
pub async fn device_key(
    State(state): State<AppState>,
    Path(fingerprint): Path<String>,
    _request: DeviceRequest<()>,
) -> ApiResult<Json<DeviceKey>> {
    let fingerprint = fingerprint.to_uppercase();
    let found = state
        .db
        .call(move |c| {
            c.query_row(
                "SELECT id, name, platform, key FROM devices
                 WHERE fingerprint = ?1 AND removed = 0 AND agentless = 0",
                [fingerprint],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
        })
        .await?;
    let (id, name, platform, key) =
        found.ok_or_else(|| ApiError::not_found("no paired device has that fingerprint"))?;
    Ok(Json(DeviceKey {
        id,
        name,
        platform: serde_json::from_value(Value::String(platform)).unwrap_or_default(),
        key: PublicKey::from_base64(&key)
            .ok_or_else(|| ApiError::not_found("no paired device has that fingerprint"))?,
    }))
}
