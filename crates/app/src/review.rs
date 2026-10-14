//! Reviewing a folder before deleting it, such as an old backup of a PC: a
//! page of the Clean up tab with tabs for its photos (a grid of previews),
//! documents, videos, the files worth a look first and everything else.
//! Looking inside files for words and comparing with the rest of the scan
//! run on this device when asked; nothing is sent anywhere.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use freesize_agent::review as contents;
use freesize_core::review::{self as sorting, Group};
use freesize_core::{NodeId, SizeMode, Tree, flags, format_count, format_size};
use serde::{Deserialize, Serialize};
use slint::Image;

use crate::CleanRow;
use crate::filecleanup::{REVIEW, Section};
use crate::insights::date;

/// The tabs, as the page's drop-down shows them.
pub const TABS: [&str; 5] = [
    "Photos",
    "Documents",
    "Videos",
    "Worth a look",
    "Everything else",
];
pub const PHOTOS: usize = 0;
const IMPORTANT: usize = 3;
/// Rows listed per tab, biggest first; photos shown as previews.
const MAX_ROWS: usize = 500;
pub const MAX_PHOTOS: usize = 400;

const ITEM: i32 = 1;
const GROUP: i32 = 2;

/// The settings key of the word list in Settings.
pub const WORDS_KEY: &str = "review-words";

/// The words looked for inside files, kept in the data folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub words: Vec<String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            words: contents::DEFAULT_WORDS
                .iter()
                .map(|w| w.to_string())
                .collect(),
        }
    }
}

impl Settings {
    fn file() -> Option<PathBuf> {
        Some(crate::growth::data_dir()?.join("review.json"))
    }

    pub fn load() -> Settings {
        Settings::file()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(file) = Settings::file()
            && let Ok(text) = serde_json::to_string_pretty(self)
        {
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(file, text);
        }
    }
}

/// A folder under review, and what was found in it so far.
pub struct Review {
    pub folder: NodeId,
    pub tab: usize,
    /// Per file, the words found in it; `None` until looked.
    pub words: Option<HashMap<NodeId, Vec<String>>>,
    /// The words that were looked for.
    pub looked_for: Vec<String>,
    /// Per file kept elsewhere too, one of its other copies; `None` until
    /// compared.
    pub copies: Option<HashMap<NodeId, NodeId>>,
    /// Whether anything outside the folder could be compared with.
    pub compared_any: bool,
    pub previews: HashMap<NodeId, Image>,
    /// The page as last made, and what it was made from.
    cache: Option<(Key, Section)>,
}

/// What the page depends on, besides the review's results: deletions
/// change the files counted below the folder and the scan's root.
type Key = (usize, SizeMode, bool, bool, usize, u32, u32);

impl Review {
    /// The page, made again only when something it shows changed.
    pub fn page(&mut self, tree: &Tree, mode: SizeMode, root_path: &Path) -> Section {
        let key = (
            self.tab,
            mode,
            self.words.is_some(),
            self.copies.is_some(),
            self.previews.len(),
            tree.node(self.folder).files,
            tree.node(tree.root()).files,
        );
        if let Some((cached, page)) = &self.cache
            && *cached == key
        {
            return page.clone();
        }
        let page = section(tree, mode, self, root_path);
        self.cache = Some((key, page.clone()));
        page
    }

    pub fn new(folder: NodeId) -> Review {
        Review {
            folder,
            tab: PHOTOS,
            words: None,
            looked_for: Vec::new(),
            copies: None,
            compared_any: false,
            previews: HashMap::new(),
            cache: None,
        }
    }
}

/// Searches running for the review: (done, of how many).
#[derive(Clone, Copy, Default)]
pub struct Running {
    pub words: Option<(u64, u64)>,
    pub copies: Option<(u64, u64)>,
}

/// Whether a node or a folder it's in was deleted.
fn gone(tree: &Tree, id: NodeId) -> bool {
    std::iter::once(id)
        .chain(tree.ancestors(id))
        .any(|a| tree.node(a).has(flags::DELETED))
}

