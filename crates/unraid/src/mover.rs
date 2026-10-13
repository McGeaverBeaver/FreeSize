//! Moving a share's folder from one disk to another, for balancing: a plan
//! first (what moves, whether it fits, what's in the way), then a copy
//! where every file is checked against the original, and only then the
//! originals go to the source disk's quarantine. The space there is freed
//! when the quarantine is emptied, so nothing is lost if anything looks
//! wrong.
//!
//! It works on the disks' own paths only (`/mnt/disk1/...`), never through
//! `/mnt/user`: copying between a disk path and a user share path can
//! destroy the very files being copied.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use freesize_agent::quarantine::{QUARANTINE_DIR, Quarantine};
use freesize_core::format_size;
use freesize_proto::unraid::{DiskKind, MoveJob};
use serde::Serialize;
use xxhash_rust::xxh3::Xxh3;

use crate::System;
use crate::array::Array;
use crate::protect::{Rule, check};

/// Files named in a plan's problems at most.
const MAX_LISTED: usize = 20;
/// Folders listed at most, for picking one.
const MAX_FOLDERS: usize = 2000;

/// A move worked out, before anything is copied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub job: MoveJob,
    /// The real folders, on this machine.
    pub source: PathBuf,
    pub target: PathBuf,
    /// What stops it.
    pub problems: Vec<String>,
    /// What to know first.
    pub warnings: Vec<String>,
}

impl Plan {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

fn plain(path: &str) -> bool {
    path.split('/').filter(|p| !p.is_empty()).all(|part| {
        part != "."
            && part != ".."
            && !part.contains('\0')
            && matches!(
                Path::new(part).components().next(),
                Some(Component::Normal(_))
            )
    })
}

/// Works out moving `folder` (in `share`; empty for all of the share on
/// that disk) from disk `from` to disk `to`.
#[allow(clippy::too_many_arguments)]
pub fn plan(
    system: &System,
    array: &Array,
    rules: &[Rule],
    share: &str,
    folder: &str,
    from: &str,
    to: &str,
    now: i64,
) -> Plan {
    let folder = folder.trim_matches('/');
    let mut plan = Plan {
        job: MoveJob {
            id: format!("m{now}"),
            share: share.to_string(),
            folder: folder.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            state: "planned".into(),
            started: now,
            ..MoveJob::default()
        },
        ..Plan::default()
    };
    if share.is_empty()
        || share.contains('/')
        || share.starts_with('.')
        || !plain(share)
        || !plain(folder)
    {
        plan.problems
            .push("choose a share and a folder inside it".into());
        return plan;
    }
    let disk = |name: &str| {
        array
            .disk(name)
            .filter(|d| matches!(d.kind, DiskKind::Data | DiskKind::Pool) && d.path.is_some())
    };
    let (Some(source_disk), Some(target_disk)) = (disk(from), disk(to)) else {
        plan.problems
            .push("both disks must be array disks or pools that are mounted".into());
        return plan;
    };
    if from == to {
        plan.problems.push("choose two different disks".into());
        return plan;
    }
    let rest = if folder.is_empty() {
        share.to_string()
    } else {
        format!("{share}/{folder}")
    };
    let source_path = format!("{}/{rest}", source_disk.path.as_deref().unwrap_or_default());
    let target_path = format!("{}/{rest}", target_disk.path.as_deref().unwrap_or_default());
    plan.source = system.file(&source_path);
    plan.target = system.file(&target_path);

    // In use by Docker, a VM or the system: not moved while it runs.
    let in_use: Vec<Rule> = rules.iter().filter(|r| !r.whole).cloned().collect();
    if let Some(why) = check(&in_use, &source_path) {
        plan.problems
            .push(format!("{source_path} can't be moved: {why}"));
        return plan;
    }
    if std::fs::symlink_metadata(&plan.source).is_err() {
        plan.problems.push(format!("{source_path} isn't there"));
        return plan;
    }
    let open = freesize_agent::guard::open_files(&system.file("/proc"));
    if let Some((file, process, _)) = open.iter().find(|(f, _, _)| f.starts_with(&plan.source)) {
        plan.problems.push(format!(
            "{} is open in {process}; stop it first",
            system.unraid_path(file).unwrap_or_default()
        ));
    }

    // What moves, what's in the way.
    let mut allocated = 0u64;
    let mut linked = (0u64, 0u64);
    let mut special = 0u64;
    let mut conflicts = Vec::new();
    let mut stack = vec![plan.source.clone()];
    while let Some(path) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let relative = path.strip_prefix(&plan.source).unwrap_or(Path::new(""));
        let there = plan.target.join(relative);
        if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
            if std::fs::symlink_metadata(&there).is_ok_and(|m| !m.is_dir()) {
                conflicts.push(there);
            }
            continue;
        }
        if !(meta.is_file() || meta.file_type().is_symlink()) {
            special += 1;
            continue;
        }
        plan.job.files += 1;
        plan.job.bytes += meta.len();
        allocated += crate::allocated(&meta);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.is_file() && meta.nlink() > 1 {
                linked.0 += 1;
                linked.1 += meta.len();
            }
        }
        if std::fs::symlink_metadata(&there).is_ok() {
            conflicts.push(there);
        }
    }
    if !conflicts.is_empty() {
        let shown: Vec<String> = conflicts
            .iter()
            .take(MAX_LISTED)
            .map(|p| system.unraid_path(p).unwrap_or_default())
            .collect();
        plan.problems.push(format!(
            "{} already on {to}, and nothing is overwritten: {}{}",
            match conflicts.len() {
                1 => "1 item is".to_string(),
                n => format!("{n} items are"),
            },
            shown.join(", "),
            if conflicts.len() > MAX_LISTED {
                ", …"
            } else {
                ""
            }
        ));
    }
    let margin = (target_disk.size / 100).max(1 << 30);
    if target_disk.free < allocated + margin {
        plan.problems.push(format!(
            "{to} has {} free; this needs {} plus {} to spare",
            format_size(target_disk.free),
            format_size(allocated),
            format_size(margin)
        ));
    }
    if linked.0 > 0 {
        plan.warnings.push(format!(
            "{} hard-linked files ({}) become separate copies on {to}; their other links stay \
             on {from}, so less space is freed there",
            linked.0,
            format_size(linked.1)
        ));
    }
    if special > 0 {
        plan.warnings
            .push(format!("{special} sockets or device files aren't copied"));
    }
    if target_disk.asleep || source_disk.asleep {
        plan.warnings.push("sleeping disks will spin up".into());
    }
    plan
}

