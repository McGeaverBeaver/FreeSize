//! This round's disk scans, to answer questions about any server path
//! (`/mnt/user/...` across every disk, or one disk's own path) without
//! touching the disks again, so sleeping disks stay asleep.
//!
//! A disk's tree is dropped as soon as it's scanned: what later steps ask
//! about is kept as an index of every folder, plus the files under the few
//! places that are looked at file by file (ISOs, VM folders, Nextcloud's
//! versions and trash). A big array's files would otherwise all sit in
//! memory at once.

use std::collections::BTreeMap;
use std::ops::Bound;

use freesize_core::{SizeMode, Tree};

use crate::array::Array;

/// One disk's scan, reduced to what the round still needs.
pub struct DiskScan {
    pub disk: String,
    /// Where it's mounted, e.g. `/mnt/disk1`.
    pub mount: String,
    /// Every folder and the kept files, by path relative to the mount
    /// (the mount itself is ""), with "/" between names.
    pub index: BTreeMap<String, Indexed>,
    pub taken: i64,
}

/// What the index keeps of a folder or file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Indexed {
    pub dir: bool,
    pub sizes: Sizes,
    /// Seconds since 1970; a folder's is its newest file's.
    pub modified: i64,
}

/// What a path holds: the size the files say, and the space on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sizes {
    pub apparent: u64,
    pub allocated: u64,
}

/// One entry of a folder, merged across disks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub dir: bool,
    pub sizes: Sizes,
    pub disks: Vec<String>,
}

/// Which files an index keeps, as server paths under `/mnt/user` (the
/// same places on every disk). Folders are always kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keep {
    /// Every file under these folders. A `*` stands for one name
    /// (`/mnt/user/nextcloud/*/files_versions`).
    pub under: Vec<String>,
    /// The files directly in these folders.
    pub in_folder: Vec<String>,
    /// These files themselves.
    pub files: Vec<String>,
}

impl Keep {
    /// Patterns as relative paths below a share root, split into names.
    fn relative(path: &str) -> Option<Vec<String>> {
        let rest = path
            .strip_prefix("/mnt/user/")
            .or_else(|| path.strip_prefix("/mnt/user0/"))?;
        Some(
            rest.trim_matches('/')
                .split('/')
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect(),
        )
    }

    fn matches(pattern: &[String], names: &[&str]) -> bool {
        pattern.len() == names.len() && pattern.iter().zip(names).all(|(p, n)| p == "*" || p == n)
    }

    /// Whether a file at `names` (relative to its disk) is kept.
    fn keeps(&self, names: &[&str]) -> bool {
        let parent = &names[..names.len().saturating_sub(1)];
        self.files
            .iter()
            .filter_map(|p| Self::relative(p))
            .any(|p| Self::matches(&p, names))
            || self
                .in_folder
                .iter()
                .filter_map(|p| Self::relative(p))
                .any(|p| Self::matches(&p, parent))
            || self
                .under
                .iter()
                .filter_map(|p| Self::relative(p))
                .any(|p| names.len() > p.len() && Self::matches(&p, &names[..p.len()]))
    }
}

impl DiskScan {
    /// Indexes a scan of a disk's mount and lets the tree go.
    pub fn compact(disk: &str, mount: &str, tree: &Tree, taken: i64, keep: &Keep) -> DiskScan {
        let mut index = BTreeMap::new();
        let root = tree.root();
        let entry = |id| {
            let node = tree.node(id);
            Indexed {
                dir: node.is_dir(),
                sizes: Sizes {
                    apparent: node.size_in(SizeMode::Logical),
                    allocated: node.size_in(SizeMode::Allocated),
                },
                modified: node.modified,
            }
        };
        index.insert(String::new(), entry(root));
        // Depth first with the path so far, so each node's path costs one
        // join rather than a walk up.
        let mut stack: Vec<(freesize_core::NodeId, String)> = tree
            .children(root)
            .map(|c| (c, tree.name(c).into_owned()))
            .collect();
        while let Some((id, path)) = stack.pop() {
            let node = tree.node(id);
            if node.is_dir() {
                for child in tree.children(id) {
                    stack.push((child, format!("{path}/{}", tree.name(child))));
                }
                index.insert(path, entry(id));
            } else {
                let names: Vec<&str> = path.split('/').collect();
                if keep.keeps(&names) {
                    index.insert(path, entry(id));
                }
            }
        }
        DiskScan {
            disk: disk.to_string(),
            mount: mount.to_string(),
            index,
            taken,
        }
    }

