//! Alerts: disks filling up, fast growth, when a disk will be full at the
//! past month's rate, servers gone quiet, and requests left waiting for
//! someone to approve them. They're checked every minute and sent to ntfy,
//! Gotify, Pushover or a webhook when they start and when they're over.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::auth::{Admin, User};
use crate::dashboard::visible;
use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, audit};
use disktree_core::format_size;

/// How often alerts are checked.
pub const CHECK_EVERY: Duration = Duration::from_secs(60);
/// A request waits this long before anyone is nudged: an open app answers
/// sooner.
const NUDGE_AFTER: i64 = 120;
/// Over alerts are kept this long.
const KEEP_RESOLVED: i64 = 90 * 86_400;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rules {
    /// A disk at least this full, in percent; 0 turns it off.
    pub disk_full_percent: u32,
    /// A disk that grew by at least this share of its size in a week, in
    /// percent; 0 turns it off.
    pub growth_week_percent: u32,
    /// A disk that will be full within this many days at the past month's
    /// rate; 0 turns it off.
    pub forecast_days: u32,
    /// A server (a device nobody is at) not heard from in this many hours;
    /// 0 turns it off.
    pub offline_hours: u32,
    /// A nudge when a request waits for its device or approver.
    pub requests: bool,
}

impl Default for Rules {
    fn default() -> Rules {
        Rules {
            disk_full_percent: 90,
            growth_week_percent: 5,
            forecast_days: 30,
            offline_hours: 24,
            requests: true,
        }
    }
}

impl Rules {
    fn check(&self) -> ApiResult<()> {
        if self.disk_full_percent != 0 && !(50..=99).contains(&self.disk_full_percent) {
            return Err(ApiError::bad_request("a full disk is 50 to 99 percent"));
        }
        if self.growth_week_percent > 100 {
            return Err(ApiError::bad_request("growth is 1 to 100 percent a week"));
        }
        if self.forecast_days > 365 {
            return Err(ApiError::bad_request("forecasts reach at most 365 days"));
        }
        if self.offline_hours > 24 * 30 {
            return Err(ApiError::bad_request("quiet servers: at most 720 hours"));
        }
        Ok(())
    }
}

pub fn load_rules(c: &Connection) -> rusqlite::Result<Rules> {
    let text: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE key = 'alert_rules'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(text
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default())
}

/// Something wrong right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Firing {
    /// Stays the same while it lasts, e.g. `disk_full:DEVICE:/mnt/user`.
    pub key: String,
    pub kind: &'static str,
    pub device_id: Option<String>,
    /// info, warning or critical.
    pub severity: &'static str,
    pub title: String,
    pub message: String,
}

/// What alerts need from what a device says about itself.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Facts {
    attended: bool,
    approver: Option<String>,
}

struct DiskRow {
    device: String,
    device_name: String,
    path: String,
    name: String,
    total: i64,
    free: i64,
}

fn severity_rank(severity: &str) -> u8 {
    match severity {
        "critical" => 2,
        "warning" => 1,
        _ => 0,
    }
}

fn percent(used: i64, total: i64) -> f64 {
    if total <= 0 {
        0.0
    } else {
        used as f64 * 100.0 / total as f64
    }
}

/// Bytes a day, from a least-squares line through `(day, used)`.
fn slope(samples: &[(i64, i64)]) -> f64 {
    let n = samples.len() as f64;
    let mean_x = samples.iter().map(|s| s.0 as f64).sum::<f64>() / n;
    let mean_y = samples.iter().map(|s| s.1 as f64).sum::<f64>() / n;
    let (mut num, mut den) = (0.0, 0.0);
    for &(x, y) in samples {
        num += (x as f64 - mean_x) * (y as f64 - mean_y);
        den += (x as f64 - mean_x).powi(2);
    }
    if den == 0.0 { 0.0 } else { num / den }
}

