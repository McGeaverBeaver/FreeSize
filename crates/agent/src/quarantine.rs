//! The quarantine: deleting moves items aside on the same filesystem
//! (instant, and nothing is copied between disks), where they can be put
//! back. Space is freed only when someone empties it, which is a request of
//! its own and approved like any delete. Nothing empties it on a timer.
//!
//! Each batch is a folder `<filesystem root>/<QUARANTINE_DIR>/<id>/` holding
//! the items and a `manifest.json` saying where each came from. A batch
//! whose items sat on several disks has such a folder on each.

use std::path::{Path, PathBuf};

use freesize_proto::unraid::{QuarantineBatch, QuarantineItem};
use serde::{Deserialize, Serialize};

/// The quarantine's folder at the top of each filesystem.
pub const QUARANTINE_DIR: &str = ".disktree-quarantine";

/// Where an item really is, and the quarantine on its filesystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// The item itself (on Unraid, one disk's copy of a `/mnt/user` path).
    pub path: PathBuf,
    /// The quarantine folder on the same filesystem.
    pub quarantine: PathBuf,
}

/// Knows where things are stored.
pub trait Places: Send + Sync {
    /// The places a path's files are, each with its quarantine. On most
    /// systems that's the path itself; on Unraid a `/mnt/user` path can be
    /// on several disks.
    fn places(&self, path: &Path) -> Result<Vec<Place>, String>;
    /// The quarantine folders to look in now (on Unraid, only on disks that
    /// are awake).
    fn roots(&self) -> Vec<PathBuf>;
    /// How to show a path to people.
    fn shown(&self, path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }
}

/// The quarantine at the top of the filesystem a path is on.
pub struct MountPlaces {
    /// Quarantines found so far, to look in.
    known: std::sync::Mutex<Vec<PathBuf>>,
}

impl MountPlaces {
    /// `roots`: folders whose filesystems may hold a quarantine already
    /// (the scanned folders).
    pub fn new(roots: &[PathBuf]) -> MountPlaces {
        let known = roots
            .iter()
            .filter_map(|r| filesystem_root(r).ok())
            .map(|r| r.join(QUARANTINE_DIR))
            .collect();
        MountPlaces {
            known: std::sync::Mutex::new(known),
        }
    }
}

impl Places for MountPlaces {
    fn places(&self, path: &Path) -> Result<Vec<Place>, String> {
        let quarantine = filesystem_root(path)?.join(QUARANTINE_DIR);
        let mut known = self.known.lock().unwrap_or_else(|e| e.into_inner());
        if !known.contains(&quarantine) {
            known.push(quarantine.clone());
        }
        Ok(vec![Place {
            path: path.to_path_buf(),
            quarantine,
        }])
    }

    fn roots(&self) -> Vec<PathBuf> {
        let mut roots = self.known.lock().unwrap_or_else(|e| e.into_inner()).clone();
        roots.sort();
        roots.dedup();
        roots
    }
}

/// The top folder of the filesystem holding `path`: the highest folder
/// above it on the same device.
#[cfg(unix)]
pub fn filesystem_root(path: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::MetadataExt;
    let start = path
        .parent()
        .unwrap_or(path)
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let device = std::fs::metadata(&start).map_err(|e| e.to_string())?.dev();
    let mut root = start.clone();
    for ancestor in start.ancestors().skip(1) {
        match std::fs::metadata(ancestor) {
            Ok(meta) if meta.dev() == device => root = ancestor.to_path_buf(),
            _ => break,
        }
    }
    Ok(root)
}

/// The drive a path is on (`C:\`).
#[cfg(not(unix))]
pub fn filesystem_root(path: &Path) -> Result<PathBuf, String> {
    let full = path
        .parent()
        .unwrap_or(path)
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    full.ancestors()
        .last()
        .map(Path::to_path_buf)
        .ok_or_else(|| "no drive".into())
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    id: String,
    when: i64,
    by: String,
    #[serde(default)]
    reason: String,
    items: Vec<StoredItem>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct StoredItem {
    /// The path as asked for (on Unraid maybe `/mnt/user/...`).
    original: String,
    /// The disk's own path it was at.
    physical: String,
    /// Where it is now.
    stored: String,
    bytes: u64,
    /// What it was set aside as (a junk rule's title); empty for none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    group: String,
}

/// What a move into the quarantine did, per item asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moved {
    pub results: Vec<Result<u64, String>>,
}

