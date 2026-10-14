//! Folder sizes saved after a scan, and what changed between two of them.
//!
//! A snapshot keeps the biggest folders only (not files), so it stays small
//! enough to keep many of them: a few kilobytes for a phone, about a
//! megabyte for a whole PC drive.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::tree::{NodeId, SizeMode, Tree, flags};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// When it was taken, in seconds since 1970.
    pub taken: i64,
    /// Folders by path below the scanned folder (`/` between names, `""` for
    /// the scanned folder itself), biggest first.
    pub folders: Vec<FolderSize>,
    /// When smaller folders were left out, the size on disk of the smallest
    /// one kept: a folder missing from the list was smaller than this.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub floor: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderSize {
    pub path: String,
    pub size: u64,
    pub allocated: u64,
}

impl FolderSize {
    fn size_in(&self, mode: SizeMode) -> u64 {
        match mode {
            SizeMode::Logical => self.size,
            SizeMode::Allocated => self.allocated,
        }
    }
}

impl Snapshot {
    /// The scanned folder's total.
    pub fn total(&self, mode: SizeMode) -> u64 {
        self.folders
            .iter()
            .find(|f| f.path.is_empty())
            .map_or(0, |f| f.size_in(mode))
    }

    /// The same snapshot with only its `max` biggest folders, for keeping
    /// old scans small.
    pub fn trimmed(&self, max: usize) -> Snapshot {
        if self.folders.len() <= max {
            return self.clone();
        }
        let folders: Vec<FolderSize> = self.folders.iter().take(max).cloned().collect();
        let floor = folders.iter().map(|f| f.allocated).min().unwrap_or(0);
        Snapshot {
            taken: self.taken,
            folders,
            floor: floor.max(self.floor),
        }
    }

    /// Sizes by path, for looking items up.
    pub fn sizes(&self, mode: SizeMode) -> HashMap<&str, u64> {
        self.folders
            .iter()
            .map(|f| (f.path.as_str(), f.size_in(mode)))
            .collect()
    }
}

/// The `max` biggest folders of `tree`. A folder is never smaller than the
/// folders in it, so with the biggest kept, their parents are kept too.
pub fn take(tree: &Tree, taken: i64, max: usize) -> Snapshot {
    let root = tree.root();
    let mut dirs: Vec<NodeId> = std::iter::once(root)
        .chain(tree.descendants(root))
        .filter(|&id| tree.node(id).is_dir() && !tree.node(id).has(flags::DELETED))
        .collect();
    dirs.sort_by(|&a, &b| {
        tree.node(b)
            .allocated
            .cmp(&tree.node(a).allocated)
            .then(a.cmp(&b))
    });
    let floor = if dirs.len() > max {
        dirs.get(max.saturating_sub(1))
            .map_or(0, |&id| tree.node(id).allocated)
    } else {
        0
    };
    dirs.truncate(max);
    Snapshot {
        taken,
        floor,
        folders: dirs
            .into_iter()
            .map(|id| FolderSize {
                path: relative_path(tree, id),
                size: tree.node(id).size,
                allocated: tree.node(id).allocated,
            })
            .collect(),
    }
}

/// `id`'s path below the tree's root, with `/` between names.
pub fn relative_path(tree: &Tree, id: NodeId) -> String {
    let mut names: Vec<String> = tree
        .ancestors(id)
        .take_while(|&a| a != tree.root())
        .map(|a| tree.name(a).into_owned())
        .collect();
    names.reverse();
    names.join("/")
}

