//! The app's quarantine: deleting moves files aside on the same disk, where
//! Put back restores them byte for byte; the space is freed only when
//! someone empties it. Shared with the Hub's requests, so a delete from the
//! dashboard lands in the same place as one made here.
//!
//! On a desktop the quarantine for the disk FreeSize's data lives on is in
//! that data folder, which the person owns; other disks (and the phone's
//! shared storage) get `.disktree-quarantine` at their top, as servers do.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

pub use freesize_agent::quarantine::Conflict;
use freesize_agent::quarantine::{MountPlaces, Moved, Place, Places, QUARANTINE_DIR, Quarantine};
use freesize_core::format_size;
use freesize_proto::unraid::QuarantineBatch;

use crate::insights::ago;
use crate::{CleanRow, QuarantineRow};

/// The Clean up tab's section, and the id of its button.
pub const HEADING: &str = "Quarantine";
/// Prefixes of the Clean up tab's per-batch buttons (the batch id follows).
pub const RESTORE_PREFIX: &str = "restore ";
pub const EMPTY_PREFIX: &str = "empty ";
/// Prefix of the batch cards' key: opens the batch's own page.
pub const OPEN_PREFIX: &str = "open-batch ";
/// The heading's button: empties every batch, after the confirmation.
pub const EMPTY_ALL: &str = "Empty all";
/// The home screen's button for the same.
pub const EMPTY_ALL_ACTION: &str = "quarantine-empty-all";

const BATCH: i32 = 3;

/// Where quarantines are on this device.
struct DevicePlaces {
    mounts: MountPlaces,
    /// The quarantine in FreeSize's data folder, and the top of the disk
    /// that folder is on.
    own: Option<(PathBuf, PathBuf)>,
    /// Phones: the storage roots a quarantine goes at the top of
    /// (`/storage/emulated/0`, SD cards). Their filesystem's own top is
    /// above them (`/storage/emulated`), where nothing can be written.
    tops: Vec<PathBuf>,
    extra: Mutex<Vec<PathBuf>>,
}

/// The quarantine for `path` when it's under one of `tops` that sits below
/// the filesystem's top: at that storage root rather than the filesystem's.
fn quarantine_in_top(tops: &[PathBuf], path: &Path, current: &Path) -> Option<PathBuf> {
    let filesystem = current.parent()?;
    tops.iter()
        .filter(|top| path.starts_with(top) && top.starts_with(filesystem) && *top != filesystem)
        .max_by_key(|top| top.components().count())
        .map(|top| top.join(QUARANTINE_DIR))
}

impl Places for DevicePlaces {
    fn places(&self, path: &Path) -> Result<Vec<Place>, String> {
        let mut places = self.mounts.places(path)?;
        for place in &mut places {
            if let Some(quarantine) = quarantine_in_top(&self.tops, path, &place.quarantine) {
                place.quarantine = quarantine;
            }
        }
        if let Some((own, disk)) = &self.own {
            for place in &mut places {
                if place.quarantine == disk.join(QUARANTINE_DIR) {
                    place.quarantine = own.clone();
                }
            }
        }
        let mut extra = self.extra.lock().unwrap_or_else(|e| e.into_inner());
        for place in &places {
            if !extra.contains(&place.quarantine) {
                extra.push(place.quarantine.clone());
            }
        }
        Ok(places)
    }

    fn roots(&self) -> Vec<PathBuf> {
        let mut roots = self.mounts.roots();
        roots.extend(self.own.iter().map(|(own, _)| own.clone()));
        roots.extend(self.tops.iter().map(|top| top.join(QUARANTINE_DIR)));
        roots.extend(
            self.extra
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .cloned(),
        );
        roots.sort();
        roots.dedup();
        roots
    }
}

fn store() -> &'static Quarantine {
    static STORE: OnceLock<Quarantine> = OnceLock::new();
    STORE.get_or_init(|| {
        let mut roots: Vec<PathBuf> = crate::locations::list()
            .into_iter()
            .map(|l| l.path)
            .collect();
        if let Some(home) = std::env::home_dir() {
            roots.push(home);
        }
        let own = (!cfg!(target_os = "android"))
            .then(crate::growth::data_dir)
            .flatten()
            .and_then(|data| {
                // Made now, or a first delete before FreeSize saved anything
                // would go to the top of the disk, which people can't write.
                let _ = std::fs::create_dir_all(&data);
                let disk = freesize_agent::quarantine::filesystem_root(&data).ok()?;
                Some((data.join("quarantine"), disk))
            });
        let tops = if cfg!(target_os = "android") {
            roots.clone()
        } else {
            Vec::new()
        };
        Quarantine::new(Box::new(DevicePlaces {
            mounts: MountPlaces::new(&roots),
            own,
            tops,
            extra: Mutex::new(Vec::new()),
        }))
    })
}

