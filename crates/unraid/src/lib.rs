//! What DiskTree knows about Unraid: the array and its pools, where each
//! share's files sit, Docker and VMs sorted by how safe they are to clean,
//! what must never be touched, the quarantine, what keeps disks awake,
//! permissions, and moving folders between disks.
//!
//! Everything reads the server's own files (`/var/local/emhttp`,
//! `/boot/config`, `/proc`) through [`System`], so tests run against a
//! folder laid out like a server.

pub mod array;
pub mod docker;
pub mod ini;
pub mod protect;
pub mod scans;
pub mod shares;
pub mod vms;

use std::path::{Path, PathBuf};

/// Where Unraid's files are: `/` on a server, a test folder in tests.
#[derive(Clone, Debug)]
pub struct System {
    root: PathBuf,
}

impl System {
    /// This machine.
    pub fn real() -> System {
        System { root: "/".into() }
    }

    /// A folder laid out like a server, for tests.
    pub fn at(root: impl Into<PathBuf>) -> System {
        System { root: root.into() }
    }

    /// The file for an absolute path on the server.
    pub fn file(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    /// The server's absolute path of a file under this system.
    pub fn unraid_path(&self, file: &Path) -> Option<String> {
        let rest = file.strip_prefix(&self.root).ok()?;
        Some(format!("/{}", rest.to_string_lossy()))
    }

    pub fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.file(path)).ok()
    }

    /// Whether this is an Unraid server.
    pub fn is_unraid(&self) -> bool {
        self.file("/etc/unraid-version").exists()
    }

    /// Unraid's version, e.g. "7.1.2".
    pub fn version(&self) -> String {
        self.read("/etc/unraid-version")
            .map(|text| ini::parse_cfg(&text))
            .and_then(|cfg| cfg.get("version").cloned())
            .unwrap_or_default()
    }
}

/// Seconds since 1970.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
