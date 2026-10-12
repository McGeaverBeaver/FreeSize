//! How long scans are kept: every scan of the last week, then one a day for
//! three months, one a week for a year, and one a month after that. Each
//! scan keeps its folder sizes and biggest files, so any two kept scans can
//! be compared.

use rusqlite::{Connection, params};

use crate::db::now;

const DAY: i64 = 86_400;

/// The bucket a scan of age `age` falls in; one scan per bucket is kept.
fn bucket(taken: i64, age: i64) -> (u8, i64) {
    if age < 7 * DAY {
        (0, taken)
    } else if age < 90 * DAY {
        (1, taken.div_euclid(DAY))
    } else if age < 365 * DAY {
        (2, taken.div_euclid(7 * DAY))
    } else {
        (3, taken.div_euclid(30 * DAY))
    }
}

/// Deletes scans of `device`'s `root` that the schedule doesn't keep.
pub fn prune(c: &Connection, device: &str, root: &str) -> rusqlite::Result<usize> {
    let now = now();
    let mut stmt = c.prepare(
        "SELECT id, taken FROM scans WHERE device_id = ?1 AND root = ?2 ORDER BY taken DESC, id DESC",
    )?;
    let scans: Vec<(i64, i64)> = stmt
        .query_map(params![device, root], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut seen = std::collections::HashSet::new();
    let mut removed = 0;
    for (id, taken) in scans {
        // Newest first, so the first scan of each bucket stays.
        if !seen.insert(bucket(taken, now - taken)) {
            removed += c.execute("DELETE FROM scans WHERE id = ?1", [id])?;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn keeps_fewer_scans_as_they_age() {
        let db = Db::memory().unwrap();
        db.now(|c| {
            let now = now();
            let add = |taken: i64| {
                c.execute(
                    "INSERT INTO scans (device_id, root, label, taken, size, allocated, files, dirs,
                                        errors, complete, folders, types, received)
                     VALUES ('D', '/', '', ?1, 0, 0, 0, 0, 0, 1, x'', '[]', 0)",
                    [taken],
                )
                .unwrap();
            };
            // Two scans today, three on one day a month ago, two in one week
            // half a year ago, two in one month two years ago.
            for t in [now - 60, now - 120] {
                add(t);
            }
            let month_ago = (now - 30 * DAY).div_euclid(DAY) * DAY;
            for t in [month_ago + 100, month_ago + 200, month_ago + 300] {
                add(t);
            }
            let half_year = (now - 180 * DAY).div_euclid(7 * DAY) * 7 * DAY;
            for t in [half_year + 100, half_year + DAY] {
                add(t);
            }
            let old = (now - 730 * DAY).div_euclid(30 * DAY) * 30 * DAY;
            for t in [old + 100, old + 5 * DAY] {
                add(t);
            }
            assert_eq!(prune(c, "D", "/").unwrap(), 4);
            let kept: Vec<i64> = c
                .prepare("SELECT taken FROM scans ORDER BY taken DESC")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            assert_eq!(
                kept,
                [
                    now - 60,
                    now - 120,
                    month_ago + 300,
                    half_year + DAY,
                    old + 5 * DAY
                ]
            );
        });
    }
}