/// A folder of a share across the disks, for picking what to move: the
/// folders in it, and which disks hold each.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Folders {
    /// The disks this folder is on.
    pub disks: Vec<String>,
    /// Sleeping disks, not looked at: listing them would spin them up.
    pub asleep: Vec<String>,
    pub children: Vec<Folder>,
    /// Folders left out past `MAX_FOLDERS`.
    pub more: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Folder {
    pub name: String,
    pub disks: Vec<String>,
}

/// The folders in `folder` (in `share`; empty for the share itself) on each
/// array disk and pool. Sleeping disks are left alone unless `wake`.
pub fn folders(
    system: &System,
    array: &Array,
    share: &str,
    folder: &str,
    wake: bool,
) -> Result<Folders, String> {
    let folder = folder.trim_matches('/');
    if share.is_empty()
        || share.contains('/')
        || share.starts_with('.')
        || !plain(share)
        || !plain(folder)
    {
        return Err("choose a share and a folder inside it".into());
    }
    let rest = if folder.is_empty() {
        share.to_string()
    } else {
        format!("{share}/{folder}")
    };
    let mut out = Folders::default();
    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for disk in &array.disks {
        let (DiskKind::Data | DiskKind::Pool, Some(mount)) = (&disk.kind, &disk.path) else {
            continue;
        };
        if disk.asleep && !wake {
            out.asleep.push(disk.name.clone());
            continue;
        }
        let Ok(entries) = std::fs::read_dir(system.file(&format!("{mount}/{rest}"))) else {
            continue;
        };
        out.disks.push(disk.name.clone());
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // Hidden folders: the recycle bin's, FreeSize's own and such.
            if name.starts_with('.') || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            children.entry(name).or_default().push(disk.name.clone());
        }
    }
    let mut children: Vec<Folder> = children
        .into_iter()
        .map(|(name, disks)| Folder { name, disks })
        .collect();
    children.sort_by_cached_key(|f| f.name.to_lowercase());
    out.more = children.len().saturating_sub(MAX_FOLDERS);
    children.truncate(MAX_FOLDERS);
    out.children = children;
    Ok(out)
}