/// What a put back does when something is at the item's old place now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Conflict {
    /// Leave the item in the quarantine.
    #[default]
    Skip,
    /// Put it back beside, as "name (put back).ext".
    KeepBoth,
    /// Move what's there into the quarantine, then put the item back.
    Replace,
}

/// A name beside `path` that's free: "name (put back).ext", then
/// "name (put back 2).ext" and on.
fn free_name(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (1..)
        .map(|n| {
            let tag = if n == 1 {
                "put back".to_string()
            } else {
                format!("put back {n}")
            };
            path.with_file_name(format!("{stem} ({tag}){ext}"))
        })
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .expect("some name is free")
}

/// Saves what's left of a batch on one disk, or removes its folder when
/// nothing is.
fn write_or_remove(dir: &Path, mut manifest: Manifest, left: Vec<StoredItem>) {
    if left.is_empty() {
        let _ = std::fs::remove_file(dir.join("manifest.json"));
        let _ = std::fs::remove_dir(dir);
    } else {
        manifest.items = left;
        let _ = std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap_or_default(),
        );
    }
}

pub struct Quarantine {
    places: Box<dyn Places>,
}

/// Batch ids are made here; anything else is refused, so an id can't
/// lead out of the quarantine.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Bytes on disk under a path, without following links.
pub fn size_on_disk(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    #[cfg(unix)]
    let own = {
        use std::os::unix::fs::MetadataExt;
        meta.blocks() * 512
    };
    #[cfg(not(unix))]
    let own = meta.len();
    if !meta.is_dir() {
        return own;
    }
    own + std::fs::read_dir(path)
        .map(|entries| entries.flatten().map(|e| size_on_disk(&e.path())).sum())
        .unwrap_or(0)
}

impl Quarantine {
    pub fn new(places: Box<dyn Places>) -> Quarantine {
        Quarantine { places }
    }

    /// Moves `items` aside as batch `id`. Nothing is copied: an item that
    /// can't be moved within its own filesystem stays where it is.
    pub fn move_in(&self, id: &str, by: &str, reason: &str, items: &[PathBuf], now: i64) -> Moved {
        self.move_in_group(id, by, reason, "", items, now)
    }

