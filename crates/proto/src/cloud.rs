//! What the FreeSize Nextcloud app reports (0.20): it runs inside
//! Nextcloud, so it sees the real files of the users the admin chose, and
//! says per user what could be cleaned up: files nobody changed in a year,
//! the biggest ones, the same file twice (by a SHA-256 of the contents, read
//! on the server), and what the trash bin and old versions take. Deletes go
//! through Nextcloud into its trash bin, as an [`crate::Action::Delete`]
//! whose `root` is the user's id and whose paths are relative to the user's
//! files.

use serde::{Deserialize, Serialize};

/// Most files listed per list and user.
pub const MAX_FILES: usize = 100;
/// Most duplicate sets listed per user.
pub const MAX_SETS: usize = 100;
/// Files smaller than this aren't fingerprinted or listed as duplicates.
pub const MIN_DUPLICATE: u64 = 1024 * 1024;
/// A file not changed for this long is listed as old.
pub const OLD_AFTER: i64 = 365 * 24 * 60 * 60;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudReport {
    /// Seconds since 1970.
    pub taken: i64,
    /// The address Nextcloud has for itself (`overwrite.cli.url`), without a
    /// trailing slash.
    pub server: String,
    /// "34.0.5".
    pub version: String,
    /// Whether deleted files go to Nextcloud's trash bin. Without it the
    /// app refuses to delete anything.
    pub trash_bin: bool,
    pub users: Vec<CloudUser>,
}

/// One user the admin chose to cover.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudUser {
    /// The user's id, which names them in WebDAV and in commands.
    pub id: String,
    pub name: String,
    /// What the Hub matches with the person signed in, so each person sees
    /// only their own account; empty when the user has none.
    #[serde(default)]
    pub email: String,
    /// The quota in bytes; negative for none.
    pub quota: i64,
    /// What the user's files take.
    pub used: u64,
    pub files: u64,
    /// What the trash bin and the old versions of files take.
    pub trash: u64,
    pub trash_files: u64,
    pub versions: u64,
    pub versions_files: u64,
    /// Folders two levels down, relative to the user's files.
    pub folders: Vec<CloudFolder>,
    /// The biggest files, biggest first.
    pub biggest: Vec<CloudFile>,
    /// The biggest files not changed in [`OLD_AFTER`], biggest first.
    pub old: Vec<CloudFile>,
    /// How many files and bytes are that old, all told.
    pub old_files: u64,
    pub old_bytes: u64,
    /// Files with the same contents, biggest sets first.
    pub duplicates: Vec<DuplicateSet>,
    /// Fingerprinting is done a little each run: how many files of a size
    /// some other file has are fingerprinted, of how many.
    pub hashed: u64,
    pub to_hash: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudFolder {
    pub path: String,
    pub size: u64,
    pub files: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudFile {
    /// Relative to the user's files, `/` between names.
    pub path: String,
    pub size: u64,
    /// Seconds since 1970.
    pub modified: i64,
    /// Nextcloud's id of the file.
    pub fileid: i64,
}

/// Files whose contents are the same, byte for byte (by SHA-256).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateSet {
    pub size: u64,
    /// The SHA-256 of the contents, in hex.
    pub sha256: String,
    /// The copies, the one to keep first (the oldest).
    pub files: Vec<CloudFile>,
}

impl CloudUser {
    /// Every listed file, for checking a delete names only those.
    pub fn listed(&self) -> impl Iterator<Item = &CloudFile> {
        self.biggest
            .iter()
            .chain(&self.old)
            .chain(self.duplicates.iter().flat_map(|d| &d.files))
    }

    /// What deleting every copy but the first of each set frees.
    pub fn duplicate_bytes(&self) -> u64 {
        self.duplicates
            .iter()
            .map(|d| d.size * d.files.len().saturating_sub(1) as u64)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_has_this_version() {
        // A release bumps the Nextcloud app with everything else.
        let info = include_str!("../../../nextcloud/freesize/appinfo/info.xml");
        let app = include_str!("../../../nextcloud/freesize/lib/AppInfo/Application.php");
        let version = env!("CARGO_PKG_VERSION");
        assert!(info.contains(&format!("<version>{version}</version>")));
        assert!(app.contains(&format!("const VERSION = '{version}';")));
    }

    #[test]
    fn reads_what_the_app_sends() {
        // As the PHP app writes it: every field, numbers as numbers.
        let text = r#"{"taken":1760000000,"server":"https://cloud.example.com","version":"34.0.5",
            "trash_bin":true,"users":[{"id":"bob","name":"Bob","email":"bob@example.com",
            "quota":-3,"used":5000000,"files":3,"trash":100,"trash_files":1,"versions":20,
            "versions_files":2,"folders":[{"path":"Photos","size":4000000,"files":2}],
            "biggest":[{"path":"Photos/a.jpg","size":2000000,"modified":1500000000,"fileid":7}],
            "old":[],"old_files":0,"old_bytes":0,
            "duplicates":[{"size":2000000,"sha256":"ab","files":[
              {"path":"Photos/a.jpg","size":2000000,"modified":1500000000,"fileid":7},
              {"path":"Backup/a.jpg","size":2000000,"modified":1600000000,"fileid":9}]}],
            "hashed":2,"to_hash":2}]}"#;
        let report: CloudReport = serde_json::from_str(text).unwrap();
        let user = &report.users[0];
        assert_eq!(user.duplicate_bytes(), 2_000_000);
        assert_eq!(user.listed().count(), 3);
        assert_eq!(user.quota, -3);
    }
}