/// Files in more than one of the plans' sources, as paths in the share:
/// the share shows only one of each, and moving both would put two files
/// in one place. At most `MAX_LISTED`, then the count of all.
pub fn duplicates(plans: &[Plan]) -> (Vec<String>, usize) {
    if plans.len() < 2 {
        return (Vec::new(), 0);
    }
    let mut seen: HashMap<PathBuf, usize> = HashMap::new();
    for plan in plans {
        let mut stack = vec![plan.source.clone()];
        while let Some(path) = stack.pop() {
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&path) {
                    stack.extend(entries.flatten().map(|e| e.path()));
                }
            } else if let Ok(relative) = path.strip_prefix(&plan.source) {
                *seen.entry(relative.to_path_buf()).or_default() += 1;
            }
        }
    }
    let mut twice: Vec<&PathBuf> = seen
        .iter()
        .filter(|(_, n)| **n > 1)
        .map(|(p, _)| p)
        .collect();
    twice.sort();
    let total = twice.len();
    let base = plans[0].job.folder.trim_matches('/');
    let share = format!("/mnt/user/{}", plans[0].job.share);
    let shown = twice
        .into_iter()
        .take(MAX_LISTED)
        .map(|p| {
            let p = p.to_string_lossy();
            match base {
                "" => format!("{share}/{p}"),
                base => format!("{share}/{base}/{p}"),
            }
        })
        .collect();
    (shown, total)
}

/// Copies one file to a temporary name beside its target, checks the copy
/// against the original, then gives it its name. Returns the bytes.
fn copy_file(source: &Path, target: &Path, meta: &std::fs::Metadata) -> Result<u64, String> {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let partial = target.with_file_name(format!(".{name}.disktree-part"));
    let fail = |what: &str, err: std::io::Error| format!("{}: {what}: {err}", source.display());
    let mut input = std::fs::File::open(source).map_err(|e| fail("reading", e))?;
    let mut output = std::fs::File::create(&partial).map_err(|e| fail("writing", e))?;
    let mut hash = Xxh3::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut copied = 0u64;
    loop {
        let n = input.read(&mut buffer).map_err(|e| fail("reading", e))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        output
            .write_all(&buffer[..n])
            .map_err(|e| fail("writing", e))?;
        copied += n as u64;
    }
    output.sync_all().map_err(|e| fail("writing", e))?;
    drop(output);
    // Read the copy back from the disk and compare.
    let mut check = Xxh3::new();
    let mut reread = std::fs::File::open(&partial).map_err(|e| fail("checking", e))?;
    loop {
        let n = reread.read(&mut buffer).map_err(|e| fail("checking", e))?;
        if n == 0 {
            break;
        }
        check.update(&buffer[..n]);
    }
    if check.digest128() != hash.digest128() || copied != meta.len() {
        let _ = std::fs::remove_file(&partial);
        return Err(format!(
            "{}: the copy didn't match the original; the original stays",
            source.display()
        ));
    }
    keep_attributes(&partial, meta);
    std::fs::rename(&partial, target).map_err(|e| fail("naming the copy", e))?;
    Ok(copied)
}

/// Owner, mode and times from the original.
fn keep_attributes(path: &Path, meta: &std::fs::Metadata) {
    let _ = std::fs::set_permissions(path, meta.permissions());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = std::os::unix::fs::lchown(path, Some(meta.uid()), Some(meta.gid()));
    }
    if let (Ok(file), Ok(modified)) = (std::fs::File::open(path), meta.modified()) {
        let mut times = std::fs::FileTimes::new().set_modified(modified);
        if let Ok(accessed) = meta.accessed() {
            times = times.set_accessed(accessed);
        }
        let _ = file.set_times(times);
    }
}