/// What's wrong now. `active` are the keys of alerts already on, which stop
/// a disk hovering at the limit from going on and off.
pub fn evaluate(
    c: &Connection,
    rules: &Rules,
    active: &HashSet<String>,
    now: i64,
) -> rusqlite::Result<Vec<Firing>> {
    let mut firing = Vec::new();
    let mut stmt = c.prepare(
        "SELECT k.device_id, d.name, k.path, k.name, k.total, k.free FROM disks k
         JOIN devices d ON d.id = k.device_id WHERE d.removed = 0",
    )?;
    let disks: Vec<DiskRow> = stmt
        .query_map([], |r| {
            Ok(DiskRow {
                device: r.get(0)?,
                device_name: r.get(1)?,
                path: r.get(2)?,
                name: r.get(3)?,
                total: r.get(4)?,
                free: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let today = now / 86_400;
    for disk in &disks {
        if disk.total <= 0 {
            continue;
        }
        let used = (disk.total - disk.free).max(0);
        let full = percent(used, disk.total);
        let label = if disk.name.is_empty() {
            &disk.path
        } else {
            &disk.name
        };
        let mut is_full = false;
        if rules.disk_full_percent > 0 {
            let key = format!("disk_full:{}:{}", disk.device, disk.path);
            let limit = f64::from(rules.disk_full_percent);
            // Once on, it stays on until a point below the limit.
            let limit = if active.contains(&key) {
                limit - 1.0
            } else {
                limit
            };
            if full >= limit {
                is_full = true;
                firing.push(Firing {
                    key,
                    kind: "disk_full",
                    device_id: Some(disk.device.clone()),
                    severity: if full >= 98.0 { "critical" } else { "warning" },
                    title: format!("{}: {label} is {full:.0}% full", disk.device_name),
                    message: format!(
                        "{} free of {} on {}.",
                        format_size(disk.free.max(0) as u64),
                        format_size(disk.total as u64),
                        disk.path
                    ),
                });
            }
        }
        if rules.growth_week_percent == 0 && rules.forecast_days == 0 {
            continue;
        }
        let mut stmt = c.prepare_cached(
            "SELECT day, used FROM disk_samples WHERE device_id = ?1 AND path = ?2 AND day >= ?3
             ORDER BY day",
        )?;
        let samples: Vec<(i64, i64)> = stmt
            .query_map(params![disk.device, disk.path, today - 30], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        if rules.growth_week_percent > 0
            && let (Some(first), Some(last)) =
                (samples.iter().find(|s| s.0 >= today - 7), samples.last())
            && last.0 - first.0 >= 3
        {
            let grew = last.1 - first.1;
            if grew > 0
                && grew as f64 >= disk.total as f64 * f64::from(rules.growth_week_percent) / 100.0
            {
                firing.push(Firing {
                    key: format!("growth:{}:{}", disk.device, disk.path),
                    kind: "growth",
                    device_id: Some(disk.device.clone()),
                    severity: "warning",
                    title: format!(
                        "{}: {label} grew {} in {} days",
                        disk.device_name,
                        format_size(grew as u64),
                        last.0 - first.0
                    ),
                    message: format!(
                        "{} is {full:.0}% full now, with {} free.",
                        disk.path,
                        format_size(disk.free.max(0) as u64)
                    ),
                });
            }
        }
        // A full disk is already an alert.
        if rules.forecast_days > 0
            && !is_full
            && samples.len() >= 7
            && samples[samples.len() - 1].0 - samples[0].0 >= 7
        {
            let per_day = slope(&samples);
            if per_day > 0.0 {
                let days = disk.free.max(0) as f64 / per_day;
                if days <= f64::from(rules.forecast_days) {
                    firing.push(Firing {
                        key: format!("forecast:{}:{}", disk.device, disk.path),
                        kind: "forecast",
                        device_id: Some(disk.device.clone()),
                        severity: "warning",
                        title: format!(
                            "{}: {label} full in about {} days",
                            disk.device_name,
                            days.round().max(1.0)
                        ),
                        message: format!(
                            "It grew about {} a day over the past month; {} free now.",
                            format_size(per_day as u64),
                            format_size(disk.free.max(0) as u64)
                        ),
                    });
                }
            }
        }
    }

    if rules.offline_hours > 0 {
        let mut stmt = c.prepare(
            "SELECT id, name, info, last_seen FROM devices WHERE removed = 0 AND agentless = 0",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (id, name, info, last_seen) = row?;
            let info: Facts = serde_json::from_str(&info).unwrap_or_default();
            // Phones and laptops are off often; servers shouldn't be.
            if info.attended {
                continue;
            }
            let hours = (now - last_seen) / 3600;
            if hours >= i64::from(rules.offline_hours) {
                firing.push(Firing {
                    key: format!("offline:{id}"),
                    kind: "offline",
                    device_id: Some(id),
                    severity: "warning",
                    title: format!("{name} hasn't reported for {hours} hours"),
                    message: "Check that the machine is on and the DiskTree agent is running."
                        .into(),
                });
            }
        }
    }

    if rules.requests {
        let mut stmt = c.prepare(
            "SELECT m.id, m.device_id, d.name, d.info, m.summary, m.state FROM commands m
             JOIN devices d ON d.id = m.device_id
             WHERE m.state IN ('queued', 'delivered', 'needs_approver')
               AND m.created <= ?1 AND m.expires > ?2 AND d.removed = 0",
        )?;
        let rows = stmt.query_map(params![now - NUDGE_AFTER, now], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        for row in rows {
            let (id, device, name, info, summary, state) = row?;
            let info: Facts = serde_json::from_str(&info).unwrap_or_default();
            let approver: Option<String> = match (&info.approver, state.as_str()) {
                (Some(fingerprint), "needs_approver") => c
                    .query_row(
                        "SELECT name FROM devices WHERE fingerprint = ?1 AND removed = 0",
                        [fingerprint],
                        |r| r.get(0),
                    )
                    .optional()?,
                _ => None,
            };
            let where_ = approver.as_deref().unwrap_or(&name);
            firing.push(Firing {
                key: format!("request:{id}"),
                kind: "request",
                device_id: Some(device),
                severity: "info",
                title: format!("{name}: a request is waiting"),
                message: format!("{summary}. Open DiskTree on {where_} to approve or deny it."),
            });
        }
    }
    Ok(firing)
}

#[derive(Clone, Debug, Serialize)]
pub struct Alert {
    pub id: i64,
    pub key: String,
    pub kind: String,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub severity: String,
    pub title: String,
    pub message: String,
    pub opened: i64,
    pub resolved: Option<i64>,
}

const ALERT_COLUMNS: &str = "a.id, a.key, a.kind, a.device_id, d.name, a.severity, a.title, \
                             a.message, a.opened, a.resolved";

fn alert_row(r: &rusqlite::Row) -> rusqlite::Result<Alert> {
    Ok(Alert {
        id: r.get(0)?,
        key: r.get(1)?,
        kind: r.get(2)?,
        device_id: r.get(3)?,
        device_name: r.get(4)?,
        severity: r.get(5)?,
        title: r.get(6)?,
        message: r.get(7)?,
        opened: r.get(8)?,
        resolved: r.get(9)?,
    })
}

fn active_alerts(c: &Connection) -> rusqlite::Result<Vec<Alert>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {ALERT_COLUMNS} FROM alerts a LEFT JOIN devices d ON d.id = a.device_id
         WHERE a.resolved IS NULL ORDER BY a.opened DESC"
    ))?;
    let rows = stmt.query_map([], alert_row)?;
    rows.collect()
}

/// An alert to send: it started (or got worse), or it's over.
#[derive(Clone, Debug)]
pub struct Notice {
    pub alert: Alert,
    pub resolved: bool,
}

/// Records what's firing: new alerts open, gone ones close. Returns what to
/// send.
pub fn sync(c: &Connection, firing: Vec<Firing>, now: i64) -> rusqlite::Result<Vec<Notice>> {
    let active: HashMap<String, Alert> = active_alerts(c)?
        .into_iter()
        .map(|a| (a.key.clone(), a))
        .collect();
    let mut notices = Vec::new();
    let mut seen = HashSet::new();
    for fire in firing {
        seen.insert(fire.key.clone());
        match active.get(&fire.key) {
            Some(alert) => {
                c.execute(
                    "UPDATE alerts SET title = ?1, message = ?2, severity = ?3 WHERE id = ?4",
                    params![fire.title, fire.message, fire.severity, alert.id],
                )?;
                if severity_rank(fire.severity) > severity_rank(&alert.severity) {
                    let mut worse = alert.clone();
                    worse.severity = fire.severity.into();
                    worse.title = fire.title;
                    worse.message = fire.message;
                    notices.push(Notice {
                        alert: worse,
                        resolved: false,
                    });
                }
            }
            None => {
                c.execute(
                    "INSERT INTO alerts (key, kind, device_id, severity, title, message, opened)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        fire.key,
                        fire.kind,
                        fire.device_id,
                        fire.severity,
                        fire.title,
                        fire.message,
                        now
                    ],
                )?;
                let id = c.last_insert_rowid();
                let device_name = match &fire.device_id {
                    Some(device) => c
                        .query_row("SELECT name FROM devices WHERE id = ?1", [device], |r| {
                            r.get(0)
                        })
                        .optional()?,
                    None => None,
                };
                notices.push(Notice {
                    alert: Alert {
                        id,
                        key: fire.key,
                        kind: fire.kind.into(),
                        device_id: fire.device_id,
                        device_name,
                        severity: fire.severity.into(),
                        title: fire.title,
                        message: fire.message,
                        opened: now,
                        resolved: None,
                    },
                    resolved: false,
                });
            }
        }
    }
    for (key, mut alert) in active {
        if seen.contains(&key) {
            continue;
        }
        c.execute(
            "UPDATE alerts SET resolved = ?1 WHERE id = ?2",
            params![now, alert.id],
        )?;
        // A request that was answered needs no message.
        if alert.kind != "request" {
            alert.resolved = Some(now);
            notices.push(Notice {
                alert,
                resolved: true,
            });
        }
    }
    c.execute(
        "DELETE FROM alerts WHERE resolved IS NOT NULL AND resolved < ?1",
        [now - KEEP_RESOLVED],
    )?;
    Ok(notices)
}

// ----- Channels ------------------------------------------------------------

/// Where alerts go.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    /// ntfy.sh or a self-hosted ntfy server.
    Ntfy {
        server: String,
        topic: String,
        /// An access token, for protected topics.
        #[serde(default)]
        token: String,
    },
    Gotify {
        server: String,
        /// The application token.
        token: String,
    },
    Pushover {
        /// The user (or group) key.
        user: String,
        /// The application's API token.
        token: String,
    },
    /// A JSON POST, signed with HMAC-SHA256 when a secret is set.
    Webhook {
        url: String,
        #[serde(default)]
        secret: String,
    },
}

impl Target {
    fn secret_mut(&mut self) -> &mut String {
        match self {
            Target::Ntfy { token, .. }
            | Target::Gotify { token, .. }
            | Target::Pushover { token, .. } => token,
            Target::Webhook { secret, .. } => secret,
        }
    }

    /// The target as the dashboard sees it: the secret left out.
    fn masked(&self) -> (Target, bool) {
        let mut target = self.clone();
        let secret = target.secret_mut();
        let had = !secret.is_empty();
        secret.clear();
        (target, had)
    }

    fn check(&self) -> ApiResult<()> {
        let url = |text: &str| -> ApiResult<()> {
            let ok = (text.starts_with("https://") || text.starts_with("http://"))
                && text.len() <= 500
                && !text.contains(char::is_whitespace);
            if ok {
                Ok(())
            } else {
                Err(ApiError::bad_request(
                    "addresses start with https:// or http://",
                ))
            }
        };
        let filled = |text: &str, what: &str| -> ApiResult<()> {
            if text.trim().is_empty() || text.len() > 200 {
                Err(ApiError::bad_request(format!("fill in the {what}")))
            } else {
                Ok(())
            }
        };
        match self {
            Target::Ntfy { server, topic, .. } => {
                url(server)?;
                if topic.is_empty()
                    || topic.len() > 64
                    || !topic
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                {
                    return Err(ApiError::bad_request("topics are letters, digits, - and _"));
                }
                Ok(())
            }
            Target::Gotify { server, token } => {
                url(server)?;
                filled(token, "application token")
            }
            Target::Pushover { user, token } => {
                filled(user, "user key")?;
                filled(token, "API token")
            }
            Target::Webhook { url: address, .. } => url(address),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Channel {
    pub id: i64,
    pub name: String,
    pub target: Target,
    /// Whether a token or secret is stored (it's never sent back).
    pub has_secret: bool,
    pub enabled: bool,
    pub last_sent: Option<i64>,
    pub last_error: Option<String>,
}

fn channels(c: &Connection) -> rusqlite::Result<Vec<Channel>> {
    let mut stmt = c.prepare(
        "SELECT id, name, target, enabled, last_sent, last_error FROM alert_channels ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        let target: Target = serde_json::from_str(&r.get::<_, String>(2)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?;
        Ok(Channel {
            id: r.get(0)?,
            name: r.get(1)?,
            has_secret: false,
            target,
            enabled: r.get(3)?,
            last_sent: r.get(4)?,
            last_error: r.get(5)?,
        })
    })?;
    rows.collect()
}

fn masked(mut channel: Channel) -> Channel {
    let (target, had) = channel.target.masked();
    channel.target = target;
    channel.has_secret = had;
    channel
}

fn priority(severity: &str) -> u8 {
    match severity {
        "critical" => 5,
        "warning" => 4,
        _ => 3,
    }
}

/// HMAC-SHA256 (RFC 2104), for signing webhook bodies.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.map(|b| b ^ byte);
    let mut inner = Sha256::new();
    inner.update(pad(0x36));
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(pad(0x5c));
    outer.update(inner);
    outer.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What a notice says.
struct Message {
    event: &'static str,
    title: String,
    text: String,
    severity: String,
    kind: String,
    device: Option<(String, String)>,
    url: Option<String>,
}

fn message(notice: &Notice, hub: &str, hub_url: Option<&str>) -> Message {
    let alert = &notice.alert;
    let title = if notice.resolved {
        format!("Over: {}", alert.title)
    } else {
        alert.title.clone()
    };
    let link = match &alert.device_id {
        Some(device) => format!("#/device/{device}"),
        None => "#/alerts".into(),
    };
    Message {
        event: if notice.resolved {
            "resolved"
        } else {
            "opened"
        },
        title,
        text: if notice.resolved {
            format!("Back to normal. ({hub})")
        } else {
            format!("{} ({hub})", alert.message)
        },
        severity: if notice.resolved {
            "info".into()
        } else {
            alert.severity.clone()
        },
        kind: alert.kind.clone(),
        device: alert.device_id.clone().zip(alert.device_name.clone()),
        url: hub_url.map(|u| format!("{}/{link}", u.trim_end_matches('/'))),
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn checked(result: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<(), String> {
    let mut response = result.map_err(|e| e.to_string())?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response
        .body_mut()
        .read_to_string()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect::<String>();
    Err(format!("{status}: {}", body.trim()))
}

/// Sends one message. Blocking.
fn send(agent: &ureq::Agent, target: &Target, message: &Message) -> Result<(), String> {
    let priority = priority(&message.severity);
    match target {
        Target::Ntfy {
            server,
            topic,
            token,
        } => {
            // JSON publishing, so names in any language go through.
            let mut body = json!({
                "topic": topic,
                "title": message.title,
                "message": message.text,
                "priority": priority,
                "tags": [match message.severity.as_str() {
                    "critical" => "rotating_light",
                    "warning" => "warning",
                    _ => if message.event == "resolved" { "white_check_mark" } else { "floppy_disk" },
                }],
            });
            if let Some(url) = &message.url {
                body["click"] = json!(url);
            }
            let mut request = agent.post(server.trim_end_matches('/'));
            if !token.is_empty() {
                request = request.header("Authorization", &format!("Bearer {token}"));
            }
            checked(request.send_json(&body))
        }
        Target::Gotify { server, token } => {
            let mut body = json!({
                "title": message.title,
                "message": message.text,
                "priority": match priority { 5 => 8, 4 => 5, _ => 2 },
            });
            if let Some(url) = &message.url {
                body["extras"] = json!({"client::notification": {"click": {"url": url}}});
            }
            checked(
                agent
                    .post(&format!("{}/message", server.trim_end_matches('/')))
                    .header("X-Gotify-Key", token)
                    .send_json(&body),
            )
        }
        Target::Pushover { user, token } => {
            let priority = match priority {
                5 => "1",
                4 => "0",
                _ => "-1",
            };
            let mut form = vec![
                ("token", token.as_str()),
                ("user", user.as_str()),
                ("title", message.title.as_str()),
                ("message", message.text.as_str()),
                ("priority", priority),
            ];
            if let Some(url) = &message.url {
                form.push(("url", url.as_str()));
            }
            checked(
                agent
                    .post("https://api.pushover.net/1/messages.json")
                    .send_form(form),
            )
        }
        Target::Webhook { url, secret } => {
            let body = json!({
                "event": message.event,
                "kind": message.kind,
                "severity": message.severity,
                "title": message.title,
                "message": message.text,
                "device": message.device.as_ref().map(|(id, name)| json!({"id": id, "name": name})),
                "url": message.url,
                "time": now(),
            })
            .to_string();
            let mut request = agent.post(url).header("Content-Type", "application/json");
            if !secret.is_empty() {
                let signature = hex(&hmac_sha256(secret.as_bytes(), body.as_bytes()));
                request = request.header("X-DiskTree-Signature", &format!("sha256={signature}"));
            }
            checked(request.send(body.as_bytes()))
        }
    }
}

/// Sends `notices` to every enabled channel and notes how it went.
async fn deliver(state: &AppState, notices: Vec<Notice>) {
    if notices.is_empty() {
        return;
    }
    let Ok(list) = state.db.call(|c| channels(c)).await else {
        return;
    };
    let list: Vec<Channel> = list.into_iter().filter(|c| c.enabled).collect();
    if list.is_empty() {
        return;
    }
    let hub = state.config.name.clone();
    let hub_url = state.config.public_url();
    let results = tokio::task::spawn_blocking(move || {
        let agent = agent();
        let mut results = Vec::new();
        for channel in &list {
            let mut outcome = Ok(());
            for notice in &notices {
                let message = message(notice, &hub, hub_url.as_deref());
                if let Err(err) = send(&agent, &channel.target, &message) {
                    outcome = Err(err);
                }
            }
            results.push((channel.id, outcome));
        }
        results
    })
    .await
    .unwrap_or_default();
    let _ = state
        .db
        .call(move |c| -> rusqlite::Result<()> {
            for (id, outcome) in results {
                match outcome {
                    Ok(()) => c.execute(
                        "UPDATE alert_channels SET last_sent = ?1, last_error = NULL WHERE id = ?2",
                        params![now(), id],
                    )?,
                    Err(err) => {
                        tracing::warn!("couldn't send an alert to channel {id}: {err}");
                        c.execute(
                            "UPDATE alert_channels SET last_error = ?1 WHERE id = ?2",
                            params![err, id],
                        )?
                    }
                };
            }
            Ok(())
        })
        .await;
}

/// Checks once: records what's wrong and sends what changed.
pub async fn check_now(state: &AppState) -> Result<Vec<Notice>, String> {
    let notices = state
        .db
        .call(|c| -> rusqlite::Result<Vec<Notice>> {
            let rules = load_rules(c)?;
            let now = now();
            let active: HashSet<String> = active_alerts(c)?.into_iter().map(|a| a.key).collect();
            let firing = evaluate(c, &rules, &active, now)?;
            sync(c, firing, now)
        })
        .await
        .map_err(|e| e.to_string())?;
    deliver(state, notices.clone()).await;
    Ok(notices)
}

/// Checks every minute, for as long as the Hub runs.
pub async fn run(state: AppState) {
    let mut every = tokio::time::interval(CHECK_EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        if let Err(err) = check_now(&state).await {
            tracing::warn!("checking alerts failed: {err}");
        }
    }
}

// ----- API -------------------------------------------------------------------

/// `GET /api/alerts`: alerts on now and those over in the past 30 days, for
/// the devices the user may see.
pub async fn list(State(state): State<AppState>, user: User) -> ApiResult<Json<Value>> {
    let config = state.config.clone();
    let alerts = state
        .db
        .call(move |c| -> rusqlite::Result<Value> {
            let groups = visible(c, &user, &config)?;
            let mut stmt = c.prepare(&format!(
                "SELECT {ALERT_COLUMNS}, d.group_id FROM alerts a
                 LEFT JOIN devices d ON d.id = a.device_id
                 WHERE a.resolved IS NULL OR a.resolved > ?1
                 ORDER BY a.resolved IS NOT NULL, a.opened DESC LIMIT 300"
            ))?;
            let rows: Vec<(Alert, Option<i64>)> = stmt
                .query_map([now() - 30 * 86_400], |r| Ok((alert_row(r)?, r.get(10)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let (active, recent): (Vec<Alert>, Vec<Alert>) = rows
                .into_iter()
                .filter(|(_, group)| group.is_none_or(|g| groups.contains(&g)))
                .map(|(a, _)| a)
                .partition(|a| a.resolved.is_none());
            Ok(json!({"active": active, "recent": recent}))
        })
        .await?;
    Ok(Json(alerts))
}

/// `GET /api/alerts/rules`.
pub async fn get_rules(State(state): State<AppState>, _user: User) -> ApiResult<Json<Rules>> {
    Ok(Json(state.db.call(|c| load_rules(c)).await?))
}

/// `PUT /api/alerts/rules`.
pub async fn set_rules(
    State(state): State<AppState>,
    Admin(user): Admin,
    Json(rules): Json<Rules>,
) -> ApiResult<Json<Rules>> {
    rules.check()?;
    let saved = rules.clone();
    state
        .db
        .call(move |c| -> rusqlite::Result<()> {
            c.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('alert_rules', ?1)",
                [serde_json::to_string(&rules).unwrap_or_default()],
            )?;
            audit::append(
                c,
                &user.name,
                "alert rules changed",
                None,
                &serde_json::to_value(&rules).unwrap_or_default(),
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(saved))
}

/// `GET /api/alerts/channels`: where alerts go, without their secrets.
pub async fn list_channels(
    State(state): State<AppState>,
    Admin(_user): Admin,
) -> ApiResult<Json<Vec<Channel>>> {
    let list = state.db.call(|c| channels(c)).await?;
    Ok(Json(list.into_iter().map(masked).collect()))
}

#[derive(Deserialize)]
pub struct ChannelChange {
    name: Option<String>,
    /// For a change, an empty token or secret keeps the stored one.
    target: Option<Target>,
    enabled: Option<bool>,
}

fn channel_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(ApiError::bad_request("names are 1 to 60 characters"));
    }
    Ok(name.to_string())
}

/// `POST /api/alerts/channels`.
pub async fn add_channel(
    State(state): State<AppState>,
    Admin(user): Admin,
    Json(change): Json<ChannelChange>,
) -> ApiResult<Json<Value>> {
    let name = channel_name(change.name.as_deref().unwrap_or_default())?;
    let target = change
        .target
        .ok_or_else(|| ApiError::bad_request("choose where alerts go"))?;
    target.check()?;
    let id = state
        .db
        .call(move |c| -> rusqlite::Result<i64> {
            c.execute(
                "INSERT INTO alert_channels (name, target, enabled, created) VALUES (?1, ?2, 1, ?3)",
                params![name, serde_json::to_string(&target).unwrap_or_default(), now()],
            )?;
            let id = c.last_insert_rowid();
            let (shown, _) = target.masked();
            audit::append(
                c,
                &user.name,
                "alert channel added",
                None,
                &json!({"name": name, "target": shown}),
            )?;
            Ok(id)
        })
        .await?;
    Ok(Json(json!({"id": id})))
}

fn find_channel(c: &Connection, id: i64) -> ApiResult<Channel> {
    channels(c)?
        .into_iter()
        .find(|ch| ch.id == id)
        .ok_or_else(|| ApiError::not_found("no such channel"))
}

/// `PATCH /api/alerts/channels/{id}`.
pub async fn update_channel(
    State(state): State<AppState>,
    Admin(user): Admin,
    Path(id): Path<i64>,
    Json(change): Json<ChannelChange>,
) -> ApiResult<Json<Value>> {
    let name = change.name.as_deref().map(channel_name).transpose()?;
    state
        .db
        .call(move |c| -> ApiResult<()> {
            let mut channel = find_channel(c, id)?;
            if let Some(mut target) = change.target {
                let same_kind =
                    std::mem::discriminant(&target) == std::mem::discriminant(&channel.target);
                if same_kind && target.secret_mut().is_empty() {
                    *target.secret_mut() = channel.target.secret_mut().clone();
                }
                target.check()?;
                channel.target = target;
            }
            if let Some(name) = name {
                channel.name = name;
            }
            if let Some(enabled) = change.enabled {
                channel.enabled = enabled;
            }
            c.execute(
                "UPDATE alert_channels SET name = ?1, target = ?2, enabled = ?3 WHERE id = ?4",
                params![
                    channel.name,
                    serde_json::to_string(&channel.target).unwrap_or_default(),
                    channel.enabled,
                    id
                ],
            )?;
            let (shown, _) = channel.target.masked();
            audit::append(
                c,
                &user.name,
                "alert channel changed",
                None,
                &json!({"name": channel.name, "target": shown, "enabled": channel.enabled}),
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({"ok": true})))
}

/// `DELETE /api/alerts/channels/{id}`.
pub async fn remove_channel(
    State(state): State<AppState>,
    Admin(user): Admin,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    state
        .db
        .call(move |c| -> ApiResult<()> {
            let channel = find_channel(c, id)?;
            c.execute("DELETE FROM alert_channels WHERE id = ?1", [id])?;
            audit::append(
                c,
                &user.name,
                "alert channel removed",
                None,
                &json!({"name": channel.name}),
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({"ok": true})))
}

/// `POST /api/alerts/channels/{id}/test`: sends a test message now.
pub async fn test_channel(
    State(state): State<AppState>,
    Admin(user): Admin,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let channel = state.db.call(move |c| find_channel(c, id)).await?;
    let hub = state.config.name.clone();
    let hub_url = state.config.public_url();
    let notice = Notice {
        alert: Alert {
            id: 0,
            key: "test".into(),
            kind: "test".into(),
            device_id: None,
            device_name: None,
            severity: "info".into(),
            title: "DiskTree Hub test".into(),
            message: format!(
                "{} sent a test from the dashboard; alerts will look like this.",
                user.name
            ),
            opened: now(),
            resolved: None,
        },
        resolved: false,
    };
    let target = channel.target.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut message = message(&notice, &hub, hub_url.as_deref());
        message.event = "test";
        send(&agent(), &target, &message)
    })
    .await
    .map_err(|_| ApiError::internal("sending failed"))?;
    let error = result.as_ref().err().cloned();
    state
        .db
        .call(move |c| match &error {
            None => c.execute(
                "UPDATE alert_channels SET last_sent = ?1, last_error = NULL WHERE id = ?2",
                params![now(), id],
            ),
            Some(err) => c.execute(
                "UPDATE alert_channels SET last_error = ?1 WHERE id = ?2",
                params![err, id],
            ),
        })
        .await?;
    match result {
        Ok(()) => Ok(Json(json!({"ok": true}))),
        Err(err) => Err(ApiError::new(
            axum::http::StatusCode::BAD_GATEWAY,
            format!("{} didn't take it: {err}", channel.name),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn setup() -> Db {
        let db = Db::memory().unwrap();
        db.now(|c| {
            c.execute_batch(
                "INSERT INTO devices (id, name, platform, key, fingerprint, group_id, info, paired, last_seen)
                 VALUES ('nas', 'NAS', 'linux', 'k1', 'F1', 1, '{\"attended\":false}', 0, 0),
                        ('phone', 'Phone', 'android', 'k2', 'F2', 1, '{\"attended\":true}', 0, 0);",
            )
            .unwrap();
        });
        db
    }

    fn disk(db: &Db, device: &str, path: &str, total: i64, free: i64) {
        db.now(|c| {
            c.execute(
                "INSERT OR REPLACE INTO disks (device_id, path, name, total, free, updated)
                 VALUES (?1, ?2, ?2, ?3, ?4, 0)",
                params![device, path, total, free],
            )
            .unwrap();
        });
    }

    fn keys(firing: &[Firing]) -> Vec<&str> {
        let mut keys: Vec<&str> = firing.iter().map(|f| f.key.as_str()).collect();
        keys.sort();
        keys
    }

    #[test]
    fn full_disks_stay_on_until_clearly_below() {
        let db = setup();
        let rules = Rules {
            offline_hours: 0,
            ..Rules::default()
        };
        let now = 100 * 86_400;
        disk(&db, "nas", "/mnt/user", 1000, 96);
        let firing = db.now(|c| evaluate(c, &rules, &HashSet::new(), now).unwrap());
        assert_eq!(keys(&firing), ["disk_full:nas:/mnt/user"]);
        assert_eq!(firing[0].title, "NAS: /mnt/user is 90% full");
        assert_eq!(firing[0].severity, "warning");
        let notices = db.now(|c| sync(c, firing, now).unwrap());
        assert_eq!(notices.len(), 1);

        // Just under the limit: still on.
        disk(&db, "nas", "/mnt/user", 1000, 105);
        let active: HashSet<String> = ["disk_full:nas:/mnt/user".to_string()].into();
        let firing = db.now(|c| evaluate(c, &rules, &active, now).unwrap());
        assert_eq!(firing.len(), 1);
        assert!(db.now(|c| sync(c, firing, now).unwrap()).is_empty());

        // Worse: sent again as critical.
        disk(&db, "nas", "/mnt/user", 1000, 10);
        let firing = db.now(|c| evaluate(c, &rules, &active, now).unwrap());
        let notices = db.now(|c| sync(c, firing, now).unwrap());
        assert_eq!(notices[0].alert.severity, "critical");

        // Cleared: over, and said so.
        disk(&db, "nas", "/mnt/user", 1000, 500);
        let firing = db.now(|c| evaluate(c, &rules, &active, now).unwrap());
        assert!(firing.is_empty());
        let notices = db.now(|c| sync(c, firing, now + 60).unwrap());
        assert!(notices[0].resolved);
        assert!(db.now(|c| active_alerts(c).unwrap()).is_empty());
    }

    #[test]
    fn growth_and_forecast_from_samples() {
        let db = setup();
        let rules = Rules {
            disk_full_percent: 0,
            offline_hours: 0,
            ..Rules::default()
        };
        let today = 1000;
        let gb = 1_000_000_000i64;
        disk(&db, "nas", "/mnt/user", 1000 * gb, 300 * gb);
        db.now(|c| {
            for day in 0..=20 {
                // 10 GB a day.
                c.execute(
                    "INSERT INTO disk_samples (device_id, path, day, total, used)
                     VALUES ('nas', '/mnt/user', ?1, ?2, ?3)",
                    params![today - 20 + day, 1000 * gb, 500 * gb + day * 10 * gb],
                )
                .unwrap();
            }
        });
        let firing = db.now(|c| evaluate(c, &rules, &HashSet::new(), today * 86_400).unwrap());
        // 70 GB in 7 days is 7% of the disk; 300 GB free lasts 30 days.
        assert_eq!(
            keys(&firing),
            ["forecast:nas:/mnt/user", "growth:nas:/mnt/user"]
        );
        let forecast = firing.iter().find(|f| f.kind == "forecast").unwrap();
        assert_eq!(forecast.title, "NAS: /mnt/user full in about 30 days");
        let slower = Rules {
            growth_week_percent: 10,
            forecast_days: 20,
            ..rules
        };
        assert!(
            db.now(|c| evaluate(c, &slower, &HashSet::new(), today * 86_400).unwrap())
                .is_empty()
        );
    }

    #[test]
    fn quiet_servers_and_waiting_requests() {
        let db = setup();
        let rules = Rules {
            disk_full_percent: 0,
            ..Rules::default()
        };
        let now = 30 * 3600;
        db.now(|c| {
            c.execute_batch(
                "INSERT INTO commands (id, device_id, kind, summary, admin, admin_name, created,
                                       expires, signed, state, updated)
                 VALUES ('c1', 'phone', 'delete', 'Delete a.iso', 'u', 'Robert', 100, 999999, '{}', 'queued', 100),
                        ('c2', 'phone', 'delete', 'Delete b.iso', 'u', 'Robert', 107990, 999999, '{}', 'queued', 100),
                        ('c3', 'phone', 'delete', 'Delete c.iso', 'u', 'Robert', 100, 999999, '{}', 'shown', 100);",
            )
            .unwrap();
        });
        let firing = db.now(|c| evaluate(c, &rules, &HashSet::new(), now).unwrap());
        // The phone is often off; the NAS shouldn't be. A request that just
        // came, or that the device shows, needs no nudge.
        assert_eq!(keys(&firing), ["offline:nas", "request:c1"]);
        let request = firing.iter().find(|f| f.kind == "request").unwrap();
        assert_eq!(
            request.message,
            "Delete a.iso. Open DiskTree on Phone to approve or deny it."
        );
        db.now(|c| sync(c, firing, now).unwrap());
        // Answered: over, without a message.
        db.now(|c| {
            c.execute("UPDATE commands SET state = 'done' WHERE id = 'c1'", [])
                .unwrap()
        });
        let firing = db.now(|c| evaluate(c, &rules, &HashSet::new(), now).unwrap());
        let notices = db.now(|c| sync(c, firing, now).unwrap());
        assert!(notices.is_empty());
    }

    #[test]
    fn rules_are_checked() {
        assert!(Rules::default().check().is_ok());
        assert!(
            Rules {
                disk_full_percent: 30,
                ..Rules::default()
            }
            .check()
            .is_err()
        );
        assert!(
            Rules {
                disk_full_percent: 0,
                forecast_days: 0,
                ..Rules::default()
            }
            .check()
            .is_ok()
        );
    }

    #[test]
    fn secrets_stay_on_the_hub() {
        let target = Target::Ntfy {
            server: "https://ntfy.sh".into(),
            topic: "disks".into(),
            token: "tk_secret".into(),
        };
        let (shown, had) = target.masked();
        assert!(had);
        assert!(!serde_json::to_string(&shown).unwrap().contains("tk_secret"));
        assert!(target.check().is_ok());
        let bad = Target::Ntfy {
            server: "ftp://x".into(),
            topic: "a b".into(),
            token: String::new(),
        };
        assert!(bad.check().is_err());
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2.
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 6: a key longer than a block.
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }
}