    /// The index key of a path on this disk.
    fn key(&self, path: &str) -> Option<String> {
        let rest = path.strip_prefix(self.mount.as_str())?;
        if !(rest.is_empty() || rest.starts_with('/')) {
            return None;
        }
        Some(rest.trim_matches('/').to_string())
    }

    /// Everything below a folder (not the folder itself).
    fn below<'a>(&'a self, key: &str) -> impl Iterator<Item = (&'a str, &'a Indexed)> + 'a {
        let prefix = if key.is_empty() {
            String::new()
        } else {
            format!("{key}/")
        };
        let end = format!("{prefix}\u{10FFFF}");
        self.index
            .range::<str, _>((Bound::Excluded(key), Bound::Excluded(end.as_str())))
            .filter(move |(k, _)| k.starts_with(&prefix))
            .map(|(k, v)| (k.as_str(), v))
    }
}

#[derive(Default)]
pub struct Scans {
    pub disks: Vec<DiskScan>,
}

impl Scans {
    /// Each scanned disk's index key for a server path, where it has it.
    fn keys<'a>(&'a self, array: &Array, path: &str) -> Vec<(&'a DiskScan, String)> {
        array
            .physical(path)
            .into_iter()
            .filter_map(|(disk, physical)| {
                let scan = self.disks.iter().find(|s| s.disk == disk)?;
                let key = scan.key(&physical)?;
                scan.index.contains_key(&key).then_some((scan, key))
            })
            .collect()
    }

    /// The size of a path, over every scanned disk holding it; `None` if no
    /// scanned disk has it.
    pub fn size(&self, array: &Array, path: &str) -> Option<Sizes> {
        let keys = self.keys(array, path);
        if keys.is_empty() {
            return None;
        }
        let mut sizes = Sizes::default();
        for (scan, key) in keys {
            let entry = scan.index[&key];
            sizes.apparent += entry.sizes.apparent;
            sizes.allocated += entry.sizes.allocated;
        }
        Some(sizes)
    }

    /// A folder's entries, merged across disks.
    pub fn list(&self, array: &Array, path: &str) -> BTreeMap<String, Entry> {
        let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
        for (scan, key) in self.keys(array, path) {
            let depth = if key.is_empty() {
                0
            } else {
                key.matches('/').count() + 1
            };
            for (child, indexed) in scan.below(&key) {
                if child.matches('/').count() != depth {
                    continue;
                }
                let name = child.rsplit('/').next().unwrap_or(child).to_string();
                let entry = entries.entry(name).or_default();
                entry.dir |= indexed.dir;
                entry.sizes.apparent += indexed.sizes.apparent;
                entry.sizes.allocated += indexed.sizes.allocated;
                entry.disks.push(scan.disk.clone());
            }
        }
        entries
    }

    /// Every kept file under a folder, as paths below it, merged across
    /// disks; at most `limit` of them.
    pub fn files(&self, array: &Array, path: &str, limit: usize) -> BTreeMap<String, Sizes> {
        let mut files: BTreeMap<String, Sizes> = BTreeMap::new();
        for (scan, key) in self.keys(array, path) {
            let skip = if key.is_empty() { 0 } else { key.len() + 1 };
            for (child, indexed) in scan.below(&key) {
                if files.len() >= limit {
                    break;
                }
                if indexed.dir {
                    continue;
                }
                let entry = files.entry(child[skip..].to_string()).or_default();
                entry.apparent += indexed.sizes.apparent;
                entry.allocated += indexed.sizes.allocated;
            }
        }
        files
    }

    /// The kept files under a folder last changed before `cutoff`, over
    /// every disk: how many and how much.
    pub fn older_than(&self, array: &Array, path: &str, cutoff: i64) -> (u64, Sizes) {
        let mut count = 0;
        let mut sizes = Sizes::default();
        for (scan, key) in self.keys(array, path) {
            for (_, indexed) in scan.below(&key) {
                if !indexed.dir && indexed.modified < cutoff {
                    count += 1;
                    sizes.apparent += indexed.sizes.apparent;
                    sizes.allocated += indexed.sizes.allocated;
                }
            }
        }
        (count, sizes)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::System;
    use crate::array::{self, tests::server};

    /// Scans of the test server's awake disks, after writing `files` (server
    /// paths and sizes), keeping the files under `isos` and `domains`.
    pub fn scanned(system: &System, files: &[(&str, usize)]) -> (Array, Scans) {
        for (path, bytes) in files {
            let file = system.file(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, vec![7u8; *bytes]).unwrap();
        }
        let array = array::read(system);
        let keep = Keep {
            under: vec!["/mnt/user/isos".into(), "/mnt/user/domains".into()],
            in_folder: vec!["/mnt/user/appdata".into()],
            files: Vec::new(),
        };
        let mut scans = Scans::default();
        for disk in array.roots(false) {
            let mount = disk.path.clone().unwrap();
            let tree = freesize_core::scan(
                system.file(&mount),
                &freesize_core::ScanOptions::default(),
                &freesize_core::ScanProgress::new(),
            )
            .unwrap()
            .tree;
            scans
                .disks
                .push(DiskScan::compact(&disk.name, &mount, &tree, 0, &keep));
        }
        (array, scans)
    }

    #[test]
    fn answers_for_user_paths() {
        let (_dir, system) = server();
        let (array, scans) = scanned(
            &system,
            &[
                ("/mnt/disk1/isos/a.iso", 30_000),
                ("/mnt/cache/isos/b.iso", 10_000),
                ("/mnt/cache/isos/sub/c.iso", 5_000),
                // disk2 sleeps: not scanned.
                ("/mnt/disk2/isos/d.iso", 1_000),
                // Files outside the kept places aren't indexed; their folders are.
                ("/mnt/disk1/Movies/a.mkv", 2_000),
                ("/mnt/disk1/appdata/note.txt", 10),
                ("/mnt/disk1/appdata/plex/db", 20),
            ],
        );
        let isos = scans.size(&array, "/mnt/user/isos").unwrap();
        assert!(isos.apparent >= 45_000);
        assert_eq!(scans.size(&array, "/mnt/user/nothing"), None);
        let listed = scans.list(&array, "/mnt/user/isos");
        assert_eq!(listed.keys().collect::<Vec<_>>(), ["a.iso", "b.iso", "sub"]);
        assert_eq!(listed["a.iso"].disks, ["disk1"]);
        assert!(listed["sub"].dir);
        let files = scans.files(&array, "/mnt/user/isos", 100);
        assert_eq!(
            files.keys().collect::<Vec<_>>(),
            ["a.iso", "b.iso", "sub/c.iso"]
        );
        assert_eq!(scans.files(&array, "/mnt/user/isos", 2).len(), 2);
        // A disk's own path.
        assert_eq!(
            scans
                .size(&array, "/mnt/cache/isos/b.iso")
                .unwrap()
                .apparent,
            10_000
        );
        // Folders everywhere, files only where kept.
        assert_eq!(
            scans.size(&array, "/mnt/user/Movies").unwrap().apparent,
            2_000
        );
        assert_eq!(scans.size(&array, "/mnt/user/Movies/a.mkv"), None);
        let appdata = scans.list(&array, "/mnt/user/appdata");
        assert_eq!(appdata.keys().collect::<Vec<_>>(), ["note.txt", "plex"]);
        assert!(scans.files(&array, "/mnt/user/appdata/plex", 10).is_empty());
        // Ages: everything was just written.
        let (count, sizes) = scans.older_than(&array, "/mnt/user/isos", i64::MAX);
        assert_eq!(count, 3);
        assert_eq!(sizes.apparent, 45_000);
        assert_eq!(scans.older_than(&array, "/mnt/user/isos", 0).0, 0);
    }

    #[test]
    fn keep_patterns() {
        let keep = Keep {
            under: vec!["/mnt/user/nextcloud/*/files_versions".into()],
            in_folder: vec!["/mnt/user/appdata".into()],
            files: vec!["/mnt/user/vms/win/vdisk1.img".into()],
        };
        assert!(keep.keeps(&["nextcloud", "bob", "files_versions", "a.txt.v1"]));
        assert!(keep.keeps(&["nextcloud", "bob", "files_versions", "sub", "b.v2"]));
        assert!(!keep.keeps(&["nextcloud", "bob", "files", "a.txt"]));
        assert!(keep.keeps(&["appdata", "note.txt"]));
        assert!(!keep.keeps(&["appdata", "plex", "db"]));
        assert!(keep.keeps(&["vms", "win", "vdisk1.img"]));
        assert!(!keep.keeps(&["vms", "win", "vdisk2.img"]));
    }
}
