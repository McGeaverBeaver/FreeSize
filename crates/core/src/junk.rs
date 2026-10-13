//! The junk cleaner: the known places where the system and well-known apps
//! keep space they can do without, checked directly rather than through a
//! scan, so it takes seconds. Every rule says what the junk is, what
//! cleaning it means and whether it comes back, is marked safe, "check
//! first" or "use the tool", and names the program that must not be
//! running. Nothing here deletes anything unless asked to through
//! [`delete`], and only items marked safe belong there; "check first" items
//! go to the quarantine, which the callers own.
//!
//! Three rules for the rules: each has a fixture (paths it must find and
//! paths it must not, checked by a test over every platform's list), each
//! has a blurb and says whether it refills, and an app that's running is
//! skipped.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub use crate::reclaim::Safety;

/// What a rule belongs to on the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    /// Temporary files, crash dumps, update leftovers, logs.
    System,
    /// Browser caches, nothing that signs anyone out.
    Browsers,
    /// Caches of well-known apps.
    Apps,
    /// Shader caches and download leftovers.
    Games,
    /// Package and build caches.
    Developer,
    /// The trash, old installers and downloads, screen recordings.
    Trash,
}

impl Category {
    pub const ALL: [Category; 6] = [
        Category::System,
        Category::Browsers,
        Category::Apps,
        Category::Games,
        Category::Developer,
        Category::Trash,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Category::System => "system",
            Category::Browsers => "browsers",
            Category::Apps => "apps",
            Category::Games => "games",
            Category::Developer => "developer",
            Category::Trash => "trash",
        }
    }

    pub fn from_id(id: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.id() == id)
    }

    pub fn title(self) -> &'static str {
        match self {
            Category::System => "System",
            Category::Browsers => "Browsers",
            Category::Apps => "Apps",
            Category::Games => "Games",
            Category::Developer => "Developer tools",
            Category::Trash => "Trash and downloads",
        }
    }

    /// One line under the heading.
    pub fn blurb(self) -> &'static str {
        match self {
            Category::System => {
                "Temporary files, crash dumps and update leftovers. What the system should \
                 clean itself gets a button to its own tool."
            }
            Category::Browsers => {
                "Caches only. History, cookies and saved passwords stay, so nothing signs \
                 you out."
            }
            Category::Apps => "Caches of well-known apps. An app that's running is skipped.",
            Category::Games => "Shader caches and download leftovers; games build them again.",
            Category::Developer => {
                "Package and build caches; the next build or install downloads what it needs."
            }
            Category::Trash => {
                "The trash, old installers, old downloads and screen recordings. Look before \
                 these go: they're yours."
            }
        }
    }
}

/// Which system's rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
    Android,
    /// An Unraid server: trash folders in its shares.
    Unraid,
}

impl Platform {
    pub const ALL: [Platform; 5] = [
        Platform::Windows,
        Platform::MacOs,
        Platform::Linux,
        Platform::Android,
        Platform::Unraid,
    ];

    /// The one this program runs on.
    pub fn current() -> Platform {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(target_os = "android") {
            Platform::Android
        } else {
            Platform::Linux
        }
    }
}

/// What inside a matched path is junk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Take {
    /// Everything inside the folder; the folder itself stays, since the
    /// program expects it.
    Contents,
    /// The matched file or folder itself.
    Whole,
    /// Files below it with one of the extensions (any file if the list is
    /// empty), last changed more than `days` days ago (any age if 0).
    Files {
        extensions: &'static [&'static str],
        days: u32,
    },
    /// Folders below it with nothing in them.
    EmptyFolders,
}

/// Paths a rule must find and paths it must not, relative to the places
/// in [`Env`] (`~/x`, `$LOCAL/x`, …). Found paths are made 200 days old,
/// missed ones new, so age rules can be checked too.
#[derive(Clone, Copy, Debug)]
pub struct Fixture {
    pub hit: &'static [&'static str],
    pub miss: &'static [&'static str],
}

/// A known place of junk.
#[derive(Clone, Copy, Debug)]
pub struct Rule {
    /// Stable id the Hub and the command line use, e.g. `chrome-cache`.
    pub id: &'static str,
    pub category: Category,
    /// "Chrome's cache".
    pub title: &'static str,
    /// What happens after cleaning: "Pages load from the internet again
    /// the first time; you stay signed in."
    pub blurb: &'static str,
    /// Comes back within days of normal use.
    pub refills: bool,
    pub safety: Safety,
    /// The program that must not be running, as shown ("Chrome"); empty
    /// when none.
    pub app: &'static str,
    /// Its process names, lowercase and without `.exe`, or the name of
    /// its `.app` bundle on macOS.
    pub processes: &'static [&'static str],
    /// For [`Safety::UseTool`]: the tool, as a command or a settings path.
    pub tool: &'static str,
    /// Where to look; `*` stands for any one name, also as part of a name.
    pub patterns: &'static [&'static str],
    pub take: Take,
    /// Entries left alone inside a [`Take::Contents`] folder, and top-level
    /// folders skipped by [`Take::EmptyFolders`].
    pub keep: &'static [&'static str],
    /// Entries changed within this many days are left alone (0: all go).
    /// Temporary files may still be in use.
    pub min_age_days: u32,
    pub fixture: Fixture,
}

impl Rule {
    /// Items of this rule are ticked by default: only what's safe and not
    /// in use.
    pub fn ticked_by_default(&self) -> bool {
        self.safety == Safety::Safe
    }
}

/// The folders the rules are written against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Env {
    /// `~`.
    pub home: PathBuf,
    /// `$LOCAL`: `%LOCALAPPDATA%`.
    pub local: PathBuf,
    /// `$ROAMING`: `%APPDATA%`.
    pub roaming: PathBuf,
    /// `$WINDIR`.
    pub windir: PathBuf,
    /// `$DRIVE`: the system drive's top folder.
    pub drive: PathBuf,
    /// `$PROGRAMDATA`.
    pub program_data: PathBuf,
    /// `$PF86`: `%ProgramFiles(x86)%`, where Steam installs by default.
    pub program_files_x86: PathBuf,
    /// `$VAR`: `/var`.
    pub var: PathBuf,
    /// `$STORAGE`: Android's shared storage.
    pub storage: PathBuf,
    /// `$SHARES`: `/mnt/user` on Unraid.
    pub shares: PathBuf,
    /// `$ROOT`: the top of the file system (`/`).
    pub root: PathBuf,
}

impl Env {
    /// The real places on this machine.
    pub fn system() -> Env {
        let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
        let home = var("HOME")
            .or_else(|| var("USERPROFILE"))
            .unwrap_or_else(|| PathBuf::from("/"));
        let drive = var("SystemDrive").map_or_else(
            || PathBuf::from("C:\\"),
            |d| PathBuf::from(format!("{}\\", d.to_string_lossy())),
        );
        Env {
            local: var("LOCALAPPDATA").unwrap_or_else(|| home.join("AppData/Local")),
            roaming: var("APPDATA").unwrap_or_else(|| home.join("AppData/Roaming")),
            windir: var("windir").unwrap_or_else(|| drive.join("Windows")),
            program_data: var("ProgramData").unwrap_or_else(|| drive.join("ProgramData")),
            program_files_x86: var("ProgramFiles(x86)")
                .unwrap_or_else(|| drive.join("Program Files (x86)")),
            drive,
            var: PathBuf::from("/var"),
            storage: PathBuf::from("/storage/emulated/0"),
            shares: PathBuf::from("/mnt/user"),
            root: PathBuf::from("/"),
            home,
        }
    }

    /// Android: the rules look in the shared storage at `storage`.
    pub fn android(storage: &Path) -> Env {
        Env {
            storage: storage.to_path_buf(),
            ..Env::system()
        }
    }

    /// Every place under one folder, for tests and for trying rules on a
    /// copy of a machine's folders.
    pub fn under(root: &Path) -> Env {
        let home = root.join("home");
        Env {
            local: home.join("AppData/Local"),
            roaming: home.join("AppData/Roaming"),
            windir: root.join("Windows"),
            drive: root.to_path_buf(),
            program_data: root.join("ProgramData"),
            program_files_x86: root.join("Program Files (x86)"),
            var: root.join("var"),
            storage: root.join("storage"),
            shares: root.join("mnt/user"),
            root: root.to_path_buf(),
            home,
        }
    }

    /// A pattern or fixture path as a real path.
    pub fn expand(&self, template: &str) -> PathBuf {
        let (head, rest) = template.split_once('/').unwrap_or((template, ""));
        let base = match head {
            "~" => &self.home,
            "$LOCAL" => &self.local,
            "$ROAMING" => &self.roaming,
            "$WINDIR" => &self.windir,
            "$DRIVE" => &self.drive,
            "$PROGRAMDATA" => &self.program_data,
            "$PF86" => &self.program_files_x86,
            "$VAR" => &self.var,
            "$STORAGE" => &self.storage,
            "$SHARES" => &self.shares,
            "$ROOT" => &self.root,
            _ => panic!("junk rule path doesn't start with a known place: {template}"),
        };
        let mut path = base.clone();
        for part in rest.split('/').filter(|p| !p.is_empty()) {
            path.push(part);
        }
        path
    }
}

/// What of an item goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Covers {
    /// The path itself.
    Itself,
    /// These entries inside the folder; the folder stays.
    Entries(Vec<OsString>),
}

/// One thing a rule found: a file, a folder, or a folder's contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub path: PathBuf,
    pub bytes: u64,
    pub files: u64,
    pub covers: Covers,
    /// A word on the item where the path doesn't say enough: the program a
    /// leftover belonged to. Empty for most rules.
    pub note: String,
}

impl Item {
    /// Whether `path` would go with this item.
    pub fn covers(&self, path: &Path) -> bool {
        match &self.covers {
            Covers::Itself => path.starts_with(&self.path),
            Covers::Entries(entries) => {
                let Ok(rest) = path.strip_prefix(&self.path) else {
                    return false;
                };
                rest.components()
                    .next()
                    .is_some_and(|first| entries.iter().any(|e| e.as_os_str() == first.as_os_str()))
            }
        }
    }

    /// The paths that go, for listing them.
    pub fn paths(&self) -> Vec<PathBuf> {
        match &self.covers {
            Covers::Itself => vec![self.path.clone()],
            Covers::Entries(entries) => entries.iter().map(|e| self.path.join(e)).collect(),
        }
    }
}

/// What one rule found.
#[derive(Clone, Debug)]
pub struct Finding {
    pub rule: &'static Rule,
    /// Biggest first.
    pub items: Vec<Item>,
    pub bytes: u64,
    pub files: u64,
    /// The rule's program is running, so nothing of it is offered until
    /// it's closed.
    pub running: bool,
}

impl Finding {
    /// Safe, and not in use: cleaned unless unticked.
    pub fn ticked(&self) -> bool {
        self.rule.ticked_by_default() && !self.running
    }

    /// Whether the person may tick it: anything not in use that isn't
    /// removed with the system's own tool. "Check first" findings start
    /// unticked but can be ticked; they go to the quarantine.
    pub fn can_tick(&self) -> bool {
        !self.running && self.rule.safety != Safety::UseTool
    }

    /// Why it isn't ticked, for the screen; empty when it is.
    pub fn why_not(&self) -> String {
        if self.running {
            format!("{} is running; close it first.", self.rule.app)
        } else {
            match self.rule.safety {
                Safety::Safe => String::new(),
                Safety::CheckFirst => "Look first; goes to the quarantine.".into(),
                Safety::UseTool => format!("Use its own tool: {}", self.rule.tool),
            }
        }
    }
}

