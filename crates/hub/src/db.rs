//! The SQLite database: one file in the data folder, one connection shared
//! behind a mutex. Queries are small and run on Tokio's blocking threads.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

/// Schema versions, applied in order; `PRAGMA user_version` records how many
/// ran. Never edit one that shipped: add another.
const MIGRATIONS: &[&str] = &[
    // 1: devices, pairing, reports, commands, audit, sign-in.
    "
    CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);

    CREATE TABLE device_groups (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE,
        -- Grant lengths offered on the device, in minutes, as JSON.
        grant_minutes TEXT NOT NULL DEFAULT '[10,30]',
        -- Sign-in groups that may look at these devices, as JSON.
        viewer_groups TEXT NOT NULL DEFAULT '[]'
    );
    INSERT INTO device_groups (name) VALUES ('Devices');

    CREATE TABLE devices (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        platform TEXT NOT NULL,
        key TEXT NOT NULL UNIQUE,
        fingerprint TEXT NOT NULL UNIQUE,
        group_id INTEGER NOT NULL REFERENCES device_groups (id),
        info TEXT NOT NULL,
        paired INTEGER NOT NULL,
        last_seen INTEGER NOT NULL,
        removed INTEGER NOT NULL DEFAULT 0
    );

    CREATE TABLE pairing_codes (
        hash TEXT PRIMARY KEY,
        group_id INTEGER NOT NULL REFERENCES device_groups (id),
        created INTEGER NOT NULL,
        expires INTEGER NOT NULL,
        created_by TEXT NOT NULL,
        used_by TEXT
    );

    CREATE TABLE nonces (
        device_id TEXT NOT NULL,
        nonce TEXT NOT NULL,
        time INTEGER NOT NULL,
        PRIMARY KEY (device_id, nonce)
    ) WITHOUT ROWID;

    CREATE TABLE disks (
        device_id TEXT NOT NULL,
        path TEXT NOT NULL,
        name TEXT NOT NULL,
        total INTEGER NOT NULL,
        free INTEGER NOT NULL,
        updated INTEGER NOT NULL,
        PRIMARY KEY (device_id, path)
    );

    -- One sample of each disk per day, for trends.
    CREATE TABLE disk_samples (
        device_id TEXT NOT NULL,
        path TEXT NOT NULL,
        day INTEGER NOT NULL,
        total INTEGER NOT NULL,
        used INTEGER NOT NULL,
        PRIMARY KEY (device_id, path, day)
    );

    CREATE TABLE scans (
        id INTEGER PRIMARY KEY,
        device_id TEXT NOT NULL,
        root TEXT NOT NULL,
        label TEXT NOT NULL,
        taken INTEGER NOT NULL,
        size INTEGER NOT NULL,
        allocated INTEGER NOT NULL,
        files INTEGER NOT NULL,
        dirs INTEGER NOT NULL,
        errors INTEGER NOT NULL,
        complete INTEGER NOT NULL,
        -- Folder sizes as gzipped JSON.
        folders BLOB NOT NULL,
        types TEXT NOT NULL,
        received INTEGER NOT NULL
    );
    CREATE INDEX scans_by_device ON scans (device_id, root, taken);

    -- The biggest files of each scan, for listing and searching.
    CREATE TABLE scan_files (
        scan_id INTEGER NOT NULL REFERENCES scans (id) ON DELETE CASCADE,
        path TEXT NOT NULL,
        size INTEGER NOT NULL,
        allocated INTEGER NOT NULL,
        modified INTEGER NOT NULL
    );
    CREATE INDEX scan_files_by_scan ON scan_files (scan_id);

    CREATE TABLE apps (
        device_id TEXT PRIMARY KEY,
        taken INTEGER NOT NULL,
        report TEXT NOT NULL
    );

    CREATE TABLE commands (
        id TEXT PRIMARY KEY,
        device_id TEXT NOT NULL,
        kind TEXT NOT NULL,
        summary TEXT NOT NULL,
        admin TEXT NOT NULL,
        admin_name TEXT NOT NULL,
        created INTEGER NOT NULL,
        expires INTEGER NOT NULL,
        -- The command as signed by the Hub.
        signed TEXT NOT NULL,
        -- queued, delivered, cancelled, or what the device reported.
        state TEXT NOT NULL,
        detail TEXT NOT NULL DEFAULT '',
        approval TEXT,
        freed INTEGER NOT NULL DEFAULT 0,
        failed TEXT NOT NULL DEFAULT '[]',
        updated INTEGER NOT NULL,
        -- When it last went to the device, and when its cancellation did.
        delivered INTEGER,
        cancel_delivered INTEGER
    );
    CREATE INDEX commands_by_device ON commands (device_id, state);

    -- Commands waiting for a designated approver, and their decisions.
    CREATE TABLE approvals (
        command_id TEXT PRIMARY KEY REFERENCES commands (id),
        approver TEXT NOT NULL,
        asked INTEGER NOT NULL,
        delivered INTEGER,
        decision TEXT,
        decided INTEGER,
        decision_delivered INTEGER
    );

    CREATE TABLE audit (
        id INTEGER PRIMARY KEY,
        time INTEGER NOT NULL,
        actor TEXT NOT NULL,
        action TEXT NOT NULL,
        device_id TEXT,
        details TEXT NOT NULL,
        prev TEXT NOT NULL,
        hash TEXT NOT NULL
    );

    CREATE TABLE sessions (
        hash TEXT PRIMARY KEY,
        subject TEXT NOT NULL,
        name TEXT NOT NULL,
        email TEXT NOT NULL,
        groups TEXT NOT NULL,
        admin INTEGER NOT NULL,
        csrf TEXT NOT NULL,
        created INTEGER NOT NULL,
        expires INTEGER NOT NULL
    );

    -- Sign-ins on their way through the identity provider.
    CREATE TABLE logins (
        state TEXT PRIMARY KEY,
        verifier TEXT NOT NULL,
        nonce TEXT NOT NULL,
        next TEXT NOT NULL,
        created INTEGER NOT NULL
    );
    ",
    // 2: devices without an agent (scans uploaded as files), saved searches.
    "
    ALTER TABLE devices ADD COLUMN agentless INTEGER NOT NULL DEFAULT 0;

    CREATE TABLE saved_searches (
        id INTEGER PRIMARY KEY,
        subject TEXT NOT NULL,
        name TEXT NOT NULL,
        query TEXT NOT NULL,
        min_size INTEGER NOT NULL DEFAULT 0,
        created INTEGER NOT NULL
    );
    CREATE INDEX saved_searches_by_subject ON saved_searches (subject);
    ",
    // 3: alerts and where they're sent.
    "
    CREATE TABLE alerts (
        id INTEGER PRIMARY KEY,
        -- The same while the problem lasts, e.g. disk_full:DEVICE:/mnt/user.
        key TEXT NOT NULL,
        kind TEXT NOT NULL,
        device_id TEXT,
        severity TEXT NOT NULL,
        title TEXT NOT NULL,
        message TEXT NOT NULL,
        opened INTEGER NOT NULL,
        resolved INTEGER
    );
    CREATE INDEX alerts_by_state ON alerts (resolved, key);

    CREATE TABLE alert_channels (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL,
        -- Where and how, with its token, as JSON.
        target TEXT NOT NULL,
        enabled INTEGER NOT NULL DEFAULT 1,
        created INTEGER NOT NULL,
        last_sent INTEGER,
        last_error TEXT
    );
    ",
    // 4: what Unraid servers report beyond disks and scans.
    "
    CREATE TABLE unraid (
        device_id TEXT PRIMARY KEY,
        taken INTEGER NOT NULL,
        report TEXT NOT NULL
    );

    -- What a device's quarantine holds.
    CREATE TABLE quarantine (
        device_id TEXT PRIMARY KEY,
        updated INTEGER NOT NULL,
        batches TEXT NOT NULL
    );
    ",
    // 5: the folders a scan couldn't read, as a JSON list of paths.
    "
    ALTER TABLE scans ADD COLUMN unreadable TEXT NOT NULL DEFAULT '[]';
    ",
    // 6: what each device's junk cleaner found (0.10).
    "
    CREATE TABLE junk (
        device_id TEXT PRIMARY KEY,
        taken INTEGER NOT NULL,
        report TEXT NOT NULL
    );
    ",
    // 7: Nextcloud accounts the Hub reads, their files, and what each
    // device keeps of its Nextcloud folders (0.12).
    "
    CREATE TABLE nextcloud (
        id INTEGER PRIMARY KEY,
        server TEXT NOT NULL,
        user TEXT NOT NULL,
        -- The app password Nextcloud's sign-in flow handed the Hub.
        password TEXT NOT NULL,
        added INTEGER NOT NULL,
        added_by TEXT NOT NULL,
        synced INTEGER,
        error TEXT,
        files INTEGER NOT NULL DEFAULT 0,
        folders INTEGER NOT NULL DEFAULT 0,
        bytes INTEGER NOT NULL DEFAULT 0,
        trash INTEGER NOT NULL DEFAULT 0,
        quota_used INTEGER NOT NULL DEFAULT 0,
        UNIQUE (server, user)
    );

    CREATE TABLE nextcloud_files (
        account_id INTEGER NOT NULL REFERENCES nextcloud (id) ON DELETE CASCADE,
        path TEXT NOT NULL,
        fileid INTEGER NOT NULL,
        size INTEGER NOT NULL,
        is_dir INTEGER NOT NULL,
        modified INTEGER NOT NULL,
        checksums TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (account_id, path)
    );

    CREATE TABLE sync (
        device_id TEXT PRIMARY KEY,
        taken INTEGER NOT NULL,
        report TEXT NOT NULL
    );
    ",
    // 8: how much the server says an account has left (0.13); -1 until
    // the next listing, and for an unlimited account.
    "
    ALTER TABLE nextcloud ADD COLUMN quota_free INTEGER NOT NULL DEFAULT -1;
    ",
    // 9: the account's own id, which names it in WebDAV and in the
    // devices' reports (0.16.3); NULL until the next listing asks for it.
    // `user` stays the name the person signed in with, which the app
    // password belongs to.
    "
    ALTER TABLE nextcloud ADD COLUMN dav_user TEXT;
    ",
    // 10: what the FreeSize Nextcloud app reports about the users it
    // covers (0.20).
    "
    CREATE TABLE cloud (
        device_id TEXT PRIMARY KEY,
        taken INTEGER NOT NULL,
        report TEXT NOT NULL
    );
    ",
];

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Db> {
        let conn = Connection::open(path)?;
        Db::setup(conn)
    }

    /// A database in memory, for tests.
    pub fn memory() -> rusqlite::Result<Db> {
        Db::setup(Connection::open_in_memory()?)
    }

    fn setup(mut conn: Connection) -> rusqlite::Result<Db> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
        )?;
        migrate(&mut conn)?;
        Ok(Db(Arc::new(Mutex::new(conn))))
    }

    /// Runs `f` with the connection on a blocking thread.
    pub async fn call<R, F>(&self, f: F) -> R
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> R + Send + 'static,
    {
        let db = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = db.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut conn)
        })
        .await
        .expect("database task panicked")
    }

    /// Runs `f` with the connection on the current thread, for start-up
    /// and tests.
    pub fn now<R>(&self, f: impl FnOnce(&mut Connection) -> R) -> R {
        let mut conn = self.0.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut conn)
    }
}

fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let done: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(done as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
    }
    Ok(())
}

/// Seconds since 1970.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_once() {
        let dir = std::env::temp_dir().join(format!("freesize-hub-db-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hub.db");
        let db = Db::open(&path).unwrap();
        let groups: i64 = db.now(|c| {
            c.query_row("SELECT count(*) FROM device_groups", [], |r| r.get(0))
                .unwrap()
        });
        assert_eq!(groups, 1);
        drop(db);
        // Opening again runs nothing twice.
        let db = Db::open(&path).unwrap();
        let version: i64 = db.now(|c| {
            c.query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap()
        });
        assert_eq!(version as usize, MIGRATIONS.len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
