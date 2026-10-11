//! Parallel directory scanner.
//!
//! Worker threads list directories and read metadata; a single builder thread
//! (the caller's) owns the [`Tree`], inserts each listing as one contiguous
//! block of children and hands the subdirectories back to the workers. Only
//! the builder touches the tree, so there is no locking on the hot path.

use std::collections::HashSet;
use std::fs::{self, DirEntry};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::unbounded;

use crate::platform::{self, EntryInfo, Volume};
use crate::tree::{Node, NodeId, NodeKind, Tree, flags};

#[derive(Clone, Debug)]
pub struct ScanOptions {
    /// Worker threads; 0 picks a default from the CPU count.
    pub threads: usize,
    /// Don't descend into directories on other filesystems (like `du -x`).
    pub one_filesystem: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            threads: 0,
            one_filesystem: true,
        }
    }
}

impl ScanOptions {
    fn thread_count(&self) -> usize {
        if self.threads > 0 {
            return self.threads;
        }
        // Scanning mostly waits on the disk, so use more threads than cores.
        let cores = thread::available_parallelism().map_or(4, |n| n.get());
        (cores * 2).clamp(4, 32)
    }
}

/// Live counters a UI can poll while a scan runs, plus a cancel switch.
#[derive(Debug, Default)]
pub struct ScanProgress {
    files: AtomicU64,
    dirs: AtomicU64,
    bytes: AtomicU64,
    errors: AtomicU64,
    cancelled: AtomicBool,
    current: Mutex<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct ProgressSnapshot {
    pub files: u64,
    pub dirs: u64,
    /// Logical bytes seen so far.
    pub bytes: u64,
    pub errors: u64,
    /// A directory currently being read.
    pub current: PathBuf,
}

impl ScanProgress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the scan to stop. It returns soon after with the partial tree.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            files: self.files.load(Ordering::Relaxed),
            dirs: self.dirs.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            current: self.current.lock().map(|p| p.clone()).unwrap_or_default(),
        }
    }
}

#[derive(Debug)]
pub struct ScanResult {
    pub tree: Tree,
    pub elapsed: Duration,
    /// Entries or directories that could not be read.
    pub errors: u64,
    /// The scan was cancelled; the tree is incomplete.
    pub cancelled: bool,
}

/// Scans `root` and returns the tree of everything below it.
///
/// Symbolic links (and Windows junctions) are recorded but never followed.
/// Only reading the root itself can fail; unreadable entries below it are
/// flagged with [`flags::ERROR`] and counted in [`ScanResult::errors`].
pub fn scan(
    root: impl AsRef<Path>,
    options: &ScanOptions,
    progress: &ScanProgress,
) -> io::Result<ScanResult> {
    let started = Instant::now();
    let root = std::path::absolute(root.as_ref())?;
    let root_meta = fs::metadata(&root)?;
    let volume = platform::volume(&root);
    let root_info = platform::entry_info(&root_meta, &|| root.clone(), &volume);

    let mut root_node = Node::new(
        if root_meta.is_dir() {
            NodeKind::Dir
        } else {
            NodeKind::File
        },
        0,
    );
    root_node.allocated = root_info.allocated;
    root_node.modified = root_info.modified;
    if !root_meta.is_dir() {
        root_node.size = root_meta.len();
        root_node.files = 1;
    }
    let mut tree = Tree::with_root(root.as_os_str(), root_node);

    if root_meta.is_dir() {
        let ctx = Ctx {
            one_filesystem: options.one_filesystem,
            root_device: root_info.device,
            volume,
            progress,
        };
        build(&mut tree, &root, &ctx, options.thread_count());
        tree.aggregate();
    }

    Ok(ScanResult {
        tree,
        elapsed: started.elapsed(),
        errors: progress.errors.load(Ordering::Relaxed),
        cancelled: progress.is_cancelled(),
    })
}

struct Ctx<'a> {
    one_filesystem: bool,
    root_device: u64,
    volume: Volume,
    progress: &'a ScanProgress,
}

struct Job {
    node: NodeId,
    path: PathBuf,
}

/// The contents of one directory, produced by a worker.
#[derive(Default)]
struct Listing {
    node: NodeId,
    error: bool,
    /// All entry names back to back; entries point into it.
    names: Vec<u8>,
    entries: Vec<RawEntry>,
    /// Paths of the entries with `descend` set, in order.
    subdirs: Vec<PathBuf>,
}

struct RawEntry {
    name_start: u32,
    name_len: u16,
    kind: NodeKind,
    flags: u8,
    size: u64,
    info: EntryInfo,
    descend: bool,
}

fn build(tree: &mut Tree, root: &Path, ctx: &Ctx, threads: usize) {
    let (job_tx, job_rx) = unbounded::<Job>();
    let (listing_tx, listing_rx) = unbounded::<Listing>();

    thread::scope(|scope| {
        for _ in 0..threads {
            let job_rx = job_rx.clone();
            let listing_tx = listing_tx.clone();
            scope.spawn(move || {
                for job in job_rx {
                    // A panic would lose this directory and leave the builder
                    // waiting for it forever, so turn it into an error listing.
                    let listing = catch_unwind(AssertUnwindSafe(|| list_dir(&job, ctx)))
                        .unwrap_or_else(|_| {
                            ctx.progress.errors.fetch_add(1, Ordering::Relaxed);
                            Listing {
                                node: job.node,
                                error: true,
                                ..Listing::default()
                            }
                        });
                    if listing_tx.send(listing).is_err() {
                        break;
                    }
                }
            });
        }
        drop(job_rx);
        drop(listing_tx);

        let mut hardlinks = HashSet::new();
        let mut pending = 1usize;
        let mut received = 0u64;
        job_tx
            .send(Job {
                node: tree.root(),
                path: root.to_path_buf(),
            })
            .expect("workers are running");

        while pending > 0 {
            let Ok(listing) = listing_rx.recv() else {
                break;
            };
            pending -= 1;
            received += 1;
            let subdirs = insert_listing(tree, listing, &mut hardlinks);
            for (i, (node, path)) in subdirs.into_iter().enumerate() {
                if i == 0
                    && received.is_multiple_of(64)
                    && let Ok(mut current) = ctx.progress.current.lock()
                {
                    current.clone_from(&path);
                }
                pending += 1;
                if job_tx.send(Job { node, path }).is_err() {
                    pending -= 1;
                }
            }
        }
        drop(job_tx);
    });
}

