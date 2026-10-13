//! What a device keeps of its Nextcloud folders (0.12): per sync folder,
//! how much of it is stored on the device and how much is only a
//! placeholder, the biggest files one by one, and nothing else. The Hub
//! joins these across devices on the server, the account and the path, and
//! with what it reads from the server itself, to say "kept on N devices".

use serde::{Deserialize, Serialize};

/// Files one by one from this size up; smaller ones are only counted.
pub const BIG_FILE: u64 = 100 * 1024 * 1024;
/// Folders are listed this many levels below each sync folder.
pub const DIR_DEPTH: usize = 2;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    /// Seconds since 1970.
    pub taken: i64,
    pub accounts: Vec<SyncAccount>,
}

/// One Nextcloud account the device's client is signed in to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncAccount {
    /// The server as the client has it, e.g. `https://cloud.example.com`,
    /// without a trailing slash.
    pub server: String,
    /// The user the client signs in as.
    pub user: String,
    /// "desktop" or "android".
    pub client: String,
    pub folders: Vec<SyncFolder>,
}

/// One folder the client keeps in step with the server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncFolder {
    /// The folder on the device, as the device writes it.
    pub local: String,
    /// The folder on the server it mirrors, `/` for everything or e.g.
    /// `/Photos`.
    pub remote: String,
    /// What the client knows of the folder: the sizes of every file in it,
    /// stored on the device or not.
    pub size: u64,
    /// Bytes stored on the device (placeholders and online-only files
    /// count nothing).
    pub on_disk: u64,
    pub files: u64,
    pub on_disk_files: u64,
    /// "off", "suffix" or "wincfapi" as the client's settings say; empty
    /// when unknown. With virtual files on, files can be placeholders.
    #[serde(default)]
    pub virtual_files: String,
    /// Subfolders, two levels down, with `/` between names and relative
    /// to the sync folder.
    pub dirs: Vec<SyncDir>,
    /// Files of [`BIG_FILE`] and over, one by one.
    pub big: Vec<SyncFile>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncDir {
    pub path: String,
    pub size: u64,
    pub on_disk: u64,
    pub files: u64,
    pub on_disk_files: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncFile {
    /// Relative to the sync folder, with `/` between names.
    pub path: String,
    pub size: u64,
    /// Seconds since 1970.
    pub modified: i64,
    /// Whether the content is stored on the device.
    pub on_disk: bool,
}

impl SyncFolder {
    /// The folder on the server a relative path is at, with `/` between
    /// names and no trailing slash: `/Photos/2019` for `2019` in the sync
    /// folder of `/Photos`.
    pub fn remote_path(&self, relative: &str) -> String {
        join_remote(&self.remote, relative)
    }
}

/// Joins a folder on the server and a path below it.
pub fn join_remote(remote: &str, relative: &str) -> String {
    let base = remote.trim_end_matches('/');
    let relative = relative.trim_matches('/');
    if relative.is_empty() {
        if base.is_empty() {
            "/".to_string()
        } else {
            base.to_string()
        }
    } else {
        format!("{base}/{relative}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths_join() {
        let folder = SyncFolder {
            remote: "/".into(),
            ..SyncFolder::default()
        };
        assert_eq!(folder.remote_path(""), "/");
        assert_eq!(folder.remote_path("Photos/2019"), "/Photos/2019");
        let photos = SyncFolder {
            remote: "/Photos/".into(),
            ..SyncFolder::default()
        };
        assert_eq!(photos.remote_path(""), "/Photos");
        assert_eq!(photos.remote_path("2019/"), "/Photos/2019");
        assert_eq!(join_remote("", "a"), "/a");
    }
}