/// Lowercase names of the programs running now: process names, and on
/// macOS the paths of their executables, for matching against
/// [`Rule::processes`].
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", windows)),
    allow(unused_mut)
)]
pub fn running_programs() -> Vec<String> {
    let mut names = Vec::new();
    #[cfg(target_os = "linux")]
    {
        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                if !entry
                    .file_name()
                    .to_string_lossy()
                    .bytes()
                    .all(|b| b.is_ascii_digit())
                {
                    continue;
                }
                if let Ok(comm) = fs::read_to_string(entry.path().join("comm")) {
                    names.push(comm.trim().to_lowercase());
                }
                if let Ok(exe) = fs::read_link(entry.path().join("exe")) {
                    names.push(exe.to_string_lossy().to_lowercase());
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("ps")
            .args(["-axo", "comm="])
            .output()
        {
            names.extend(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(|l| l.trim().to_lowercase())
                    .filter(|l| !l.is_empty()),
            );
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        if let Ok(output) = std::process::Command::new("tasklist")
            .args(["/fo", "csv", "/nh"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let name = line.trim_start_matches('"');
                let Some((name, _)) = name.split_once('"') else {
                    continue;
                };
                let name = name.to_lowercase();
                names.push(name.strip_suffix(".exe").unwrap_or(&name).to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Whether one of `processes` is among the running programs.
pub fn is_running(running: &[String], processes: &[&str]) -> bool {
    processes.iter().any(|p| {
        let bundle = format!("/{p}.app/");
        let tail = format!("/{p}");
        running
            .iter()
            .any(|r| r == p || r.ends_with(&tail) || r.contains(&bundle))
    })
}

/// Everything the platform's rules find, by category then size. `running`
/// comes from [`running_programs`]; `now` is seconds since 1970.
pub fn find(platform: Platform, env: &Env, running: &[String], now: i64) -> Vec<Finding> {
    let mut findings: Vec<Finding> = Vec::new();
    let mut taken: Vec<Item> = Vec::new();
    for rule in rules_for(platform) {
        let mut items = Vec::new();
        for pattern in rule.patterns {
            for path in expand_pattern(&env.expand(pattern)) {
                for item in items_of(rule, &path, now, &taken) {
                    // What an earlier rule already covers isn't listed
                    // twice, nor is a folder around it.
                    let overlaps = taken
                        .iter()
                        .any(|t| t.covers(&item.path) || item.covers(&t.path));
                    if !overlaps && !items.iter().any(|i: &Item| i.path == item.path) {
                        items.push(item);
                    }
                }
            }
        }
        if items.is_empty() {
            continue;
        }
        items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
        taken.extend(items.iter().cloned());
        findings.push(Finding {
            bytes: items.iter().map(|i| i.bytes).sum(),
            files: items.iter().map(|i| i.files).sum(),
            running: !rule.processes.is_empty() && is_running(running, rule.processes),
            rule,
            items,
        });
    }
    sort(&mut findings);
    findings
}

/// By category, biggest first within one.
fn sort(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        a.rule
            .category
            .cmp(&b.rule.category)
            .then_with(|| b.bytes.cmp(&a.bytes))
            .then_with(|| a.rule.id.cmp(b.rule.id))
    });
}

/// [`find`], plus the leftovers of removed programs
/// ([`crate::leftovers`]), whose record of programs is kept at `record`
/// (none: the record isn't kept, and only id-named folders can be
/// leftovers). What a rule already covers isn't a leftover too.
pub fn find_all(
    platform: Platform,
    env: &Env,
    running: &[String],
    now: i64,
    record: Option<&Path>,
) -> Vec<Finding> {
    let mut findings = find(platform, env, running, now);
    if let Some(mut leftovers) = crate::leftovers::look(platform, env, now, record) {
        leftovers.items.retain(|item| {
            !findings
                .iter()
                .flat_map(|f| &f.items)
                .any(|taken| taken.covers(&item.path) || item.covers(&taken.path))
        });
        if let Some(leftovers) = crate::leftovers::finding(leftovers.items) {
            findings.push(leftovers);
            sort(&mut findings);
        }
    }
    findings
}

/// [`find_all`] for this machine, now.
pub fn find_here() -> Vec<Finding> {
    find_all(
        Platform::current(),
        &Env::system(),
        &running_programs(),
        now(),
        crate::leftovers::record_path().as_deref(),
    )
}

/// Seconds since 1970.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Total of every finding.
pub fn total(findings: &[Finding]) -> u64 {
    findings.iter().map(|f| f.bytes).sum()
}

/// Total of what's ticked by default.
pub fn safe_total(findings: &[Finding]) -> u64 {
    findings
        .iter()
        .filter(|f| f.ticked())
        .map(|f| f.bytes)
        .sum()
}

/// The rule with this id on the platform, the leftovers of removed
/// programs included on desktops.
pub fn rule_by_id(platform: Platform, id: &str) -> Option<&'static Rule> {
    if id == crate::leftovers::ID
        && matches!(
            platform,
            Platform::Windows | Platform::MacOs | Platform::Linux
        )
    {
        return Some(&crate::leftovers::RULE);
    }
    rules_for(platform).iter().find(|r| r.id == id)
}

/// The paths a pattern stands for: `*` is any one name, or any part of a
/// name (`thumbcache_*.db`). Only what exists is returned.
pub fn expand_pattern(pattern: &Path) -> Vec<PathBuf> {
    let parts: Vec<String> = pattern
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let first_star = parts
        .iter()
        .position(|p| p.contains('*'))
        .unwrap_or(parts.len());
    let base: PathBuf = pattern.components().take(first_star).collect();
    let mut paths = vec![base];
    for (i, part) in parts.iter().enumerate().skip(first_star) {
        let mut next = Vec::new();
        for base in &paths {
            if part.contains('*') {
                let Ok(entries) = fs::read_dir(base) else {
                    continue;
                };
                let mut names: Vec<OsString> = entries
                    .flatten()
                    .map(|e| e.file_name())
                    .filter(|name| wildcard(part, &name.to_string_lossy()))
                    .collect();
                names.sort();
                next.extend(names.into_iter().map(|name| base.join(name)));
            } else {
                let path = base.join(part);
                if i + 1 == parts.len() || path.is_dir() {
                    next.push(path);
                }
            }
        }
        paths = next;
        if paths.is_empty() {
            break;
        }
    }
    paths
        .into_iter()
        .filter(|p| p.symlink_metadata().is_ok())
        .collect()
}

/// `thumbcache_*.db` against a name; one `*` at most matters.
fn wildcard(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((prefix, suffix)) => {
            name.len() >= prefix.len() + suffix.len()
                && name.starts_with(prefix)
                && name.ends_with(suffix)
        }
    }
}

/// What a rule takes at one matched path. Entries of a folder that an
/// earlier rule covers (`taken`) are left to that rule.
fn items_of(rule: &Rule, path: &Path, now: i64, taken: &[Item]) -> Vec<Item> {
    let cutoff = |days: u32| (days > 0).then(|| now - i64::from(days) * 86_400);
    match rule.take {
        Take::Whole => {
            let Ok(meta) = fs::symlink_metadata(path) else {
                return Vec::new();
            };
            let (bytes, files) = if meta.is_dir() {
                size_of(path)
            } else {
                (meta.len(), 1)
            };
            (bytes > 0)
                .then_some(Item {
                    path: path.to_path_buf(),
                    bytes,
                    files,
                    covers: Covers::Itself,
                    note: String::new(),
                })
                .into_iter()
                .collect()
        }
        Take::Contents => {
            let Ok(entries) = fs::read_dir(path) else {
                return Vec::new();
            };
            let min_age = cutoff(rule.min_age_days);
            let mut names = Vec::new();
            let (mut bytes, mut files) = (0, 0);
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let name = entry.file_name();
                if rule.keep.iter().any(|k| name.as_os_str() == *k) {
                    continue;
                }
                let entry_path = entry.path();
                if taken
                    .iter()
                    .any(|t| t.covers(&entry_path) || t.path.starts_with(&entry_path))
                {
                    continue;
                }
                let Ok(meta) = fs::symlink_metadata(&entry_path) else {
                    continue;
                };
                let (entry_bytes, entry_files, newest) = if meta.is_dir() {
                    size_and_newest(&entry_path)
                } else {
                    (meta.len(), 1, modified(&meta))
                };
                if min_age.is_some_and(|cutoff| newest > cutoff) {
                    continue;
                }
                bytes += entry_bytes;
                files += entry_files;
                names.push(name);
            }
            if names.is_empty() || (bytes == 0 && files == 0) {
                return Vec::new();
            }
            vec![Item {
                path: path.to_path_buf(),
                bytes,
                files,
                covers: Covers::Entries(names),
                note: String::new(),
            }]
        }
        Take::Files { extensions, days } => {
            let cutoff = cutoff(days);
            let mut items = Vec::new();
            walk(path, &mut |file, meta| {
                if !meta.is_file() {
                    return true;
                }
                let ext = file
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if !extensions.is_empty() && !extensions.contains(&ext.as_str()) {
                    return true;
                }
                if cutoff.is_some_and(|cutoff| modified(meta) > cutoff) {
                    return true;
                }
                if meta.len() > 0 {
                    items.push(Item {
                        path: file.to_path_buf(),
                        bytes: meta.len(),
                        files: 1,
                        covers: Covers::Itself,
                        note: String::new(),
                    });
                }
                true
            });
            items
        }
        Take::EmptyFolders => {
            let mut items = Vec::new();
            empty_folders(path, rule.keep, true, &mut items);
            items
        }
    }
}

/// Lists the topmost empty folders below `dir` (a folder holding only
/// empty folders counts); says whether `dir` itself is empty.
fn empty_folders(dir: &Path, keep: &[&str], top: bool, out: &mut Vec<Item>) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    let mut empty = true;
    let mut subfolders = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if top && keep.iter().any(|k| name.as_os_str() == *k) {
            empty = false;
            continue;
        }
        let path = entry.path();
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => subfolders.push(path),
            _ => empty = false,
        }
    }
    subfolders.sort();
    let mut empty_below = Vec::new();
    for sub in subfolders {
        let before = out.len();
        if empty_folders(&sub, keep, false, out) {
            out.truncate(before);
            empty_below.push(sub);
        } else {
            empty = false;
        }
    }
    if empty && !top {
        return true;
    }
    out.extend(empty_below.into_iter().map(|path| Item {
        path,
        bytes: 0,
        files: 0,
        covers: Covers::Itself,
        note: String::new(),
    }));
    false
}

pub(crate) fn modified(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

/// Visits everything below `dir`, without following symlinks; `visit`
/// returns false to skip a folder's contents.
fn walk(dir: &Path, visit: &mut dyn FnMut(&Path, &fs::Metadata) -> bool) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if visit(&path, &meta) && meta.is_dir() {
            walk(&path, visit);
        }
    }
}

/// Bytes and files below a folder.
pub fn size_of(dir: &Path) -> (u64, u64) {
    let (bytes, files, _) = size_and_newest(dir);
    (bytes, files)
}

pub(crate) fn size_and_newest(dir: &Path) -> (u64, u64, i64) {
    let (mut bytes, mut files, mut newest) = (0, 0, 0);
    walk(dir, &mut |_, meta| {
        if meta.is_file() {
            bytes += meta.len();
            files += 1;
            newest = newest.max(modified(meta));
        }
        true
    });
    (bytes, files, newest)
}

/// Something that couldn't go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub path: PathBuf,
    pub why: String,
}

/// What deleting did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cleaned {
    pub freed: u64,
    /// Files and folders that stayed, usually because something has them
    /// open.
    pub skipped: Vec<Skipped>,
}

impl Cleaned {
    pub fn add(&mut self, other: Cleaned) {
        self.freed += other.freed;
        self.skipped.extend(other.skipped);
    }
}

/// Deletes a safe item for good: its entries, or the item itself. Files in
/// use stay and are reported; the rest of a folder still goes. Only for
/// [`Safety::Safe`] rules; the caller sends "check first" items to the
/// quarantine instead.
pub fn delete(item: &Item) -> Cleaned {
    let mut cleaned = Cleaned::default();
    for path in item.paths() {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            let (before, _) = size_of(&path);
            let result = remove_tree(&path, &mut cleaned.skipped);
            let after = if path.is_dir() { size_of(&path).0 } else { 0 };
            cleaned.freed += before.saturating_sub(after);
            if let Err(err) = result {
                cleaned.skipped.push(Skipped { path, why: err });
            }
        } else {
            match fs::remove_file(&path) {
                Ok(()) => cleaned.freed += meta.len(),
                Err(err) => cleaned.skipped.push(Skipped {
                    path,
                    why: err.to_string(),
                }),
            }
        }
    }
    cleaned
}

