//! Space that can be won back in a scanned tree: folders that tools rebuild
//! on their own, large files nobody has changed in a long time, and files
//! stored more than once.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use xxhash_rust::xxh3::{Xxh3, xxh3_128};

use crate::tree::{NodeId, NodeKind, SizeMode, Tree, flags};

/// A folder that a tool recreates when it's needed again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rebuildable {
    pub node: NodeId,
    /// What the folder holds, e.g. "npm packages".
    pub what: &'static str,
    /// How it comes back, e.g. "npm install puts them back".
    pub how: &'static str,
}

/// When a folder counts as rebuildable.
struct Rule {
    name: &'static str,
    /// The parent folder must contain one of these entries.
    sibling: &'static [&'static str],
    /// The parent folder must have this name.
    parent: Option<&'static str>,
    what: &'static str,
    how: &'static str,
}

const RULES: &[Rule] = &[
    Rule {
        name: "node_modules",
        sibling: &[],
        parent: None,
        what: "npm packages",
        how: "npm install puts them back",
    },
    Rule {
        name: "target",
        sibling: &["Cargo.toml"],
        parent: None,
        what: "Rust build output",
        how: "cargo build makes it again",
    },
    Rule {
        name: ".gradle",
        sibling: &[],
        parent: None,
        what: "Gradle cache",
        how: "Gradle makes it again",
    },
    Rule {
        name: "build",
        sibling: &["build.gradle", "build.gradle.kts"],
        parent: None,
        what: "Gradle build output",
        how: "the next build makes it again",
    },
    Rule {
        name: "__pycache__",
        sibling: &[],
        parent: None,
        what: "Python bytecode",
        how: "Python makes it again",
    },
    Rule {
        name: ".next",
        sibling: &["package.json"],
        parent: None,
        what: "Next.js build cache",
        how: "the next build makes it again",
    },
    Rule {
        name: ".parcel-cache",
        sibling: &[],
        parent: None,
        what: "Parcel build cache",
        how: "the next build makes it again",
    },
    Rule {
        name: ".turbo",
        sibling: &[],
        parent: None,
        what: "Turborepo cache",
        how: "the next build makes it again",
    },
    Rule {
        name: "DerivedData",
        sibling: &[],
        parent: Some("Xcode"),
        what: "Xcode build data",
        how: "Xcode makes it again",
    },
    Rule {
        name: ".cache",
        // A home folder: ~/.cache next to ~/.config or ~/.local.
        sibling: &[".config", ".local"],
        parent: None,
        what: "app caches",
        how: "apps make them again; close them first",
    },
    Rule {
        name: "Caches",
        sibling: &[],
        parent: Some("Library"),
        what: "app caches",
        how: "apps make them again; close them first",
    },
    Rule {
        name: "_cacache",
        sibling: &[],
        parent: Some(".npm"),
        what: "npm download cache",
        how: "npm downloads packages again when needed",
    },
    Rule {
        name: "registry",
        sibling: &[],
        parent: Some(".cargo"),
        what: "Cargo download cache",
        how: "cargo downloads crates again when needed",
    },
    Rule {
        name: "shadercache",
        sibling: &[],
        parent: Some("steamapps"),
        what: "Steam shader cache",
        how: "Steam downloads or builds it again; first launches may stutter",
    },
];

/// A folder holding a Python virtual environment.
const VENV_MARKER: &str = "pyvenv.cfg";

/// Rebuildable folders below `under`, in tree order. A matched folder isn't
/// searched further, so `node_modules` inside `node_modules` isn't listed
/// twice.
pub fn rebuildable(tree: &Tree, under: NodeId) -> Vec<Rebuildable> {
    let mut found = Vec::new();
    let mut stack = vec![under];
    while let Some(dir) = stack.pop() {
        for child in tree.children(dir) {
            if tree.node(child).kind != NodeKind::Dir {
                continue;
            }
            match rule_for(tree, dir, child) {
                Some((what, how)) => found.push(Rebuildable {
                    node: child,
                    what,
                    how,
                }),
                None => stack.push(child),
            }
        }
    }
    found.sort_by_key(|r| r.node);
    found
}