fn insert_listing(
    tree: &mut Tree,
    listing: Listing,
    hardlinks: &mut HashSet<(u64, u64)>,
) -> Vec<(NodeId, PathBuf)> {
    let parent = listing.node;
    if listing.error {
        tree.nodes[parent as usize].flags |= flags::ERROR;
    }
    if listing.entries.is_empty() {
        return Vec::new();
    }

    let first = NodeId::try_from(tree.nodes.len()).expect("more than 4 billion entries");
    let count = u32::try_from(listing.entries.len()).expect("directory too large");
    let parent_node = &mut tree.nodes[parent as usize];
    parent_node.first_child = first;
    parent_node.child_count = count;

    let base = u32::try_from(tree.names.len()).expect("name buffer exceeds 4 GiB");
    tree.names.extend_from_slice(&listing.names);

    let mut subdir_paths = listing.subdirs.into_iter();
    let mut subdirs = Vec::new();
    tree.nodes.reserve(listing.entries.len());
    for entry in listing.entries {
        let mut node = Node::new(entry.kind, parent);
        node.name_start = base + entry.name_start;
        node.name_len = entry.name_len;
        node.flags = entry.flags;
        node.size = entry.size;
        node.allocated = entry.info.allocated;
        node.modified = entry.info.modified;
        node.files = u32::from(entry.kind != NodeKind::Dir);
        if let Some(key) = entry.info.hardlink
            && !hardlinks.insert(key)
        {
            node.flags |= flags::HARDLINK_DUP;
        }
        let id = tree.nodes.len() as NodeId;
        tree.nodes.push(node);
        if entry.descend
            && let Some(path) = subdir_paths.next()
        {
            subdirs.push((id, path));
        }
    }
    subdirs
}

fn list_dir(job: &Job, ctx: &Ctx) -> Listing {
    let mut listing = Listing {
        node: job.node,
        ..Listing::default()
    };
    if ctx.progress.is_cancelled() {
        return listing;
    }
    let read_dir = match fs::read_dir(&job.path) {
        Ok(rd) => rd,
        Err(_) => {
            listing.error = true;
            ctx.progress.errors.fetch_add(1, Ordering::Relaxed);
            return listing;
        }
    };

    let (mut files, mut dirs, mut bytes, mut errors) = (0u64, 0u64, 0u64, 0u64);
    for entry in read_dir {
        let Ok(entry) = entry else {
            listing.error = true;
            errors += 1;
            continue;
        };
        let name = entry.file_name();
        let name = name.as_encoded_bytes();
        let Ok(name_len) = u16::try_from(name.len()) else {
            errors += 1;
            continue;
        };
        let name_start = listing.names.len() as u32;
        listing.names.extend_from_slice(name);

        let raw = read_entry(&entry, ctx, name_start, name_len);
        match raw.kind {
            NodeKind::Dir => dirs += 1,
            _ => {
                files += 1;
                bytes += raw.size;
            }
        }
        if raw.flags & flags::ERROR != 0 {
            errors += 1;
        }
        if raw.descend {
            listing.subdirs.push(entry.path());
        }
        listing.entries.push(raw);
    }

    let p = ctx.progress;
    p.files.fetch_add(files, Ordering::Relaxed);
    p.dirs.fetch_add(dirs, Ordering::Relaxed);
    p.bytes.fetch_add(bytes, Ordering::Relaxed);
    if errors > 0 {
        p.errors.fetch_add(errors, Ordering::Relaxed);
    }
    listing
}

fn read_entry(entry: &DirEntry, ctx: &Ctx, name_start: u32, name_len: u16) -> RawEntry {
    let mut raw = RawEntry {
        name_start,
        name_len,
        kind: NodeKind::Other,
        flags: 0,
        size: 0,
        info: EntryInfo::default(),
        descend: false,
    };

    // On Windows this metadata comes free with the directory listing; on Unix
    // it is an lstat, so symlinks describe themselves rather than their target.
    let meta = match entry.metadata() {
        Ok(meta) => meta,
        Err(_) => {
            raw.flags |= flags::ERROR;
            if let Ok(ft) = entry.file_type() {
                raw.kind = kind_of(&ft);
            }
            return raw;
        }
    };

    raw.kind = kind_of(&meta.file_type());
    raw.info = platform::entry_info(&meta, &|| entry.path(), &ctx.volume);
    if raw.info.cloud {
        raw.flags |= flags::CLOUD;
    }

    if raw.kind == NodeKind::Dir {
        if ctx.one_filesystem && raw.info.device != ctx.root_device {
            raw.flags |= flags::MOUNT_POINT;
        } else {
            raw.descend = true;
        }
    } else {
        raw.size = meta.len();
    }
    raw
}

fn kind_of(ft: &fs::FileType) -> NodeKind {
    if ft.is_symlink() {
        NodeKind::Symlink
    } else if ft.is_dir() {
        NodeKind::Dir
    } else if ft.is_file() {
        NodeKind::File
    } else {
        NodeKind::Other
    }
}
