//! Rows of the scan view's Clean up tab: folders tools rebuild, large files
//! nobody has changed in a year, and duplicate files.

use std::path::Path;

use disktree_core::{
    DuplicateGroup, NodeId, SizeMode, Tree, flags, format_count, format_size, old_files,
    rebuildable,
};

use crate::CleanRow;
use crate::insights::date;

const HEADING: i32 = 0;
const ITEM: i32 = 1;
const GROUP: i32 = 2;

/// Rebuildable folders and old files smaller than these aren't listed.
const MIN_FOLDER: u64 = 1024 * 1024;
const MIN_OLD_FILE: u64 = 10 * 1024 * 1024;
/// Duplicate files smaller than this aren't searched for.
pub const MIN_DUPLICATE: u64 = 1024 * 1024;
const MAX_ROWS: usize = 50;
const MAX_GROUPS: usize = 100;
const YEAR: i64 = 365 * 24 * 60 * 60;

/// Where the duplicate search stands.
pub enum Duplicates<'a> {
    NotSearched,
    Searching { done: u64, files: u64 },
    Found(&'a [DuplicateGroup]),
}

/// Every row of the tab.
#[cfg(test)]
pub fn rows(tree: &Tree, mode: SizeMode, now: i64, duplicates: Duplicates) -> Vec<CleanRow> {
    let mut rows = found_rows(tree, mode, now);
    rows.extend(duplicate_rows(tree, duplicates));
    rows
}

/// Rebuildable folders and old files. These walk the whole tree, so the
/// controller keeps them while the duplicate search updates its progress.
pub fn found_rows(tree: &Tree, mode: SizeMode, now: i64) -> Vec<CleanRow> {
    let root = tree.root();
    let root_path = tree.path(root);
    let mut rows = Vec::new();

    let mut folders: Vec<_> = rebuildable(tree, root)
        .into_iter()
        .filter(|r| tree.node(r.node).size_in(mode) >= MIN_FOLDER)
        .collect();
    folders.sort_by_key(|r| std::cmp::Reverse(tree.node(r.node).size_in(mode)));
    let total: u64 = folders
        .iter()
        .map(|r| tree.node(r.node).size_in(mode))
        .sum();
    rows.push(heading(
        "Rebuildable folders",
        &if folders.is_empty() {
            "None found in this scan.".to_string()
        } else {
            format!(
                "{} in folders that tools make again when they're needed. Select one, then use \
                 Delete.",
                format_size(total)
            )
        },
        "",
    ));
    rows.extend(folders.iter().take(MAX_ROWS).map(|r| CleanRow {
        kind: ITEM,
        id: r.node as i32,
        title: format!("{} · {}", tree.name(r.node), r.what).into(),
        subtitle: format!("{} · {}", place(tree, &root_path, r.node), r.how).into(),
        detail: format_size(tree.node(r.node).size_in(mode)).into(),
        ..Default::default()
    }));

    let old: Vec<NodeId> = old_files(tree, root, mode, now - YEAR, MAX_ROWS)
        .into_iter()
        .filter(|&id| tree.node(id).size_in(mode) >= MIN_OLD_FILE)
        .collect();
    rows.push(heading(
        "Not changed in over a year",
        if old.is_empty() {
            "No large files that old."
        } else {
            "The largest files nobody has modified for a year or more."
        },
        "",
    ));
    rows.extend(old.iter().map(|&id| {
        let node = tree.node(id);
        CleanRow {
            kind: ITEM,
            id: id as i32,
            title: tree.name(id).as_ref().into(),
            subtitle: format!(
                "{} · last changed {}",
                place(tree, &root_path, node.parent),
                date(node.modified.max(0) as u64)
            )
            .into(),
            detail: format_size(node.size_in(mode)).into(),
            ..Default::default()
        }
    }));

    rows
}