/// The folder's files still there, biggest first.
pub fn files(tree: &Tree, folder: NodeId, mode: SizeMode) -> Vec<NodeId> {
    sorting::files(tree, folder, mode)
        .into_iter()
        .filter(|&id| !gone(tree, id))
        .collect()
}

/// The photos shown on the Photos tab, in the order shown: by folder, then
/// by name.
pub fn photos(tree: &Tree, folder: NodeId, mode: SizeMode) -> Vec<NodeId> {
    let mut photos: Vec<(PathBuf, NodeId)> = files(tree, folder, mode)
        .into_iter()
        .filter(|&id| sorting::group_of(&tree.name(id)) == Group::Photos)
        .map(|id| (tree.path(id), id))
        .collect();
    photos.sort();
    photos.into_iter().map(|(_, id)| id).collect()
}

/// The files that can be looked inside, with their paths.
pub fn searchable(tree: &Tree, folder: NodeId, mode: SizeMode) -> Vec<(NodeId, PathBuf)> {
    files(tree, folder, mode)
        .into_iter()
        .filter(|&id| !tree.node(id).has(flags::CLOUD))
        .map(|id| (id, tree.path(id)))
        .filter(|(_, path)| contents::searchable(path))
        .collect()
}

/// The review's tab of a file.
fn tab_of(tree: &Tree, id: NodeId, words: Option<&HashMap<NodeId, Vec<String>>>) -> [bool; 5] {
    let name = tree.name(id);
    let group = sorting::group_of(&name);
    let important = sorting::important(&name, &tree.path(id).to_string_lossy()).is_some()
        || words.is_some_and(|w| w.contains_key(&id));
    [
        group == Group::Photos,
        group == Group::Documents,
        group == Group::Videos,
        important,
        group == Group::Other,
    ]
}

