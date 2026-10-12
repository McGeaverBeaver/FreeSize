//! What deleting really frees. A file with other hard links elsewhere (the
//! *arr apps hard-link downloads into the library) frees nothing until its
//! last link goes, and on ZFS, snapshots keep deleted data until they're
//! destroyed.

#[cfg(unix)]
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Estimate {
    /// Space the items take.
    pub bytes: u64,
    /// What deleting them frees, as far as files go.
    pub freed: u64,
    /// Files hard-linked from outside the items: freed only when those
    /// links go too.
    pub linked: u64,
    /// ZFS datasets with snapshots, which keep deleted data.
    pub snapshotted: Vec<String>,
}

impl Estimate {
    /// One line for a prompt, e.g. "Frees about 12 GB; 3 GB are hard links
    /// to files kept elsewhere and free nothing."
    pub fn describe(&self) -> String {
        let size = disktree_core::format_size;
        let mut text = format!("Frees about {}", size(self.freed));
        if self.linked > 0 {
            text.push_str(&format!(
                "; {} are hard links to files kept elsewhere (e.g. by the *arr apps) and free \
                 nothing until those go too",
                size(self.linked)
            ));
        }
        text.push('.');
        if !self.snapshotted.is_empty() {
            text.push_str(&format!(
                " ZFS snapshots of {} keep the data until they're destroyed.",
                self.snapshotted.join(", ")
            ));
        }
        text
    }
}

#[cfg(unix)]
pub fn estimate(paths: &[PathBuf]) -> Estimate {
    use std::os::unix::fs::MetadataExt;

    #[derive(Default)]
    struct Inode {
        links: u64,
        seen: u64,
        bytes: u64,
    }
    let mut estimate = Estimate::default();
    let mut inodes: HashMap<(u64, u64), Inode> = HashMap::new();
    let mut stack: Vec<PathBuf> = paths.to_vec();
    while let Some(path) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let bytes = meta.blocks() * 512;
        if meta.is_dir() {
            estimate.bytes += bytes;
            estimate.freed += bytes;
            if let Ok(entries) = std::fs::read_dir(&path) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        } else if meta.nlink() > 1 {
            let inode = inodes.entry((meta.dev(), meta.ino())).or_default();
            inode.links = meta.nlink();
            inode.seen += 1;
            inode.bytes = bytes;
        } else {
            estimate.bytes += bytes;
            estimate.freed += bytes;
        }
    }
    for inode in inodes.values() {
        estimate.bytes += inode.bytes;
        // Every link is among the items: deleting them all frees it.
        if inode.seen >= inode.links {
            estimate.freed += inode.bytes;
        } else {
            estimate.linked += inode.bytes;
        }
    }
    estimate.snapshotted = zfs_snapshots(paths);
    estimate
}

#[cfg(not(unix))]
pub fn estimate(paths: &[PathBuf]) -> Estimate {
    let bytes = paths
        .iter()
        .map(|p| crate::quarantine::size_on_disk(p))
        .sum();
    Estimate {
        bytes,
        freed: bytes,
        ..Estimate::default()
    }
}

/// The ZFS datasets holding `paths` that have snapshots.
#[cfg(unix)]
fn zfs_snapshots(paths: &[PathBuf]) -> Vec<String> {
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
        return Vec::new();
    };
    let mut datasets: Vec<String> = Vec::new();
    for path in paths {
        if let Some(dataset) = zfs_dataset(&mounts, path)
            && !datasets.contains(&dataset)
        {
            datasets.push(dataset);
        }
    }
    datasets
        .into_iter()
        .filter(|dataset| {
            std::process::Command::new("zfs")
                .args([
                    "list", "-H", "-t", "snapshot", "-o", "name", "-d", "1", dataset,
                ])
                .output()
                .map(|out| out.status.success() && !out.stdout.trim_ascii().is_empty())
                .unwrap_or(false)
        })
        .collect()
}

/// The ZFS dataset mounted at the deepest folder above `path`, from
/// `/proc/mounts`.
pub fn zfs_dataset(mounts: &str, path: &Path) -> Option<String> {
    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let device = fields.next()?;
            let mount = fields.next()?.replace("\\040", " ");
            let fs = fields.next()?;
            (fs == "zfs" && path.starts_with(&mount)).then(|| (mount.len(), device.to_string()))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, device)| device)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn hard_links_free_nothing_unless_all_go() {
        let dir = tempfile::tempdir().unwrap();
        let downloads = dir.path().join("downloads");
        let library = dir.path().join("library");
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::create_dir_all(&library).unwrap();
        std::fs::write(downloads.join("movie.mkv"), vec![1u8; 100_000]).unwrap();
        std::fs::hard_link(downloads.join("movie.mkv"), library.join("movie.mkv")).unwrap();
        std::fs::write(downloads.join("sample.mkv"), vec![1u8; 20_000]).unwrap();

        // The library keeps the movie: only the sample is freed.
        let one = estimate(std::slice::from_ref(&downloads));
        assert!(one.linked >= 100_000, "{one:?}");
        assert!(one.freed >= 20_000 && one.freed < 100_000, "{one:?}");
        assert!(one.describe().contains("hard links"));
        // Both links go: all of it.
        let both = estimate(&[downloads, library]);
        assert_eq!(both.linked, 0);
        assert!(both.freed >= 120_000);
        assert_eq!(both.freed, both.bytes);
    }

    #[test]
    fn finds_zfs_datasets() {
        let mounts = "cache /mnt/cache zfs rw 0 0\ncache/appdata /mnt/cache/appdata zfs rw 0 0\n\
                      /dev/md1p1 /mnt/disk1 xfs rw 0 0\n";
        assert_eq!(
            zfs_dataset(mounts, Path::new("/mnt/cache/appdata/plex")).as_deref(),
            Some("cache/appdata")
        );
        assert_eq!(
            zfs_dataset(mounts, Path::new("/mnt/cache/isos")).as_deref(),
            Some("cache")
        );
        assert_eq!(zfs_dataset(mounts, Path::new("/mnt/disk1/x")), None);
        let estimate = Estimate {
            bytes: 10,
            freed: 10,
            linked: 0,
            snapshotted: vec!["cache/appdata".into()],
        };
        assert!(
            estimate
                .describe()
                .contains("ZFS snapshots of cache/appdata")
        );
    }
}