    /// Like [`Quarantine::move_in`], marking the items as `group`. Moving
    /// into a batch that exists already adds to it.
    pub fn move_in_group(
        &self,
        id: &str,
        by: &str,
        reason: &str,
        group: &str,
        items: &[PathBuf],
        now: i64,
    ) -> Moved {
        let mut results = Vec::new();
        if !valid_id(id) {
            return Moved {
                results: items
                    .iter()
                    .map(|_| Err("bad batch id".to_string()))
                    .collect(),
            };
        }
        // Manifests per quarantine folder, written as items arrive so a
        // crash halfway still says where everything went.
        let mut manifests: Vec<(PathBuf, Manifest)> = Vec::new();
        for (n, item) in items.iter().enumerate() {
            let places = match self.places.places(item) {
                Ok(places) if !places.is_empty() => places,
                Ok(_) => {
                    results.push(Err("not there any more".into()));
                    continue;
                }
                Err(err) => {
                    results.push(Err(err));
                    continue;
                }
            };
            let mut bytes = 0;
            let mut errors = Vec::new();
            for (k, place) in places.iter().enumerate() {
                if place.path.starts_with(&place.quarantine)
                    || place.quarantine.starts_with(&place.path)
                {
                    errors.push(format!("{}: the quarantine itself", place.path.display()));
                    continue;
                }
                let batch_dir = place.quarantine.join(id);
                let name = place
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "item".into());
                // A batch added to may hold that name from an earlier move.
                let stored = (0..)
                    .map(|j| match j {
                        0 => batch_dir.join(format!("{n}-{k}-{name}")),
                        j => batch_dir.join(format!("{n}-{k}-{j}-{name}")),
                    })
                    .find(|p| std::fs::symlink_metadata(p).is_err())
                    .expect("some name is free");
                let size = size_on_disk(&place.path);
                let moved = std::fs::create_dir_all(&batch_dir)
                    .and_then(|()| std::fs::rename(&place.path, &stored));
                match moved {
                    Ok(()) => {
                        bytes += size;
                        let manifest = match manifests.iter_mut().find(|(dir, _)| *dir == batch_dir)
                        {
                            Some((_, m)) => m,
                            None => {
                                // A batch added to keeps what it holds.
                                let earlier =
                                    std::fs::read_to_string(batch_dir.join("manifest.json"))
                                        .ok()
                                        .and_then(|t| serde_json::from_str::<Manifest>(&t).ok())
                                        .filter(|m| m.id == id);
                                manifests.push((
                                    batch_dir.clone(),
                                    earlier.unwrap_or_else(|| Manifest {
                                        id: id.to_string(),
                                        when: now,
                                        by: by.to_string(),
                                        reason: reason.to_string(),
                                        items: Vec::new(),
                                    }),
                                ));
                                &mut manifests.last_mut().expect("just pushed").1
                            }
                        };
                        manifest.items.push(StoredItem {
                            original: self.places.shown(item),
                            physical: place.path.to_string_lossy().into_owned(),
                            stored: stored.to_string_lossy().into_owned(),
                            bytes: size,
                            group: group.to_string(),
                        });
                        let text = serde_json::to_string_pretty(&*manifest).unwrap_or_default();
                        if let Err(err) = std::fs::write(batch_dir.join("manifest.json"), text) {
                            errors.push(format!("couldn't note where it went: {err}"));
                        }
                    }
                    Err(err) => errors.push(format!("{}: {err}", place.path.display())),
                }
            }
            results.push(if errors.is_empty() || bytes > 0 {
                Ok(bytes)
            } else {
                Err(errors.join("; "))
            });
        }
        Moved { results }
    }

    fn manifests(&self) -> Vec<(PathBuf, Manifest)> {
        let mut found = Vec::new();
        for root in self.places.roots() {
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            for entry in entries.flatten() {
                let dir = entry.path();
                let Some(manifest) = std::fs::read_to_string(dir.join("manifest.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str::<Manifest>(&t).ok())
                else {
                    continue;
                };
                if valid_id(&manifest.id)
                    && dir.file_name().is_some_and(|n| n == manifest.id.as_str())
                {
                    found.push((dir, manifest));
                }
            }
        }
        found
    }

    /// What's in the quarantine, newest first.
    pub fn list(&self) -> Vec<QuarantineBatch> {
        let mut batches: Vec<QuarantineBatch> = Vec::new();
        for (_, manifest) in self.manifests() {
            let items = manifest.items.iter().map(|i| QuarantineItem {
                original: i.original.clone(),
                stored: i.stored.clone(),
                bytes: i.bytes,
                group: i.group.clone(),
            });
            match batches.iter_mut().find(|b| b.id == manifest.id) {
                Some(batch) => batch.items.extend(items),
                None => batches.push(QuarantineBatch {
                    id: manifest.id.clone(),
                    when: manifest.when,
                    by: manifest.by.clone(),
                    reason: manifest.reason.clone(),
                    bytes: 0,
                    items: items.collect(),
                }),
            }
        }
        for batch in &mut batches {
            batch.bytes = batch.items.iter().map(|i| i.bytes).sum();
        }
        batches.sort_by(|a, b| b.when.cmp(&a.when).then(a.id.cmp(&b.id)));
        batches
    }

    /// Puts a batch back. An item whose old place is taken stays in the
    /// quarantine; nothing is overwritten.
    pub fn restore(&self, id: &str) -> Result<String, String> {
        self.restore_items(id, None, Conflict::Skip)
    }

    /// The batch's parts on each disk, by id.
    fn parts(&self, id: &str) -> Result<Vec<(PathBuf, Manifest)>, String> {
        if !valid_id(id) {
            return Err("no such batch".into());
        }
        let parts: Vec<(PathBuf, Manifest)> = self
            .manifests()
            .into_iter()
            .filter(|(_, m)| m.id == id)
            .collect();
        if parts.is_empty() {
            return Err("that batch isn't in the quarantine (or its disk is asleep)".into());
        }
        Ok(parts)
    }

    /// Of the items `stored` names (all when `None`), the ones whose old
    /// place is taken now, as shown.
    pub fn taken(&self, id: &str, stored: Option<&[String]>) -> Vec<String> {
        let Ok(parts) = self.parts(id) else {
            return Vec::new();
        };
        parts
            .iter()
            .flat_map(|(_, m)| &m.items)
            .filter(|i| stored.is_none_or(|s| s.contains(&i.stored)))
            .filter(|i| std::fs::symlink_metadata(&i.physical).is_ok())
            .map(|i| i.original.clone())
            .collect()
    }

    /// Puts back the items `stored` names (all when `None`). An item whose
    /// old place is taken is dealt with as `conflict` says; what was in the
    /// way is never overwritten, only moved into the quarantine itself.
    pub fn restore_items(
        &self,
        id: &str,
        stored: Option<&[String]>,
        conflict: Conflict,
    ) -> Result<String, String> {
        let parts = self.parts(id)?;
        let (mut back, mut kept) = (0, Vec::new());
        let aside_id = format!("r{}", crate::now());
        for (dir, mut manifest) in parts {
            let mut left = Vec::new();
            for item in manifest.items.drain(..) {
                if stored.is_some_and(|s| !s.contains(&item.stored)) {
                    left.push(item);
                    continue;
                }
                let mut physical = PathBuf::from(&item.physical);
                if std::fs::symlink_metadata(&physical).is_ok() {
                    match conflict {
                        Conflict::Skip => {
                            kept.push(format!("{}: something is there now", item.original));
                            left.push(item);
                            continue;
                        }
                        Conflict::KeepBoth => physical = free_name(&physical),
                        Conflict::Replace => {
                            let moved = self.move_in(
                                &aside_id,
                                "",
                                "replaced by a put back",
                                std::slice::from_ref(&physical),
                                crate::now(),
                            );
                            if let Some(Err(err)) = moved.results.into_iter().next() {
                                kept.push(format!("{}: {err}", item.original));
                                left.push(item);
                                continue;
                            }
                        }
                    }
                }
                let moved = physical
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::rename(&item.stored, &physical));
                match moved {
                    Ok(()) => back += 1,
                    Err(err) => {
                        kept.push(format!("{}: {err}", item.original));
                        left.push(item);
                    }
                }
            }
            write_or_remove(&dir, manifest, left);
        }
        if kept.is_empty() {
            Ok(format!(
                "put back {back} {}",
                if back == 1 { "item" } else { "items" }
            ))
        } else if back > 0 {
            Ok(format!(
                "put back {back}; still in the quarantine: {}",
                kept.join("; ")
            ))
        } else {
            Err(kept.join("; "))
        }
    }

    /// Deletes some items of a batch for good; returns the bytes they held.
    pub fn empty_items(&self, id: &str, stored: &[String]) -> Result<u64, String> {
        let parts = self.parts(id)?;
        let (mut freed, mut errors) = (0, Vec::new());
        for (dir, mut manifest) in parts {
            let mut left = Vec::new();
            for item in manifest.items.drain(..) {
                if !stored.contains(&item.stored) {
                    left.push(item);
                    continue;
                }
                let path = PathBuf::from(&item.stored);
                // Only ever inside this batch's own folder.
                if !path.starts_with(&dir) {
                    errors.push(format!("{}: not in the quarantine", item.original));
                    left.push(item);
                    continue;
                }
                let removed = match std::fs::symlink_metadata(&path) {
                    Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&path),
                    Ok(_) => std::fs::remove_file(&path),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(err) => Err(err),
                };
                match removed {
                    Ok(()) => freed += item.bytes,
                    Err(err) => {
                        errors.push(format!("{}: {err}", item.original));
                        left.push(item);
                    }
                }
            }
            write_or_remove(&dir, manifest, left);
        }
        if freed == 0 && !errors.is_empty() {
            return Err(errors.join("; "));
        }
        Ok(freed)
    }

    /// Deletes batches for good; returns the bytes they held.
    pub fn empty(&self, ids: &[String]) -> Result<u64, String> {
        let wanted: Vec<&String> = ids.iter().filter(|id| valid_id(id)).collect();
        if wanted.is_empty() {
            return Err("nothing to empty".into());
        }
        let mut freed = 0;
        let mut found = 0;
        let mut errors = Vec::new();
        for (dir, manifest) in self.manifests() {
            if !wanted.contains(&&manifest.id) {
                continue;
            }
            found += 1;
            let bytes: u64 = manifest.items.iter().map(|i| i.bytes).sum();
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => freed += bytes,
                Err(err) => errors.push(format!("{}: {err}", dir.display())),
            }
        }
        if found == 0 {
            return Err(
                "those batches aren't in the quarantine (or their disks are asleep)".into(),
            );
        }
        if freed == 0 && !errors.is_empty() {
            return Err(errors.join("; "));
        }
        Ok(freed)
    }
}