/// The node at `path` (as in a snapshot) in `tree`, if it's still there.
pub fn find(tree: &Tree, path: &str) -> Option<NodeId> {
    let mut id = tree.root();
    for name in path.split('/').filter(|n| !n.is_empty()) {
        id = tree.child_by_name(id, Path::new(name).as_os_str())?;
    }
    Some(id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Grew,
    Shrank,
    New,
    Gone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
    pub before: u64,
    pub after: u64,
}

impl Change {
    pub fn delta(&self) -> i64 {
        self.after as i64 - self.before as i64
    }
}

/// A child explaining at least this share of its folder's change is listed
/// instead of the folder.
const CONCENTRATED: f64 = 0.8;

/// What changed from `old` to `new`, by at least `min_change` bytes, biggest
/// change first.
///
/// Only folders that explain a change are listed: when most of a folder's
/// growth sits in one of its folders, that one is listed instead, so
/// `Pictures/2025/Trip` appears rather than all three levels. New and gone
/// folders are listed at the top of what's new or gone.
pub fn compare(old: &Snapshot, new: &Snapshot, mode: SizeMode, min_change: u64) -> Vec<Change> {
    let before = old.sizes(mode);
    let after = new.sizes(mode);
    let parent = |path: &str| -> Option<String> {
        if path.is_empty() {
            None
        } else {
            Some(path.rsplit_once('/').map_or("", |(p, _)| p).to_string())
        }
    };
    // Every folder in either snapshot, with its children.
    let all: HashSet<&str> = before.keys().chain(after.keys()).copied().collect();
    let mut children: HashMap<String, Vec<&str>> = HashMap::new();
    for &path in &all {
        if let Some(p) = parent(path) {
            children.entry(p).or_default().push(path);
        }
    }
    let delta = |path: &str| -> i64 {
        after.get(path).copied().unwrap_or(0) as i64 - before.get(path).copied().unwrap_or(0) as i64
    };

    let mut out = Vec::new();
    for &path in &all {
        let (was, is) = (before.get(path).copied(), after.get(path).copied());
        let d = delta(path);
        if d.unsigned_abs() < min_change {
            continue;
        }
        let kind = match (was, is) {
            (None, Some(_)) => {
                // Only the top of a new branch.
                if parent(path).is_some_and(|p| !before.contains_key(p.as_str())) {
                    continue;
                }
                ChangeKind::New
            }
            (Some(_), None) => {
                if parent(path).is_some_and(|p| !after.contains_key(p.as_str())) {
                    continue;
                }
                ChangeKind::Gone
            }
            (Some(_), Some(_)) => {
                let concentrated = children.get(path).is_some_and(|kids| {
                    kids.iter().any(|&kid| {
                        let k = delta(kid);
                        k.signum() == d.signum()
                            && k.unsigned_abs() as f64 >= d.unsigned_abs() as f64 * CONCENTRATED
                    })
                });
                if concentrated {
                    continue;
                }
                if d > 0 {
                    ChangeKind::Grew
                } else {
                    ChangeKind::Shrank
                }
            }
            (None, None) => continue,
        };
        out.push(Change {
            path: path.to_string(),
            kind,
            before: was.unwrap_or(0),
            after: is.unwrap_or(0),
        });
    }
    out.sort_by(|a, b| {
        b.delta()
            .unsigned_abs()
            .cmp(&a.delta().unsigned_abs())
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{ScanOptions, ScanProgress, scan};

    fn write(root: &Path, path: &str, bytes: usize) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![1u8; bytes]).unwrap();
    }

    fn snap(root: &Path, taken: i64) -> (Tree, Snapshot) {
        let tree = scan(root, &ScanOptions::default(), &ScanProgress::new())
            .unwrap()
            .tree;
        let snapshot = take(&tree, taken, 1000);
        (tree, snapshot)
    }

    #[test]
    fn lists_the_folders_that_explain_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Pictures/2025/Trip/a.jpg", 1000);
        write(root, "Pictures/2024/b.jpg", 3000);
        write(root, "Old/x.bin", 4000);
        write(root, "Docs/notes.txt", 500);
        let (_, before) = snap(root, 100);
        assert_eq!(before.folders[0].path, "");
        assert_eq!(before.total(SizeMode::Logical), 8500);

        // Growth deep down, a new folder, a deleted one.
        write(root, "Pictures/2025/Trip/c.jpg", 9000);
        write(root, "Music/Album/song.flac", 6000);
        fs::remove_dir_all(root.join("Old")).unwrap();
        let (tree, after) = snap(root, 200);

        let changes = compare(&before, &after, SizeMode::Logical, 100);
        let listed: Vec<(&str, ChangeKind, i64)> = changes
            .iter()
            .map(|c| (c.path.as_str(), c.kind, c.delta()))
            .collect();
        // The root grew by 11,000, but 9,000 of it is in Pictures, which is
        // all in 2025, which is all in Trip: only Trip is listed.
        assert_eq!(
            listed,
            [
                ("Pictures/2025/Trip", ChangeKind::Grew, 9000),
                ("Music", ChangeKind::New, 6000),
                ("Old", ChangeKind::Gone, -4000),
            ]
        );
        assert_eq!(
            find(&tree, "Pictures/2025/Trip").map(|id| tree.name(id).into_owned()),
            Some("Trip".to_string())
        );
        assert_eq!(find(&tree, "Old"), None);
        assert_eq!(find(&tree, ""), Some(tree.root()));
    }

    #[test]
    fn keeps_the_biggest_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "a/big", 5000);
        write(root, "b/small", 10);
        write(root, "c/mid", 900);
        let (_, snapshot) = snap(root, 1);
        let tree = scan(root, &ScanOptions::default(), &ScanProgress::new())
            .unwrap()
            .tree;
        let top = take(&tree, 1, 2);
        assert_eq!(top.folders.len(), 2);
        assert_eq!(top.floor, tree.node(find(&tree, "a").unwrap()).allocated);
        assert_eq!(snapshot.floor, 0);
        assert_eq!(snapshot.trimmed(2), top);
        assert_eq!(top.folders[0].path, "");
        assert_eq!(top.folders[1].path, "a");
        assert_eq!(snapshot.folders.len(), 4);
        // Round trip through JSON.
        let json = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(serde_json::from_str::<Snapshot>(&json).unwrap(), snapshot);
    }
}