/// The page.
pub fn section(tree: &Tree, mode: SizeMode, review: &Review, root_path: &Path) -> Section {
    let folder = review.folder;
    let all = files(tree, folder, mode);
    let size = |id: NodeId| tree.node(id).size_in(mode);
    let total: u64 = all.iter().map(|&id| size(id)).sum();
    let words = review.words.as_ref();
    let in_tab: Vec<[bool; 5]> = all.iter().map(|&id| tab_of(tree, id, words)).collect();
    let tab = review.tab.min(TABS.len() - 1);
    let shown: Vec<NodeId> = all
        .iter()
        .zip(&in_tab)
        .filter(|(_, tabs)| tabs[tab])
        .map(|(&id, _)| id)
        .collect();

    let filters = TABS
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let (n, bytes) = all
                .iter()
                .zip(&in_tab)
                .filter(|(_, tabs)| tabs[i])
                .fold((0u64, 0u64), |(n, b), (&id, _)| (n + 1, b + size(id)));
            (
                i.to_string(),
                format!("{label} · {}, {}", files_word(n), format_size(bytes)),
            )
        })
        .collect();

    let name = if folder == tree.root() {
        root_path.file_name().map_or_else(
            || root_path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    } else {
        tree.name(folder).into_owned()
    };
    let mut blurb = format!(
        "{}, {}, in {}. Look through them before deleting the folder; Open shows the one \
         highlighted. Nothing goes unless you select it.",
        files_word(all.len() as u64),
        format_size(total),
        place(tree, root_path, folder)
    );
    if tab == IMPORTANT {
        blurb.push_str(
            " These are the files people most often miss: documents, spreadsheets, password \
             vaults, keys, wallets, tax files, email and camera photos.",
        );
    }

    // Compared with the rest of the scan.
    let mut action = String::new();
    let mut picks = Vec::new();
    match &review.copies {
        None => action = "Compare with my current files".into(),
        Some(_) if !review.compared_any => blurb.push_str(
            "\n\nNone of these files is anywhere else in this scan. To compare with more, scan \
             a bigger folder, such as the whole drive, and review this folder there.",
        ),
        Some(copies) => {
            let kept: Vec<NodeId> = shown
                .iter()
                .copied()
                .filter(|id| copies.contains_key(id))
                .collect();
            let noun = if tab == PHOTOS { "photos" } else { "files" };
            blurb.push_str(&format!(
                "\n\n{} of these {} {noun} ({}%) are kept elsewhere too{}, by content. The ones \
                 marked Only here aren't anywhere else in the scan.",
                format_count(kept.len() as u64),
                format_count(shown.len() as u64),
                kept.len() * 100 / shown.len().max(1),
                most_in(tree, root_path, &kept, copies)
                    .map(|p| format!(", most in {p}"))
                    .unwrap_or_default()
            ));
            picks = kept;
        }
    }

    // Looked inside.
    let mut action2 = String::new();
    match words {
        None => action2 = "Look inside files".into(),
        Some(found) => {
            let listed = review.looked_for.join(", ");
            blurb.push_str(&if found.is_empty() {
                format!("\n\nLooked inside for {listed}: no file here contains any of them.")
            } else {
                format!(
                    "\n\nLooked inside for {listed}: {} {} some of them, listed under Worth a \
                     look. Only the words are shown, never the text around them.",
                    files_word(found.len() as u64),
                    if found.len() == 1 {
                        "contains"
                    } else {
                        "contain"
                    }
                )
            });
        }
    }

    let only_here = |id: NodeId| review.copies.as_ref().is_some_and(|c| !c.contains_key(&id));
    let mut rows = Vec::new();
    if tab == PHOTOS {
        let photos: HashSet<NodeId> = shown.iter().copied().collect();
        let ordered: Vec<NodeId> = self::photos(tree, folder, mode)
            .into_iter()
            .filter(|id| photos.contains(id))
            .collect();
        let mut by_folder: BTreeMap<String, Vec<NodeId>> = BTreeMap::new();
        for &id in ordered.iter().take(MAX_PHOTOS) {
            by_folder
                .entry(place(tree, root_path, tree.node(id).parent))
                .or_default()
                .push(id);
        }
        for (place, ids) in by_folder {
            let bytes: u64 = ids.iter().map(|&id| size(id)).sum();
            rows.push(group(format!(
                "{place} · {} photo{}, {}",
                format_count(ids.len() as u64),
                if ids.len() == 1 { "" } else { "s" },
                format_size(bytes)
            )));
            rows.extend(ids.into_iter().map(|id| {
                let image = review.previews.get(&id).cloned();
                CleanRow {
                    kind: ITEM,
                    id: id as i32,
                    title: tree.name(id).as_ref().into(),
                    subtitle: date(tree.node(id).modified.max(0) as u64).into(),
                    detail: format_size(size(id)).into(),
                    has_image: image.is_some(),
                    image: image.unwrap_or_default(),
                    checkable: true,
                    badge: if only_here(id) { "Only here" } else { "" }.into(),
                    ..Default::default()
                }
            }));
        }
        if ordered.len() > MAX_PHOTOS {
            rows.push(group(format!(
                "… and {} more photos; Largest files lists them all",
                format_count((ordered.len() - MAX_PHOTOS) as u64)
            )));
        }
    } else {
        let row = |id: NodeId, extra: &str| {
            let node = tree.node(id);
            let mut subtitle = format!(
                "{} · changed {}",
                place(tree, root_path, node.parent),
                date(node.modified.max(0) as u64)
            );
            if !extra.is_empty() {
                subtitle = format!("{extra} · {subtitle}");
            }
            CleanRow {
                kind: ITEM,
                id: id as i32,
                title: tree.name(id).as_ref().into(),
                subtitle: subtitle.into(),
                detail: format_size(size(id)).into(),
                checkable: true,
                badge: if only_here(id) { "Only here" } else { "" }.into(),
                warn: only_here(id),
                ..Default::default()
            }
        };
        let words_of = |id: NodeId| {
            words
                .and_then(|w| w.get(&id))
                .map(|found| format!("contains {}", found.join(", ")))
                .unwrap_or_default()
        };
        if tab == IMPORTANT {
            // Grouped by why, files with words first.
            let mut groups: Vec<(String, Vec<NodeId>)> = Vec::new();
            for &id in &shown {
                let why = if words.is_some_and(|w| w.contains_key(&id)) {
                    "Contain words you look for"
                } else {
                    sorting::important(&tree.name(id), &tree.path(id).to_string_lossy())
                        .unwrap_or("Other")
                };
                match groups.iter_mut().find(|(w, _)| w == why) {
                    Some((_, ids)) => ids.push(id),
                    None => groups.push((why.to_string(), vec![id])),
                }
            }
            groups.sort_by_key(|(why, _)| order(why));
            let mut listed = 0;
            for (why, ids) in groups {
                rows.push(group(format!("{why} · {}", files_word(ids.len() as u64))));
                for &id in ids.iter().take(MAX_ROWS.saturating_sub(listed)) {
                    rows.push(row(id, &words_of(id)));
                    listed += 1;
                }
            }
        } else {
            for &id in shown.iter().take(MAX_ROWS) {
                rows.push(row(id, &words_of(id)));
            }
        }
        if shown.len() > MAX_ROWS {
            rows.push(group(format!(
                "… and {} more, smaller ones",
                format_count((shown.len() - MAX_ROWS) as u64)
            )));
        }
    }
    if shown.is_empty() {
        rows.push(group(format!("No {} here.", TABS[tab].to_lowercase())));
    }

    Section {
        key: REVIEW,
        title: format!("Review: {name}"),
        blurb,
        note: format!("{}, {}", files_word(all.len() as u64), format_size(total)),
        action,
        action2,
        rows,
        picks,
        pick_label: "Select the ones kept elsewhere",
        filters,
        filter_index: Some(tab),
        ..Default::default()
    }
}