/// Carries out requests on a server: deletes go to the quarantine, which
/// can be put back or emptied by later requests.
pub struct QuarantineExecutor {
    pub quarantine: Quarantine,
    /// This device's name, for notes to the approver.
    pub device: String,
    /// Whose junk rules apply (an Unraid server's differ from Linux's).
    pub platform: freesize_core::junk::Platform,
}

impl crate::commands::Executor for QuarantineExecutor {
    fn delete(&self, path: &Path) -> Result<u64, String> {
        let id = format!("d{}", crate::now());
        let moved = self
            .quarantine
            .move_in(&id, "", "", &[path.to_path_buf()], crate::now());
        moved
            .results
            .into_iter()
            .next()
            .unwrap_or(Err("nothing moved".into()))
    }

    fn delete_all(
        &self,
        targets: &[PathBuf],
        command: &freesize_proto::Command,
    ) -> crate::commands::Deleted {
        let moved = self.quarantine.move_in(
            &command.id,
            &command.by.name,
            &command.reason,
            targets,
            crate::now(),
        );
        let bytes: u64 = moved.results.iter().flatten().sum();
        let done = moved.results.iter().filter(|r| r.is_ok()).count();
        crate::commands::Deleted {
            detail: format!(
                "moved {done} of {} to the quarantine ({}); the space is freed when it's emptied",
                targets.len(),
                freesize_core::format_size(bytes)
            ),
            results: moved.results,
            freed: 0,
        }
    }