/// The duplicate files section.
pub fn duplicate_rows(tree: &Tree, duplicates: Duplicates) -> Vec<CleanRow> {
    let root_path = tree.path(tree.root());
    let mut rows = Vec::new();
    match duplicates {
        Duplicates::NotSearched => rows.push(heading(
            "Duplicate files",
            "Finds files of 1 MB or more with identical contents. It reads the files, so it \
             can take a while.",
            "Find duplicates",
        )),
        Duplicates::Searching { done, files } => rows.push(heading(
            "Duplicate files",
            &format!(
                "Comparing files: {} of {}…",
                format_count(done),
                format_count(files)
            ),
            "Stop",
        )),
        Duplicates::Found(groups) => {
            // Leave out copies deleted since the search.
            let groups: Vec<(u64, Vec<NodeId>)> = groups
                .iter()
                .map(|g| {
                    let left: Vec<NodeId> = g
                        .nodes
                        .iter()
                        .copied()
                        .filter(|&id| !tree.ancestors(id).any(|a| tree.node(a).has(flags::DELETED)))
                        .collect();
                    (g.size, left)
                })
                .filter(|(_, left)| left.len() > 1)
                .collect();
            let wasted: u64 = groups
                .iter()
                .map(|(size, nodes)| size * (nodes.len() as u64 - 1))
                .sum();
            rows.push(heading(
                "Duplicate files",
                &if groups.is_empty() {
                    "No duplicate files of 1 MB or more.".to_string()
                } else {
                    format!(
                        "{} of identical files. Keeping one copy of each frees {}.",
                        match groups.len() {
                            1 => "1 set".to_string(),
                            n => format!("{} sets", format_count(n as u64)),
                        },
                        format_size(wasted)
                    )
                },
                "Search again",
            ));
            for (size, nodes) in groups.iter().take(MAX_GROUPS) {
                rows.push(CleanRow {
                    kind: GROUP,
                    title: format!("{} copies · {} each", nodes.len(), format_size(*size)).into(),
                    ..Default::default()
                });
                rows.extend(nodes.iter().map(|&id| CleanRow {
                    kind: ITEM,
                    id: id as i32,
                    title: tree.name(id).as_ref().into(),
                    subtitle: place(tree, &root_path, tree.node(id).parent).into(),
                    ..Default::default()
                }));
            }
        }
    }
    rows
}

fn heading(title: &str, subtitle: &str, action: &str) -> CleanRow {
    CleanRow {
        kind: HEADING,
        id: -1,
        title: title.into(),
        subtitle: subtitle.into(),
        action: action.into(),
        ..Default::default()
    }
}

/// A folder's path relative to the scanned folder, or the scanned folder's
/// name for itself.
fn place(tree: &Tree, root_path: &Path, id: NodeId) -> String {
    let path = tree.path(id);
    match path.strip_prefix(root_path) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().into_owned(),
        _ => root_path.file_name().map_or_else(
            || root_path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use disktree_core::{DuplicateProgress, ScanOptions, ScanProgress, find_duplicates, scan};

    use super::*;

    fn write(root: &Path, path: &str, bytes: usize, fill: u8) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![fill; bytes]).unwrap();
    }

    #[test]
    fn lists_folders_and_duplicates() {
        const MB: usize = 1024 * 1024;
        let dir = std::env::temp_dir().join(format!("disktree-clean-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write(&dir, "web/package.json", 2, b'{');
        write(&dir, "web/node_modules/big.js", 2 * MB, 1);
        write(&dir, "a/video.mp4", 3 * MB, 7);
        write(&dir, "b/video.mp4", 3 * MB, 7);
        let tree = scan(&dir, &ScanOptions::default(), &ScanProgress::new())
            .unwrap()
            .tree;
        let now = 1_790_000_000;

        let before = rows(&tree, SizeMode::Logical, now, Duplicates::NotSearched);
        let titles: Vec<_> = before.iter().map(|r| r.title.to_string()).collect();
        assert_eq!(titles[0], "Rebuildable folders");
        assert_eq!(titles[1], "node_modules · npm packages");
        assert_eq!(
            before[1].subtitle,
            "web/node_modules · npm install puts them back"
        );
        assert_eq!(before.last().unwrap().action, "Find duplicates");

        let groups = find_duplicates(&tree, tree.root(), MIN_DUPLICATE, &DuplicateProgress::new());
        let after = rows(&tree, SizeMode::Logical, now, Duplicates::Found(&groups));
        let dupes = after
            .iter()
            .position(|r| r.title == "Duplicate files")
            .unwrap();
        assert_eq!(
            after[dupes].subtitle,
            "1 set of identical files. Keeping one copy of each frees 3.00 MB."
        );
        assert_eq!(after[dupes + 1].title, "2 copies · 3.00 MB each");
        let mut folders: Vec<_> = after[dupes + 2..]
            .iter()
            .map(|r| r.subtitle.to_string())
            .collect();
        folders.sort();
        assert_eq!(folders, ["a", "b"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