/// Adds the searches still running to a page made by [`section`].
pub fn show_running(section: &mut Section, running: Running) {
    if let Some((done, of)) = running.copies {
        section.blurb.push_str(&format!(
            "\n\nComparing with your other files: {} of {}…",
            format_count(done),
            format_count(of)
        ));
        section.action = "Stop comparing".into();
    }
    if let Some((done, of)) = running.words {
        section.blurb.push_str(&format!(
            "\n\nLooking inside files: {} of {}…",
            format_count(done),
            format_count(of)
        ));
        section.action2 = "Stop looking".into();
    }
}

/// The order of the Worth a look groups.
fn order(why: &str) -> usize {
    [
        "Contain words you look for",
        "Password vault",
        "Key or certificate",
        "Crypto wallet",
        "Tax or accounting",
        "Email",
        "Document",
        "Spreadsheet",
        "Camera photo",
    ]
    .iter()
    .position(|w| *w == why)
    .unwrap_or(99)
}

/// Where most of the other copies are: the folder holding the most.
fn most_in(
    tree: &Tree,
    root_path: &Path,
    kept: &[NodeId],
    copies: &HashMap<NodeId, NodeId>,
) -> Option<String> {
    let mut counts: HashMap<NodeId, usize> = HashMap::new();
    for id in kept {
        let other = *copies.get(id)?;
        *counts.entry(tree.node(other).parent).or_default() += 1;
    }
    let (&parent, &n) = counts
        .iter()
        .max_by_key(|(id, n)| (**n, std::cmp::Reverse(**id)))?;
    // Only worth saying when it's most of them.
    (n * 2 >= kept.len()).then(|| place(tree, root_path, parent))
}

fn group(title: String) -> CleanRow {
    CleanRow {
        kind: GROUP,
        id: -1,
        title: title.into(),
        ..Default::default()
    }
}