    fn restore(&self, batch: &str) -> Result<String, String> {
        self.quarantine.restore(batch)
    }

    fn empty_quarantine(&self, batches: &[String]) -> Result<u64, String> {
        self.quarantine.empty(batches)
    }

    fn clean(&self, kind: &str) -> Result<crate::junk::Outcome, String> {
        let finding = crate::junk::find_rule(self.platform, kind)?;
        let id = format!("j{}", crate::now());
        crate::junk::clean(&finding, &|paths| {
            let moved = self
                .quarantine
                .move_in(&id, "", finding.rule.title, paths, crate::now());
            let bytes: u64 = moved.results.iter().flatten().sum();
            match moved.results.iter().find_map(|r| r.as_ref().err()) {
                Some(err) if bytes == 0 => Err(err.clone()),
                _ => Ok(bytes),
            }
        })
    }

    fn online_only(
        &self,
        root: &Path,
        paths: &[String],
        keep: bool,
    ) -> Result<crate::nextcloud::PinOutcome, String> {
        crate::nextcloud::online_only(root, paths, keep)
    }

    fn move_in(
        &self,
        source: &Path,
        into: &Path,
        target: &str,
    ) -> Result<crate::nextcloud::MoveOutcome, String> {
        let id = format!("m{}", crate::now());
        crate::nextcloud::move_in(source, into, target, &|paths| {
            let moved = self.quarantine.move_in(
                &id,
                "",
                "moved into the Nextcloud folder",
                paths,
                crate::now(),
            );
            let bytes: u64 = moved.results.iter().flatten().sum();
            match moved.results.iter().find_map(|r| r.as_ref().err()) {
                Some(err) => Err(err.clone()),
                None => Ok(bytes),
            }
        })
    }