/// Removes a folder file by file, so what's in use is skipped rather than
/// stopping everything.
fn remove_tree(dir: &Path, skipped: &mut Vec<Skipped>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| e.to_string())?;
    let mut all_gone = true;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let result = if meta.is_dir() {
            remove_tree(&path, skipped)
        } else {
            fs::remove_file(&path).map_err(|e| e.to_string())
        };
        if let Err(err) = result {
            all_gone = false;
            if !meta.is_dir() {
                skipped.push(Skipped { path, why: err });
            }
        }
    }
    if all_gone {
        fs::remove_dir(dir).map_err(|e| e.to_string())
    } else {
        Err("some files are in use".into())
    }
}

/// Ids of the rules on every platform, for checking a rule id someone
/// sends is one at all.
pub fn known_rule(id: &str) -> bool {
    Platform::ALL
        .into_iter()
        .any(|p| rule_by_id(p, id).is_some())
}

/// The platform's rules, specific ones before any that would match the
/// same folders.
pub fn rules_for(platform: Platform) -> &'static [Rule] {
    match platform {
        Platform::Windows => WINDOWS,
        Platform::MacOs => MACOS,
        Platform::Linux => LINUX,
        Platform::Android => ANDROID,
        Platform::Unraid => UNRAID,
    }
}

// ----- The rules -----------------------------------------------------------

const NO_APP: &[&str] = &[];
const CHROME: &[&str] = &["chrome", "google chrome"];
const EDGE: &[&str] = &["msedge", "microsoft edge"];
const BRAVE: &[&str] = &["brave", "brave browser"];
const FIREFOX: &[&str] = &["firefox", "firefox-bin"];
const SPOTIFY: &[&str] = &["spotify"];
const DISCORD: &[&str] = &["discord"];
const SLACK: &[&str] = &["slack"];
const TEAMS: &[&str] = &["ms-teams", "teams", "microsoft teams"];
const VSCODE: &[&str] = &["code", "visual studio code"];
const STEAM: &[&str] = &["steam", "steam_osx", "steamwebhelper"];
const ADOBE: &[&str] = &[
    "adobe premiere pro",
    "aftereffects",
    "adobe after effects",
    "adobe media encoder",
];
const ZOOM: &[&str] = &["zoom", "zoom.us"];

const OLD_DOWNLOADS: Take = Take::Files {
    extensions: &[],
    days: 90,
};
const SCREEN_RECORDINGS: Take = Take::Files {
    extensions: &["mp4", "mov", "webm", "mkv"],
    days: 30,
};

#[allow(clippy::too_many_arguments)]
const fn rule(
    id: &'static str,
    category: Category,
    title: &'static str,
    blurb: &'static str,
    refills: bool,
    safety: Safety,
    patterns: &'static [&'static str],
    take: Take,
    fixture: Fixture,
) -> Rule {
    Rule {
        id,
        category,
        title,
        blurb,
        refills,
        safety,
        app: "",
        processes: NO_APP,
        tool: "",
        patterns,
        take,
        keep: &[],
        min_age_days: 0,
        fixture,
    }
}

const fn with_app(mut rule: Rule, app: &'static str, processes: &'static [&'static str]) -> Rule {
    rule.app = app;
    rule.processes = processes;
    rule
}

const fn with_tool(mut rule: Rule, tool: &'static str) -> Rule {
    rule.safety = Safety::UseTool;
    rule.tool = tool;
    rule
}

const fn keeping(mut rule: Rule, keep: &'static [&'static str]) -> Rule {
    rule.keep = keep;
    rule
}

const fn older_than(mut rule: Rule, days: u32) -> Rule {
    rule.min_age_days = days;
    rule
}

const fn fixture(hit: &'static [&'static str], miss: &'static [&'static str]) -> Fixture {
    Fixture { hit, miss }
}

