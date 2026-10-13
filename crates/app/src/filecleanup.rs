//! Rows of the scan view's Clean up tab: folders tools rebuild, large files
//! nobody has changed in a year, and duplicate files.

use std::path::Path;

use freesize_core::{
    DuplicateGroup, NodeId, Safety, SizeMode, Spot, Tree, flags, format_count, format_size,
    old_files, rebuildable,
};

use crate::CleanRow;
use crate::elevate::Offer;
use crate::insights::date;
use crate::similar;

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

/// Where the search for similar photos stands. Found groups come with a
/// preview for each photo, where one was made.
pub enum Similar<'a> {
    NotSearched,
    Searching {
        done: u64,
        files: u64,
    },
    Found(
        &'a [similar::Group],
        &'a dyn Fn(NodeId) -> Option<slint::Image>,
    ),
}

/// Titles of the sections with a button, which tell the buttons apart.
pub const DUPLICATES: &str = "Duplicate files";
pub const SIMILAR: &str = "Similar photos";
pub const UNREADABLE: &str = "Folders FreeSize couldn't read";

/// Unreadable folders listed under their heading.
const MAX_UNREADABLE: usize = 20;

/// Space no file accounts for on a scanned drive: the drive's used space
/// when it was scanned, and what the system said about it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hidden {
    pub used: u64,
    pub notes: Vec<String>,
}

/// Every row of the tab.
#[cfg(test)]
pub fn rows(tree: &Tree, mode: SizeMode, now: i64, duplicates: Duplicates) -> Vec<CleanRow> {
    let mut rows = found_rows(tree, mode, now, &[], None, &Offer::default());
    rows.extend(duplicate_rows(tree, duplicates));
    rows
}