fn rule_for(tree: &Tree, parent: NodeId, dir: NodeId) -> Option<(&'static str, &'static str)> {
    let name = tree.name(dir);
    let has_child = |of: NodeId, name: &str| tree.child_by_name(of, name.as_ref()).is_some();
    for rule in RULES {
        if name != rule.name {
            continue;
        }
        if let Some(wanted) = rule.parent
            && tree.name(parent) != wanted
        {
            continue;
        }
        if !rule.sibling.is_empty() && !rule.sibling.iter().any(|s| has_child(parent, s)) {
            continue;
        }
        return Some((rule.what, rule.how));
    }
    has_child(dir, VENV_MARKER).then_some((
        "Python virtual environment",
        "python -m venv and pip install make it again",
    ))
}

/// The `n` largest files below `under` last modified before `before`
/// (seconds since 1970), largest first.
pub fn old_files(tree: &Tree, under: NodeId, mode: SizeMode, before: i64, n: usize) -> Vec<NodeId> {
    let mut files: Vec<(u64, NodeId)> = tree
        .descendants(under)
        .filter_map(|id| {
            let node = tree.node(id);
            let old = node.modified > 0 && node.modified < before;
            (node.kind == NodeKind::File && !node.has(flags::HARDLINK_DUP) && old)
                .then(|| (node.size_in(mode), id))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    files.truncate(n);
    files.into_iter().map(|(_, id)| id).collect()
}

/// Files with identical contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateGroup {
    /// Size of each copy in bytes.
    pub size: u64,
    /// Two or more files, in tree order.
    pub nodes: Vec<NodeId>,
}

impl DuplicateGroup {
    /// Space freed by keeping only one copy.
    pub fn wasted(&self) -> u64 {
        self.size * (self.nodes.len() as u64 - 1)
    }
}

/// Progress of [`find_duplicates`], readable from another thread.
#[derive(Debug, Default)]
pub struct DuplicateProgress {
    /// Files that need reading, known once sizes are compared.
    pub files: AtomicU64,
    /// Files read so far.
    pub done: AtomicU64,
    cancelled: AtomicBool,
}

impl DuplicateProgress {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// How much of each end of a file the first pass reads.
const SAMPLE: u64 = 64 * 1024;

/// Files of equal size below some folder: the input of the content
/// comparison. Owns its paths, so the slow part can run on another thread
/// while the tree stays where it is.
#[derive(Clone, Debug, Default)]
pub struct DuplicateCandidates {
    /// Size, then the files of that size. Biggest first.
    groups: Vec<(u64, Vec<(NodeId, PathBuf)>)>,
}

/// Collects the files below `under` of at least `min_size` bytes that share
/// their size with another file. Cloud placeholders are skipped, since
/// reading them would download them, and so are extra hard links, which
/// take no space. Fast: nothing is read yet.
pub fn duplicate_candidates(tree: &Tree, under: NodeId, min_size: u64) -> DuplicateCandidates {
    let mut by_size: HashMap<u64, Vec<NodeId>> = HashMap::new();
    for id in tree.descendants(under) {
        let node = tree.node(id);
        let skip = flags::HARDLINK_DUP | flags::CLOUD | flags::ERROR;
        if node.kind == NodeKind::File && node.flags & skip == 0 && node.size >= min_size.max(1) {
            by_size.entry(node.size).or_default().push(id);
        }
    }
    let mut groups: Vec<(u64, Vec<(NodeId, PathBuf)>)> = by_size
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(size, ids)| {
            (
                size,
                ids.into_iter().map(|id| (id, tree.path(id))).collect(),
            )
        })
        .collect();
    // Biggest first, so stopping early still finds what matters most.
    groups.sort_by_key(|(size, _)| std::cmp::Reverse(*size));
    DuplicateCandidates { groups }
}

impl DuplicateCandidates {
    /// How many files the comparison will read.
    pub fn files(&self) -> usize {
        self.groups.iter().map(|(_, files)| files.len()).sum()
    }

    /// Compares the files' contents, most space wasted first. Files of the
    /// same size are compared by a hash of their first and last 64 KB, and
    /// only files that still match are read in full. Unreadable files are
    /// left out. Cancelling returns what was confirmed so far.
    pub fn find(self, progress: &DuplicateProgress) -> Vec<DuplicateGroup> {
        progress.files.store(self.files() as u64, Ordering::Relaxed);
        let mut groups = Vec::new();
        'sizes: for (size, files) in self.groups {
            let mut by_sample: HashMap<u128, Vec<(NodeId, PathBuf)>> = HashMap::new();
            for (id, path) in files {
                if progress.is_cancelled() {
                    break 'sizes;
                }
                if let Some(hash) = sample_hash(&path, size) {
                    by_sample.entry(hash).or_default().push((id, path));
                }
                progress.done.fetch_add(1, Ordering::Relaxed);
            }
            for same in by_sample.into_values().filter(|files| files.len() > 1) {
                // Small files were read whole by the sample already.
                if size <= 2 * SAMPLE {
                    let nodes = same.into_iter().map(|(id, _)| id).collect();
                    groups.push(DuplicateGroup { size, nodes });
                    continue;
                }
                let mut by_content: HashMap<u128, Vec<NodeId>> = HashMap::new();
                for (id, path) in same {
                    if progress.is_cancelled() {
                        break 'sizes;
                    }
                    if let Some(hash) = full_hash(&path) {
                        by_content.entry(hash).or_default().push(id);
                    }
                }
                groups.extend(
                    by_content
                        .into_values()
                        .filter(|ids| ids.len() > 1)
                        .map(|nodes| DuplicateGroup { size, nodes }),
                );
            }
        }
        for group in &mut groups {
            group.nodes.sort_unstable();
        }
        groups.sort_by(|a, b| b.wasted().cmp(&a.wasted()).then(a.nodes.cmp(&b.nodes)));
        groups
    }
}