static WINDOWS: &[Rule] = &[
    // System: what Windows' own Storage settings would clean, with the
    // tool where Windows should do it itself.
    older_than(
        rule(
            "windows-temp",
            Category::System,
            "Windows temporary files",
            "Files programs and installers left in Windows\\Temp. Anything in use stays; \
             nothing from the last day is touched.",
            true,
            Safety::Safe,
            &["$WINDIR/Temp"],
            Take::Contents,
            fixture(
                &["$WINDIR/Temp/setup123.tmp"],
                &["$WINDIR/System32/kernel32.dll"],
            ),
        ),
        1,
    ),
    older_than(
        rule(
            "user-temp",
            Category::System,
            "Your temporary files",
            "Files programs left in your Temp folder. Anything in use stays; nothing from \
             the last day is touched.",
            true,
            Safety::Safe,
            &["$LOCAL/Temp"],
            Take::Contents,
            fixture(&["$LOCAL/Temp/old.tmp"], &["$LOCAL/Keep/data.db"]),
        ),
        1,
    ),
    rule(
        "crash-dumps",
        Category::System,
        "Crash dumps",
        "Memory dumps written when a program or Windows crashed. Only needed to report a \
         crash that already happened.",
        false,
        Safety::Safe,
        &["$LOCAL/CrashDumps", "$WINDIR/Minidump"],
        Take::Contents,
        fixture(
            &[
                "$LOCAL/CrashDumps/app.exe.1234.dmp",
                "$WINDIR/Minidump/012345-1.dmp",
            ],
            &["$WINDIR/System32/config/SYSTEM"],
        ),
    ),
    rule(
        "memory-dump",
        Category::System,
        "Full crash dump",
        "MEMORY.DMP, everything in memory when Windows last crashed. Only needed to report \
         that crash.",
        false,
        Safety::Safe,
        &["$WINDIR/MEMORY.DMP"],
        Take::Whole,
        fixture(&["$WINDIR/MEMORY.DMP"], &["$WINDIR/explorer.exe"]),
    ),
    rule(
        "error-reports",
        Category::System,
        "Windows error reports",
        "Reports about crashes, sent to Microsoft or waiting to be. Nothing you use needs \
         them.",
        false,
        Safety::Safe,
        &[
            "$LOCAL/Microsoft/Windows/WER/ReportArchive",
            "$LOCAL/Microsoft/Windows/WER/ReportQueue",
            "$PROGRAMDATA/Microsoft/Windows/WER/ReportArchive",
            "$PROGRAMDATA/Microsoft/Windows/WER/ReportQueue",
        ],
        Take::Contents,
        fixture(
            &["$LOCAL/Microsoft/Windows/WER/ReportArchive/AppCrash_x/Report.wer"],
            &["$LOCAL/Microsoft/Windows/Explorer/iconcache.db"],
        ),
    ),
    with_tool(
        rule(
            "windows-update",
            Category::System,
            "Windows Update downloads",
            "Update files already installed. Windows cleans these itself and knows which \
             are still needed.",
            true,
            Safety::UseTool,
            &["$WINDIR/SoftwareDistribution/Download"],
            Take::Contents,
            fixture(
                &["$WINDIR/SoftwareDistribution/Download/abc/update.cab"],
                &["$WINDIR/SoftwareDistribution/DataStore/DataStore.edb"],
            ),
        ),
        "Settings → System → Storage → Temporary files → Windows Update Cleanup",
    ),
    with_tool(
        rule(
            "delivery-optimization",
            Category::System,
            "Delivery Optimization cache",
            "Update files kept to share with other PCs. Windows clears it from its Storage \
             settings.",
            true,
            Safety::UseTool,
            &[
                "$WINDIR/ServiceProfiles/NetworkService/AppData/Local/Microsoft/Windows/DeliveryOptimization/Cache",
            ],
            Take::Contents,
            fixture(
                &[
                    "$WINDIR/ServiceProfiles/NetworkService/AppData/Local/Microsoft/Windows/DeliveryOptimization/Cache/x/chunk",
                ],
                &["$WINDIR/ServiceProfiles/NetworkService/NTUSER.DAT"],
            ),
        ),
        "Settings → System → Storage → Temporary files → Delivery Optimization Files",
    ),
    with_tool(
        rule(
            "windows-old",
            Category::System,
            "Previous Windows installation",
            "Windows.old, kept for going back to the version before an upgrade. Windows \
             removes it itself after ten days, or from its Storage settings.",
            false,
            Safety::UseTool,
            &[
                "$DRIVE/Windows.old",
                "$DRIVE/$WINDOWS.~BT",
                "$DRIVE/$Windows.~WS",
            ],
            Take::Whole,
            fixture(
                &["$DRIVE/Windows.old/Users/me/file.txt"],
                &["$DRIVE/Users/me/file.txt"],
            ),
        ),
        "Settings → System → Storage → Temporary files → Previous Windows installation(s)",
    ),
    with_tool(
        rule(
            "explorer-thumbnails",
            Category::System,
            "Thumbnail cache",
            "Explorer's previews of pictures and videos. Explorer keeps them open, so \
             Windows clears them from its Storage settings; they're made again as you \
             browse.",
            true,
            Safety::UseTool,
            &["$LOCAL/Microsoft/Windows/Explorer/thumbcache_*.db"],
            Take::Whole,
            fixture(
                &["$LOCAL/Microsoft/Windows/Explorer/thumbcache_256.db"],
                &["$LOCAL/Microsoft/Windows/Explorer/iconcache_256.db"],
            ),
        ),
        "Settings → System → Storage → Temporary files → Thumbnails",
    ),
    with_tool(
        rule(
            "hibernation",
            Category::System,
            "Hibernation file",
            "hiberfil.sys holds memory while the PC hibernates, and Fast Startup uses it \
             too. Turn hibernation off to drop it.",
            false,
            Safety::UseTool,
            &["$DRIVE/hiberfil.sys"],
            Take::Whole,
            fixture(&["$DRIVE/hiberfil.sys"], &["$DRIVE/pagefile.sys"]),
        ),
        "powercfg /h off (as administrator)",
    ),
    // Browsers: the cache folders of each profile, never the profile.
    with_app(
        rule(
            "chrome-cache",
            Category::Browsers,
            "Chrome's cache",
            "Pages load from the internet again the first time; you stay signed in.",
            true,
            Safety::Safe,
            &[
                "$LOCAL/Google/Chrome/User Data/*/Cache",
                "$LOCAL/Google/Chrome/User Data/*/Code Cache",
                "$LOCAL/Google/Chrome/User Data/*/GPUCache",
                "$LOCAL/Google/Chrome/User Data/*/Service Worker/CacheStorage",
            ],
            Take::Contents,
            fixture(
                &[
                    "$LOCAL/Google/Chrome/User Data/Default/Cache/Cache_Data/f_0001",
                    "$LOCAL/Google/Chrome/User Data/Profile 1/Code Cache/js/index",
                ],
                &[
                    "$LOCAL/Google/Chrome/User Data/Default/Bookmarks",
                    "$LOCAL/Google/Chrome/User Data/Default/Login Data",
                ],
            ),
        ),
        "Chrome",
        CHROME,
    ),
    with_app(
        rule(
            "edge-cache",
            Category::Browsers,
            "Edge's cache",
            "Pages load from the internet again the first time; you stay signed in.",
            true,
            Safety::Safe,
            &[
                "$LOCAL/Microsoft/Edge/User Data/*/Cache",
                "$LOCAL/Microsoft/Edge/User Data/*/Code Cache",
                "$LOCAL/Microsoft/Edge/User Data/*/GPUCache",
                "$LOCAL/Microsoft/Edge/User Data/*/Service Worker/CacheStorage",
            ],
            Take::Contents,
            fixture(
                &["$LOCAL/Microsoft/Edge/User Data/Default/Cache/Cache_Data/f_0001"],
                &["$LOCAL/Microsoft/Edge/User Data/Default/History"],
            ),
        ),
        "Edge",
        EDGE,
    ),
    with_app(
        rule(
            "brave-cache",
            Category::Browsers,
            "Brave's cache",
            "Pages load from the internet again the first time; you stay signed in.",
            true,
            Safety::Safe,
            &[
                "$LOCAL/BraveSoftware/Brave-Browser/User Data/*/Cache",
                "$LOCAL/BraveSoftware/Brave-Browser/User Data/*/Code Cache",
                "$LOCAL/BraveSoftware/Brave-Browser/User Data/*/GPUCache",
            ],
            Take::Contents,
            fixture(
                &["$LOCAL/BraveSoftware/Brave-Browser/User Data/Default/Cache/f_0001"],
                &["$LOCAL/BraveSoftware/Brave-Browser/User Data/Default/Preferences"],
            ),
        ),
        "Brave",
        BRAVE,
    ),
    with_app(
        rule(
            "firefox-cache",
            Category::Browsers,
            "Firefox's cache",
            "Pages load from the internet again the first time; you stay signed in.",
            true,
            Safety::Safe,
            &[
                "$LOCAL/Mozilla/Firefox/Profiles/*/cache2",
                "$LOCAL/Mozilla/Firefox/Profiles/*/startupCache",
            ],
            Take::Contents,
            fixture(
                &["$LOCAL/Mozilla/Firefox/Profiles/abcd.default/cache2/entries/ABC"],
                &["$ROAMING/Mozilla/Firefox/Profiles/abcd.default/places.sqlite"],
            ),
        ),
        "Firefox",
        FIREFOX,
    ),
    rule(
        "inet-cache",
        Category::Browsers,
        "Windows' internet cache",
        "Files Windows and older programs fetched from the web; made again when needed.",
        true,
        Safety::Safe,
        &["$LOCAL/Microsoft/Windows/INetCache"],
        Take::Contents,
        fixture(
            &["$LOCAL/Microsoft/Windows/INetCache/IE/ABCD/page.htm"],
            &["$LOCAL/Microsoft/Windows/INetCookies/x.txt"],
        ),
    ),
    // Apps.
    with_app(
        rule(
            "spotify-cache",
            Category::Apps,
            "Spotify's cache",
            "Songs you streamed, kept to replay without downloading again. Downloads for \
             offline listening stay.",
            true,
            Safety::Safe,
            &["$LOCAL/Spotify/Storage", "$LOCAL/Spotify/Data"],
            Take::Contents,
            fixture(
                &["$LOCAL/Spotify/Storage/ab/abcdef.file"],
                &["$ROAMING/Spotify/prefs"],
            ),
        ),
        "Spotify",
        SPOTIFY,
    ),
    with_app(
        rule(
            "discord-cache",
            Category::Apps,
            "Discord's cache",
            "Images and attachments seen in chats; loaded again when you scroll back to \
             them.",
            true,
            Safety::Safe,
            &[
                "$ROAMING/discord/Cache",
                "$ROAMING/discord/Code Cache",
                "$ROAMING/discord/GPUCache",
            ],
            Take::Contents,
            fixture(
                &["$ROAMING/discord/Cache/Cache_Data/f_0001"],
                &["$ROAMING/discord/settings.json"],
            ),
        ),
        "Discord",
        DISCORD,
    ),
    with_app(
        rule(
            "slack-cache",
            Category::Apps,
            "Slack's cache",
            "Files and images seen in channels; loaded again when needed.",
            true,
            Safety::Safe,
            &[
                "$ROAMING/Slack/Cache",
                "$ROAMING/Slack/Code Cache",
                "$ROAMING/Slack/GPUCache",
                "$ROAMING/Slack/Service Worker/CacheStorage",
            ],
            Take::Contents,
            fixture(
                &["$ROAMING/Slack/Cache/Cache_Data/f_0001"],
                &["$ROAMING/Slack/storage/root-state.json"],
            ),
        ),
        "Slack",
        SLACK,
    ),
    with_app(
        rule(
            "teams-cache",
            Category::Apps,
            "Teams' cache",
            "Images and web parts of Teams; loaded again when needed. You stay signed in.",
            true,
            Safety::Safe,
            &[
                "$LOCAL/Packages/MSTeams_8wekyb3d8bbwe/LocalCache/Microsoft/MSTeams/EBWebView/*/Cache",
                "$LOCAL/Packages/MSTeams_8wekyb3d8bbwe/LocalCache/Microsoft/MSTeams/EBWebView/*/Code Cache",
                "$ROAMING/Microsoft/Teams/Cache",
                "$ROAMING/Microsoft/Teams/Code Cache",
                "$ROAMING/Microsoft/Teams/GPUCache",
                "$ROAMING/Microsoft/Teams/Service Worker/CacheStorage",
            ],
            Take::Contents,
            fixture(
                &[
                    "$LOCAL/Packages/MSTeams_8wekyb3d8bbwe/LocalCache/Microsoft/MSTeams/EBWebView/WV2Profile_tfw/Cache/f_0001",
                    "$ROAMING/Microsoft/Teams/Cache/f_0001",
                ],
                &["$ROAMING/Microsoft/Teams/desktop-config.json"],
            ),
        ),
        "Teams",
        TEAMS,
    ),
    with_app(
        rule(
            "zoom-logs",
            Category::Apps,
            "Zoom's logs",
            "Log files from past meetings; only Zoom's support ever asks for them.",
            true,
            Safety::Safe,
            &["$ROAMING/Zoom/logs"],
            Take::Contents,
            fixture(
                &["$ROAMING/Zoom/logs/zoom.log"],
                &["$ROAMING/Zoom/data/zoomus.db"],
            ),
        ),
        "Zoom",
        ZOOM,
    ),
    with_app(
        rule(
            "vscode-cache",
            Category::Apps,
            "VS Code's cache",
            "Cached code and extension downloads; settings and extensions stay. VS Code \
             starts a little slower once.",
            true,
            Safety::Safe,
            &[
                "$ROAMING/Code/Cache",
                "$ROAMING/Code/CachedData",
                "$ROAMING/Code/CachedExtensionVSIXs",
                "$ROAMING/Code/Code Cache",
                "$ROAMING/Code/GPUCache",
            ],
            Take::Contents,
            fixture(
                &["$ROAMING/Code/CachedData/abc/main.js.code"],
                &["$ROAMING/Code/User/settings.json"],
            ),
        ),
        "VS Code",
        VSCODE,
    ),
    with_app(
        rule(
            "adobe-media-cache",
            Category::Apps,
            "Adobe's media cache",
            "Premiere Pro, After Effects and Media Encoder rebuild it when a project opens; \
             the first playback is slower.",
            true,
            Safety::Safe,
            &[
                "$ROAMING/Adobe/Common/Media Cache Files",
                "$ROAMING/Adobe/Common/Media Cache",
                "$ROAMING/Adobe/Common/Peak Files",
            ],
            Take::Contents,
            fixture(
                &["$ROAMING/Adobe/Common/Media Cache Files/clip.cfa"],
                &["$ROAMING/Adobe/Adobe Premiere Pro/24.0/Profile/x.prproj"],
            ),
        ),
        "Premiere Pro",
        ADOBE,
    ),
    // Games.
    rule(
        "shader-cache",
        Category::Games,
        "Graphics shader cache",
        "Shaders the graphics driver compiled for games and apps; it compiles them again, \
         so the first launch of each game may stutter.",
        true,
        Safety::Safe,
        &[
            "$LOCAL/NVIDIA/DXCache",
            "$LOCAL/NVIDIA/GLCache",
            "$LOCAL/AMD/DxCache",
            "$LOCAL/AMD/GLCache",
            "$LOCAL/D3DSCache",
        ],
        Take::Contents,
        fixture(
            &["$LOCAL/NVIDIA/DXCache/abc.toc"],
            &["$LOCAL/NVIDIA/NvBackend/x.db"],
        ),
    ),
    with_app(
        rule(
            "steam-shader-cache",
            Category::Games,
            "Steam's shader cache",
            "Shaders Steam downloaded or built for each game; it fetches or builds them \
             again, so the first launch may stutter.",
            true,
            Safety::Safe,
            &["$PF86/Steam/steamapps/shadercache"],
            Take::Contents,
            fixture(
                &["$PF86/Steam/steamapps/shadercache/440/fozpipelinesv6/x"],
                &["$PF86/Steam/steamapps/common/Game/game.exe"],
            ),
        ),
        "Steam",
        STEAM,
    ),
    with_app(
        rule(
            "steam-downloads",
            Category::Games,
            "Steam's download leftovers",
            "Parts of games being downloaded or updated. Only safe to remove when no \
             download is paused; Steam downloads the rest again.",
            false,
            Safety::CheckFirst,
            &[
                "$PF86/Steam/steamapps/downloading",
                "$PF86/Steam/steamapps/temp",
            ],
            Take::Contents,
            fixture(
                &["$PF86/Steam/steamapps/downloading/440/part"],
                &["$PF86/Steam/steamapps/common/Game/data.pak"],
            ),
        ),
        "Steam",
        STEAM,
    ),
    rule(
        "epic-webcache",
        Category::Games,
        "Epic Games Launcher's cache",
        "The launcher's store pages; loaded again when it opens.",
        true,
        Safety::Safe,
        &[
            "$LOCAL/EpicGamesLauncher/Saved/webcache",
            "$LOCAL/EpicGamesLauncher/Saved/webcache_*",
        ],
        Take::Contents,
        fixture(
            &["$LOCAL/EpicGamesLauncher/Saved/webcache_4430/Cache/f_0001"],
            &["$LOCAL/EpicGamesLauncher/Saved/Config/Windows/GameUserSettings.ini"],
        ),
    ),
    // Developer tools.
    rule(
        "npm-cache",
        Category::Developer,
        "npm's cache",
        "Downloaded packages; npm fetches what a project needs again on the next install.",
        true,
        Safety::Safe,
        &["$LOCAL/npm-cache", "$ROAMING/npm-cache"],
        Take::Contents,
        fixture(
            &["$LOCAL/npm-cache/_cacache/index-v5/ab"],
            &["$ROAMING/npm/node_modules/x/index.js"],
        ),
    ),
    rule(
        "pip-cache",
        Category::Developer,
        "pip's cache",
        "Downloaded Python packages; pip fetches them again when needed.",
        true,
        Safety::Safe,
        &["$LOCAL/pip/cache"],
        Take::Contents,
        fixture(
            &["$LOCAL/pip/cache/wheels/ab/x.whl"],
            &["$LOCAL/Programs/Python/python.exe"],
        ),
    ),
    rule(
        "cargo-cache",
        Category::Developer,
        "Cargo's download cache",
        "Downloaded crates and their unpacked sources; cargo fetches them again on the next \
         build that needs them.",
        true,
        Safety::Safe,
        &["~/.cargo/registry/cache", "~/.cargo/registry/src"],
        Take::Contents,
        fixture(
            &["~/.cargo/registry/cache/index/serde-1.0.0.crate"],
            &["~/.cargo/bin/cargo.exe"],
        ),
    ),
    rule(
        "gradle-cache",
        Category::Developer,
        "Gradle's caches",
        "Downloaded libraries and build outputs; the next build downloads what it needs \
         and takes longer once.",
        true,
        Safety::Safe,
        &["~/.gradle/caches"],
        Take::Contents,
        fixture(
            &["~/.gradle/caches/modules-2/files-2.1/x.jar"],
            &["~/.gradle/gradle.properties"],
        ),
    ),
    rule(
        "nuget-cache",
        Category::Developer,
        "NuGet's caches",
        "Downloaded .NET packages; restored again on the next build.",
        true,
        Safety::Safe,
        &[
            "$LOCAL/NuGet/v3-cache",
            "$LOCAL/NuGet/plugins-cache",
            "~/.nuget/packages",
        ],
        Take::Contents,
        fixture(
            &["~/.nuget/packages/newtonsoft.json/13.0.1/lib.dll"],
            &["~/.nuget/NuGet/NuGet.Config"],
        ),
    ),
    rule(
        "yarn-cache",
        Category::Developer,
        "Yarn's cache",
        "Downloaded packages; fetched again on the next install.",
        true,
        Safety::Safe,
        &["$LOCAL/Yarn/Cache"],
        Take::Contents,
        fixture(
            &["$LOCAL/Yarn/Cache/v6/npm-x/index.js"],
            &["$LOCAL/Yarn/Data/global/x"],
        ),
    ),
    rule(
        "go-build-cache",
        Category::Developer,
        "Go's build cache",
        "Compiled packages; the next build compiles them again.",
        true,
        Safety::Safe,
        &["$LOCAL/go-build"],
        Take::Contents,
        fixture(&["$LOCAL/go-build/00/abc-d"], &["~/go/bin/tool.exe"]),
    ),
    // Trash and downloads: the person's own files, so they're looked at.
    keeping(
        rule(
            "recycle-bin",
            Category::Trash,
            "Recycle Bin",
            "Files you deleted. Emptying it is for good.",
            false,
            Safety::Safe,
            &["$DRIVE/$Recycle.Bin/*"],
            Take::Contents,
            fixture(
                &[
                    "$DRIVE/$Recycle.Bin/S-1-5-21-1/$R1ABC.txt",
                    "$DRIVE/$Recycle.Bin/S-1-5-21-1/$I1ABC.txt",
                ],
                &["$DRIVE/$Recycle.Bin/S-1-5-21-1/desktop.ini"],
            ),
        ),
        &["desktop.ini"],
    ),
    rule(
        "old-installers",
        Category::Trash,
        "Installers in Downloads",
        "Setup programs you downloaded and probably ran already. Download them again if \
         you ever need them.",
        false,
        Safety::CheckFirst,
        &["~/Downloads"],
        Take::Files {
            extensions: &["exe", "msi", "msix", "msixbundle", "appx", "appinstaller"],
            days: 0,
        },
        fixture(
            &["~/Downloads/setup-1.0.exe", "~/Downloads/tool.msi"],
            &["~/Downloads/photo.jpg"],
        ),
    ),
    rule(
        "old-downloads",
        Category::Trash,
        "Old downloads",
        "Files in Downloads not changed for 3 months. Look through them: some may be \
         the only copy.",
        false,
        Safety::CheckFirst,
        &["~/Downloads"],
        OLD_DOWNLOADS,
        fixture(
            &["~/Downloads/archive.zip"],
            &["~/Downloads/new-report.pdf"],
        ),
    ),
    rule(
        "screen-recordings",
        Category::Trash,
        "Screen recordings",
        "Recordings from the Game Bar older than a month. They add up fast; keep the \
         ones you want.",
        false,
        Safety::CheckFirst,
        &["~/Videos/Captures"],
        SCREEN_RECORDINGS,
        fixture(
            &["~/Videos/Captures/Game 2026-01-02.mp4"],
            &["~/Videos/holiday.mp4"],
        ),
    ),
];

