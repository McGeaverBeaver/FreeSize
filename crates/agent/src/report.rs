//! Turning what a device knows into reports for the Hub.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use freesize_core::snapshot::{self, relative_path};
use freesize_core::{SizeMode, Tree, disk_space, extension_stats, largest_files};
use freesize_proto::{Disk, FileEntry, ScanReport, TypeEntry};

/// Folders kept per scan report.
pub const MAX_FOLDERS: usize = 10_000;
/// Files kept per scan report.
pub const MAX_FILES: usize = 500;
/// Unreadable folders named per scan report.
pub const MAX_UNREADABLE: usize = 50;

/// A finished scan as the Hub stores it: folder sizes, the biggest files and
/// file types.
pub fn scan_report(
    tree: &Tree,
    label: &str,
    taken: i64,
    errors: u64,
    complete: bool,
) -> ScanReport {
    let root = tree.root();
    let node = tree.node(root);
    let folders = snapshot::take(tree, taken, MAX_FOLDERS).folders;
    let largest = largest_files(tree, root, SizeMode::Allocated, MAX_FILES)
        .into_iter()
        .map(|id| {
            let file = tree.node(id);
            FileEntry {
                path: relative_path(tree, id),
                size: file.size,
                allocated: file.allocated,
                modified: file.modified,
            }
        })
        .collect();
    let types = extension_stats(tree, root, SizeMode::Allocated)
        .into_iter()
        .take(100)
        .map(|s| TypeEntry {
            extension: s.extension,
            files: s.files,
            size: s.size,
        })
        .collect();
    let unreadable = tree
        .unreadable()
        .into_iter()
        .take(MAX_UNREADABLE)
        .map(|id| relative_path(tree, id))
        .collect();
    ScanReport {
        root: tree.path(root).to_string_lossy().into_owned(),
        label: label.to_string(),
        taken,
        size: node.size,
        allocated: node.allocated,
        files: u64::from(node.files),
        dirs: u64::from(node.dirs),
        errors,
        complete,
        unreadable,
        folders,
        largest,
        types,
    }
}

/// A disk report for each of `places` (label and path), one per file system.
pub fn disks(places: &[(String, PathBuf)]) -> Vec<Disk> {
    let mut seen = HashSet::new();
    places
        .iter()
        .filter_map(|(name, path)| {
            let space = disk_space(path)?;
            // Bind mounts and folders on the same disk report the same numbers.
            if space.total == 0 || !seen.insert((space.total, space.free / (64 * 1024 * 1024))) {
                return None;
            }
            Some(Disk {
                name: name.clone(),
                path: path.to_string_lossy().into_owned(),
                total: space.total,
                free: space.free,
            })
        })
        .collect()
}

/// The drives of this machine worth reporting.
pub fn system_places() -> Vec<(String, PathBuf)> {
    #[cfg(windows)]
    {
        ('C'..='Z')
            .map(|letter| PathBuf::from(format!("{letter}:\\")))
            .filter(|p| std::fs::metadata(p).is_ok())
            .map(|p| {
                (
                    format!("{}", p.display())
                        .trim_end_matches('\\')
                        .to_string(),
                    p,
                )
            })
            .collect()
    }
    #[cfg(target_os = "macos")]
    {
        let mut places = vec![("Macintosh HD".to_string(), PathBuf::from("/"))];
        if let Ok(entries) = std::fs::read_dir("/Volumes") {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_symlink() {
                    continue;
                }
                places.push((entry.file_name().to_string_lossy().into_owned(), path));
            }
        }
        places
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
        linux_places(&mounts)
    }
}

/// File systems from `/proc/self/mounts` that hold data: disks, ZFS and
/// btrfs pools, Unraid's array and pools; not `/proc`, tmpfs, snaps or
/// overlay layers.
pub fn linux_places(mounts: &str) -> Vec<(String, PathBuf)> {
    const REAL: &[&str] = &[
        "ext2",
        "ext3",
        "ext4",
        "xfs",
        "btrfs",
        "zfs",
        "f2fs",
        "vfat",
        "exfat",
        "ntfs",
        "ntfs3",
        "fuseblk",
        "fuse.shfs",
        "jfs",
        "reiserfs",
        "bcachefs",
        "nfs",
        "nfs4",
        "cifs",
        "smb3",
        "fuse.mergerfs",
    ];
    let mut places = Vec::new();
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let (Some(_device), Some(mount), Some(kind)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let mount = mount.replace("\\040", " ");
        let skip = !REAL.contains(&kind)
            || mount.starts_with("/snap/")
            || mount.starts_with("/boot/efi")
            || mount.starts_with("/var/lib/docker")
            || mount.starts_with("/run/");
        if skip {
            continue;
        }
        let name = if mount == "/" {
            "System".to_string()
        } else {
            Path::new(&mount)
                .file_name()
                .map_or(mount.clone(), |n| n.to_string_lossy().into_owned())
        };
        places.push((name, PathBuf::from(mount)));
    }
    places
}

#[cfg(test)]
mod tests {
    use std::fs;

    use freesize_core::{ScanOptions, ScanProgress, scan};

    use super::*;

    #[test]
    fn scan_reports_carry_folders_files_and_types() {
        let dir =
            std::env::temp_dir().join(format!("freesize-agent-report-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Movies/2024")).unwrap();
        fs::write(dir.join("Movies/2024/trip.mp4"), vec![1u8; 300_000]).unwrap();
        fs::write(dir.join("notes.txt"), b"hi").unwrap();
        let tree = scan(&dir, &ScanOptions::default(), &ScanProgress::new())
            .unwrap()
            .tree;
        let report = scan_report(&tree, "Test", 1_790_000_000, 0, true);
        assert_eq!(report.label, "Test");
        assert_eq!(report.files, 2);
        assert!(report.folders.iter().any(|f| f.path.is_empty()));
        assert!(report.folders.iter().any(|f| f.path == "Movies/2024"));
        assert_eq!(report.largest[0].path, "Movies/2024/trip.mp4");
        assert_eq!(report.types[0].extension, "mp4");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_file_systems_only() {
        let mounts = "\
/dev/sda2 / ext4 rw,relatime 0 0
proc /proc proc rw 0 0
tmpfs /run tmpfs rw 0 0
/dev/md1p1 /mnt/disk1 xfs rw 0 0
shfs /mnt/user fuse.shfs rw 0 0
/dev/loop3 /snap/core/1 squashfs ro 0 0
tank/media /mnt/tank/media zfs rw 0 0
//nas/share /mnt/my\\040share cifs rw 0 0
overlay /var/lib/docker/overlay2/x/merged overlay rw 0 0
";
        let places = linux_places(mounts);
        let names: Vec<&str> = places.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["System", "disk1", "user", "media", "my share"]);
    }
}