/// Finds files below `under` of at least `min_size` bytes whose contents are
/// identical: [`duplicate_candidates`] followed by
/// [`DuplicateCandidates::find`].
pub fn find_duplicates(
    tree: &Tree,
    under: NodeId,
    min_size: u64,
    progress: &DuplicateProgress,
) -> Vec<DuplicateGroup> {
    duplicate_candidates(tree, under, min_size).find(progress)
}

/// Hash of the first and last [`SAMPLE`] bytes (the whole file if small).
fn sample_hash(path: &Path, size: u64) -> Option<u128> {
    let mut file = File::open(path).ok()?;
    if size <= 2 * SAMPLE {
        let mut data = Vec::with_capacity(size as usize);
        file.read_to_end(&mut data).ok()?;
        return Some(xxh3_128(&data));
    }
    let mut buf = vec![0u8; SAMPLE as usize];
    let mut hasher = Xxh3::new();
    file.read_exact(&mut buf).ok()?;
    hasher.update(&buf);
    file.seek(SeekFrom::Start(size - SAMPLE)).ok()?;
    file.read_exact(&mut buf).ok()?;
    hasher.update(&buf);
    Some(hasher.digest128())
}

fn full_hash(path: &Path) -> Option<u128> {
    let mut file = File::open(path).ok()?;
    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => return Some(hasher.digest128()),
            Ok(n) => hasher.update(&buf[..n]),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{ScanOptions, ScanProgress, scan};

    fn write(root: &Path, path: &str, data: &[u8]) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }

    fn scanned(root: &Path) -> Tree {
        scan(root, &ScanOptions::default(), &ScanProgress::new())
            .unwrap()
            .tree
    }

    /// Names, sorted: tree order depends on which scan worker ran first.
    fn names(tree: &Tree, ids: &[NodeId]) -> Vec<String> {
        let mut names: Vec<String> = ids.iter().map(|&id| tree.name(id).into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn finds_rebuildable_folders_only_where_they_belong() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "web/package.json", b"{}");
        write(root, "web/node_modules/a/index.js", b"x");
        write(root, "web/node_modules/a/node_modules/b.js", b"x");
        write(root, "rust/Cargo.toml", b"");
        write(root, "rust/target/debug/app", b"x");
        write(root, "photos/target/shot.jpg", b"x");
        write(root, "py/venv/pyvenv.cfg", b"");
        write(root, "py/src/__pycache__/m.pyc", b"x");
        write(root, "home/.config/x", b"");
        write(root, "home/.cache/thumbs/t.png", b"x");
        write(root, "other/.cache/keep", b"x");
        let tree = scanned(root);
        let found = rebuildable(&tree, tree.root());
        let mut seen: Vec<(String, &str)> = found
            .iter()
            .map(|r| {
                (
                    tree.path(r.node)
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    r.what,
                )
            })
            .collect();
        seen.sort();
        assert_eq!(
            seen,
            [
                ("home/.cache".to_string(), "app caches"),
                ("py/src/__pycache__".to_string(), "Python bytecode"),
                ("py/venv".to_string(), "Python virtual environment"),
                ("rust/target".to_string(), "Rust build output"),
                ("web/node_modules".to_string(), "npm packages"),
            ]
        );
    }

    #[test]
    fn lists_old_files_by_size() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "new.bin", &[0; 300]);
        write(root, "old_big.bin", &[0; 200]);
        write(root, "old_small.bin", &[0; 100]);
        let year_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(400 * 86_400);
        for name in ["old_big.bin", "old_small.bin"] {
            let file = fs::File::options()
                .write(true)
                .open(root.join(name))
                .unwrap();
            file.set_modified(year_ago).unwrap();
        }
        let tree = scanned(root);
        let cutoff = (std::time::SystemTime::now() - std::time::Duration::from_secs(365 * 86_400))
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let old = old_files(&tree, tree.root(), SizeMode::Logical, cutoff, 10);
        assert_eq!(names(&tree, &old), ["old_big.bin", "old_small.bin"]);
    }

    #[test]
    fn finds_duplicates_by_content() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let mut almost = big.clone();
        almost[150_000] ^= 1; // same size, same ends, different middle
        write(root, "a/movie.mp4", &big);
        write(root, "b/movie copy.mp4", &big);
        write(root, "c/edited.mp4", &almost);
        write(root, "small1.txt", b"hello world");
        write(root, "small2.txt", b"hello world");
        write(root, "tiny.txt", b"hi");
        write(root, "tiny2.txt", b"hi");
        let tree = scanned(root);
        let progress = DuplicateProgress::new();
        let groups = find_duplicates(&tree, tree.root(), 5, &progress);
        assert_eq!(groups.len(), 2);
        assert_eq!(
            names(&tree, &groups[0].nodes),
            ["movie copy.mp4", "movie.mp4"]
        );
        assert_eq!(groups[0].wasted(), 300_000);
        assert_eq!(names(&tree, &groups[1].nodes), ["small1.txt", "small2.txt"]);
        assert_eq!(progress.done.load(Ordering::Relaxed), 5);
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_are_not_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "one.bin", &[7; 1000]);
        fs::hard_link(root.join("one.bin"), root.join("two.bin")).unwrap();
        let tree = scanned(root);
        assert!(find_duplicates(&tree, tree.root(), 1, &DuplicateProgress::new()).is_empty());
    }

    #[test]
    fn cancelling_stops_early() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "x1", &[1; 100]);
        write(root, "x2", &[1; 100]);
        let tree = scanned(root);
        let progress = DuplicateProgress::new();
        progress.cancel();
        assert!(find_duplicates(&tree, tree.root(), 1, &progress).is_empty());
    }
}