/// A batch id for a delete made here: the time, and a counter for several
/// in one second.
/// A new batch's id.
pub fn new_batch_id() -> String {
    batch_id()
}

fn batch_id() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    format!(
        "a{}-{}",
        freesize_agent::now(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Moves `paths` into the quarantine as one batch, for the person at this
/// device. Returns the bytes set aside and a message per item that
/// couldn't be moved.
pub fn delete_all(paths: &[PathBuf], reason: &str) -> (u64, Vec<String>) {
    let moved = move_in(&batch_id(), "", reason, paths);
    let bytes = moved.results.iter().flatten().sum();
    let errors = paths
        .iter()
        .zip(&moved.results)
        .filter_map(|(path, result)| {
            result
                .as_ref()
                .err()
                .map(|err| format!("{}: {err}", path.display()))
        })
        .collect();
    (bytes, errors)
}

/// Moves several items into the quarantine as one batch, so Put back
/// returns them together; what happened to each, in order.
pub fn delete_each(paths: &[PathBuf]) -> Vec<Result<(), String>> {
    move_in(&batch_id(), "", "", paths)
        .results
        .into_iter()
        .map(|result| result.map(|_| ()))
        .collect()
}

/// Moves items into batch `id`, adding to it if it exists, marked as
/// `group`. Returns the bytes set aside and a message per item that
/// couldn't be moved.
pub fn delete_group(id: &str, reason: &str, group: &str, paths: &[PathBuf]) -> (u64, Vec<String>) {
    let moved = store().move_in_group(id, "", reason, group, paths, freesize_agent::now());
    let bytes = moved.results.iter().flatten().sum();
    let errors = paths
        .iter()
        .zip(&moved.results)
        .filter_map(|(path, result)| {
            result
                .as_ref()
                .err()
                .map(|err| format!("{}: {err}", path.display()))
        })
        .collect();
    (bytes, errors)
}

/// Moves one item into the quarantine.
pub fn delete(path: &Path) -> Result<(), String> {
    let (_, errors) = delete_all(std::slice::from_ref(&path.to_path_buf()), "");
    match errors.into_iter().next() {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// Moves items in as batch `id` (a Hub request's id, or one made here).
pub fn move_in(id: &str, by: &str, reason: &str, paths: &[PathBuf]) -> Moved {
    store().move_in(id, by, reason, paths, freesize_agent::now())
}

/// What the quarantine holds, newest first.
pub fn list() -> Vec<QuarantineBatch> {
    store().list()
}

/// Puts a batch back where it was; says what happened.
pub fn restore(id: &str) -> Result<String, String> {
    store().restore(id)
}

/// Deletes batches for good; the bytes freed.
pub fn empty(ids: &[String]) -> Result<u64, String> {
    store().empty(ids)
}

/// Of a batch's items (by where they're stored), the ones whose old place
/// is taken now.
pub fn taken(id: &str, stored: &[String]) -> Vec<String> {
    store().taken(id, Some(stored))
}

/// Puts some items of a batch back.
pub fn restore_items(id: &str, stored: &[String], conflict: Conflict) -> Result<String, String> {
    store().restore_items(id, Some(stored), conflict)
}

/// Deletes some items of a batch for good; the bytes freed.
pub fn empty_items(id: &str, stored: &[String]) -> Result<u64, String> {
    store().empty_items(id, stored)
}

pub fn total(batches: &[QuarantineBatch]) -> u64 {
    batches.iter().map(|b| b.bytes).sum()
}

/// Items of these batches, for the confirmation before emptying.
pub fn confirm_text(batches: &[QuarantineBatch]) -> String {
    let items: Vec<&str> = batches
        .iter()
        .flat_map(|b| b.items.iter().map(|i| i.original.as_str()))
        .collect();
    let mut text = format!(
        "Delete {} for good, freeing {}? They can't be put back after this.",
        if items.len() == 1 {
            "this item".to_string()
        } else {
            format!("these {} items", items.len())
        },
        format_size(total(batches))
    );
    for item in items.iter().take(12) {
        text.push_str("\n• ");
        text.push_str(item);
    }
    if items.len() > 12 {
        text.push_str(&format!("\n• … and {} more", items.len() - 12));
    }
    text
}

/// The Quarantine place: what deleting set aside, newest first, each with
/// Put back and Empty. On phones it's part of Clean, and only while it
/// holds something.
pub fn section_rows(batches: &[QuarantineBatch], now: i64, always: bool) -> Vec<crate::HomeRow> {
    use crate::home::{heading, notice, notice2};
    if batches.is_empty() {
        if !always {
            return Vec::new();
        }
        return vec![
            heading(HEADING, "", None),
            notice(
                "Nothing in the quarantine",
                "What you delete in FreeSize waits here first, so you can put it back. Its \
                 space is freed only when you empty it, and never on its own.",
                None,
            ),
        ];
    }
    let items: usize = batches.iter().map(|b| b.items.len()).sum();
    let mut rows = vec![heading(
        HEADING,
        &format!(
            "{} waiting, {}. Put back returns things where they were; Empty frees their \
                 space for good.",
            if items == 1 {
                "One item".to_string()
            } else {
                format!("{items} items")
            },
            format_size(total(batches))
        ),
        Some((EMPTY_ALL, EMPTY_ALL_ACTION)),
    )];
    let mut newest: Vec<&QuarantineBatch> = batches.iter().collect();
    newest.sort_by_key(|b| std::cmp::Reverse(b.when));
    rows.extend(newest.into_iter().map(|batch| {
        let when = if now - batch.when < 60 {
            "Just now".to_string()
        } else {
            capitalize(&ago(now.max(0) as u64, batch.when.max(0) as u64))
        };
        let who = if batch.by.is_empty() {
            String::new()
        } else {
            format!(" · asked for by {} from the Hub", batch.by)
        };
        let reason = if batch.reason.is_empty() || batch_title(batch) == batch.reason {
            String::new()
        } else {
            format!(" · {}", batch.reason)
        };
        // A clean's batch says which rules it held; others where from.
        let groups = groups(batch);
        let from = if groups.is_empty() {
            batch
                .items
                .first()
                .and_then(|i| Path::new(&i.original).parent())
                .map(|p| format!("\nFrom {}", p.display()))
                .unwrap_or_default()
        } else {
            format!("\n{}", groups.join(", "))
        };
        crate::HomeRow {
            open_key: format!("{OPEN_PREFIX}{}", batch.id).into(),
            ..notice2(
                &batch_title(batch),
                &format!("{} · {when}{who}{reason}{from}", format_size(batch.bytes)),
                ("Put back", &format!("{RESTORE_PREFIX}{}", batch.id)),
                ("Empty…", &format!("{EMPTY_PREFIX}{}", batch.id)),
            )
        }
    }));
    rows
}

/// A batch's name: a clean's is "Clean junk"; others name their items.
pub fn batch_title(batch: &QuarantineBatch) -> String {
    if !groups(batch).is_empty() && !batch.reason.is_empty() {
        return batch.reason.clone();
    }
    let mut names: Vec<String> = batch
        .items
        .iter()
        .take(3)
        .map(|i| file_name(&i.original))
        .collect();
    if batch.items.len() > 3 {
        names.push(format!("{} more", batch.items.len() - 3));
    }
    names.join(", ")
}

/// The kinds a batch's items were set aside as, in order.
fn groups(batch: &QuarantineBatch) -> Vec<String> {
    let mut groups: Vec<String> = Vec::new();
    for item in &batch.items {
        if !item.group.is_empty() && !groups.contains(&item.group) {
            groups.push(item.group.clone());
        }
    }
    groups
}

/// Programs and scripts: opening one from the quarantine would run it, so
/// they're only shown in their folder.
pub fn runs_when_opened(path: &Path) -> bool {
    const RUNNABLE: &[&str] = &[
        "exe", "msi", "msix", "appx", "bat", "cmd", "com", "scr", "vbs", "ps1", "sh", "run", "apk",
        "jar", "appimage", "deb", "rpm", "pkg", "app",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| RUNNABLE.contains(&e.to_ascii_lowercase().as_str()))
}

/// Row kinds of a batch's page.
const VIEW_HEADING: i32 = 0;
const VIEW_ITEM: i32 = 1;
const VIEW_ENTRY: i32 = 2;
/// Entries listed inside a folder before "and N more".
const ENTRIES_SHOWN: usize = 500;

/// A batch's own page: its items like a file manager shows them, grouped
/// by the junk rule or the folder they came from; folders open.
#[derive(Clone, Debug, Default)]
pub struct View {
    pub batch: String,
    /// Inside a folder item: where it's stored now, then the folders
    /// opened inside it. Empty: the batch's items.
    pub folders: Vec<PathBuf>,
    /// Ticked items, by where they're stored.
    pub ticked: HashSet<String>,
    /// Per row id, the path it stands for and, for items, their key.
    targets: Vec<(PathBuf, Option<String>)>,
}

/// What the page's top shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewHead {
    pub title: String,
    pub subtitle: String,
    /// "Clean junk › Old stuff › 2024", inside folders.
    pub crumbs: String,
    /// "2 selected · 4.1 MB", or empty.
    pub picked: String,
}

impl View {
    pub fn new(batch: &str) -> View {
        View {
            batch: batch.to_string(),
            ..View::default()
        }
    }

    /// The path and item key of a row.
    pub fn target(&self, id: i32) -> Option<&(PathBuf, Option<String>)> {
        usize::try_from(id).ok().and_then(|i| self.targets.get(i))
    }

    /// Ticks or unticks an item row.
    pub fn toggle(&mut self, id: i32) {
        if let Some((_, Some(key))) = self.target(id).cloned()
            && !self.ticked.remove(&key)
        {
            self.ticked.insert(key);
        }
    }

    /// The batch's items ticked now, or every one when none is.
    pub fn chosen(&self, batch: &QuarantineBatch) -> Vec<String> {
        let ticked: Vec<String> = batch
            .items
            .iter()
            .filter(|i| self.ticked.contains(&i.stored))
            .map(|i| i.stored.clone())
            .collect();
        if ticked.is_empty() {
            batch.items.iter().map(|i| i.stored.clone()).collect()
        } else {
            ticked
        }
    }

    /// Builds the page from the quarantine as it is now; `None` when the
    /// batch is gone.
    pub fn rows(
        &mut self,
        batches: &[QuarantineBatch],
        now: i64,
    ) -> Option<(ViewHead, Vec<QuarantineRow>)> {
        let batch = batches.iter().find(|b| b.id == self.batch)?;
        self.ticked
            .retain(|key| batch.items.iter().any(|i| &i.stored == key));
        self.targets.clear();
        let when = if now - batch.when < 60 {
            "just now".to_string()
        } else {
            ago(now.max(0) as u64, batch.when.max(0) as u64)
        };
        let mut head = ViewHead {
            title: batch_title(batch),
            subtitle: format!(
                "{} · {} · moved {when}",
                format_size(batch.bytes),
                if batch.items.len() == 1 {
                    "1 item".to_string()
                } else {
                    format!("{} items", batch.items.len())
                }
            ),
            ..ViewHead::default()
        };
        let picked: Vec<_> = batch
            .items
            .iter()
            .filter(|i| self.ticked.contains(&i.stored))
            .collect();
        if !picked.is_empty() {
            head.picked = format!(
                "{} selected · {}",
                picked.len(),
                format_size(picked.iter().map(|i| i.bytes).sum())
            );
        }
        let mut rows = Vec::new();
        if let Some(folder) = self.folders.last().cloned() {
            let mut crumbs = vec![head.title.clone()];
            crumbs.extend(self.folders.iter().enumerate().map(|(i, f)| {
                if i == 0 {
                    shown_name(f)
                } else {
                    f.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                }
            }));
            head.crumbs = crumbs.join(" › ");
            self.entry_rows(&folder, &mut rows);
            return Some((head, rows));
        }
        // By rule for a clean's batch, else by the folder they came from.
        let mut sections: Vec<(String, Vec<usize>)> = Vec::new();
        for (index, item) in batch.items.iter().enumerate() {
            let key = if item.group.is_empty() {
                Path::new(&item.original)
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            } else {
                item.group.clone()
            };
            match sections.iter_mut().find(|(k, _)| *k == key) {
                Some((_, list)) => list.push(index),
                None => sections.push((key, vec![index])),
            }
        }
        for (key, list) in sections {
            rows.push(QuarantineRow {
                kind: VIEW_HEADING,
                id: -1,
                title: key.into(),
                detail: format_size(list.iter().map(|&i| batch.items[i].bytes).sum()).into(),
                ..QuarantineRow::default()
            });
            for index in list {
                let item = &batch.items[index];
                let stored = PathBuf::from(&item.stored);
                let is_dir = stored.is_dir();
                let id = self.targets.len() as i32;
                self.targets
                    .push((stored.clone(), Some(item.stored.clone())));
                rows.push(QuarantineRow {
                    kind: VIEW_ITEM,
                    id,
                    title: file_name(&item.original).into(),
                    // Under a rule's heading, where it came from; under a
                    // folder's, the heading says it.
                    subtitle: if item.group.is_empty() {
                        "".into()
                    } else {
                        format!(
                            "From {}",
                            Path::new(&item.original)
                                .parent()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default()
                        )
                        .into()
                    },
                    detail: format_size(item.bytes).into(),
                    is_dir,
                    ticked: self.ticked.contains(&item.stored),
                    can_open: is_dir || !runs_when_opened(&stored),
                });
            }
        }
        Some((head, rows))
    }

    /// The entries of a folder inside the quarantine: folders first.
    fn entry_rows(&mut self, folder: &Path, rows: &mut Vec<QuarantineRow>) {
        let mut entries: Vec<(bool, String, PathBuf)> = std::fs::read_dir(folder)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| {
                        let path = e.path();
                        let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
                        (is_dir, e.file_name().to_string_lossy().into_owned(), path)
                    })
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(a.1.to_lowercase().cmp(&b.1.to_lowercase()))
        });
        if entries.is_empty() {
            rows.push(QuarantineRow {
                kind: VIEW_HEADING,
                id: -1,
                title: "This folder is empty".into(),
                ..QuarantineRow::default()
            });
        }
        let more = entries.len().saturating_sub(ENTRIES_SHOWN);
        for (is_dir, name, path) in entries.into_iter().take(ENTRIES_SHOWN) {
            let id = self.targets.len() as i32;
            let bytes = freesize_agent::quarantine::size_on_disk(&path);
            let can_open = is_dir || !runs_when_opened(&path);
            self.targets.push((path, None));
            rows.push(QuarantineRow {
                kind: VIEW_ENTRY,
                id,
                title: name.into(),
                detail: format_size(bytes).into(),
                is_dir,
                can_open,
                ..QuarantineRow::default()
            });
        }
        if more > 0 {
            rows.push(QuarantineRow {
                kind: VIEW_HEADING,
                id: -1,
                title: format!("… and {more} more").into(),
                ..QuarantineRow::default()
            });
        }
    }
}