static MACOS: &[Rule] = &[
    // System.
    with_tool(
        rule(
            "macos-updates",
            Category::System,
            "Downloaded macOS updates",
            "Updates waiting to be installed. Install them, or let Software Update drop \
             them.",
            false,
            Safety::UseTool,
            &["$ROOT/Library/Updates"],
            Take::Contents,
            fixture(
                &["$ROOT/Library/Updates/001-23456/update.pkg"],
                &["$VAR/log/system.log"],
            ),
        ),
        "System Settings → General → Software Update",
    ),
    rule(
        "ios-backups",
        Category::System,
        "iPhone and iPad backups",
        "Backups of devices synced with this Mac. Keep the latest of each device; older \
         ones are only useful to go back in time.",
        false,
        Safety::CheckFirst,
        &["~/Library/Application Support/MobileSync/Backup/*"],
        Take::Whole,
        fixture(
            &["~/Library/Application Support/MobileSync/Backup/abcdef0123/Manifest.db"],
            &["~/Library/Application Support/AddressBook/x.db"],
        ),
    ),
    // Browsers.
    with_app(
        rule(
            "chrome-cache",
            Category::Browsers,
            "Chrome's cache",
            "Pages load from the internet again the first time; you stay signed in.",
            true,
            Safety::Safe,
            &[
                "~/Library/Caches/Google/Chrome/*/Cache",
                "~/Library/Caches/Google/Chrome/*/Code Cache",
                "~/Library/Application Support/Google/Chrome/*/Service Worker/CacheStorage",
            ],
            Take::Contents,
            fixture(
                &["~/Library/Caches/Google/Chrome/Default/Cache/Cache_Data/f_0001"],
                &["~/Library/Application Support/Google/Chrome/Default/Bookmarks"],
            ),
        ),
        "Chrome",
        CHROME,
    ),
    with_app(
        rule(
            "edge-cache",
            Category::Browsers,
            "Edge's cache",
            "Pages load from the internet again the first time; you stay signed in.",
            true,
            Safety::Safe,
            &[
                "~/Library/Caches/Microsoft Edge/*/Cache",
                "~/Library/Caches/Microsoft Edge/*/Code Cache",
            ],
            Take::Contents,
            fixture(
                &["~/Library/Caches/Microsoft Edge/Default/Cache/f_0001"],
                &["~/Library/Application Support/Microsoft Edge/Default/History"],
            ),
        ),
        "Edge",
        EDGE,
    ),
    with_app(
        rule(
            "brave-cache",
            Category::Browsers,
            "Brave's cache",
            "Pages load from the internet again the first time; you stay signed in.",
            true,
            Safety::Safe,
            &[
                "~/Library/Caches/BraveSoftware/Brave-Browser/*/Cache",
                "~/Library/Caches/BraveSoftware/Brave-Browser/*/Code Cache",
            ],
            Take::Contents,
            fixture(
                &["~/Library/Caches/BraveSoftware/Brave-Browser/Default/Cache/f_0001"],
                &["~/Library/Application Support/BraveSoftware/Brave-Browser/Default/Preferences"],
            ),
        ),
        "Brave",
        BRAVE,
    ),
    with_app(
        rule(
            "firefox-cache",
            Category::Browsers,
            "Firefox's cache",
            "Pages load from the internet again the first time; you stay signed in.",
            true,
            Safety::Safe,
            &["~/Library/Caches/Firefox/Profiles/*/cache2"],
            Take::Contents,
            fixture(
                &["~/Library/Caches/Firefox/Profiles/abcd.default/cache2/entries/ABC"],
                &["~/Library/Application Support/Firefox/Profiles/abcd.default/places.sqlite"],
            ),
        ),
        "Firefox",
        FIREFOX,
    ),
    with_app(
        rule(
            "safari-cache",
            Category::Browsers,
            "Safari's cache",
            "Pages load from the internet again the first time; you stay signed in. \
             Needs Full Disk Access for FreeSize.",
            true,
            Safety::Safe,
            &[
                "~/Library/Caches/com.apple.Safari",
                "~/Library/Containers/com.apple.Safari/Data/Library/Caches",
            ],
            Take::Contents,
            fixture(
                &["~/Library/Caches/com.apple.Safari/Cache.db"],
                &["~/Library/Safari/Bookmarks.plist"],
            ),
        ),
        "Safari",
        &["safari"],
    ),
    // Apps.
    with_app(
        rule(
            "spotify-cache",
            Category::Apps,
            "Spotify's cache",
            "Songs you streamed, kept to replay without downloading again. Downloads for \
             offline listening stay.",
            true,
            Safety::Safe,
            &["~/Library/Application Support/Spotify/PersistentCache/Storage"],
            Take::Contents,
            fixture(
                &["~/Library/Application Support/Spotify/PersistentCache/Storage/ab/abcdef.file"],
                &["~/Library/Application Support/Spotify/prefs"],
            ),
        ),
        "Spotify",
        SPOTIFY,
    ),
    with_app(
        rule(
            "discord-cache",
            Category::Apps,
            "Discord's cache",
            "Images and attachments seen in chats; loaded again when you scroll back to \
             them.",
            true,
            Safety::Safe,
            &[
                "~/Library/Application Support/discord/Cache",
                "~/Library/Application Support/discord/Code Cache",
                "~/Library/Application Support/discord/GPUCache",
            ],
            Take::Contents,
            fixture(
                &["~/Library/Application Support/discord/Cache/Cache_Data/f_0001"],
                &["~/Library/Application Support/discord/settings.json"],
            ),
        ),
        "Discord",
        DISCORD,
    ),
    with_app(
        rule(
            "slack-cache",
            Category::Apps,
            "Slack's cache",
            "Files and images seen in channels; loaded again when needed.",
            true,
            Safety::Safe,
            &[
                "~/Library/Application Support/Slack/Cache",
                "~/Library/Application Support/Slack/Service Worker/CacheStorage",
                "~/Library/Containers/com.tinyspeck.slackmacgap/Data/Library/Application Support/Slack/Cache",
                "~/Library/Containers/com.tinyspeck.slackmacgap/Data/Library/Application Support/Slack/Service Worker/CacheStorage",
            ],
            Take::Contents,
            fixture(
                &[
                    "~/Library/Containers/com.tinyspeck.slackmacgap/Data/Library/Application Support/Slack/Cache/f_0001",
                ],
                &[
                    "~/Library/Containers/com.tinyspeck.slackmacgap/Data/Library/Application Support/Slack/storage/root-state.json",
                ],
            ),
        ),
        "Slack",
        SLACK,
    ),
    with_app(
        rule(
            "teams-cache",
            Category::Apps,
            "Teams' cache",
            "Images and web parts of Teams; loaded again when needed. You stay signed in.",
            true,
            Safety::Safe,
            &["~/Library/Containers/com.microsoft.teams2/Data/Library/Caches"],
            Take::Contents,
            fixture(
                &["~/Library/Containers/com.microsoft.teams2/Data/Library/Caches/x/f_0001"],
                &["~/Library/Containers/com.microsoft.teams2/Data/Library/Preferences/x.plist"],
            ),
        ),
        "Teams",
        TEAMS,
    ),
    with_app(
        rule(
            "zoom-logs",
            Category::Apps,
            "Zoom's logs",
            "Log files from past meetings; only Zoom's support ever asks for them.",
            true,
            Safety::Safe,
            &["~/Library/Logs/zoom.us"],
            Take::Contents,
            fixture(
                &["~/Library/Logs/zoom.us/zoom.log"],
                &["~/Library/Application Support/zoom.us/data/zoomus.db"],
            ),
        ),
        "Zoom",
        ZOOM,
    ),
    with_app(
        rule(
            "vscode-cache",
            Category::Apps,
            "VS Code's cache",
            "Cached code and extension downloads; settings and extensions stay. VS Code \
             starts a little slower once.",
            true,
            Safety::Safe,
            &[
                "~/Library/Application Support/Code/Cache",
                "~/Library/Application Support/Code/CachedData",
                "~/Library/Application Support/Code/CachedExtensionVSIXs",
                "~/Library/Application Support/Code/Code Cache",
                "~/Library/Application Support/Code/GPUCache",
            ],
            Take::Contents,
            fixture(
                &["~/Library/Application Support/Code/CachedData/abc/main.js.code"],
                &["~/Library/Application Support/Code/User/settings.json"],
            ),
        ),
        "VS Code",
        VSCODE,
    ),
    with_app(
        rule(
            "adobe-media-cache",
            Category::Apps,
            "Adobe's media cache",
            "Premiere Pro, After Effects and Media Encoder rebuild it when a project opens; \
             the first playback is slower.",
            true,
            Safety::Safe,
            &[
                "~/Library/Application Support/Adobe/Common/Media Cache Files",
                "~/Library/Application Support/Adobe/Common/Media Cache",
                "~/Library/Application Support/Adobe/Common/Peak Files",
            ],
            Take::Contents,
            fixture(
                &["~/Library/Application Support/Adobe/Common/Media Cache Files/clip.cfa"],
                &["~/Library/Application Support/Adobe/Adobe Premiere Pro/x.prproj"],
            ),
        ),
        "Premiere Pro",
        ADOBE,
    ),
    // Games.
    with_app(
        rule(
            "steam-shader-cache",
            Category::Games,
            "Steam's shader cache",
            "Shaders Steam downloaded or built for each game; it fetches or builds them \
             again, so the first launch may stutter.",
            true,
            Safety::Safe,
            &["~/Library/Application Support/Steam/steamapps/shadercache"],
            Take::Contents,
            fixture(
                &["~/Library/Application Support/Steam/steamapps/shadercache/440/x"],
                &["~/Library/Application Support/Steam/steamapps/common/Game/game"],
            ),
        ),
        "Steam",
        STEAM,
    ),
    with_app(
        rule(
            "steam-downloads",
            Category::Games,
            "Steam's download leftovers",
            "Parts of games being downloaded or updated. Only safe to remove when no \
             download is paused; Steam downloads the rest again.",
            false,
            Safety::CheckFirst,
            &["~/Library/Application Support/Steam/steamapps/downloading"],
            Take::Contents,
            fixture(
                &["~/Library/Application Support/Steam/steamapps/downloading/440/part"],
                &["~/Library/Application Support/Steam/steamapps/common/Game/data.pak"],
            ),
        ),
        "Steam",
        STEAM,
    ),
    // Developer tools.
    rule(
        "xcode-derived-data",
        Category::Developer,
        "Xcode's DerivedData",
        "Build outputs and indexes of your projects; Xcode builds them again, and the \
         first build of each project takes longer.",
        true,
        Safety::Safe,
        &["~/Library/Developer/Xcode/DerivedData"],
        Take::Contents,
        fixture(
            &["~/Library/Developer/Xcode/DerivedData/App-abc/Build/x.o"],
            &["~/Library/Developer/Xcode/UserData/x.plist"],
        ),
    ),
    rule(
        "xcode-device-support",
        Category::Developer,
        "Xcode's device support files",
        "Symbols for iOS versions of devices you connected; Xcode downloads them again \
         for a connected device.",
        false,
        Safety::Safe,
        &[
            "~/Library/Developer/Xcode/iOS DeviceSupport",
            "~/Library/Developer/Xcode/watchOS DeviceSupport",
        ],
        Take::Contents,
        fixture(
            &["~/Library/Developer/Xcode/iOS DeviceSupport/17.0 (21A329)/Symbols/x"],
            &["~/Library/Developer/Xcode/Archives/2026/App.xcarchive/Info.plist"],
        ),
    ),
    rule(
        "xcode-archives",
        Category::Developer,
        "Xcode archives",
        "Builds you archived for the App Store or for symbolicating crashes. Keep the \
         ones of versions still in use.",
        false,
        Safety::CheckFirst,
        &["~/Library/Developer/Xcode/Archives/*/*"],
        Take::Whole,
        fixture(
            &["~/Library/Developer/Xcode/Archives/2026-01-02/App.xcarchive/Info.plist"],
            &["~/Library/Developer/Xcode/UserData/x.plist"],
        ),
    ),
    with_tool(
        rule(
            "ios-simulators",
            Category::Developer,
            "iOS simulators",
            "Simulator devices, including ones of runtimes no longer installed. Xcode's \
             own command removes the unavailable ones.",
            false,
            Safety::UseTool,
            &["~/Library/Developer/CoreSimulator/Devices"],
            Take::Contents,
            fixture(
                &["~/Library/Developer/CoreSimulator/Devices/ABC/data/x"],
                &["~/Library/Developer/Xcode/UserData/x.plist"],
            ),
        ),
        "xcrun simctl delete unavailable",
    ),
    rule(
        "homebrew-cache",
        Category::Developer,
        "Homebrew's downloads",
        "Downloaded bottles and sources; brew fetches them again when needed. `brew \
         cleanup` does the same.",
        true,
        Safety::Safe,
        &["~/Library/Caches/Homebrew"],
        Take::Contents,
        fixture(
            &["~/Library/Caches/Homebrew/downloads/x.tar.gz"],
            &["~/Library/Caches/Homebrew.keep/x"],
        ),
    ),
    rule(
        "npm-cache",
        Category::Developer,
        "npm's cache",
        "Downloaded packages; npm fetches what a project needs again on the next install.",
        true,
        Safety::Safe,
        &["~/.npm/_cacache"],
        Take::Contents,
        fixture(&["~/.npm/_cacache/index-v5/ab"], &["~/.npm/_logs/x.log"]),
    ),
    rule(
        "pip-cache",
        Category::Developer,
        "pip's cache",
        "Downloaded Python packages; pip fetches them again when needed.",
        true,
        Safety::Safe,
        &["~/Library/Caches/pip"],
        Take::Contents,
        fixture(
            &["~/Library/Caches/pip/wheels/ab/x.whl"],
            &["~/.pyenv/versions/3.12/bin/python"],
        ),
    ),
    rule(
        "cargo-cache",
        Category::Developer,
        "Cargo's download cache",
        "Downloaded crates and their unpacked sources; cargo fetches them again on the next \
         build that needs them.",
        true,
        Safety::Safe,
        &["~/.cargo/registry/cache", "~/.cargo/registry/src"],
        Take::Contents,
        fixture(
            &["~/.cargo/registry/cache/index/serde-1.0.0.crate"],
            &["~/.cargo/bin/cargo"],
        ),
    ),
    rule(
        "gradle-cache",
        Category::Developer,
        "Gradle's caches",
        "Downloaded libraries and build outputs; the next build downloads what it needs \
         and takes longer once.",
        true,
        Safety::Safe,
        &["~/.gradle/caches"],
        Take::Contents,
        fixture(
            &["~/.gradle/caches/modules-2/files-2.1/x.jar"],
            &["~/.gradle/gradle.properties"],
        ),
    ),
    rule(
        "cocoapods-cache",
        Category::Developer,
        "CocoaPods' cache",
        "Downloaded pods; fetched again on the next install.",
        true,
        Safety::Safe,
        &["~/Library/Caches/CocoaPods"],
        Take::Contents,
        fixture(
            &["~/Library/Caches/CocoaPods/Pods/Release/x/y"],
            &["~/.cocoapods/repos/trunk/x"],
        ),
    ),
    rule(
        "yarn-cache",
        Category::Developer,
        "Yarn's cache",
        "Downloaded packages; fetched again on the next install.",
        true,
        Safety::Safe,
        &["~/Library/Caches/Yarn"],
        Take::Contents,
        fixture(&["~/Library/Caches/Yarn/v6/npm-x/index.js"], &["~/.yarnrc"]),
    ),
    rule(
        "go-build-cache",
        Category::Developer,
        "Go's build cache",
        "Compiled packages; the next build compiles them again.",
        true,
        Safety::Safe,
        &["~/Library/Caches/go-build"],
        Take::Contents,
        fixture(&["~/Library/Caches/go-build/00/abc-d"], &["~/go/bin/tool"]),
    ),
    // Trash and downloads.
    rule(
        "trash",
        Category::Trash,
        "Trash",
        "Files you deleted. Emptying it is for good.",
        false,
        Safety::Safe,
        &["~/.Trash"],
        Take::Contents,
        fixture(&["~/.Trash/old.pdf"], &["~/Documents/keep.pdf"]),
    ),
    rule(
        "old-installers",
        Category::Trash,
        "Installers in Downloads",
        "Disk images and packages you downloaded and probably installed already. \
         Download them again if you ever need them.",
        false,
        Safety::CheckFirst,
        &["~/Downloads"],
        Take::Files {
            extensions: &["dmg", "pkg", "mpkg"],
            days: 0,
        },
        fixture(&["~/Downloads/App-1.0.dmg"], &["~/Downloads/photo.jpg"]),
    ),
    rule(
        "old-downloads",
        Category::Trash,
        "Old downloads",
        "Files in Downloads not changed for 3 months. Look through them: some may be \
         the only copy.",
        false,
        Safety::CheckFirst,
        &["~/Downloads"],
        OLD_DOWNLOADS,
        fixture(
            &["~/Downloads/archive.zip"],
            &["~/Downloads/new-report.pdf"],
        ),
    ),
    rule(
        "screen-recordings",
        Category::Trash,
        "Screen recordings",
        "Recordings saved on the Desktop by the screenshot tool, older than a month. \
         They add up fast; keep the ones you want.",
        false,
        Safety::CheckFirst,
        &["~/Desktop"],
        Take::Files {
            extensions: &["mov"],
            days: 30,
        },
        fixture(
            &["~/Desktop/Screen Recording 2026-01-02.mov"],
            &["~/Desktop/notes.txt"],
        ),
    ),
    // Generic, after the rules for particular folders inside it.
    rule(
        "user-caches",
        Category::System,
        "App caches",
        "Each app's cache folder in Library/Caches. Apps make them again; one that's \
         open may need a restart afterwards.",
        true,
        Safety::Safe,
        &["~/Library/Caches/*"],
        Take::Contents,
        fixture(
            &["~/Library/Caches/com.example.app/Cache.db"],
            &["~/Library/Application Support/com.example.app/data.db"],
        ),
    ),
    older_than(
        rule(
            "app-logs",
            Category::System,
            "App logs",
            "Log files older than a week. Only needed to report a problem that already \
             happened.",
            true,
            Safety::Safe,
            &["~/Library/Logs"],
            Take::Contents,
            fixture(
                &["~/Library/Logs/com.example.app/app.log"],
                &["~/Library/Preferences/x.plist"],
            ),
        ),
        7,
    ),
];