/// How a system or app cache should be removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Safety {
    /// Delete it; what uses it makes it again.
    Safe,
    /// Probably not needed, but look first.
    CheckFirst,
    /// Remove it with the tool named in `how`, not by deleting it.
    UseTool,
}

/// A known place where the system or an app keeps space it can do without.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpotRule {
    /// Absolute path; `*` stands for any one name.
    pub pattern: String,
    pub what: &'static str,
    pub how: &'static str,
    pub safety: Safety,
}

/// A system or app cache found in a scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spot {
    pub node: NodeId,
    pub what: &'static str,
    pub how: &'static str,
    pub safety: Safety,
}

/// The places `rules` describe that lie inside `tree`, biggest first.
/// Names compare without regard to case when `ignore_case` (Windows, macOS).
pub fn system_spots(tree: &Tree, rules: &[SpotRule], ignore_case: bool) -> Vec<Spot> {
    let root = tree.root();
    let root_path = tree.path(root);
    let mut found: Vec<Spot> = Vec::new();
    for rule in rules {
        let pattern = Path::new(&rule.pattern);
        let rest = if ignore_case {
            strip_prefix_ignore_case(pattern, &root_path)
        } else {
            pattern.strip_prefix(&root_path).ok().map(Path::to_path_buf)
        };
        let Some(rest) = rest else {
            continue;
        };
        let names: Vec<String> = rest
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let mut matches = Vec::new();
        walk_pattern(tree, root, &names, ignore_case, &mut matches);
        for node in matches {
            if tree.node(node).allocated > 0 && !found.iter().any(|s| s.node == node) {
                found.push(Spot {
                    node,
                    what: rule.what,
                    how: rule.how,
                    safety: rule.safety,
                });
            }
        }
    }
    found.sort_by_key(|s| std::cmp::Reverse(tree.node(s.node).allocated));
    found
}

