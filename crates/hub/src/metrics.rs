//! Metrics for Prometheus (`/metrics`) and the same as JSON for Home
//! Assistant's REST sensors (`/metrics/json`), behind the bearer token set
//! in `FREESIZE_METRICS_TOKEN`. Without the token they're off.

use std::fmt::Write;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;

use crate::AppState;
use crate::dashboard::{ScanRow, latest_scans};
use crate::db::now;

/// A device counts as online if it was heard from this recently (as on the
/// dashboard).
const ONLINE_SECONDS: i64 = 120;

#[derive(Serialize)]
struct DiskMetric {
    name: String,
    path: String,
    total: i64,
    free: i64,
    used: i64,
    used_percent: f64,
}

#[derive(Serialize)]
struct DeviceMetric {
    id: String,
    name: String,
    platform: String,
    group: String,
    last_seen: i64,
    online: bool,
    agentless: bool,
    disks: Vec<DiskMetric>,
    scans: Vec<ScanRow>,
}

struct Snapshot {
    devices: Vec<DeviceMetric>,
    requests: Vec<(String, i64)>,
    alerts: Vec<(String, String, i64)>,
}

fn gather(c: &Connection) -> rusqlite::Result<Snapshot> {
    let now = now();
    let mut stmt = c.prepare(
        "SELECT d.id, d.name, d.platform, g.name, d.last_seen, d.agentless FROM devices d
         JOIN device_groups g ON g.id = d.group_id WHERE d.removed = 0
         ORDER BY d.name COLLATE NOCASE",
    )?;
    let mut devices: Vec<DeviceMetric> = stmt
        .query_map([], |r| {
            let last_seen: i64 = r.get(4)?;
            let agentless: bool = r.get(5)?;
            Ok(DeviceMetric {
                id: r.get(0)?,
                name: r.get(1)?,
                platform: r.get(2)?,
                group: r.get(3)?,
                last_seen,
                online: !agentless && now - last_seen < ONLINE_SECONDS,
                agentless,
                disks: Vec::new(),
                scans: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut disks =
        c.prepare("SELECT name, path, total, free FROM disks WHERE device_id = ?1 ORDER BY path")?;
    for device in &mut devices {
        device.disks = disks
            .query_map([&device.id], |r| {
                let total: i64 = r.get(2)?;
                let free: i64 = r.get(3)?;
                let used = (total - free).max(0);
                Ok(DiskMetric {
                    name: r.get(0)?,
                    path: r.get(1)?,
                    total,
                    free,
                    used,
                    used_percent: if total > 0 {
                        (used as f64 * 1000.0 / total as f64).round() / 10.0
                    } else {
                        0.0
                    },
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        device.scans = latest_scans(c, &device.id)?;
    }
    let mut stmt =
        c.prepare("SELECT state, count(*) FROM commands GROUP BY state ORDER BY state")?;
    let requests = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut stmt = c.prepare(
        "SELECT kind, severity, count(*) FROM alerts WHERE resolved IS NULL
         GROUP BY kind, severity ORDER BY kind, severity",
    )?;
    let alerts = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Snapshot {
        devices,
        requests,
        alerts,
    })
}

/// Compares without giving away how much matched.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Why the request is turned away, if it doesn't carry the metrics token.
fn refused(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    let Some(token) = state
        .config
        .metrics_token
        .as_deref()
        .filter(|t| !t.is_empty())
    else {
        return Some(
            (
                StatusCode::NOT_FOUND,
                "Metrics are off. Set FREESIZE_METRICS_TOKEN to turn them on.\n",
            )
                .into_response(),
        );
    };
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if same(given.trim().as_bytes(), token.as_bytes()) {
        return None;
    }
    let mut response = (StatusCode::UNAUTHORIZED, "Wrong or missing token.\n").into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"FreeSize Hub metrics\""),
    );
    Some(response)
}

/// A label value in Prometheus's text format.
fn label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

struct Family<'a> {
    out: &'a mut String,
}

impl Family<'_> {
    fn head(&mut self, name: &str, help: &str) {
        let _ = writeln!(self.out, "# HELP {name} {help}\n# TYPE {name} gauge");
    }

    fn line(&mut self, name: &str, labels: &[(&str, &str)], value: impl std::fmt::Display) {
        let labels: Vec<String> = labels
            .iter()
            .map(|(k, v)| format!("{k}=\"{}\"", label(v)))
            .collect();
        let _ = writeln!(self.out, "{name}{{{}}} {value}", labels.join(","));
    }
}

fn prometheus_text(hub: &str, snapshot: &Snapshot) -> String {
    let mut out = String::new();
    let mut f = Family { out: &mut out };
    f.head("disktree_hub_info", "The Hub's name and version.");
    f.line(
        "disktree_hub_info",
        &[("name", hub), ("version", env!("CARGO_PKG_VERSION"))],
        1,
    );
    let device_labels = |d: &DeviceMetric| -> [(&'static str, String); 4] {
        [
            ("device", d.id.clone()),
            ("name", d.name.clone()),
            ("platform", d.platform.clone()),
            ("group", d.group.clone()),
        ]
    };
    f.head(
        "disktree_device_last_seen_timestamp_seconds",
        "When the device last called the Hub (or a scan was uploaded).",
    );
    for d in &snapshot.devices {
        let labels = device_labels(d);
        let refs: Vec<(&str, &str)> = labels.iter().map(|(k, v)| (*k, v.as_str())).collect();
        f.line(
            "disktree_device_last_seen_timestamp_seconds",
            &refs,
            d.last_seen,
        );
    }
    f.head(
        "disktree_device_online",
        "1 if the device called in the last two minutes.",
    );
    for d in &snapshot.devices {
        let labels = device_labels(d);
        let refs: Vec<(&str, &str)> = labels.iter().map(|(k, v)| (*k, v.as_str())).collect();
        f.line("disktree_device_online", &refs, u8::from(d.online));
    }
    for (metric, help, pick) in [
        (
            "disktree_disk_size_bytes",
            "Size of a disk the device reported.",
            (|d: &DiskMetric| d.total) as fn(&DiskMetric) -> i64,
        ),
        (
            "disktree_disk_free_bytes",
            "Free space on a disk.",
            |d: &DiskMetric| d.free,
        ),
        (
            "disktree_disk_used_bytes",
            "Space in use on a disk.",
            |d: &DiskMetric| d.used,
        ),
    ] {
        f.head(metric, help);
        for d in &snapshot.devices {
            for disk in &d.disks {
                f.line(
                    metric,
                    &[
                        ("device", &d.id),
                        ("name", &d.name),
                        ("disk", &disk.name),
                        ("path", &disk.path),
                    ],
                    pick(disk),
                );
            }
        }
    }
    for (metric, help, pick) in [
        (
            "disktree_scan_size_bytes",
            "Space on disk of a scanned folder, from its newest scan.",
            (|s: &ScanRow| s.allocated) as fn(&ScanRow) -> i64,
        ),
        (
            "disktree_scan_files",
            "Files in a scanned folder.",
            |s: &ScanRow| s.files,
        ),
        (
            "disktree_scan_timestamp_seconds",
            "When a folder was last scanned.",
            |s: &ScanRow| s.taken,
        ),
    ] {
        f.head(metric, help);
        for d in &snapshot.devices {
            for scan in &d.scans {
                f.line(
                    metric,
                    &[
                        ("device", &d.id),
                        ("name", &d.name),
                        ("root", &scan.root),
                        ("label", &scan.label),
                    ],
                    pick(scan),
                );
            }
        }
    }
    f.head("disktree_requests", "Requests to devices, by state.");
    for (state, n) in &snapshot.requests {
        f.line("disktree_requests", &[("state", state)], n);
    }
    f.head(
        "disktree_alerts_active",
        "Alerts on now, by kind and severity.",
    );
    for (kind, severity, n) in &snapshot.alerts {
        f.line(
            "disktree_alerts_active",
            &[("kind", kind), ("severity", severity)],
            n,
        );
    }
    out
}

/// `GET /metrics`: Prometheus's text format.
pub async fn prometheus(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(denied) = refused(&state, &headers) {
        return denied;
    }
    match state.db.call(|c| gather(c)).await {
        Ok(snapshot) => (
            [(
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            prometheus_text(&state.config.name, &snapshot),
        )
            .into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

/// `GET /metrics/json`: devices, disks and alert counts, for Home
/// Assistant's REST sensor and other scripts.
pub async fn summary(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(denied) = refused(&state, &headers) {
        return denied;
    }
    match state.db.call(|c| gather(c)).await {
        Ok(snapshot) => {
            let alerts: i64 = snapshot
                .alerts
                .iter()
                .filter(|(kind, _, _)| kind != "request")
                .map(|(_, _, n)| n)
                .sum();
            let waiting: i64 = snapshot
                .requests
                .iter()
                .filter(|(s, _)| {
                    ["queued", "delivered", "shown", "needs_approver"].contains(&s.as_str())
                })
                .map(|(_, n)| n)
                .sum();
            axum::Json(json!({
                "hub": state.config.name,
                "version": env!("CARGO_PKG_VERSION"),
                "alerts": alerts,
                "waiting_requests": waiting,
                "devices": snapshot.devices,
            }))
            .into_response()
        }
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_escaped() {
        assert_eq!(label(r#"Bob's "NAS"\x"#), r#"Bob's \"NAS\"\\x"#);
        assert_eq!(label("a\nb"), "a\\nb");
    }

    #[test]
    fn tokens_compare_whole() {
        assert!(same(b"secret", b"secret"));
        assert!(!same(b"secret", b"secreT"));
        assert!(!same(b"secret", b"secret2"));
        assert!(!same(b"", b"x"));
    }
}