static LINUX: &[Rule] = &[
    // System.
    rule(
        "thumbnails",
        Category::System,
        "Thumbnail cache",
        "Previews of pictures and videos; file managers make them again as you browse.",
        true,
        Safety::Safe,
        &["~/.cache/thumbnails"],
        Take::Contents,
        fixture(
            &["~/.cache/thumbnails/large/abc.png"],
            &["~/.local/share/applications/x.desktop"],
        ),
    ),
    rule(
        "crash-reports",
        Category::System,
        "Crash reports",
        "Core dumps and crash reports. Only needed to report a crash that already \
         happened; some need root to remove.",
        false,
        Safety::Safe,
        &["$VAR/crash", "$VAR/lib/systemd/coredump"],
        Take::Contents,
        fixture(
            &["$VAR/crash/_usr_bin_app.1000.crash"],
            &["$VAR/lib/dpkg/status"],
        ),
    ),
    with_tool(
        rule(
            "journal",
            Category::System,
            "System journal",
            "systemd's logs. Its own tool keeps the newest and drops the rest.",
            true,
            Safety::UseTool,
            &["$VAR/log/journal"],
            Take::Contents,
            fixture(
                &["$VAR/log/journal/abc/system.journal"],
                &["$VAR/log/syslog"],
            ),
        ),
        "sudo journalctl --vacuum-size=200M",
    ),
    with_tool(
        rule(
            "apt-cache",
            Category::System,
            "Downloaded packages (apt)",
            "Package files already installed. apt removes them itself.",
            true,
            Safety::UseTool,
            &["$VAR/cache/apt/archives"],
            Take::Contents,
            fixture(
                &["$VAR/cache/apt/archives/x_1.0_amd64.deb"],
                &["$VAR/lib/apt/lists/x"],
            ),
        ),
        "sudo apt clean",
    ),
    with_tool(
        rule(
            "dnf-cache",
            Category::System,
            "Downloaded packages (dnf)",
            "Package files already installed. dnf removes them itself.",
            true,
            Safety::UseTool,
            &["$VAR/cache/dnf"],
            Take::Contents,
            fixture(
                &["$VAR/cache/dnf/fedora/packages/x.rpm"],
                &["$VAR/lib/rpm/rpmdb.sqlite"],
            ),
        ),
        "sudo dnf clean all",
    ),
    with_tool(
        rule(
            "pacman-cache",
            Category::System,
            "Downloaded packages (pacman)",
            "Package files already installed. paccache keeps the newest versions.",
            true,
            Safety::UseTool,
            &["$VAR/cache/pacman/pkg"],
            Take::Contents,
            fixture(
                &["$VAR/cache/pacman/pkg/x-1.0.pkg.tar.zst"],
                &["$VAR/lib/pacman/local/x/desc"],
            ),
        ),
        "sudo paccache -r",
    ),
    // Browsers.
    with_app(
        rule(
            "chrome-cache",
            Category::Browsers,
            "Chrome's cache",
            "Pages load from the internet again the first time; you stay signed in.",
            true,
            Safety::Safe,
            &[
                "~/.cache/google-chrome/*/Cache",
                "~/.cache/google-chrome/*/Code Cache",
                "~/.config/google-chrome/*/Service Worker/CacheStorage",
            ],
            Take::Contents,
            fixture(
                &["~/.cache/google-chrome/Default/Cache/Cache_Data/f_0001"],
                &["~/.config/google-chrome/Default/Bookmarks"],
            ),
        ),
        "Chrome",
        CHROME,
    ),
    with_app(
        rule(
            "chromium-cache",
            Category::Browsers,
            "Chromium's cache",
            "Pages load from the internet again the first time; you stay signed in.",
            true,
            Safety::Safe,
            &[
                "~/.cache/chromium/*/Cache",
                "~/.cache/chromium/*/Code Cache",
                "~/snap/chromium/common/chromium/*/Cache",
            ],
            Take::Contents,
            fixture(
                &["~/.cache/chromium/Default/Cache/f_0001"],
                &["~/.config/chromium/Default/History"],
            ),
        ),
        "Chromium",
        &["chromium", "chromium-browser"],
    ),
    with_app(
        rule(
            "brave-cache",
            Category::Browsers,
            "Brave's cache",
            "Pages load from the internet again the first time; you stay signed in.",
            true,
            Safety::Safe,
            &[
                "~/.cache/BraveSoftware/Brave-Browser/*/Cache",
                "~/.cache/BraveSoftware/Brave-Browser/*/Code Cache",
            ],
            Take::Contents,
            fixture(
                &["~/.cache/BraveSoftware/Brave-Browser/Default/Cache/f_0001"],
                &["~/.config/BraveSoftware/Brave-Browser/Default/Preferences"],
            ),
        ),
        "Brave",
        BRAVE,
    ),
    with_app(
        rule(
            "firefox-cache",
            Category::Browsers,
            "Firefox's cache",
            "Pages load from the internet again the first time; you stay signed in.",
            true,
            Safety::Safe,
            &[
                "~/.cache/mozilla/firefox/*/cache2",
                "~/snap/firefox/common/.cache/mozilla/firefox/*/cache2",
                "~/.var/app/org.mozilla.firefox/cache/mozilla/firefox/*/cache2",
            ],
            Take::Contents,
            fixture(
                &[
                    "~/.cache/mozilla/firefox/abcd.default/cache2/entries/ABC",
                    "~/snap/firefox/common/.cache/mozilla/firefox/abcd.default/cache2/entries/ABC",
                ],
                &["~/.mozilla/firefox/abcd.default/places.sqlite"],
            ),
        ),
        "Firefox",
        FIREFOX,
    ),
    // Apps.
    with_app(
        rule(
            "spotify-cache",
            Category::Apps,
            "Spotify's cache",
            "Songs you streamed, kept to replay without downloading again. Downloads for \
             offline listening stay.",
            true,
            Safety::Safe,
            &[
                "~/.cache/spotify/Storage",
                "~/snap/spotify/common/.cache/spotify/Storage",
                "~/.var/app/com.spotify.Client/cache/spotify/Storage",
            ],
            Take::Contents,
            fixture(
                &["~/.cache/spotify/Storage/ab/abcdef.file"],
                &["~/.config/spotify/prefs"],
            ),
        ),
        "Spotify",
        SPOTIFY,
    ),
    with_app(
        rule(
            "discord-cache",
            Category::Apps,
            "Discord's cache",
            "Images and attachments seen in chats; loaded again when you scroll back to \
             them.",
            true,
            Safety::Safe,
            &[
                "~/.config/discord/Cache",
                "~/.config/discord/Code Cache",
                "~/.config/discord/GPUCache",
                "~/.var/app/com.discordapp.Discord/config/discord/Cache",
                "~/.var/app/com.discordapp.Discord/config/discord/Code Cache",
            ],
            Take::Contents,
            fixture(
                &["~/.config/discord/Cache/Cache_Data/f_0001"],
                &["~/.config/discord/settings.json"],
            ),
        ),
        "Discord",
        DISCORD,
    ),
    with_app(
        rule(
            "slack-cache",
            Category::Apps,
            "Slack's cache",
            "Files and images seen in channels; loaded again when needed.",
            true,
            Safety::Safe,
            &[
                "~/.config/Slack/Cache",
                "~/.config/Slack/Code Cache",
                "~/.config/Slack/GPUCache",
                "~/.config/Slack/Service Worker/CacheStorage",
                "~/snap/slack/common/.config/Slack/Cache",
            ],
            Take::Contents,
            fixture(
                &["~/.config/Slack/Cache/Cache_Data/f_0001"],
                &["~/.config/Slack/storage/root-state.json"],
            ),
        ),
        "Slack",
        SLACK,
    ),
    with_app(
        rule(
            "vscode-cache",
            Category::Apps,
            "VS Code's cache",
            "Cached code and extension downloads; settings and extensions stay. VS Code \
             starts a little slower once.",
            true,
            Safety::Safe,
            &[
                "~/.config/Code/Cache",
                "~/.config/Code/CachedData",
                "~/.config/Code/CachedExtensionVSIXs",
                "~/.config/Code/Code Cache",
                "~/.config/Code/GPUCache",
            ],
            Take::Contents,
            fixture(
                &["~/.config/Code/CachedData/abc/main.js.code"],
                &["~/.config/Code/User/settings.json"],
            ),
        ),
        "VS Code",
        VSCODE,
    ),
    // Games.
    rule(
        "mesa-shader-cache",
        Category::Games,
        "Graphics shader cache",
        "Shaders the graphics driver compiled for games and apps; it compiles them again, \
         so the first launch of each game may stutter.",
        true,
        Safety::Safe,
        &[
            "~/.cache/mesa_shader_cache",
            "~/.cache/mesa_shader_cache_db",
            "~/.cache/nv",
            "~/.cache/radv_builtin_shaders",
        ],
        Take::Contents,
        fixture(
            &["~/.cache/mesa_shader_cache/ab/abcdef"],
            &["~/.cache/fontconfig/x.cache"],
        ),
    ),
    with_app(
        rule(
            "steam-shader-cache",
            Category::Games,
            "Steam's shader cache",
            "Shaders Steam downloaded or built for each game; it fetches or builds them \
             again, so the first launch may stutter.",
            true,
            Safety::Safe,
            &[
                "~/.local/share/Steam/steamapps/shadercache",
                "~/.steam/steam/steamapps/shadercache",
                "~/.var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/shadercache",
            ],
            Take::Contents,
            fixture(
                &["~/.local/share/Steam/steamapps/shadercache/440/fozpipelinesv6/x"],
                &["~/.local/share/Steam/steamapps/common/Game/game"],
            ),
        ),
        "Steam",
        STEAM,
    ),
    with_app(
        rule(
            "steam-downloads",
            Category::Games,
            "Steam's download leftovers",
            "Parts of games being downloaded or updated. Only safe to remove when no \
             download is paused; Steam downloads the rest again.",
            false,
            Safety::CheckFirst,
            &[
                "~/.local/share/Steam/steamapps/downloading",
                "~/.steam/steam/steamapps/downloading",
            ],
            Take::Contents,
            fixture(
                &["~/.local/share/Steam/steamapps/downloading/440/part"],
                &["~/.local/share/Steam/steamapps/common/Game/data.pak"],
            ),
        ),
        "Steam",
        STEAM,
    ),
    // Developer tools.
    rule(
        "npm-cache",
        Category::Developer,
        "npm's cache",
        "Downloaded packages; npm fetches what a project needs again on the next install.",
        true,
        Safety::Safe,
        &["~/.npm/_cacache"],
        Take::Contents,
        fixture(&["~/.npm/_cacache/index-v5/ab"], &["~/.npm/_logs/x.log"]),
    ),
    rule(
        "pip-cache",
        Category::Developer,
        "pip's cache",
        "Downloaded Python packages; pip fetches them again when needed.",
        true,
        Safety::Safe,
        &["~/.cache/pip"],
        Take::Contents,
        fixture(
            &["~/.cache/pip/wheels/ab/x.whl"],
            &["~/.local/lib/python3.12/site-packages/x.py"],
        ),
    ),
    rule(
        "cargo-cache",
        Category::Developer,
        "Cargo's download cache",
        "Downloaded crates and their unpacked sources; cargo fetches them again on the next \
         build that needs them.",
        true,
        Safety::Safe,
        &["~/.cargo/registry/cache", "~/.cargo/registry/src"],
        Take::Contents,
        fixture(
            &["~/.cargo/registry/cache/index/serde-1.0.0.crate"],
            &["~/.cargo/bin/cargo"],
        ),
    ),
    rule(
        "gradle-cache",
        Category::Developer,
        "Gradle's caches",
        "Downloaded libraries and build outputs; the next build downloads what it needs \
         and takes longer once.",
        true,
        Safety::Safe,
        &["~/.gradle/caches"],
        Take::Contents,
        fixture(
            &["~/.gradle/caches/modules-2/files-2.1/x.jar"],
            &["~/.gradle/gradle.properties"],
        ),
    ),
    rule(
        "yarn-cache",
        Category::Developer,
        "Yarn's cache",
        "Downloaded packages; fetched again on the next install.",
        true,
        Safety::Safe,
        &["~/.cache/yarn"],
        Take::Contents,
        fixture(&["~/.cache/yarn/v6/npm-x/index.js"], &["~/.yarnrc"]),
    ),
    rule(
        "go-build-cache",
        Category::Developer,
        "Go's build cache",
        "Compiled packages; the next build compiles them again.",
        true,
        Safety::Safe,
        &["~/.cache/go-build"],
        Take::Contents,
        fixture(&["~/.cache/go-build/00/abc-d"], &["~/go/bin/tool"]),
    ),
    // Trash and downloads.
    rule(
        "trash",
        Category::Trash,
        "Trash",
        "Files you deleted. Emptying it is for good.",
        false,
        Safety::Safe,
        &["~/.local/share/Trash/*"],
        Take::Contents,
        fixture(
            &[
                "~/.local/share/Trash/files/old.pdf",
                "~/.local/share/Trash/info/old.pdf.trashinfo",
            ],
            &["~/.local/share/applications/x.desktop"],
        ),
    ),
    rule(
        "old-installers",
        Category::Trash,
        "Installers in Downloads",
        "Packages and images you downloaded and probably installed already. Download \
         them again if you ever need them.",
        false,
        Safety::CheckFirst,
        &["~/Downloads"],
        Take::Files {
            extensions: &[
                "deb",
                "rpm",
                "appimage",
                "run",
                "snap",
                "flatpak",
                "flatpakref",
            ],
            days: 0,
        },
        fixture(
            &["~/Downloads/app_1.0_amd64.deb", "~/Downloads/Tool.AppImage"],
            &["~/Downloads/photo.jpg"],
        ),
    ),
    rule(
        "old-downloads",
        Category::Trash,
        "Old downloads",
        "Files in Downloads not changed for 3 months. Look through them: some may be \
         the only copy.",
        false,
        Safety::CheckFirst,
        &["~/Downloads"],
        OLD_DOWNLOADS,
        fixture(
            &["~/Downloads/archive.zip"],
            &["~/Downloads/new-report.pdf"],
        ),
    ),
    rule(
        "screen-recordings",
        Category::Trash,
        "Screen recordings",
        "Screencasts older than a month. They add up fast; keep the ones you want.",
        false,
        Safety::CheckFirst,
        &["~/Videos/Screencasts"],
        SCREEN_RECORDINGS,
        fixture(
            &["~/Videos/Screencasts/Screencast 2026-01-02.webm"],
            &["~/Videos/holiday.mp4"],
        ),
    ),
    // Generic, after the rules for particular folders inside it.
    rule(
        "flatpak-caches",
        Category::System,
        "Flatpak app caches",
        "Each Flatpak app's cache; apps make them again. Close the app first.",
        true,
        Safety::Safe,
        &["~/.var/app/*/cache"],
        Take::Contents,
        fixture(
            &["~/.var/app/org.example.App/cache/x"],
            &["~/.var/app/org.example.App/data/x"],
        ),
    ),
];