fn strip_prefix_ignore_case(path: &Path, prefix: &Path) -> Option<PathBuf> {
    let mut path_parts = path.components();
    for want in prefix.components() {
        let got = path_parts.next()?;
        if !got
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&want.as_os_str().to_string_lossy())
        {
            return None;
        }
    }
    Some(path_parts.as_path().to_path_buf())
}

fn walk_pattern(
    tree: &Tree,
    at: NodeId,
    names: &[String],
    ignore_case: bool,
    out: &mut Vec<NodeId>,
) {
    let Some((first, rest)) = names.split_first() else {
        out.push(at);
        return;
    };
    for child in tree.children(at) {
        let name = tree.name(child);
        let matches = first == "*"
            || if ignore_case {
                name.eq_ignore_ascii_case(first)
            } else {
                name == first.as_str()
            };
        if matches && (rest.is_empty() || tree.node(child).is_dir()) {
            walk_pattern(tree, child, rest, ignore_case, out);
        }
    }
}

/// The usual places of this platform's system and app caches, from the
/// environment (home folder, `%LOCALAPPDATA%`, `%windir%`).
pub fn default_spot_rules() -> Vec<SpotRule> {
    let mut rules = Vec::new();
    let mut add = |pattern: PathBuf, what, how, safety| {
        rules.push(SpotRule {
            pattern: pattern.to_string_lossy().into_owned(),
            what,
            how,
            safety,
        });
    };
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);

    if cfg!(windows) {
        // SystemDrive is "C:"; the drive's top folder is "C:" plus a backslash.
        let drive = env("SystemDrive").map_or_else(
            || PathBuf::from("C:\\"),
            |d| PathBuf::from(format!("{}\\", d.to_string_lossy())),
        );
        let windows = env("windir").unwrap_or_else(|| drive.join("Windows"));
        add(
            drive.join("Windows.old"),
            "Previous Windows installation",
            "Settings → System → Storage → Temporary files → Previous Windows installation(s)",
            Safety::UseTool,
        );
        for name in ["$WINDOWS.~BT", "$Windows.~WS"] {
            add(
                drive.join(name),
                "Windows upgrade files",
                "Settings → System → Storage → Temporary files",
                Safety::UseTool,
            );
        }
        add(
            windows.join("SoftwareDistribution").join("Download"),
            "Windows Update downloads",
            "Settings → System → Storage → Temporary files → Windows Update Cleanup",
            Safety::UseTool,
        );
        add(
            windows.join("ServiceProfiles/NetworkService/AppData/Local/Microsoft/Windows/DeliveryOptimization/Cache"),
            "Delivery Optimization cache",
            "Settings → System → Storage → Temporary files → Delivery Optimization Files",
            Safety::UseTool,
        );
        add(
            windows.join("Temp"),
            "Windows temporary files",
            "files in use stay; the rest can go",
            Safety::Safe,
        );
        add(
            windows.join("Minidump"),
            "Crash dumps",
            "only needed to report a crash",
            Safety::Safe,
        );
        add(
            windows.join("MEMORY.DMP"),
            "Crash dump",
            "only needed to report a crash",
            Safety::Safe,
        );
        add(
            drive.join("hiberfil.sys"),
            "Hibernation file",
            "run `powercfg /h off` as administrator if you don't use hibernation or Fast Startup",
            Safety::UseTool,
        );
        if let Some(local) = env("LOCALAPPDATA") {
            add(
                local.join("Temp"),
                "Your temporary files",
                "files in use stay; the rest can go",
                Safety::Safe,
            );
            add(
                local.join("CrashDumps"),
                "Crash dumps",
                "only needed to report a crash",
                Safety::Safe,
            );
            add(
                local.join("Microsoft/Windows/INetCache"),
                "Internet cache",
                "rebuilt as needed",
                Safety::Safe,
            );
            for (browser, dir) in [
                ("Chrome", "Google/Chrome/User Data"),
                ("Edge", "Microsoft/Edge/User Data"),
                ("Brave", "BraveSoftware/Brave-Browser/User Data"),
            ] {
                let what = match browser {
                    "Chrome" => "Chrome cache",
                    "Edge" => "Edge cache",
                    _ => "Brave cache",
                };
                for cache in ["Cache", "Code Cache", "GPUCache"] {
                    add(
                        local.join(dir).join("*").join(cache),
                        what,
                        "close the browser first; it makes it again",
                        Safety::Safe,
                    );
                }
            }
            add(
                local.join("Mozilla/Firefox/Profiles/*/cache2"),
                "Firefox cache",
                "close Firefox first; it makes it again",
                Safety::Safe,
            );
            for dir in [
                "NVIDIA/DXCache",
                "NVIDIA/GLCache",
                "AMD/DxCache",
                "D3DSCache",
            ] {
                add(
                    local.join(dir),
                    "Graphics shader cache",
                    "drivers make it again; first game launches may stutter",
                    Safety::Safe,
                );
            }
        }
    } else if cfg!(target_os = "macos") {
        if let Some(home) = env("HOME") {
            add(
                home.join(".Trash"),
                "Trash",
                "empty it in Finder",
                Safety::Safe,
            );
            add(
                home.join("Library/Logs"),
                "App logs",
                "old logs are rarely needed",
                Safety::CheckFirst,
            );
            add(
                home.join("Library/Application Support/MobileSync/Backup"),
                "iPhone and iPad backups",
                "Finder → your device → Manage Backups; keep the latest",
                Safety::CheckFirst,
            );
            add(
                home.join("Library/Developer/Xcode/Archives"),
                "Xcode archives",
                "old app builds; keep the ones you may need to debug",
                Safety::CheckFirst,
            );
            add(
                home.join("Library/Developer/Xcode/iOS DeviceSupport"),
                "Xcode device support",
                "Xcode downloads it again for a connected device",
                Safety::Safe,
            );
            add(
                home.join("Library/Developer/CoreSimulator/Devices"),
                "iOS simulators",
                "run `xcrun simctl delete unavailable`",
                Safety::UseTool,
            );
        }
        add(
            PathBuf::from("/Library/Updates"),
            "Downloaded macOS updates",
            "System Settings → General → Software Update",
            Safety::UseTool,
        );
    } else if cfg!(target_os = "linux") && !cfg!(target_os = "android") {
        if let Some(home) = env("HOME") {
            add(
                home.join(".cache/thumbnails"),
                "Thumbnail cache",
                "file managers make it again",
                Safety::Safe,
            );
            add(
                home.join(".local/share/Trash"),
                "Trash",
                "empty it in the file manager",
                Safety::Safe,
            );
            add(
                home.join(".var/app/*/cache"),
                "Flatpak app caches",
                "apps make them again; close them first",
                Safety::Safe,
            );
        }
        add(
            PathBuf::from("/var/cache/apt/archives"),
            "Downloaded packages",
            "run `sudo apt clean`",
            Safety::UseTool,
        );
        add(
            PathBuf::from("/var/cache/dnf"),
            "Downloaded packages",
            "run `sudo dnf clean all`",
            Safety::UseTool,
        );
        add(
            PathBuf::from("/var/cache/pacman/pkg"),
            "Downloaded packages",
            "run `sudo paccache -r`",
            Safety::UseTool,
        );
        add(
            PathBuf::from("/var/log/journal"),
            "System journal",
            "run `sudo journalctl --vacuum-size=200M`",
            Safety::UseTool,
        );
    }
    rules
}

