//! Files in user shares whose owner or mode can break access over SMB.
//! Unraid expects `nobody:users` (99:100) with read and write for owner and
//! group. App, system and VM shares are left out: containers own their
//! files as they like. Only reported; Tools → Docker Safe New Perms fixes
//! them.

use std::collections::BTreeMap;
use std::path::Path;

use freesize_proto::unraid::PermissionIssue;

use crate::System;
use crate::array::Array;

/// Shares whose owners are expected to vary.
const SKIP: &[&str] = &["appdata", "system", "domains", "isos"];
/// Examples kept per share.
const EXAMPLES: usize = 10;
/// Entries looked at per run at most, so a huge array doesn't take all
/// night (right after a scan their details are in memory anyway).
const MAX_ENTRIES: u64 = 5_000_000;

/// Why an entry's permissions are off, if they are.
#[cfg(unix)]
pub fn odd(meta: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    if meta.file_type().is_symlink() {
        return None;
    }
    let mode = meta.mode() & 0o7777;
    let wanted = if meta.is_dir() { 0o770 } else { 0o660 };
    if meta.uid() != 99 || meta.gid() != 100 || mode & wanted != wanted {
        Some(format!(
            "owner {}:{}, mode {:04o}",
            meta.uid(),
            meta.gid(),
            mode
        ))
    } else {
        None
    }
}

/// Looks through the user shares on the awake disks.
#[cfg(unix)]
pub fn report(system: &System, array: &Array, skip: &[&str]) -> Vec<PermissionIssue> {
    let mut by_share: BTreeMap<String, PermissionIssue> = BTreeMap::new();
    let mut seen = 0u64;
    for disk in array.roots(false) {
        let Some(mount) = &disk.path else { continue };
        let Ok(shares) = std::fs::read_dir(system.file(mount)) else {
            continue;
        };
        for share in shares.flatten() {
            let name = share.file_name().to_string_lossy().into_owned();
            if name.starts_with('.')
                || SKIP.contains(&name.as_str())
                || skip.contains(&name.as_str())
            {
                continue;
            }
            if !share.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let mut stack = vec![share.path()];
            while let Some(dir) = stack.pop() {
                let Ok(meta) = std::fs::symlink_metadata(&dir) else {
                    continue;
                };
                check(system, &name, &dir, &meta, &mut by_share);
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    seen += 1;
                    if seen > MAX_ENTRIES {
                        return by_share.into_values().collect();
                    }
                    let Ok(meta) = entry.metadata() else { continue };
                    if meta.is_dir() {
                        stack.push(entry.path());
                    } else {
                        check(system, &name, &entry.path(), &meta, &mut by_share);
                    }
                }
            }
        }
    }
    by_share.into_values().collect()
}

#[cfg(unix)]
fn check(
    system: &System,
    share: &str,
    path: &Path,
    meta: &std::fs::Metadata,
    by_share: &mut BTreeMap<String, PermissionIssue>,
) {
    let Some(why) = odd(meta) else { return };
    let issue = by_share
        .entry(share.to_string())
        .or_insert_with(|| PermissionIssue {
            share: share.to_string(),
            ..PermissionIssue::default()
        });
    issue.count += 1;
    if issue.examples.len() < EXAMPLES {
        let shown = system
            .unraid_path(path)
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        issue.examples.push(format!("{shown} ({why})"));
    }
}

#[cfg(not(unix))]
pub fn report(_system: &System, _array: &Array, _skip: &[&str]) -> Vec<PermissionIssue> {
    Vec::new()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::array::{self, tests::server};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[test]
    fn reports_odd_owners_and_modes() {
        let (_dir, system) = server();
        let array = array::read(&system);
        for path in [
            "/mnt/disk1/Movies/a.mkv",
            "/mnt/disk1/Movies/sub/b.mkv",
            "/mnt/disk1/appdata/plex/db",
            "/mnt/disk2/Movies/c.mkv",
        ] {
            let file = system.file(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, b"x").unwrap();
        }
        std::fs::set_permissions(
            system.file("/mnt/disk1/Movies/sub/b.mkv"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let meta = std::fs::metadata(system.file("/mnt/disk1/Movies/a.mkv")).unwrap();
        let issues = report(&system, &array, &[]);
        // appdata is skipped; disk2 sleeps.
        assert_eq!(issues.len(), 1);
        let movies = &issues[0];
        assert_eq!(movies.share, "Movies");
        if meta.uid() == 99 && meta.gid() == 100 {
            // Only the 0600 file.
            assert_eq!(movies.count, 1);
        } else {
            // Not nobody:users here: the folders and files all count.
            assert_eq!(movies.count, 4);
        }
        assert!(
            movies
                .examples
                .iter()
                .any(|e| e.contains("/mnt/disk1/Movies/sub/b.mkv") && e.contains("0600"))
        );
        assert!(report(&system, &array, &["Movies"]).is_empty());
    }
}