static ANDROID: &[Rule] = &[
    rule(
        "old-downloads",
        Category::Trash,
        "Old downloads",
        "Files in Download not changed for 3 months. Look through them: some may be the \
         only copy.",
        false,
        Safety::CheckFirst,
        &["$STORAGE/Download"],
        OLD_DOWNLOADS,
        fixture(
            &["$STORAGE/Download/archive.zip"],
            &[
                "$STORAGE/Download/new.pdf",
                "$STORAGE/DCIM/Camera/IMG_1.jpg",
            ],
        ),
    ),
    rule(
        "screen-recordings",
        Category::Trash,
        "Screen recordings",
        "Recordings older than a month. They add up fast; keep the ones you want.",
        false,
        Safety::CheckFirst,
        &[
            "$STORAGE/Movies/Screen recordings",
            "$STORAGE/Movies/ScreenRecords",
            "$STORAGE/Movies/ScreenRecorder",
            "$STORAGE/DCIM/Screen recordings",
            "$STORAGE/DCIM/ScreenRecorder",
            "$STORAGE/Pictures/Screen recordings",
        ],
        SCREEN_RECORDINGS,
        fixture(
            &["$STORAGE/Movies/Screen recordings/Screen_Recording_20260102.mp4"],
            &["$STORAGE/Movies/holiday.mp4"],
        ),
    ),
    keeping(
        rule(
            "empty-folders",
            Category::Trash,
            "Empty folders",
            "Folders with nothing in them, left by removed apps and old downloads. They \
             take no space, only clutter.",
            false,
            Safety::Safe,
            &["$STORAGE"],
            Take::EmptyFolders,
            fixture(
                &["$STORAGE/OldApp/", "$STORAGE/Pictures/Gone/sub/"],
                &[
                    "$STORAGE/Android/data/com.x/files/",
                    "$STORAGE/DCIM/Camera/IMG_1.jpg",
                ],
            ),
        ),
        &["Android"],
    ),
];