#[cfg(test)]
mod spot_tests {
    use std::fs;

    use super::*;
    use crate::{ScanOptions, ScanProgress, scan};

    #[test]
    fn finds_known_places_inside_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (path, bytes) in [
            ("Users/me/AppData/Local/Temp/a.tmp", 3000),
            (
                "Users/me/AppData/Local/Google/Chrome/User Data/Default/Cache/x",
                5000,
            ),
            (
                "Users/me/AppData/Local/Google/Chrome/User Data/Profile 1/Cache/y",
                1000,
            ),
            ("Users/me/Documents/keep.txt", 100),
        ] {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![1u8; bytes]).unwrap();
        }
        let local = root.join("Users/me/AppData/Local");
        let rule = |pattern: PathBuf, what| SpotRule {
            pattern: pattern.to_string_lossy().into_owned(),
            what,
            how: "",
            safety: Safety::Safe,
        };
        let rules = [
            rule(local.join("temp"), "temp"),
            rule(local.join("Google/Chrome/User Data/*/Cache"), "chrome"),
            rule(root.join("Elsewhere/Cache"), "missing"),
            rule(PathBuf::from("/not/in/the/scan"), "outside"),
        ];
        let tree = scan(root, &ScanOptions::default(), &ScanProgress::new())
            .unwrap()
            .tree;
        let found: Vec<(&str, String)> = system_spots(&tree, &rules, true)
            .iter()
            .map(|s| (s.what, tree.name(s.node).into_owned()))
            .collect();
        assert_eq!(found.len(), 3);
        assert!(found.contains(&("temp", "Temp".to_string())));
        assert_eq!(found.iter().filter(|(w, _)| *w == "chrome").count(), 2);
        // Case matters where the platform says so.
        assert_eq!(system_spots(&tree, &rules[..1], false).len(), 0);
        let steam = rebuildable_names(&["steamapps/shadercache/1/x"]);
        assert_eq!(steam, ["shadercache"]);
    }

    fn rebuildable_names(paths: &[&str]) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        for path in paths {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"x").unwrap();
        }
        let tree = scan(dir.path(), &ScanOptions::default(), &ScanProgress::new())
            .unwrap()
            .tree;
        rebuildable(&tree, tree.root())
            .iter()
            .map(|r| tree.name(r.node).into_owned())
            .collect()
    }
}