/// Carries out a plan. `progress` hears about it after each file;
/// `cancel` stops it between files (copies made so far stay, the originals
/// are untouched).
pub fn run(
    system: &System,
    plan: &Plan,
    quarantine: &Quarantine,
    progress: &mut dyn FnMut(&MoveJob),
    cancel: &AtomicBool,
) -> MoveJob {
    let mut job = plan.job.clone();
    job.state = "running".into();
    progress(&job);
    let finish = |mut job: MoveJob, state: &str, detail: String| {
        job.state = state.into();
        job.detail = detail;
        job.finished = Some(crate::now());
        job
    };
    if !plan.ok() {
        return finish(job, "failed", plan.problems.join("; "));
    }
    // Folders first (deepest last), files after; folder times are set at
    // the end, since adding files changes them.
    let mut folders: Vec<(PathBuf, std::fs::Metadata)> = Vec::new();
    let mut stack = vec![plan.source.clone()];
    let mut last_report = std::time::Instant::now();
    while let Some(path) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return finish(
                job,
                "cancelled",
                "stopped; the copies made so far are on the target disk, the originals untouched"
                    .into(),
            );
        }
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(&plan.source)
            .unwrap_or(Path::new(""))
            .to_path_buf();
        let target = plan.target.join(&relative);
        if path.file_name().is_some_and(|n| n == QUARANTINE_DIR) {
            continue;
        }
        let outcome: Result<u64, String> = if meta.is_dir() {
            let made =
                std::fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()));
            if made.is_ok() {
                if let Ok(entries) = std::fs::read_dir(&path) {
                    stack.extend(entries.flatten().map(|e| e.path()));
                }
                folders.push((target, meta));
            }
            made.map(|()| 0)
        } else if meta.file_type().is_symlink() {
            #[cfg(unix)]
            let made = std::fs::read_link(&path)
                .and_then(|link| std::os::unix::fs::symlink(link, &target))
                .map_err(|e| format!("{}: {e}", target.display()));
            #[cfg(not(unix))]
            let made: Result<(), String> = Err("links aren't copied here".into());
            made.map(|()| {
                job.done_files += 1;
                0
            })
        } else if meta.is_file() {
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            copy_file(&path, &target, &meta).inspect(|_| job.done_files += 1)
        } else {
            Ok(0)
        };
        match outcome {
            Ok(bytes) => job.done_bytes += bytes,
            Err(err) => {
                return finish(
                    job,
                    "failed",
                    format!(
                        "{err}. The copies made so far are on the target disk; the originals are untouched."
                    ),
                );
            }
        }
        if last_report.elapsed().as_secs() >= 1 {
            progress(&job);
            last_report = std::time::Instant::now();
        }
    }
    for (folder, meta) in folders.iter().rev() {
        keep_attributes(folder, meta);
    }
    // All copied and checked: the originals go to their disk's quarantine.
    let moved = quarantine.move_in(
        &job.id,
        &format!("the move to {}", job.to),
        &format!(
            "moving {} from {} to {}",
            plan_label(&job),
            job.from,
            job.to
        ),
        std::slice::from_ref(&plan.source),
        crate::now(),
    );
    let shown_bytes = format_size(job.done_bytes);
    match moved.results.first() {
        Some(Ok(_)) => {
            let detail = format!(
                "copied and checked {} files ({shown_bytes}) to {}; the originals are in {}'s \
                 quarantine: empty it to free the space, or put them back",
                job.done_files, job.to, job.from
            );
            finish(job, "done", detail)
        }
        Some(Err(err)) => {
            let detail = format!(
                "copied and checked {} files ({shown_bytes}), but the originals couldn't be \
                 moved aside ({err}); both copies are there now. {}",
                job.done_files,
                system.unraid_path(&plan.source).unwrap_or_default()
            );
            finish(job, "failed", detail)
        }
        None => finish(job, "failed", "nothing to move".into()),
    }
}

