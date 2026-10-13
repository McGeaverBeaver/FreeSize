//! Housekeeping once a day: scans and disk samples older than the retention
//! setting go, and the audit log's head is written to the Hub's log, so the
//! chain has an anchor outside the database (`/api/audit/verify` checks the
//! chain; the logged head shows whether its end was cut off).

use std::time::Duration;

use rusqlite::Connection;

use crate::AppState;
use crate::audit;
use crate::db::now;

const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Deletes scans and disk samples taken more than `days` days ago; 0 keeps
/// them for good (the scan schedule in `history.rs` still thins them).
/// Returns how many scans and samples went.
pub fn prune(c: &Connection, days: u32) -> rusqlite::Result<(usize, usize)> {
    if days == 0 {
        return Ok((0, 0));
    }
    let cutoff = now() - i64::from(days) * 86_400;
    let scans = c.execute("DELETE FROM scans WHERE taken < ?1", [cutoff])?;
    let samples = c.execute(
        "DELETE FROM disk_samples WHERE day < ?1",
        [cutoff.div_euclid(86_400)],
    )?;
    Ok((scans, samples))
}

/// What the daily run logs.
pub fn once(c: &Connection, days: u32) -> rusqlite::Result<String> {
    let (scans, samples) = prune(c, days)?;
    let head = audit::head(c)?.map_or("empty".to_string(), |(id, hash)| format!("#{id} {hash}"));
    Ok(format!(
        "audit log head {head}; retention {}{}",
        if days == 0 {
            "unlimited".to_string()
        } else {
            format!("{days} days")
        },
        if scans + samples > 0 {
            format!(", removed {scans} scans and {samples} disk samples")
        } else {
            String::new()
        }
    ))
}

/// Runs at start and then once a day, for as long as the Hub runs.
pub async fn run(state: AppState) {
    let mut every = tokio::time::interval(EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let days = state.config.retention_days;
        match state.db.call(move |c| once(c, days)).await {
            Ok(line) => tracing::info!("{line}"),
            Err(err) => tracing::warn!("daily housekeeping failed: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn old_scans_go_only_with_a_retention_set() {
        let db = Db::memory().unwrap();
        db.now(|c| {
            let add = |taken: i64| {
                c.execute(
                    "INSERT INTO scans (device_id, root, label, taken, size, allocated, files, dirs,
                                        errors, complete, folders, types, received)
                     VALUES ('D', '/', '', ?1, 0, 0, 0, 0, 0, 1, x'', '[]', 0)",
                    [taken],
                )
                .unwrap();
            };
            add(now() - 400 * 86_400);
            add(now() - 10 * 86_400);
            c.execute(
                "INSERT INTO disk_samples (device_id, path, day, total, used)
                 VALUES ('D', '/', ?1, 1, 1)",
                [now() / 86_400 - 400],
            )
            .unwrap();
            assert_eq!(prune(c, 0).unwrap(), (0, 0));
            assert_eq!(prune(c, 365).unwrap(), (1, 1));
            let left: i64 = c
                .query_row("SELECT count(*) FROM scans", [], |r| r.get(0))
                .unwrap();
            assert_eq!(left, 1);
            audit::append(c, "test", "did a thing", None, &serde_json::json!({})).unwrap();
            let line = once(c, 365).unwrap();
            assert!(line.starts_with("audit log head #1 "), "{line}");
            assert!(line.contains("retention 365 days"));
        });
    }
}
