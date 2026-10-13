//! The audit log: who asked, who approved, what ran, the result. Entries are
//! only ever added. Each one carries the hash of the one before, so changing
//! or removing an entry breaks the chain from there on, which `verify` finds.

use freesize_proto::keys::sha256_hex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;

use crate::db::now;

/// The `prev` of the first entry.
const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub id: i64,
    pub time: i64,
    pub actor: String,
    pub action: String,
    pub device_id: Option<String>,
    pub details: Value,
    pub hash: String,
}

fn entry_hash(
    id: i64,
    time: i64,
    actor: &str,
    action: &str,
    device: Option<&str>,
    details: &str,
    prev: &str,
) -> String {
    sha256_hex(
        format!(
            "{id}\n{time}\n{actor}\n{action}\n{}\n{details}\n{prev}",
            device.unwrap_or("")
        )
        .as_bytes(),
    )
}

/// Adds an entry. `actor` is a person ("Robert (admin)") or a device.
pub fn append(
    conn: &Connection,
    actor: &str,
    action: &str,
    device: Option<&str>,
    details: &Value,
) -> rusqlite::Result<()> {
    let last: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, hash FROM audit ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (id, prev) = match last {
        Some((id, hash)) => (id + 1, hash),
        None => (1, GENESIS.to_string()),
    };
    let time = now();
    let details = details.to_string();
    let hash = entry_hash(id, time, actor, action, device, &details, &prev);
    conn.execute(
        "INSERT INTO audit (id, time, actor, action, device_id, details, prev, hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![id, time, actor, action, device, details, prev, hash],
    )?;
    Ok(())
}

/// Entries newest first, before `before` if given.
pub fn list(
    conn: &Connection,
    before: Option<i64>,
    device: Option<&str>,
    limit: u32,
) -> rusqlite::Result<Vec<Entry>> {
    let mut stmt = conn.prepare(
        "SELECT id, time, actor, action, device_id, details, hash FROM audit
         WHERE id < ?1 AND (?2 IS NULL OR device_id = ?2)
         ORDER BY id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![before.unwrap_or(i64::MAX), device, limit], |r| {
        let details: String = r.get(5)?;
        Ok(Entry {
            id: r.get(0)?,
            time: r.get(1)?,
            actor: r.get(2)?,
            action: r.get(3)?,
            device_id: r.get(4)?,
            details: serde_json::from_str(&details).unwrap_or(Value::Null),
            hash: r.get(6)?,
        })
    })?;
    rows.collect()
}

/// The result of checking the chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Check {
    pub entries: i64,
    /// The first entry that doesn't fit, and why.
    pub broken_at: Option<i64>,
    pub problem: Option<String>,
}

/// Checks every entry's hash and link to the one before.
pub fn verify(conn: &Connection) -> rusqlite::Result<Check> {
    let mut stmt = conn.prepare(
        "SELECT id, time, actor, action, device_id, details, prev, hash FROM audit ORDER BY id",
    )?;
    let mut rows = stmt.query([])?;
    let mut expected_prev = GENESIS.to_string();
    let mut expected_id = 1;
    let mut entries = 0;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let device: Option<String> = row.get(4)?;
        let details: String = row.get(5)?;
        let prev: String = row.get(6)?;
        let hash: String = row.get(7)?;
        let fail = |problem: &str| Check {
            entries,
            broken_at: Some(id),
            problem: Some(problem.to_string()),
        };
        if id != expected_id {
            return Ok(fail("an entry before this one is missing"));
        }
        if prev != expected_prev {
            return Ok(fail("doesn't follow the entry before it"));
        }
        let computed = entry_hash(
            id,
            row.get(1)?,
            &row.get::<_, String>(2)?,
            &row.get::<_, String>(3)?,
            device.as_deref(),
            &details,
            &prev,
        );
        if computed != hash {
            return Ok(fail("was changed after it was written"));
        }
        expected_prev = hash;
        expected_id += 1;
        entries += 1;
    }
    Ok(Check {
        entries,
        broken_at: None,
        problem: None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::db::Db;

    #[test]
    fn chain_detects_changes_and_gaps() {
        let db = Db::memory().unwrap();
        db.now(|c| {
            append(
                c,
                "Robert",
                "pairing code made",
                None,
                &json!({"group": "Devices"}),
            )
            .unwrap();
            append(c, "Pixel 9", "paired", Some("D1"), &json!({})).unwrap();
            append(
                c,
                "Robert",
                "delete requested",
                Some("D1"),
                &json!({"paths": ["a"]}),
            )
            .unwrap();
            assert_eq!(verify(c).unwrap().entries, 3);
            assert_eq!(verify(c).unwrap().broken_at, None);
            let listed = list(c, None, Some("D1"), 10).unwrap();
            assert_eq!(listed.len(), 2);
            assert_eq!(listed[0].action, "delete requested");

            c.execute("UPDATE audit SET details = '{}' WHERE id = 3", [])
                .unwrap();
            assert_eq!(verify(c).unwrap().broken_at, Some(3));
            c.execute("DELETE FROM audit WHERE id = 3", []).unwrap();
            assert_eq!(verify(c).unwrap().broken_at, None);
            c.execute("DELETE FROM audit WHERE id = 1", []).unwrap();
            let check = verify(c).unwrap();
            assert_eq!(check.broken_at, Some(2));
        });
    }
}