fn files_word(n: u64) -> String {
    if n == 1 {
        "1 file".into()
    } else {
        format!("{} files", format_count(n))
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

    use freesize_core::{DuplicateProgress, ScanOptions, ScanProgress, scan};

    use super::*;

    fn write(root: &Path, path: &str, data: &[u8]) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }

    #[test]
    fn tabs_words_and_copies() {
        let dir = std::env::temp_dir().join(format!("freesize-review-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let root = dir.as_path();
        write(root, "Backup/DCIM/IMG_0001.jpg", &[1u8; 80_000]);
        write(root, "Backup/DCIM/IMG_0002.jpg", &[2u8; 90_000]);
        write(root, "Pictures/IMG_0001.jpg", &[1u8; 80_000]);
        write(root, "Backup/Docs/letter.txt", b"About my passport renewal");
        write(root, "Backup/vault.kdbx", &[3u8; 100]);
        write(root, "Backup/setup.exe", &[4u8; 5000]);
        let tree = scan(root, &ScanOptions::default(), &ScanProgress::new())
            .unwrap()
            .tree;
        let root_path = tree.path(tree.root());
        let backup = freesize_core::snapshot::find(&tree, "Backup").unwrap();
        let mode = SizeMode::Logical;
        let mut review = Review::new(backup);

        let page = section(&tree, mode, &review, &root_path);
        assert_eq!(page.title, "Review: Backup");
        assert_eq!(page.filter_index, Some(PHOTOS));
        assert!(page.filters[0].1.starts_with("Photos · 2 files"));
        assert!(page.filters[3].1.starts_with("Worth a look · 4 files"));
        assert_eq!(page.action, "Compare with my current files");
        assert_eq!(page.action2, "Look inside files");
        // A group per folder, then its photos.
        assert_eq!(page.rows[0].kind, GROUP);
        assert!(
            page.rows[0]
                .title
                .replace('\\', "/")
                .starts_with("Backup/DCIM · 2 photos")
        );
        assert_eq!(page.rows.len(), 3);

        // Words found, and copies compared.
        let found: HashMap<NodeId, Vec<String>> = searchable(&tree, backup, mode)
            .into_iter()
            .filter_map(|(id, path)| {
                let words = contents::words_in(&path, &["passport".to_string()]);
                (!words.is_empty()).then_some((id, words))
            })
            .collect();
        assert_eq!(found.len(), 1);
        review.words = Some(found);
        review.looked_for = vec!["passport".into()];
        let candidates = sorting::copy_candidates(&tree, backup);
        review.compared_any = candidates.files() > 0;
        review.copies = Some(candidates.find(&DuplicateProgress::new()));

        let page = section(&tree, mode, &review, &root_path);
        assert!(
            page.blurb
                .contains("1 of these 2 photos (50%) are kept elsewhere too, most in Pictures"),
            "{}",
            page.blurb
        );
        assert!(
            page.blurb
                .contains("Looked inside for passport: 1 file contains some of them")
        );
        let badges: Vec<_> = page.rows.iter().map(|r| r.badge.to_string()).collect();
        assert_eq!(badges, ["", "", "Only here"]);
        assert_eq!(page.picks.len(), 1);

        review.tab = IMPORTANT;
        let page = section(&tree, mode, &review, &root_path);
        let titles: Vec<_> = page.rows.iter().map(|r| r.title.to_string()).collect();
        assert_eq!(titles[0], "Contain words you look for · 1 file");
        assert_eq!(titles[1], "letter.txt");
        assert!(page.rows[1].subtitle.starts_with("contains passport · "));
        assert_eq!(titles[2], "Password vault · 1 file");
        assert_eq!(titles[4], "Camera photo · 2 files");

        review.tab = 2;
        let page = section(&tree, mode, &review, &root_path);
        assert_eq!(page.rows[0].title, "No videos here.");
        let _ = fs::remove_dir_all(&dir);
    }
}