    fn note(&self, request: &crate::commands::Request) -> String {
        use freesize_proto::Action;
        match &request.command.action {
            Action::Delete { .. } => format!(
                "{} moves them to its quarantine on the same disk: they can be put back, and \
                 the space is freed only when the quarantine is emptied. {}",
                self.device,
                crate::estimate::estimate(&request.targets).describe()
            )
            .replace("Frees about", "Emptying it later frees about"),
            Action::EmptyQuarantine { batches, .. } => {
                let stored: Vec<PathBuf> = self
                    .quarantine
                    .list()
                    .into_iter()
                    .filter(|b| batches.contains(&b.id))
                    .flat_map(|b| b.items.into_iter().map(|i| PathBuf::from(i.stored)))
                    .collect();
                crate::estimate::estimate(&stored).describe()
            }
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two "disks" (folders); a path under `/user/` is on both, as on
    /// Unraid.
    struct TwoDisks {
        disks: Vec<PathBuf>,
        user: PathBuf,
    }

    impl Places for TwoDisks {
        fn places(&self, path: &Path) -> Result<Vec<Place>, String> {
            match path.strip_prefix(&self.user) {
                Ok(rest) => Ok(self
                    .disks
                    .iter()
                    .map(|d| Place {
                        path: d.join(rest),
                        quarantine: d.join(QUARANTINE_DIR),
                    })
                    .filter(|p| p.path.symlink_metadata().is_ok())
                    .collect()),
                Err(_) => {
                    let disk = self
                        .disks
                        .iter()
                        .find(|d| path.starts_with(d))
                        .ok_or("not on a disk")?;
                    Ok(vec![Place {
                        path: path.to_path_buf(),
                        quarantine: disk.join(QUARANTINE_DIR),
                    }])
                }
            }
        }

        fn roots(&self) -> Vec<PathBuf> {
            self.disks.iter().map(|d| d.join(QUARANTINE_DIR)).collect()
        }
    }

    fn setup() -> (tempfile::TempDir, Quarantine, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let disk1 = dir.path().join("disk1");
        let disk2 = dir.path().join("disk2");
        for (disk, bytes) in [(&disk1, 3000), (&disk2, 5000)] {
            std::fs::create_dir_all(disk.join("Movies/Old")).unwrap();
            std::fs::write(disk.join("Movies/Old/a.mkv"), vec![1u8; bytes]).unwrap();
        }
        std::fs::write(disk1.join("Movies/keep.mkv"), b"x").unwrap();
        let places = TwoDisks {
            disks: vec![disk1.clone(), disk2.clone()],
            user: dir.path().join("user"),
        };
        (dir, Quarantine::new(Box::new(places)), disk1, disk2)
    }

    #[test]
    fn moves_aside_lists_and_puts_back() {
        let (dir, q, disk1, disk2) = setup();
        let user_item = dir.path().join("user/Movies/Old");
        let moved = q.move_in(
            "b1",
            "Robert",
            "old stuff",
            std::slice::from_ref(&user_item),
            100,
        );
        assert!(moved.results[0].as_ref().unwrap() >= &8000);
        // Gone from both disks, kept on each disk's own quarantine.
        assert!(!disk1.join("Movies/Old").exists());
        assert!(!disk2.join("Movies/Old").exists());
        assert!(disk1.join(QUARANTINE_DIR).join("b1/manifest.json").exists());
        assert!(disk2.join(QUARANTINE_DIR).join("b1/manifest.json").exists());
        let list = q.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].items.len(), 2);
        assert_eq!(list[0].by, "Robert");
        assert_eq!(list[0].reason, "old stuff");
        assert!(list[0].bytes >= 8000);

        // Something new took one place: that item waits, the other goes back.
        std::fs::create_dir_all(disk2.join("Movies/Old")).unwrap();
        let answer = q.restore("b1").unwrap();
        assert!(
            answer.starts_with("put back 1; still in the quarantine"),
            "{answer}"
        );
        assert!(disk1.join("Movies/Old/a.mkv").exists());
        assert_eq!(q.list()[0].items.len(), 1);
        std::fs::remove_dir(disk2.join("Movies/Old")).unwrap();
        assert_eq!(q.restore("b1").unwrap(), "put back 1 item");
        assert_eq!(
            std::fs::read(disk2.join("Movies/Old/a.mkv")).unwrap().len(),
            5000
        );
        assert!(q.list().is_empty());
        assert!(q.restore("b1").is_err());
    }