/// Rebuildable folders and old files. These walk the whole tree, so the
/// controller keeps them while the duplicate search updates its progress.
pub fn found_rows(
    tree: &Tree,
    mode: SizeMode,
    now: i64,
    spots: &[Spot],
    hidden: Option<&Hidden>,
    offer: &Offer,
) -> Vec<CleanRow> {
    let root = tree.root();
    let root_path = tree.path(root);
    let mut rows = Vec::new();

    rows.extend(unreadable_rows(tree, &root_path, offer));

    if !spots.is_empty() {
        let total: u64 = spots.iter().map(|s| tree.node(s.node).size_in(mode)).sum();
        rows.push(heading(
            "System and app caches",
            &format!(
                "{} where the system and apps keep files they can do without. Each says how to \
                 remove it; some should go through the system's own cleanup.",
                format_size(total)
            ),
            "",
        ));
        rows.extend(spots.iter().take(MAX_ROWS).map(|spot| {
            let advice = match spot.safety {
                Safety::Safe => spot.how.to_string(),
                Safety::CheckFirst => format!("check first: {}", spot.how),
                Safety::UseTool => format!("don't delete: {}", spot.how),
            };
            CleanRow {
                kind: ITEM,
                id: spot.node as i32,
                title: {
                    let name = tree.name(spot.node);
                    if name.eq_ignore_ascii_case(spot.what) {
                        name.into_owned()
                    } else {
                        format!("{name} · {}", spot.what)
                    }
                }
                .into(),
                subtitle: format!("{} · {advice}", place(tree, &root_path, spot.node)).into(),
                detail: format_size(tree.node(spot.node).size_in(mode)).into(),
                ..Default::default()
            }
        }));
    }

    if let Some(hidden) = hidden {
        let scanned = tree.node(root).allocated;
        let gap = hidden.used.saturating_sub(scanned);
        if gap >= (hidden.used / 50).max(1024 * 1024 * 1024) {
            let unreadable = std::iter::once(root)
                .chain(tree.descendants(root))
                .filter(|&id| tree.node(id).has(flags::ERROR))
                .count();
            let mut text = hidden.notes.join(" ");
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(
                "The rest is space the file system reserves or uses for its own records, and \
                 other users' files",
            );
            if unreadable > 0 {
                text.push_str(&format!(
                    ", and {unreadable} folder{} FreeSize couldn't read",
                    if unreadable == 1 { "" } else { "s" }
                ));
            }
            text.push('.');
            rows.push(heading(
                &format!("{} not in any file", format_size(gap)),
                &text,
                "",
            ));
        }
    }

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
            DUPLICATES,
            "Finds files of 1 MB or more with identical contents. It reads the files, so it \
             can take a while.",
            "Find duplicates",
        )),
        Duplicates::Searching { done, files } => rows.push(heading(
            DUPLICATES,
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
                DUPLICATES,
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

/// The similar photos section.
pub fn similar_rows(tree: &Tree, similar: Similar) -> Vec<CleanRow> {
    let root_path = tree.path(tree.root());
    let mut rows = Vec::new();
    let (groups, preview) = match similar {
        Similar::NotSearched => {
            rows.push(heading(
                SIMILAR,
                "Finds photos that look alike, such as bursts, retakes and pictures saved \
                 twice at different sizes, so you can keep the best of each. It looks at every \
                 photo, so it can take a while.",
                "Find similar photos",
            ));
            return rows;
        }
        Similar::Searching { done, files } => {
            rows.push(heading(
                SIMILAR,
                &format!(
                    "Looking at photos: {} of {}…",
                    format_count(done),
                    format_count(files)
                ),
                "Stop",
            ));
            return rows;
        }
        Similar::Found(groups, preview) => (groups, preview),
    };
    // Leave out photos deleted since the search.
    let groups: Vec<Vec<(NodeId, u64)>> = groups
        .iter()
        .map(|g| {
            g.photos
                .iter()
                .copied()
                .zip(g.sizes.iter().copied())
                .filter(|&(id, _)| !tree.ancestors(id).any(|a| tree.node(a).has(flags::DELETED)))
                .collect::<Vec<_>>()
        })
        .filter(|left| left.len() > 1)
        .collect();
    let extra: u64 = groups
        .iter()
        .map(|g| g[1..].iter().map(|&(_, size)| size).sum::<u64>())
        .sum();
    rows.push(heading(
        SIMILAR,
        &if groups.is_empty() {
            "No photos that look alike.".to_string()
        } else {
            format!(
                "{} of photos that look alike. Keeping only the largest of each would free \
                 {}, but similar isn't identical: open them to choose.",
                match groups.len() {
                    1 => "1 set".to_string(),
                    n => format!("{} sets", format_count(n as u64)),
                },
                format_size(extra)
            )
        },
        "Search again",
    ));
    for photos in groups.iter().take(MAX_GROUPS) {
        let total: u64 = photos.iter().map(|&(_, size)| size).sum();
        rows.push(CleanRow {
            kind: GROUP,
            title: format!("{} photos · {}", photos.len(), format_size(total)).into(),
            ..Default::default()
        });
        rows.extend(photos.iter().enumerate().map(|(i, &(id, size))| {
            let image = preview(id);
            let place = place(tree, &root_path, tree.node(id).parent);
            CleanRow {
                kind: ITEM,
                id: id as i32,
                title: tree.name(id).as_ref().into(),
                subtitle: if i == 0 {
                    format!("{place} · largest")
                } else {
                    place
                }
                .into(),
                detail: format_size(size).into(),
                has_image: image.is_some(),
                image: image.unwrap_or_default(),
                ..Default::default()
            }
        }));
    }
    rows
}

/// The folders the scan couldn't list, with what a fuller scan would take.
fn unreadable_rows(tree: &Tree, root_path: &Path, offer: &Offer) -> Vec<CleanRow> {
    let missed = tree.unreadable();
    if missed.is_empty() {
        return Vec::new();
    }
    let mut text = format!(
        "{} folder{} and what's in {} are missing from this scan, so the totals are low. ",
        format_count(missed.len() as u64),
        if missed.len() == 1 { "" } else { "s" },
        if missed.len() == 1 { "it" } else { "them" },
    );
    text.push_str(&offer.note);
    let mut rows = vec![heading(UNREADABLE, &text, &offer.action)];
    rows.extend(missed.iter().take(MAX_UNREADABLE).map(|&id| CleanRow {
        kind: ITEM,
        id: id as i32,
        title: tree.name(id).into_owned().into(),
        subtitle: format!("{} · no access", place(tree, root_path, id)).into(),
        detail: "".into(),
        ..Default::default()
    }));
    if missed.len() > MAX_UNREADABLE {
        rows.push(CleanRow {
            kind: GROUP,
            id: -1,
            title: format!(
                "… and {} more",
                format_count((missed.len() - MAX_UNREADABLE) as u64)
            )
            .into(),
            ..Default::default()
        });
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

    use freesize_core::{DuplicateProgress, ScanOptions, ScanProgress, find_duplicates, scan};

    use super::*;

    fn write(root: &Path, path: &str, bytes: usize, fill: u8) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![fill; bytes]).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_folder_is_counted_marked_and_named() {
        use std::os::unix::fs::PermissionsExt;
        if freesize_core::access::rights().sees_everything() {
            return; // root reads it anyway
        }
        let dir = std::env::temp_dir().join(format!("freesize-noaccess-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write(&dir, "open/readme.txt", 10, b'x');
        write(&dir, "closed/secret.txt", 10, b'x');
        fs::set_permissions(dir.join("closed"), fs::Permissions::from_mode(0o000)).unwrap();
        let scanned = scan(&dir, &ScanOptions::default(), &ScanProgress::new()).unwrap();
        let tree = &scanned.tree;
        assert_eq!(scanned.errors, 1);
        let closed = tree.unreadable();
        assert_eq!(closed.len(), 1);
        assert!(tree.node(closed[0]).has(flags::ERROR));
        assert_eq!(tree.name(closed[0]), "closed");

        let offer = Offer {
            action: "Scan as root".into(),
            note: "Needs root.".into(),
        };
        let rows = found_rows(tree, SizeMode::Logical, 1_790_000_000, &[], None, &offer);
        assert_eq!(rows[0].title, UNREADABLE);
        assert!(
            rows[0]
                .subtitle
                .starts_with("1 folder and what's in it are missing")
        );
        assert!(rows[0].subtitle.ends_with("Needs root."));
        assert_eq!(rows[0].action, "Scan as root");
        assert_eq!(rows[1].title, "closed");
        assert_eq!(rows[1].subtitle, "closed · no access");
        assert_eq!(rows[1].id, closed[0] as i32);

        fs::set_permissions(dir.join("closed"), fs::Permissions::from_mode(0o755)).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lists_folders_and_duplicates() {
        const MB: usize = 1024 * 1024;
        let dir = std::env::temp_dir().join(format!("freesize-clean-{}", std::process::id()));
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
