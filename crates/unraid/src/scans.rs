//! This round's disk scans, to answer questions about any server path
//! (`/mnt/user/...` across every disk, or one disk's own path) without
//! touching the disks again, so sleeping disks stay asleep.

use std::collections::BTreeMap;
use std::ffi::OsStr;

use disktree_core::{NodeId, SizeMode, Tree};

use crate::array::Array;

/// One disk's scan.
pub struct DiskScan {
    pub disk: String,
    /// Where it's mounted, e.g. `/mnt/disk1`.
    pub mount: String,
    pub tree: Tree,
    pub taken: i64,
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

#[derive(Default)]
pub struct Scans {
    pub disks: Vec<DiskScan>,
}

impl Scans {
    fn node(scan: &DiskScan, path: &str) -> Option<NodeId> {
        let rest = path.strip_prefix(scan.mount.as_str())?;
        if !(rest.is_empty() || rest.starts_with('/')) {
            return None;
        }
        let mut id = scan.tree.root();
        for part in rest.split('/').filter(|p| !p.is_empty()) {
            id = scan.tree.child_by_name(id, OsStr::new(part))?;
        }
        Some(id)
    }

    /// Each scanned disk's node for a server path.
    fn nodes<'a>(&'a self, array: &Array, path: &str) -> Vec<(&'a DiskScan, NodeId)> {
        array
            .physical(path)
            .into_iter()
            .filter_map(|(disk, physical)| {
                let scan = self.disks.iter().find(|s| s.disk == disk)?;
                Some((scan, Self::node(scan, &physical)?))
            })
            .collect()
    }

    /// The size of a path, over every scanned disk holding it; `None` if no
    /// scanned disk has it.
    pub fn size(&self, array: &Array, path: &str) -> Option<Sizes> {
        let nodes = self.nodes(array, path);
        if nodes.is_empty() {
            return None;
        }
        let mut sizes = Sizes::default();
        for (scan, id) in nodes {
            let node = scan.tree.node(id);
            sizes.apparent += node.size_in(SizeMode::Logical);
            sizes.allocated += node.size_in(SizeMode::Allocated);
        }
        Some(sizes)
    }

    /// A folder's entries, merged across disks.
    pub fn list(&self, array: &Array, path: &str) -> BTreeMap<String, Entry> {
        let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
        for (scan, id) in self.nodes(array, path) {
            for child in scan.tree.children(id) {
                let node = scan.tree.node(child);
                let entry = entries
                    .entry(scan.tree.name(child).into_owned())
                    .or_default();
                entry.dir |= node.is_dir();
                entry.sizes.apparent += node.size_in(SizeMode::Logical);
                entry.sizes.allocated += node.size_in(SizeMode::Allocated);
                entry.disks.push(scan.disk.clone());
            }
        }
        entries
    }

    /// Every file under a folder, as paths below it, merged across disks;
    /// at most `limit` of them.
    pub fn files(&self, array: &Array, path: &str, limit: usize) -> BTreeMap<String, Sizes> {
        let mut files: BTreeMap<String, Sizes> = BTreeMap::new();
        for (scan, id) in self.nodes(array, path) {
            let tree = &scan.tree;
            for child in tree.descendants(id) {
                if files.len() >= limit {
                    break;
                }
                let node = tree.node(child);
                if node.is_dir() {
                    continue;
                }
                let mut names: Vec<String> = tree
                    .ancestors(child)
                    .take_while(|&a| a != id)
                    .map(|a| tree.name(a).into_owned())
                    .collect();
                names.reverse();
                let entry = files.entry(names.join("/")).or_default();
                entry.apparent += node.size_in(SizeMode::Logical);
                entry.allocated += node.size_in(SizeMode::Allocated);
            }
        }
        files
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::System;
    use crate::array::{self, tests::server};

    /// Scans of the test server's awake disks, after writing `files` (server
    /// paths and sizes).
    pub fn scanned(system: &System, files: &[(&str, usize)]) -> (Array, Scans) {
        for (path, bytes) in files {
            let file = system.file(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, vec![7u8; *bytes]).unwrap();
        }
        let array = array::read(system);
        let mut scans = Scans::default();
        for disk in array.roots(false) {
            let mount = disk.path.clone().unwrap();
            let tree = disktree_core::scan(
                system.file(&mount),
                &disktree_core::ScanOptions::default(),
                &disktree_core::ScanProgress::new(),
            )
            .unwrap()
            .tree;
            scans.disks.push(DiskScan {
                disk: disk.name.clone(),
                mount,
                tree,
                taken: 0,
            });
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
    }
}