    #[test]
    fn single_items_go_back_or_go_for_good() {
        let (_dir, q, disk1, _disk2) = setup();
        std::fs::write(disk1.join("Movies/b.mkv"), b"bb").unwrap();
        q.move_in(
            "b1",
            "a",
            "",
            &[disk1.join("Movies/keep.mkv"), disk1.join("Movies/b.mkv")],
            100,
        );
        let items = q.list()[0].items.clone();
        let keep = items
            .iter()
            .find(|i| i.original.ends_with("keep.mkv"))
            .unwrap()
            .stored
            .clone();
        let b = items
            .iter()
            .find(|i| i.original.ends_with("b.mkv"))
            .unwrap()
            .stored
            .clone();

        // A new keep.mkv took its place: kept both, the new one untouched.
        std::fs::write(disk1.join("Movies/keep.mkv"), b"new").unwrap();
        assert_eq!(q.taken("b1", Some(std::slice::from_ref(&keep))).len(), 1);
        let answer = q
            .restore_items("b1", Some(std::slice::from_ref(&keep)), Conflict::KeepBoth)
            .unwrap();
        assert_eq!(answer, "put back 1 item");
        assert_eq!(
            std::fs::read(disk1.join("Movies/keep.mkv")).unwrap(),
            b"new"
        );
        assert_eq!(
            std::fs::read(disk1.join("Movies/keep (put back).mkv")).unwrap(),
            b"x"
        );
        assert_eq!(q.list()[0].items.len(), 1);

        // The other one is deleted for good; the batch is gone with it.
        assert!(q.empty_items("b1", std::slice::from_ref(&b)).unwrap() > 0);
        assert!(!PathBuf::from(&b).exists());
        assert!(q.list().is_empty());
    }

    #[test]
    fn adding_to_a_batch_keeps_what_it_held() {
        let (_dir, q, disk1, _disk2) = setup();
        std::fs::write(disk1.join("Movies/b.mkv"), b"bb").unwrap();
        q.move_in_group(
            "b1",
            "",
            "Clean junk",
            "Old downloads",
            &[disk1.join("Movies/keep.mkv")],
            1,
        );
        q.move_in_group(
            "b1",
            "",
            "Clean junk",
            "Installers",
            &[disk1.join("Movies/b.mkv")],
            1,
        );
        let list = q.list();
        assert_eq!(list.len(), 1);
        let groups: Vec<&str> = list[0].items.iter().map(|i| i.group.as_str()).collect();
        assert_eq!(groups, ["Old downloads", "Installers"]);
        assert_eq!(q.restore("b1").unwrap(), "put back 2 items");
        assert_eq!(std::fs::read(disk1.join("Movies/b.mkv")).unwrap(), b"bb");
    }

    #[test]
    fn replacing_moves_what_was_in_the_way_aside() {
        let (_dir, q, disk1, _disk2) = setup();
        q.move_in("b1", "a", "", &[disk1.join("Movies/keep.mkv")], 100);
        std::fs::write(disk1.join("Movies/keep.mkv"), b"new").unwrap();
        q.restore_items("b1", None, Conflict::Replace).unwrap();
        assert_eq!(std::fs::read(disk1.join("Movies/keep.mkv")).unwrap(), b"x");
        let list = q.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].reason, "replaced by a put back");
    }

    #[test]
    fn empties_only_what_is_asked() {
        let (_dir, q, disk1, _disk2) = setup();
        q.move_in("b1", "a", "", &[disk1.join("Movies/Old")], 100);
        q.move_in("b2", "a", "", &[disk1.join("Movies/keep.mkv")], 200);
        assert_eq!(q.list()[0].id, "b2");
        let freed = q.empty(&["b1".into()]).unwrap();
        assert!(freed >= 3000);
        let left = q.list();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "b2");
        assert!(q.empty(&["../../etc".into()]).is_err());
        assert!(q.empty(&["b9".into()]).is_err());
    }

    #[test]
    fn refuses_bad_ids_and_missing_items() {
        let (dir, q, disk1, _disk2) = setup();
        let moved = q.move_in("../x", "a", "", &[dir.path().join("user/Movies/Old")], 1);
        assert!(moved.results[0].is_err());
        let moved = q.move_in("b3", "a", "", &[dir.path().join("user/Nothing")], 1);
        assert!(moved.results[0].is_err());
        // Never the quarantine, nor a folder holding it.
        let moved = q.move_in("b4", "a", "", std::slice::from_ref(&disk1), 1);
        assert!(moved.results[0].is_err());
        assert!(disk1.exists());
    }

    #[cfg(unix)]
    #[test]
    fn finds_the_filesystem_top() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        let root = filesystem_root(&dir.path().join("a/b/c")).unwrap();
        assert!(dir.path().canonicalize().unwrap().starts_with(&root));
        let places = MountPlaces::new(&[dir.path().to_path_buf()]);
        assert_eq!(places.roots(), [root.join(QUARANTINE_DIR)]);
    }
}
