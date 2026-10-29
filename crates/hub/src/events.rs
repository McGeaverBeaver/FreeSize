//! What devices finished: a scan, a look for junk or at the apps, a pass
//! over Nextcloud. Each is a line on the device's Activity tab and the
//! newest of each kind shows on its card (0.21). Check-ins aren't events,
//! and a report that says the same as the last one isn't either.

use axum::Json;
use axum::extract::{Path, State};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;

use crate::AppState;
use crate::auth::User;
use crate::dashboard::find_device;
use crate::error::ApiResult;

/// The same job again within this many seconds adds to the last line
/// instead of a new one, so a Nextcloud app reporting every 5 minutes
/// doesn't bury the rest.
const FOLD_SECONDS: i64 = 3600;
/// Events are kept this long.
const KEEP_SECONDS: i64 = 365 * 86_400;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Event {
    pub id: i64,
    pub time: i64,
    pub kind: String,
    pub label: String,
    pub ok: bool,
    pub detail: String,
    /// How many times it happened in the hour this line covers.
    pub repeats: i64,
}

/// Records a finished job. Scans always get a line of their own; other
/// kinds fold into the last line of the same kind and label within the
/// hour.
pub fn record(
    c: &Connection,
    device: &str,
    kind: &str,
    label: &str,
    detail: &str,
    time: i64,
) -> rusqlite::Result<()> {
    if kind != "scan" {
        let last: Option<i64> = c
            .query_row(
                "SELECT id FROM events WHERE device_id = ?1 AND kind = ?2 AND label = ?3
                   AND ok = 1 AND time >= ?4 ORDER BY time DESC LIMIT 1",
                params![device, kind, label, time - FOLD_SECONDS],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = last {
            c.execute(
                "UPDATE events SET time = ?1, detail = ?2, repeats = repeats + 1 WHERE id = ?3",
                params![time, detail, id],
            )?;
            return Ok(());
        }
    }
    c.execute(
        "INSERT INTO events (device_id, time, kind, label, ok, detail) VALUES (?1, ?2, ?3, ?4, 1, ?5)",
        params![device, time, kind, label, detail],
    )?;
    c.execute(
        "DELETE FROM events WHERE device_id = ?1 AND time < ?2",
        params![device, time - KEEP_SECONDS],
    )?;
    Ok(())
}

/// Whether a report says something new: `new` differs from the report
/// stored before it once their `taken` times are left out. A device that
/// sends the same findings every hour makes no line.
pub fn changed(previous: Option<&str>, new: &Value) -> bool {
    let Some(previous) = previous.and_then(|p| serde_json::from_str::<Value>(p).ok()) else {
        return true;
    };
    let strip = |v: &Value| {
        let mut v = v.clone();
        if let Some(map) = v.as_object_mut() {
            map.remove("taken");
        }
        v
    };
    strip(&previous) != strip(new)
}

/// When each kind of job last finished on `device`, newest first.
pub fn latest(c: &Connection, device: &str) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut stmt = c.prepare(
        "SELECT kind, max(time) FROM events WHERE device_id = ?1 AND ok = 1
         GROUP BY kind ORDER BY 2 DESC",
    )?;
    let rows = stmt.query_map([device], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

pub fn list(c: &Connection, device: &str, limit: i64) -> rusqlite::Result<Vec<Event>> {
    let mut stmt = c.prepare(
        "SELECT id, time, kind, label, ok, detail, repeats FROM events WHERE device_id = ?1
         ORDER BY time DESC, id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![device, limit], |r| {
        Ok(Event {
            id: r.get(0)?,
            time: r.get(1)?,
            kind: r.get(2)?,
            label: r.get(3)?,
            ok: r.get(4)?,
            detail: r.get(5)?,
            repeats: r.get(6)?,
        })
    })?;
    rows.collect()
}

/// `GET /api/devices/{id}/events`: the jobs it finished, newest first.
pub async fn for_device(
    State(state): State<AppState>,
    user: User,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<Event>>> {
    let config = state.config.clone();
    let events = state
        .db
        .call(move |c| -> ApiResult<Vec<Event>> {
            find_device(c, &user, &config, &id)?;
            Ok(list(c, &id, 200)?)
        })
        .await?;
    Ok(Json(events))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use serde_json::json;

    #[test]
    fn jobs_fold_within_the_hour_but_scans_dont() {
        let db = Db::memory().unwrap();
        db.now(|c| {
            record(c, "d", "junk", "Junk checked", "1 GB", 1000).unwrap();
            record(c, "d", "junk", "Junk checked", "2 GB", 2000).unwrap();
            record(
                c,
                "d",
                "junk",
                "Junk checked",
                "3 GB",
                2000 + FOLD_SECONDS + 1,
            )
            .unwrap();
            record(c, "d", "scan", "Scanned C:", "", 3000).unwrap();
            record(c, "d", "scan", "Scanned C:", "", 3001).unwrap();
            let events = list(c, "d", 10).unwrap();
            let junk: Vec<_> = events.iter().filter(|e| e.kind == "junk").collect();
            assert_eq!(junk.len(), 2);
            assert_eq!(junk[1].repeats, 2);
            assert_eq!(junk[1].detail, "2 GB");
            assert_eq!(events.iter().filter(|e| e.kind == "scan").count(), 2);
            let latest = latest(c, "d").unwrap();
            assert_eq!(latest[0], ("junk".to_string(), 2000 + FOLD_SECONDS + 1));
        });
    }

    #[test]
    fn the_same_report_again_is_no_news() {
        let old = json!({"taken": 1, "junk": [{"kind": "trash", "bytes": 5}]}).to_string();
        assert!(!changed(
            Some(&old),
            &json!({"taken": 2, "junk": [{"kind": "trash", "bytes": 5}]})
        ));
        assert!(changed(
            Some(&old),
            &json!({"taken": 2, "junk": [{"kind": "trash", "bytes": 6}]})
        ));
        assert!(changed(None, &json!({"taken": 2})));
    }
}