/// A stored path's name as it was: stored items are named
/// "<n>-<k>-<name>".
fn shown_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut parts = name.splitn(3, '-');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), Some(rest))
            if a.chars().all(|c| c.is_ascii_digit()) && b.chars().all(|c| c.is_ascii_digit()) =>
        {
            rest.to_string()
        }
        _ => name,
    }
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

/// The Clean up tab's Quarantine section: nothing when it's empty.
pub fn rows(batches: &[QuarantineBatch], now: i64) -> Vec<CleanRow> {
    if batches.is_empty() {
        return Vec::new();
    }
    let mut rows = vec![CleanRow {
        kind: 0,
        title: HEADING.into(),
        subtitle: format!(
            "{} deleted, in {}. Put back returns a batch to where it was; Empty frees its \
             space for good. Nothing here is emptied on its own.",
            format_size(total(batches)),
            if batches.len() == 1 {
                "one batch".to_string()
            } else {
                format!("{} batches", batches.len())
            }
        )
        .into(),
        action: EMPTY_ALL.into(),
        ..Default::default()
    }];
    rows.extend(batches.iter().map(|batch| {
        let names: Vec<String> = batch
            .items
            .iter()
            .take(3)
            .map(|i| {
                Path::new(&i.original)
                    .file_name()
                    .map_or_else(|| i.original.clone(), |n| n.to_string_lossy().into_owned())
            })
            .collect();
        let mut subtitle = names.join(", ");
        if batch.items.len() > 3 {
            subtitle.push_str(&format!(" and {} more", batch.items.len() - 3));
        }
        let who = if batch.by.is_empty() {
            "deleted on this device".to_string()
        } else {
            format!("asked by {} from the Hub", batch.by)
        };
        let reason = if batch.reason.is_empty() || batch_title(batch) == batch.reason {
            String::new()
        } else {
            format!(" · {}", batch.reason)
        };
        CleanRow {
            kind: BATCH,
            title: format!(
                "{} · {who}{reason}",
                if now - batch.when < 60 {
                    "Just now".to_string()
                } else {
                    ago(now.max(0) as u64, batch.when.max(0) as u64)
                }
            )
            .into(),
            subtitle: subtitle.into(),
            detail: format_size(batch.bytes).into(),
            action: batch.id.clone().into(),
            ..Default::default()
        }
    }));
    rows
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_phone_quarantine_goes_at_the_storage_root() {
        use std::path::{Path, PathBuf};
        let tops = vec![
            PathBuf::from("/storage/emulated/0"),
            PathBuf::from("/storage/1234-ABCD"),
        ];
        // Shared storage is one filesystem from /storage/emulated down.
        let below = super::quarantine_in_top(
            &tops,
            Path::new("/storage/emulated/0/Download/a.apk"),
            Path::new("/storage/emulated/.disktree-quarantine"),
        );
        assert_eq!(
            below,
            Some(PathBuf::from("/storage/emulated/0/.disktree-quarantine"))
        );
        // An SD card is its own filesystem: its top is right already.
        let card = super::quarantine_in_top(
            &tops,
            Path::new("/storage/1234-ABCD/DCIM/b.jpg"),
            Path::new("/storage/1234-ABCD/.disktree-quarantine"),
        );
        assert_eq!(card, None);
    }

    use super::*;
    use freesize_proto::unraid::QuarantineItem;

    #[test]
    fn the_section_lists_batches_with_their_buttons() {
        assert!(rows(&[], 1000).is_empty());
        let batches = vec![QuarantineBatch {
            id: "a1-0".into(),
            when: 900,
            by: String::new(),
            reason: String::new(),
            bytes: 2048,
            items: (0..5)
                .map(|n| QuarantineItem {
                    original: format!("/home/me/file{n}.txt"),
                    stored: String::new(),
                    bytes: 400,
                    ..Default::default()
                })
                .collect(),
        }];
        let rows = rows(&batches, 1000);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].title.as_str(), HEADING);
        assert_eq!(rows[0].action.as_str(), EMPTY_ALL);
        assert_eq!(rows[1].kind, BATCH);
        assert_eq!(rows[1].action.as_str(), "a1-0");
        assert!(
            rows[1].title.contains("deleted on this device"),
            "{}",
            rows[1].title
        );
        assert!(
            rows[1].subtitle.contains("and 2 more"),
            "{}",
            rows[1].subtitle
        );
        let text = confirm_text(&batches);
        assert!(text.contains("these 5 items") && text.contains("/home/me/file4.txt"));
    }

    #[test]
    fn deleting_puts_things_aside_and_put_back_restores_them() {
        let dir =
            std::env::temp_dir().join(format!("freesize-app-quarantine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("keep-me.txt");
        std::fs::write(&file, b"precious").unwrap();
        let before = list().len();
        let (bytes, errors) = delete_all(std::slice::from_ref(&file), "a test");
        assert!(errors.is_empty(), "{errors:?}");
        assert!(bytes > 0);
        assert!(!file.exists());
        let batches = list();
        assert_eq!(batches.len(), before + 1);
        let batch = batches
            .iter()
            .find(|b| b.reason == "a test")
            .expect("the batch is listed");
        assert_eq!(batch.items[0].original, file.to_string_lossy());
        restore(&batch.id).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"precious");
        // Deleted again and emptied: gone for good.
        delete(&file).unwrap();
        let id = list()
            .into_iter()
            .find(|b| b.items.iter().any(|i| i.original == file.to_string_lossy()))
            .map(|b| b.id)
            .unwrap();
        assert!(empty(&[id]).unwrap() > 0);
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