static UNRAID: &[Rule] = &[
    rule(
        "recycle-bin",
        Category::Trash,
        "Recycle Bin plugin",
        "Files deleted over the network, kept by the Recycle Bin plugin. Emptying them \
         is for good; the plugin's own page can empty per share too.",
        false,
        Safety::Safe,
        &["$SHARES/*/.Recycle.Bin"],
        Take::Contents,
        fixture(
            &["$SHARES/media/.Recycle.Bin/old.mkv"],
            &["$SHARES/media/movies/film.mkv"],
        ),
    ),
    rule(
        "client-trash",
        Category::Trash,
        "Trash folders of computers",
        "Trash folders Linux and macOS computers leave in shares (.Trash-1000, \
         .Trashes). Emptying them is for good.",
        false,
        Safety::Safe,
        &["$SHARES/*/.Trash-*", "$SHARES/*/.Trashes/*"],
        Take::Contents,
        fixture(
            &[
                "$SHARES/media/.Trash-1000/files/old.mkv",
                "$SHARES/media/.Trashes/501/old.mov",
            ],
            &["$SHARES/media/movies/film.mkv"],
        ),
    ),
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs::File;
    use std::time::{Duration, SystemTime};

    use super::*;

    /// Makes the fixture's files: found ones 200 days old (a folder when
    /// the path ends with `/`), missed ones new.
    fn build(env: &Env, paths: &[&str], old: bool) {
        for template in paths {
            let path = env.expand(template);
            if template.ends_with('/') {
                fs::create_dir_all(&path).unwrap();
                continue;
            }
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"junk junk junk").unwrap();
            if old {
                let then = SystemTime::now() - Duration::from_secs(200 * 86_400);
                File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(then)
                    .unwrap();
            }
        }
    }

    fn covered(findings: &[Finding], id: &str, path: &Path) -> bool {
        findings
            .iter()
            .filter(|f| f.rule.id == id)
            .any(|f| f.items.iter().any(|i| i.covers(path)))
    }

    #[test]
    fn every_rule_finds_its_fixture_and_nothing_else() {
        for platform in Platform::ALL {
            let dir = tempfile::tempdir().unwrap();
            let env = Env::under(dir.path());
            let rules = rules_for(platform);
            let mut ids = HashSet::new();
            for rule in rules {
                assert!(
                    ids.insert(rule.id),
                    "{platform:?}: rule id {} twice",
                    rule.id
                );
                assert!(
                    !rule.title.is_empty() && !rule.blurb.is_empty(),
                    "{}",
                    rule.id
                );
                assert!(!rule.fixture.hit.is_empty(), "{}: no fixture", rule.id);
                assert!(
                    !rule.fixture.miss.is_empty(),
                    "{}: no miss fixture",
                    rule.id
                );
                assert_eq!(
                    rule.safety == Safety::UseTool,
                    !rule.tool.is_empty(),
                    "{}: a tool rule names its tool, no other does",
                    rule.id
                );
                assert_eq!(
                    rule.app.is_empty(),
                    rule.processes.is_empty(),
                    "{}",
                    rule.id
                );
                build(&env, rule.fixture.hit, true);
                build(&env, rule.fixture.miss, false);
            }
            let findings = find(platform, &env, &[], now());
            for rule in rules {
                for hit in rule.fixture.hit {
                    let path = env.expand(hit);
                    assert!(
                        covered(&findings, rule.id, &path),
                        "{platform:?}: {} should find {hit}",
                        rule.id
                    );
                }
                for miss in rule.fixture.miss {
                    let path = env.expand(miss);
                    assert!(
                        !covered(&findings, rule.id, &path),
                        "{platform:?}: {} must not find {miss}",
                        rule.id
                    );
                }
            }
            // Nothing is listed twice: no item's paths fall under another's.
            let mut seen: Vec<&Item> = Vec::new();
            for finding in &findings {
                assert_eq!(
                    finding.bytes,
                    finding.items.iter().map(|i| i.bytes).sum::<u64>()
                );
                for item in &finding.items {
                    for earlier in &seen {
                        for path in item.paths() {
                            assert!(
                                !earlier.covers(&path),
                                "{platform:?}: {} overlaps {}",
                                path.display(),
                                earlier.path.display()
                            );
                        }
                        for path in earlier.paths() {
                            assert!(
                                !item.covers(&path),
                                "{platform:?}: {} overlaps {}",
                                path.display(),
                                item.path.display()
                            );
                        }
                    }
                    seen.push(item);
                }
            }
            // Categories keep their order, biggest first inside.
            for pair in findings.windows(2) {
                assert!(pair[0].rule.category <= pair[1].rule.category);
            }
        }
    }

    #[test]
    fn running_apps_are_skipped_and_ticks_follow_safety() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::under(dir.path());
        let chrome = rule_by_id(Platform::Linux, "chrome-cache").unwrap();
        let downloads = rule_by_id(Platform::Linux, "old-downloads").unwrap();
        build(&env, chrome.fixture.hit, true);
        build(&env, downloads.fixture.hit, true);
        let running = vec!["chrome".to_string(), "bash".to_string()];
        let findings = find(Platform::Linux, &env, &running, now());
        let chrome = findings
            .iter()
            .find(|f| f.rule.id == "chrome-cache")
            .unwrap();
        assert!(chrome.running && !chrome.ticked());
        assert_eq!(chrome.why_not(), "Chrome is running; close it first.");
        let downloads = findings
            .iter()
            .find(|f| f.rule.id == "old-downloads")
            .unwrap();
        assert!(!downloads.running && !downloads.ticked());
        let quiet = find(Platform::Linux, &env, &[], now());
        assert!(
            quiet
                .iter()
                .find(|f| f.rule.id == "chrome-cache")
                .unwrap()
                .ticked()
        );
        // macOS names come as paths; Windows and Linux as bare names.
        let mac = vec!["/applications/google chrome.app/contents/macos/google chrome".to_string()];
        assert!(is_running(&mac, CHROME));
        assert!(!is_running(&mac, FIREFOX));
        assert!(is_running(
            &["/usr/lib/firefox/firefox".to_string()],
            FIREFOX
        ));
    }

    #[test]
    fn temp_files_from_today_stay() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::under(dir.path());
        build(
            &env,
            &["$LOCAL/Temp/old.tmp", "$LOCAL/Temp/olddir/a.tmp"],
            true,
        );
        build(
            &env,
            &["$LOCAL/Temp/today.tmp", "$LOCAL/Temp/busy/b.tmp"],
            false,
        );
        let findings = find(Platform::Windows, &env, &[], now());
        let temp = findings.iter().find(|f| f.rule.id == "user-temp").unwrap();
        assert_eq!(temp.items.len(), 1);
        assert_eq!(
            temp.items[0].covers,
            Covers::Entries(vec!["old.tmp".into(), "olddir".into()])
        );
        assert_eq!(temp.bytes, 28);
        assert!(!temp.items[0].covers(&env.expand("$LOCAL/Temp/today.tmp")));
        assert!(!temp.items[0].covers(&env.expand("$LOCAL/Temp/busy/b.tmp")));
    }

    #[test]
    fn deleting_takes_entries_and_keeps_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::under(dir.path());
        build(
            &env,
            &[
                "$DRIVE/$Recycle.Bin/S-1/$R1.txt",
                "$DRIVE/$Recycle.Bin/S-1/$I1.txt",
                "$DRIVE/$Recycle.Bin/S-1/sub/deep.txt",
                "$DRIVE/$Recycle.Bin/S-1/desktop.ini",
            ],
            true,
        );
        let findings = find(Platform::Windows, &env, &[], now());
        let bin = findings
            .iter()
            .find(|f| f.rule.id == "recycle-bin")
            .unwrap();
        assert_eq!((bin.bytes, bin.files), (42, 3));
        let mut cleaned = Cleaned::default();
        for item in &bin.items {
            cleaned.add(delete(item));
        }
        assert_eq!(cleaned.freed, 42);
        assert!(cleaned.skipped.is_empty());
        let folder = env.expand("$DRIVE/$Recycle.Bin/S-1");
        assert!(folder.is_dir());
        assert!(folder.join("desktop.ini").is_file());
        assert!(!folder.join("sub").exists());
        // Whole items go with their folder.
        build(&env, &["$WINDIR/MEMORY.DMP"], true);
        let findings = find(Platform::Windows, &env, &[], now());
        let dump = findings
            .iter()
            .find(|f| f.rule.id == "memory-dump")
            .unwrap();
        assert_eq!(delete(&dump.items[0]).freed, 14);
        assert!(!env.expand("$WINDIR/MEMORY.DMP").exists());
    }

    #[test]
    fn empty_folders_are_the_topmost_ones() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::under(dir.path());
        build(
            &env,
            &[
                "$STORAGE/Gone/a/b/",
                "$STORAGE/Gone/c/",
                "$STORAGE/Android/data/x/",
            ],
            true,
        );
        build(
            &env,
            &["$STORAGE/Pictures/keep/IMG.jpg", "$STORAGE/Pictures/empty/"],
            false,
        );
        let findings = find(Platform::Android, &env, &[], now());
        let empty = findings
            .iter()
            .find(|f| f.rule.id == "empty-folders")
            .unwrap();
        let paths: Vec<PathBuf> = empty.items.iter().map(|i| i.path.clone()).collect();
        assert_eq!(
            paths,
            [
                env.expand("$STORAGE/Gone"),
                env.expand("$STORAGE/Pictures/empty")
            ]
        );
        let mut cleaned = Cleaned::default();
        for item in &empty.items {
            cleaned.add(delete(item));
        }
        assert!(cleaned.skipped.is_empty());
        assert!(!env.expand("$STORAGE/Gone").exists());
        assert!(env.expand("$STORAGE/Pictures/keep/IMG.jpg").exists());
    }

    #[test]
    fn patterns_expand_names_and_parts_of_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for path in [
            "a/Cache/x",
            "b/Cache/y",
            "c/other/z",
            "thumbcache_32.db",
            "iconcache_32.db",
        ] {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"x").unwrap();
        }
        let found = expand_pattern(&root.join("*/Cache"));
        assert_eq!(found, [root.join("a/Cache"), root.join("b/Cache")]);
        assert_eq!(
            expand_pattern(&root.join("thumbcache_*.db")),
            [root.join("thumbcache_32.db")]
        );
        assert!(expand_pattern(&root.join("missing/*")).is_empty());
        assert!(wildcard("thumbcache_*.db", "thumbcache_1024.db"));
        assert!(!wildcard("thumbcache_*.db", "thumbcache_.dbx"));
        assert!(!wildcard("*.db", "x"));
    }

    #[test]
    fn ids_and_categories_round_trip() {
        for category in Category::ALL {
            assert_eq!(Category::from_id(category.id()), Some(category));
            assert!(!category.title().is_empty() && !category.blurb().is_empty());
        }
        assert!(known_rule("chrome-cache"));
        assert!(!known_rule("registry-cleaner"));
        assert!(rule_by_id(Platform::Windows, "chrome-cache").is_some());
        assert!(rule_by_id(Platform::Unraid, "chrome-cache").is_none());
        let env = Env::under(Path::new("/root"));
        assert_eq!(
            env.expand("~/Downloads"),
            PathBuf::from("/root/home/Downloads")
        );
        assert_eq!(
            env.expand("$LOCAL"),
            PathBuf::from("/root/home/AppData/Local")
        );
    }
}