fn plan_label(job: &MoveJob) -> String {
    if job.folder.is_empty() {
        job.share.clone()
    } else {
        format!("{}/{}", job.share, job.folder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array::{self, tests::server};
    use crate::protect::UnraidPlaces;

    fn setup() -> (tempfile::TempDir, System, Array) {
        let (dir, system) = server();
        for (path, bytes) in [
            ("/mnt/disk1/Movies/Old/a.mkv", 300_000),
            ("/mnt/disk1/Movies/Old/sub/b.srt", 2_000),
            ("/mnt/disk1/Movies/Keep/c.mkv", 1_000),
        ] {
            let file = system.file(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, vec![3u8; bytes]).unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink("a.mkv", system.file("/mnt/disk1/Movies/Old/link.mkv")).unwrap();
        let mut array = array::read(&system);
        // Wake disk2 so it can take the files, and give it room.
        for disk in &mut array.disks {
            if disk.name == "disk2" {
                disk.asleep = false;
            }
        }
        (dir, system, array)
    }

    #[test]
    fn plans_and_moves_with_checks() {
        let (_dir, system, array) = setup();
        let plan = plan(
            &system,
            &array,
            &[],
            "Movies",
            "Old",
            "disk1",
            "disk2",
            1000,
        );
        assert!(plan.ok(), "{:?}", plan.problems);
        assert_eq!(plan.job.files, 3);
        assert!(plan.job.bytes >= 302_000);
        let quarantine = Quarantine::new(Box::new(UnraidPlaces {
            system: system.clone(),
            array: array.clone(),
        }));
        let mut updates = 0;
        let done = run(
            &system,
            &plan,
            &quarantine,
            &mut |_| updates += 1,
            &AtomicBool::new(false),
        );
        assert_eq!(done.state, "done", "{}", done.detail);
        assert!(updates >= 1);
        assert_eq!(done.done_files, 3);
        let moved = system.file("/mnt/disk2/Movies/Old/a.mkv");
        assert_eq!(std::fs::read(&moved).unwrap().len(), 300_000);
        assert!(system.file("/mnt/disk2/Movies/Old/sub/b.srt").exists());
        #[cfg(unix)]
        assert_eq!(
            std::fs::read_link(system.file("/mnt/disk2/Movies/Old/link.mkv")).unwrap(),
            Path::new("a.mkv")
        );
        // The originals wait in disk1's quarantine.
        assert!(!system.file("/mnt/disk1/Movies/Old").exists());
        assert!(system.file("/mnt/disk1/Movies/Keep/c.mkv").exists());
        let batches = quarantine.list();
        assert_eq!(batches[0].id, plan.job.id);
        assert_eq!(batches[0].by, "the move to disk2");
        assert_eq!(batches[0].items[0].original, "/mnt/disk1/Movies/Old");
        assert!(done.detail.contains("disk1's quarantine"));
        // No temporary files left.
        let leftovers: Vec<_> = std::fs::read_dir(system.file("/mnt/disk2/Movies/Old"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".disktree-part"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn refuses_what_is_unsafe() {
        let (_dir, system, mut array) = setup();
        // Something already there: nothing is overwritten.
        let clash = system.file("/mnt/disk2/Movies/Old/a.mkv");
        std::fs::create_dir_all(clash.parent().unwrap()).unwrap();
        std::fs::write(&clash, b"other").unwrap();
        let p = plan(&system, &array, &[], "Movies", "Old", "disk1", "disk2", 1);
        assert!(!p.ok());
        assert!(
            p.problems[0].contains("nothing is overwritten"),
            "{:?}",
            p.problems
        );
        std::fs::remove_file(&clash).unwrap();
        // No room.
        for disk in &mut array.disks {
            if disk.name == "disk2" {
                disk.free = 1000;
            }
        }
        let p = plan(&system, &array, &[], "Movies", "Old", "disk1", "disk2", 1);
        assert!(
            p.problems.iter().any(|p| p.contains("free")),
            "{:?}",
            p.problems
        );
        // Bad names, same disk, parity, protected.
        assert!(
            !plan(
                &system,
                &array,
                &[],
                "Movies",
                "../etc",
                "disk1",
                "disk2",
                1
            )
            .ok()
        );
        assert!(!plan(&system, &array, &[], "Movies", "Old", "disk1", "disk1", 1).ok());
        assert!(!plan(&system, &array, &[], "Movies", "Old", "parity", "disk1", 1).ok());
        let rules = vec![Rule {
            path: "/mnt/user/Movies/Old".into(),
            why: "mounted into plex".into(),
            all: false,
            whole: false,
        }];
        let p = plan(
            &system, &array, &rules, "Movies", "Old", "disk1", "disk2", 1,
        );
        assert!(p.problems[0].contains("mounted into plex"));
        // A whole share is fine to move.
        let rules = vec![Rule {
            path: "/mnt/user/Movies".into(),
            why: "a whole share".into(),
            all: false,
            whole: true,
        }];
        assert!(plan(&system, &array, &rules, "Movies", "", "disk1", "cache", 1).ok());
    }

    #[test]
    fn cancelling_leaves_originals() {
        let (_dir, system, array) = setup();
        let plan = plan(&system, &array, &[], "Movies", "Old", "disk1", "disk2", 7);
        let quarantine = Quarantine::new(Box::new(UnraidPlaces {
            system: system.clone(),
            array: array.clone(),
        }));
        let done = run(
            &system,
            &plan,
            &quarantine,
            &mut |_| {},
            &AtomicBool::new(true),
        );
        assert_eq!(done.state, "cancelled");
        assert!(system.file("/mnt/disk1/Movies/Old/a.mkv").exists());
        assert!(quarantine.list().is_empty());
    }

    fn write(system: &System, path: &str) {
        let file = system.file(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, b"x").unwrap();
    }

    #[test]
    fn lists_folders_without_waking_disks() {
        let (_dir, system, _) = setup();
        write(&system, "/mnt/disk2/Movies/Old/x.mkv");
        write(&system, "/mnt/cache/Movies/New/y.mkv");
        std::fs::create_dir_all(system.file("/mnt/disk1/Movies/.Recycle.Bin")).unwrap();
        // disk2 is asleep.
        let array = array::read(&system);
        let found = folders(&system, &array, "Movies", "", false).unwrap();
        assert_eq!(found.disks, ["disk1", "cache"]);
        assert_eq!(found.asleep, ["disk2"]);
        let names: Vec<(&str, Vec<&str>)> = found
            .children
            .iter()
            .map(|f| {
                (
                    f.name.as_str(),
                    f.disks.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                ("Keep", vec!["disk1"]),
                ("New", vec!["cache"]),
                ("Old", vec!["disk1"])
            ]
        );

        // Woken on purpose, and a folder further in.
        let found = folders(&system, &array, "Movies", "/Old/", true).unwrap();
        assert_eq!(found.disks, ["disk1", "disk2"]);
        assert!(found.asleep.is_empty());
        assert_eq!(found.children.len(), 1);
        assert_eq!(found.children[0].name, "sub");

        // Nowhere: no disks, nothing in it.
        let found = folders(&system, &array, "Movies", "Gone", true).unwrap();
        assert!(found.disks.is_empty() && found.children.is_empty());

        for (share, folder) in [("", ""), ("Movies", "../etc"), ("../boot", ""), ("a/b", "")] {
            assert!(
                folders(&system, &array, share, folder, true).is_err(),
                "{share} {folder}"
            );
        }
    }

    #[test]
    fn finds_files_on_two_source_disks() {
        let (_dir, system, array) = setup();
        write(&system, "/mnt/disk2/Movies/Old/a.mkv");
        write(&system, "/mnt/disk2/Movies/Old/only-here.mkv");
        let plan_from =
            |from: &str| plan(&system, &array, &[], "Movies", "Old", from, "cache", 1000);
        let one = plan_from("disk1");
        assert_eq!(duplicates(std::slice::from_ref(&one)), (vec![], 0));
        let (shown, total) = duplicates(&[one, plan_from("disk2")]);
        assert_eq!(total, 1);
        assert_eq!(shown, ["/mnt/user/Movies/Old/a.mkv"]);
    }
}
