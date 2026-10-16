//! What this device keeps of its Nextcloud folders (0.12): the desktop
//! client's accounts and sync folders from its `nextcloud.cfg`, the
//! Android app's folder under `Android/media`, and for each folder how much
//! is stored here against how much is only a placeholder. Nothing is read
//! from the client's sync journal (an SQLite file the client keeps open):
//! paths are the key, which are the same on every client of a server, and
//! the Hub matches them to the server's file ids itself.
//!
//! The two things a device does on request (0.13): free the local copies
//! of synced files on Windows, by marking the placeholders unpinned as
//! Explorer's "Free up space" does (the client frees them on its next
//! run, and brings them back when opened or marked pinned again); and
//! move a scanned folder into the sync folder so the client uploads it:
//! a rename on the same drive, otherwise a copy checked byte for byte,
//! with the original moved to the quarantine.

use std::path::{Path, PathBuf};

use freesize_core::{NodeId, NodeKind, Tree};
use freesize_core::{ScanOptions, ScanProgress, scan};
use freesize_proto::sync::{
    BIG_FILE, DIR_DEPTH, SyncAccount, SyncDir, SyncFile, SyncFolder, SyncReport,
};

/// Most subfolders listed per sync folder.
const MAX_DIRS: usize = 5_000;
/// Most big files listed per sync folder: the biggest.
const MAX_BIG: usize = 2_000;
/// The desktop client's own files in a sync folder, which aren't synced.
const CLIENT_FILES: &[&str] = &[
    ".owncloudsync.log",
    ".nextcloudsync.log",
    ".sync-exclude.lst",
];

/// A Nextcloud account as a client on this device has it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Account {
    pub server: String,
    pub user: String,
    /// "desktop" or "android".
    pub client: String,
    pub folders: Vec<Folder>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Folder {
    pub local: PathBuf,
    pub remote: String,
    /// "off", "suffix", "wincfapi", "macos" or "".
    pub virtual_files: String,
}

/// The desktop client's configuration files on this machine: this user's,
/// and every user's when run with the rights to read them (the agent as a
/// service).
pub fn config_files() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut add = |path: PathBuf| {
        if path.is_file() {
            let canonical = path.canonicalize().unwrap_or(path);
            if !found.contains(&canonical) {
                found.push(canonical);
            }
        }
    };
    if cfg!(windows) {
        if let Ok(appdata) = std::env::var("APPDATA") {
            add(Path::new(&appdata).join("Nextcloud").join("nextcloud.cfg"));
        }
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        for home in homes(&PathBuf::from(format!("{drive}\\Users"))) {
            add(home
                .join("AppData")
                .join("Roaming")
                .join("Nextcloud")
                .join("nextcloud.cfg"));
        }
    } else if cfg!(target_os = "macos") {
        if let Some(home) = std::env::home_dir() {
            add(home.join("Library/Preferences/Nextcloud/nextcloud.cfg"));
        }
        for home in homes(Path::new("/Users")) {
            add(home.join("Library/Preferences/Nextcloud/nextcloud.cfg"));
        }
    } else {
        if let Ok(config) = std::env::var("XDG_CONFIG_HOME")
            && !config.is_empty()
        {
            add(Path::new(&config).join("Nextcloud/nextcloud.cfg"));
        }
        if let Some(home) = std::env::home_dir() {
            add(home.join(".config/Nextcloud/nextcloud.cfg"));
        }
        for home in homes(Path::new("/home")) {
            add(home.join(".config/Nextcloud/nextcloud.cfg"));
        }
        add(PathBuf::from("/root/.config/Nextcloud/nextcloud.cfg"));
    }
    found
}

fn homes(under: &Path) -> Vec<PathBuf> {
    let mut homes: Vec<PathBuf> = std::fs::read_dir(under)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    homes.sort();
    homes
}

/// The desktop client's accounts on this machine.
pub fn desktop_accounts() -> Vec<Account> {
    let mut accounts = Vec::new();
    for file in config_files() {
        if let Ok(text) = std::fs::read_to_string(&file) {
            for mut account in parse_config(&text) {
                if cfg!(target_os = "macos") && account.folders.is_empty() {
                    account.folders = macos_cloud_storage(&file, &account);
                }
                if !accounts.contains(&account) {
                    accounts.push(account);
                }
            }
        }
    }
    accounts
}

/// The client's `nextcloud.cfg`: an INI file written by Qt, with the
/// accounts numbered under `[Accounts]` and each one's sync folders
/// numbered under `N\Folders`.
pub fn parse_config(text: &str) -> Vec<Account> {
    use std::collections::BTreeMap;
    let mut section = String::new();
    // account number -> key path -> value
    let mut accounts: BTreeMap<u32, BTreeMap<String, String>> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_string();
            continue;
        }
        if section != "Accounts" {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let Some((number, rest)) = key.split_once('\\') else {
            continue;
        };
        let Ok(number) = number.parse::<u32>() else {
            continue;
        };
        accounts
            .entry(number)
            .or_default()
            .insert(rest.to_string(), unquote(value.trim()));
    }
    accounts
        .into_values()
        .filter_map(|keys| {
            let server = keys.get("url")?.trim_end_matches('/').to_string();
            if server.is_empty() {
                return None;
            }
            let user = ["dav_user", "user", "webflow_user", "http_user"]
                .iter()
                .find_map(|k| keys.get(*k))
                .cloned()
                .unwrap_or_default();
            // group, folder alias (a number, or a name in old configs)
            // -> (local, remote, mode)
            let mut folders: BTreeMap<(bool, String), Folder> = BTreeMap::new();
            for (key, value) in &keys {
                let mut parts = key.split('\\');
                let group = parts.next().unwrap_or_default();
                let old = match group {
                    "Folders" => false,
                    "FoldersWithPlaceholders" => true,
                    _ => continue,
                };
                let Some(alias) = parts.next().filter(|a| !a.is_empty()) else {
                    continue;
                };
                let Some(field) = parts.next() else {
                    continue;
                };
                let folder = folders.entry((old, alias.to_string())).or_default();
                match field {
                    "localPath" => folder.local = local_path(value),
                    "targetPath" => folder.remote = value.to_string(),
                    "virtualFilesMode" => folder.virtual_files = value.to_string(),
                    _ => {}
                }
            }
            let folders: Vec<Folder> = folders
                .into_values()
                .filter(|f| !f.local.as_os_str().is_empty())
                .map(|mut f| {
                    if f.remote.is_empty() {
                        f.remote = "/".into();
                    }
                    f
                })
                .collect();
            Some(Account {
                server,
                user,
                client: "desktop".into(),
                folders,
            })
        })
        .collect()
}

/// Qt quotes values with special characters and doubles backslashes.
fn unquote(value: &str) -> String {
    let inner = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    inner.replace("\\\\", "\\")
}

/// A local path as the client writes it: with `/` between names even on
/// Windows, and a trailing `/`.
fn local_path(value: &str) -> PathBuf {
    let trimmed = value.trim_end_matches(['/', '\\']);
    if cfg!(windows) {
        PathBuf::from(trimmed.replace('/', "\\"))
    } else {
        PathBuf::from(trimmed)
    }
}

/// The macOS client with virtual files keeps the account's files under
/// `~/Library/CloudStorage/Nextcloud-<name>` instead of a sync folder.
fn macos_cloud_storage(config: &Path, account: &Account) -> Vec<Folder> {
    // ~/Library/Preferences/Nextcloud/nextcloud.cfg -> ~
    let Some(home) = config.ancestors().nth(4) else {
        return Vec::new();
    };
    let mut folders = Vec::new();
    if let Ok(entries) = std::fs::read_dir(home.join("Library/CloudStorage")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(rest) = name.strip_prefix("Nextcloud-") else {
                continue;
            };
            let host = account
                .server
                .trim_start_matches("https://")
                .trim_start_matches("http://");
            // "Nextcloud-bob@cloud.example.com" or "Nextcloud-Bob".
            if rest.contains('@') && !rest.ends_with(host) {
                continue;
            }
            folders.push(Folder {
                local: entry.path(),
                remote: "/".into(),
                virtual_files: "macos".into(),
            });
        }
    }
    folders.sort_by(|a, b| a.local.cmp(&b.local));
    folders
}

/// The Android app's accounts: one folder each under
/// `Android/media/com.nextcloud.client/nextcloud/<user>@<server>` in the
/// shared storage.
pub fn android_accounts(storage: &Path) -> Vec<Account> {
    let base = storage
        .join("Android")
        .join("media")
        .join("com.nextcloud.client")
        .join("nextcloud");
    let mut accounts = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&base) {
        let mut names: Vec<(String, PathBuf)> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect();
        names.sort();
        for (name, path) in names {
            let Some((user, host)) = name.rsplit_once('@') else {
                continue;
            };
            let host = host.replace("%2F", "/").replace("%2f", "/");
            accounts.push(Account {
                server: format!("https://{}", host.trim_end_matches('/')),
                user: user.to_string(),
                client: "android".into(),
                folders: vec![Folder {
                    local: path,
                    remote: "/".into(),
                    virtual_files: "android".into(),
                }],
            });
        }
    }
    accounts
}

/// Reads every folder of the accounts: a scan of each, summed per
/// subfolder, with the biggest files one by one.
pub fn report(accounts: &[Account], now: i64) -> SyncReport {
    SyncReport {
        taken: now,
        accounts: accounts
            .iter()
            .map(|account| SyncAccount {
                server: account.server.clone(),
                user: account.user.clone(),
                client: account.client.clone(),
                folders: account.folders.iter().map(measure).collect(),
            })
            .collect(),
    }
}

/// One sync folder's numbers.
pub fn measure(folder: &Folder) -> SyncFolder {
    let mut out = SyncFolder {
        local: folder.local.to_string_lossy().into_owned(),
        remote: folder.remote.clone(),
        virtual_files: folder.virtual_files.clone(),
        ..SyncFolder::default()
    };
    let Ok(scanned) = scan(&folder.local, &ScanOptions::default(), &ScanProgress::new()) else {
        return out;
    };
    let tree = &scanned.tree;
    let suffix = folder.virtual_files == "suffix";
    let totals = walk(tree, tree.root(), "", 0, suffix, &mut out);
    out.size = totals.size;
    out.on_disk = totals.on_disk;
    out.files = totals.files;
    out.on_disk_files = totals.on_disk_files;
    out.dirs.sort_by(|a, b| a.path.cmp(&b.path));
    out.dirs.truncate(MAX_DIRS);
    out.big
        .sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
    out.big.truncate(MAX_BIG);
    out
}

#[derive(Default)]
struct Totals {
    size: u64,
    on_disk: u64,
    files: u64,
    on_disk_files: u64,
}

fn walk(
    tree: &Tree,
    id: NodeId,
    rel: &str,
    depth: usize,
    suffix: bool,
    out: &mut SyncFolder,
) -> Totals {
    let mut totals = Totals::default();
    for child in tree.children(id) {
        let node = tree.node(child);
        let name = tree.name(child);
        let path = if rel.is_empty() {
            name.to_string()
        } else {
            format!("{rel}/{name}")
        };
        match node.kind {
            NodeKind::Dir => {
                let below = walk(tree, child, &path, depth + 1, suffix, out);
                if depth < DIR_DEPTH {
                    out.dirs.push(SyncDir {
                        path,
                        size: below.size,
                        on_disk: below.on_disk,
                        files: below.files,
                        on_disk_files: below.on_disk_files,
                    });
                }
                totals.size += below.size;
                totals.on_disk += below.on_disk;
                totals.files += below.files;
                totals.on_disk_files += below.on_disk_files;
            }
            NodeKind::File => {
                if is_client_file(&name) {
                    continue;
                }
                // With the suffix kind of virtual files, "x.nextcloud" is a
                // placeholder for "x" whose size isn't known here.
                let (path, placeholder) = match path.strip_suffix(".nextcloud") {
                    Some(real) if suffix || node.size == 0 => (real.to_string(), true),
                    _ => (path, false),
                };
                let on_disk = !placeholder && (node.size == 0 || node.allocated > 0);
                totals.size += node.size;
                totals.files += 1;
                if on_disk {
                    totals.on_disk += node.size;
                    totals.on_disk_files += 1;
                }
                if node.size >= BIG_FILE {
                    out.big.push(SyncFile {
                        path,
                        size: node.size,
                        modified: node.modified,
                        on_disk,
                    });
                }
            }
            NodeKind::Symlink | NodeKind::Other => {}
        }
    }
    totals
}

/// The client's journal and logs, which sit in the sync folder but aren't
/// part of it.
fn is_client_file(name: &str) -> bool {
    name.starts_with(".sync_") || name.starts_with("._sync_") || CLIENT_FILES.contains(&name)
}

/// Whether `path` is one of the desktop client's sync folders here, so a
/// request may name it.
pub fn is_sync_folder(path: &Path) -> bool {
    let wanted = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    desktop_accounts()
        .iter()
        .flat_map(|a| &a.folders)
        .any(|f| f.local.canonicalize().unwrap_or_else(|_| f.local.clone()) == wanted)
}

/// A relative path with `/` between names below `base`, refusing anything
/// that could lead out of it.
pub fn below(base: &Path, relative: &str) -> Result<PathBuf, String> {
    let mut out = base.to_path_buf();
    for part in relative.split('/') {
        if part.is_empty() {
            continue;
        }
        if part == "." || part == ".." || part.contains(['\\', '\0', ':']) {
            return Err(format!("\"{relative}\" isn't a path below the folder"));
        }
        out.push(part);
    }
    Ok(out)
}

/// What marking files did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PinOutcome {
    /// Files whose mark changed.
    pub files: u64,
    /// What the client will free (or fetch) for them.
    pub bytes: u64,
}

impl PinOutcome {
    pub fn detail(&self, keep: bool) -> String {
        if keep {
            format!(
                "marked {} to keep on this device ({}); the Nextcloud client fetches them on \
                 its next run",
                count(self.files),
                freesize_core::format_size(self.bytes)
            )
        } else {
            format!(
                "marked {} online-only ({}); the Nextcloud client frees them on its next run, \
                 and they stay listed",
                count(self.files),
                freesize_core::format_size(self.bytes)
            )
        }
    }
}

fn count(files: u64) -> String {
    match files {
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    }
}

/// Why this platform can't free local copies from here, pointing to what
/// can.
pub fn online_only_pointer() -> &'static str {
    if cfg!(target_os = "macos") {
        "on a Mac the Nextcloud client's files are managed by macOS: in Finder, right-click \
         the folder and choose Remove Download"
    } else if cfg!(target_os = "android") {
        "on Android, the Nextcloud app's own \"available offline\" setting decides what's kept"
    } else {
        "on Linux the Nextcloud client's virtual files (in the client, marked beta) decide \
         what's kept"
    }
}

/// Marks the files below `paths` in the sync folder `root` online-only
/// (`keep` false) or to keep on this device (`keep` true). Windows only:
/// elsewhere it says what to use instead.
pub fn online_only(root: &Path, paths: &[String], keep: bool) -> Result<PinOutcome, String> {
    if !is_sync_folder(root) {
        return Err(format!(
            "{} isn't a folder the Nextcloud client syncs on this machine",
            root.display()
        ));
    }
    let mut targets = Vec::new();
    if paths.is_empty() {
        targets.push(root.to_path_buf());
    }
    for relative in paths {
        let target = below(root, relative)?;
        if !target.exists() {
            return Err(format!("{} isn't there", target.display()));
        }
        targets.push(target);
    }
    let mut outcome = PinOutcome::default();
    for target in &targets {
        pin::mark(target, keep, &mut outcome)?;
    }
    Ok(outcome)
}

#[cfg(windows)]
mod pin {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::{GetFileAttributesW, SetFileAttributesW};

    use super::PinOutcome;

    const INVALID: u32 = u32::MAX;
    const DIRECTORY: u32 = 0x10;
    const OFFLINE: u32 = 0x1000;
    const PINNED: u32 = 0x8_0000;
    const UNPINNED: u32 = 0x10_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    /// Sets the marks on `path` and everything below it.
    pub fn mark(path: &Path, keep: bool, outcome: &mut PinOutcome) -> Result<(), String> {
        let wide = wide(path);
        // SAFETY: a NUL-terminated wide string that outlives the calls.
        let attrs = unsafe { GetFileAttributesW(wide.as_ptr()) };
        if attrs == INVALID {
            return Err(format!(
                "{}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        let wanted = if keep {
            (attrs | PINNED) & !UNPINNED
        } else {
            (attrs | UNPINNED) & !PINNED
        };
        if wanted != attrs {
            // SAFETY: as above.
            if unsafe { SetFileAttributesW(wide.as_ptr(), wanted) } == 0 {
                return Err(format!(
                    "{}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                ));
            }
            if attrs & DIRECTORY == 0 {
                outcome.files += 1;
                let dehydrated = attrs & (OFFLINE | RECALL_ON_DATA_ACCESS) != 0;
                if dehydrated == keep {
                    outcome.bytes += std::fs::metadata(path).map_or(0, |m| m.len());
                }
            }
        }
        if attrs & DIRECTORY != 0 {
            for entry in std::fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))? {
                let entry = entry.map_err(|e| format!("{}: {e}", path.display()))?;
                if entry.file_type().is_ok_and(|t| t.is_symlink()) {
                    continue;
                }
                mark(&entry.path(), keep, outcome)?;
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod pin {
    use std::path::Path;

    use super::PinOutcome;

    pub fn mark(_path: &Path, _keep: bool, _outcome: &mut PinOutcome) -> Result<(), String> {
        Err(format!(
            "this machine can't free local copies from here: {}",
            super::online_only_pointer()
        ))
    }
}

/// What moving a folder into the sync folder did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MoveOutcome {
    pub files: u64,
    pub bytes: u64,
    /// The folder was renamed in place (same drive); nothing was copied.
    pub renamed: bool,
    /// Bytes the original took, now in the quarantine (a copy).
    pub quarantined: u64,
    pub notes: Vec<String>,
}

impl MoveOutcome {
    pub fn detail(&self) -> String {
        let mut text = if self.renamed {
            format!(
                "moved {} ({}) into the sync folder on the same drive; the Nextcloud client \
                 uploads it next",
                count(self.files),
                freesize_core::format_size(self.bytes)
            )
        } else {
            format!(
                "copied {} ({}) into the sync folder, checked every byte, and moved the \
                 original to the quarantine; the Nextcloud client uploads it next",
                count(self.files),
                freesize_core::format_size(self.bytes)
            )
        };
        for note in &self.notes {
            text.push_str("; ");
            text.push_str(note);
        }
        text
    }
}

/// Moves `source` (a folder) to `target` below the sync folder `into`.
/// `quarantine` moves the original aside once a copy checked out and
/// returns its bytes.
pub fn move_in(
    source: &Path,
    into: &Path,
    target: &str,
    quarantine: &dyn Fn(&[PathBuf]) -> Result<u64, String>,
) -> Result<MoveOutcome, String> {
    if !is_sync_folder(into) {
        return Err(format!(
            "{} isn't a folder the Nextcloud client syncs on this machine",
            into.display()
        ));
    }
    if !source.is_dir() {
        return Err(format!("{} isn't a folder", source.display()));
    }
    let dest = below(into, target)?;
    if dest == into {
        return Err("say where below the sync folder it goes".into());
    }
    if dest.exists() {
        return Err(format!("{} is already there", dest.display()));
    }
    let real_source = source
        .canonicalize()
        .map_err(|e| format!("{}: {e}", source.display()))?;
    let real_into = into
        .canonicalize()
        .map_err(|e| format!("{}: {e}", into.display()))?;
    if real_into.starts_with(&real_source) {
        return Err("the sync folder is inside that folder".into());
    }
    if real_source.starts_with(&real_into) {
        return Err(format!(
            "{} is in the sync folder already",
            source.display()
        ));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    move_folder(source, &dest, quarantine)
}

/// The move itself, without the sync folder checks (tests use it).
fn move_folder(
    source: &Path,
    dest: &Path,
    quarantine: &dyn Fn(&[PathBuf]) -> Result<u64, String>,
) -> Result<MoveOutcome, String> {
    let mut outcome = MoveOutcome::default();
    match std::fs::rename(source, dest) {
        Ok(()) => {
            let (files, bytes) = measure_tree(dest);
            outcome.files = files;
            outcome.bytes = bytes;
            outcome.renamed = true;
            return Ok(outcome);
        }
        Err(err) if crosses_devices(&err) => {}
        Err(err) => return Err(format!("{} -> {}: {err}", source.display(), dest.display())),
    }
    copy_tree(source, dest, &mut outcome)?;
    if let Err(err) = check_tree(source, dest) {
        // Leave nothing half done: the copy goes, the original stays.
        let _ = std::fs::remove_dir_all(dest);
        return Err(format!(
            "the copy didn't check out, so it was removed: {err}"
        ));
    }
    outcome.quarantined = quarantine(&[source.to_path_buf()])?;
    Ok(outcome)
}

fn crosses_devices(err: &std::io::Error) -> bool {
    // EXDEV on Unix, ERROR_NOT_SAME_DEVICE on Windows.
    matches!(err.raw_os_error(), Some(18) if cfg!(unix))
        || matches!(err.raw_os_error(), Some(17) if cfg!(windows))
}

fn measure_tree(path: &Path) -> (u64, u64) {
    let mut files = 0;
    let mut bytes = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                let (f, b) = measure_tree(&entry.path());
                files += f;
                bytes += b;
            } else if kind.is_file() {
                files += 1;
                bytes += entry.metadata().map_or(0, |m| m.len());
            }
        }
    }
    (files, bytes)
}

fn copy_tree(source: &Path, dest: &Path, outcome: &mut MoveOutcome) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    for entry in std::fs::read_dir(source).map_err(|e| format!("{}: {e}", source.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", source.display()))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|e| format!("{}: {e}", from.display()))?;
        if kind.is_dir() {
            copy_tree(&from, &to, outcome)?;
        } else if kind.is_file() {
            let bytes = std::fs::copy(&from, &to)
                .map_err(|e| format!("{} -> {}: {e}", from.display(), to.display()))?;
            outcome.files += 1;
            outcome.bytes += bytes;
        } else {
            outcome
                .notes
                .push(format!("{} is a link and wasn't copied", from.display()));
        }
    }
    Ok(())
}

/// Every file in `source` has a byte-for-byte equal copy in `dest`.
fn check_tree(source: &Path, dest: &Path) -> Result<(), String> {
    use std::io::Read;
    for entry in std::fs::read_dir(source).map_err(|e| format!("{}: {e}", source.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", source.display()))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|e| format!("{}: {e}", from.display()))?;
        if kind.is_dir() {
            check_tree(&from, &to)?;
        } else if kind.is_file() {
            let mut a =
                std::fs::File::open(&from).map_err(|e| format!("{}: {e}", from.display()))?;
            let mut b = std::fs::File::open(&to).map_err(|e| format!("{}: {e}", to.display()))?;
            let (mut buf_a, mut buf_b) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
            loop {
                let read_a = a
                    .read(&mut buf_a)
                    .map_err(|e| format!("{}: {e}", from.display()))?;
                let mut read_b = 0;
                while read_b < read_a {
                    let n = b
                        .read(&mut buf_b[read_b..read_a])
                        .map_err(|e| format!("{}: {e}", to.display()))?;
                    if n == 0 {
                        break;
                    }
                    read_b += n;
                }
                if read_a != read_b || buf_a[..read_a] != buf_b[..read_b] {
                    return Err(format!("{} differs from its copy", from.display()));
                }
                if read_a == 0 {
                    let mut extra = [0u8; 1];
                    if b.read(&mut extra)
                        .map_err(|e| format!("{}: {e}", to.display()))?
                        != 0
                    {
                        return Err(format!("{} differs from its copy", from.display()));
                    }
                    break;
                }
            }
        }
    }
    Ok(())
}

/// One line per account for logs and the command line.
pub fn describe(account: &Account) -> String {
    format!(
        "{} on {} ({}): {}",
        account.user,
        account.server,
        account.client,
        account
            .folders
            .iter()
            .map(|f| format!("{} = {}", f.local.display(), f.remote))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"[General]
optionalServerNotifications=true

[Accounts]
0\Folders\1\ignoreHiddenFiles=true
0\Folders\1\journalPath=.sync_7c1b3d7a3e2f.db
0\Folders\1\localPath=/home/bob/Nextcloud/
0\Folders\1\paused=false
0\Folders\1\targetPath=/
0\Folders\1\virtualFilesMode=off
0\Folders\2\localPath="/home/bob/My Photos/"
0\Folders\2\targetPath=/Photos
0\Folders\2\virtualFilesMode=suffix
0\authType=webflow
0\dav_user=bob
0\displayName=Bob
0\url=https://cloud.example.com/
0\webflow_user=bob
1\url=https://other.example.org
1\user=alice
version=2
"#;

    /// The 35.0 Windows client: one classic folder under `Folders` and
    /// the virtual-files ones under `FoldersWithPlaceholders`, lines
    /// grouped by field rather than by folder, binary values quoted, and
    /// an old folder named rather than numbered.
    const WINDOWS_CONFIG: &str = r#"[General]
clientVersion=35.0.0 (build 20261012)

[Accounts]
0\version=13
version=13
0\url=https://cloud.example.com
0\dav_user=bob@example.com
0\serverColor=@Variant(\0\0\0\x43\x1\xff\xff\0\0gg\x9e\x9e\0\0)
0\webflow_user=bob@example.com
0\authType=webflow
0\Folders\5\localPath=C:/Program Files (x86)/Steam/userdata/
0\FoldersWithPlaceholders\1\localPath=C:/Users/bob/Documents/
0\FoldersWithPlaceholders\2\localPath=C:/Users/bob/Pictures/
0\FoldersWithPlaceholders\6\localPath=D:/DEV/
0\Folders\Nextcloud\localPath=C:/Users/bob/Nextcloud/
0\Folders\5\journalPath=.sync_35cde03cff48.db
0\Folders\5\targetPath=/Steam User Configs
0\FoldersWithPlaceholders\1\targetPath=/Documents
0\FoldersWithPlaceholders\2\targetPath=/Desktop Media/RIG2 Photos
0\FoldersWithPlaceholders\6\targetPath=/DEV
0\Folders\Nextcloud\targetPath=/
0\Folders\5\paused=false
0\Folders\5\virtualFilesMode=off
0\FoldersWithPlaceholders\1\virtualFilesMode=wincfapi
0\FoldersWithPlaceholders\2\virtualFilesMode=wincfapi
0\FoldersWithPlaceholders\6\virtualFilesMode=wincfapi
0\Folders\5\version=2
0\FoldersWithPlaceholders\1\version=3
0\Folders\5\navigationPaneClsid="@Variant(\0\0\0\x7f\0\0\0\x6QUuid\0=\x9a\xc1\xf7\xda\x13\x42\xfa\xbd\x96\x32g|\\\xdbZ)"
0\FoldersWithPlaceholders\1\navigationPaneClsid=@Variant(\0\0\0\x7f\0\0\0\x6QUuid\0\xd2\x3\x81\x61\xeb\xcf\x41\xd5\x88\xfcw\xb7\x81\xbf\x98*)
0\account_uuid=@Variant(\0\0\0\x7f\0\0\0\x6QUuid\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0)
"#;

    #[test]
    fn reads_the_windows_clients_config() {
        let accounts = parse_config(WINDOWS_CONFIG);
        assert_eq!(accounts.len(), 1);
        let bob = &accounts[0];
        assert_eq!(bob.user, "bob@example.com");
        let remotes: Vec<(&str, &str)> = bob
            .folders
            .iter()
            .map(|f| (f.remote.as_str(), f.virtual_files.as_str()))
            .collect();
        assert_eq!(
            remotes,
            vec![
                ("/Steam User Configs", "off"),
                ("/", ""),
                ("/Documents", "wincfapi"),
                ("/Desktop Media/RIG2 Photos", "wincfapi"),
                ("/DEV", "wincfapi"),
            ]
        );
        let steam = bob.folders[0].local.to_string_lossy().into_owned();
        assert!(steam.ends_with("userdata"), "{steam}");
        assert!(!steam.ends_with(['/', '\\']), "{steam}");
    }

    #[test]
    fn reads_the_desktop_config() {
        let accounts = parse_config(CONFIG);
        assert_eq!(accounts.len(), 2);
        let bob = &accounts[0];
        assert_eq!(bob.server, "https://cloud.example.com");
        assert_eq!(bob.user, "bob");
        assert_eq!(bob.client, "desktop");
        assert_eq!(bob.folders.len(), 2);
        assert_eq!(bob.folders[0].remote, "/");
        assert_eq!(bob.folders[0].virtual_files, "off");
        assert_eq!(bob.folders[1].remote, "/Photos");
        assert_eq!(bob.folders[1].virtual_files, "suffix");
        if cfg!(windows) {
            assert_eq!(bob.folders[1].local, Path::new("\\home\\bob\\My Photos"));
        } else {
            assert_eq!(bob.folders[0].local, Path::new("/home/bob/Nextcloud"));
            assert_eq!(bob.folders[1].local, Path::new("/home/bob/My Photos"));
        }
        let alice = &accounts[1];
        assert_eq!(alice.user, "alice");
        assert!(alice.folders.is_empty());
        assert_eq!(
            describe(bob),
            format!(
                "bob on https://cloud.example.com (desktop): {} = /, {} = /Photos",
                bob.folders[0].local.display(),
                bob.folders[1].local.display()
            )
        );
    }

    #[test]
    fn measures_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |path: &str, bytes: usize| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, vec![1u8; bytes]).unwrap();
        };
        write("Photos/2019/a.jpg", 3000);
        write("Photos/2019/b.jpg", 1000);
        write("Photos/2020/c.jpg", 500);
        write("Documents/notes.txt", 10);
        write("Documents/old.pdf.nextcloud", 0);
        write(".sync_7c1b3d7a3e2f.db", 4096);
        write(".owncloudsync.log", 100);
        let folder = Folder {
            local: root.to_path_buf(),
            remote: "/".into(),
            virtual_files: "suffix".into(),
        };
        let measured = measure(&folder);
        assert_eq!(measured.size, 4510);
        assert_eq!(measured.on_disk, 4510);
        assert_eq!(measured.files, 5);
        assert_eq!(measured.on_disk_files, 4);
        let paths: Vec<&str> = measured.dirs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["Documents", "Photos", "Photos/2019", "Photos/2020"]);
        assert_eq!(measured.dirs[2].size, 4000);
        assert_eq!(measured.dirs[2].files, 2);
        assert!(measured.big.is_empty());
        let report = report(
            &[Account {
                server: "https://cloud.example.com".into(),
                user: "bob".into(),
                client: "desktop".into(),
                folders: vec![folder],
            }],
            5,
        );
        assert_eq!(report.taken, 5);
        assert_eq!(report.accounts[0].folders[0].files, 5);
    }

    #[test]
    fn moves_a_folder_in_and_checks_copies() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |path: &str, bytes: &[u8]| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        };
        write("Videos/GoPro/a.mp4", &[1u8; 5000]);
        write("Videos/GoPro/2024/b.mp4", &[2u8; 300]);
        write("Videos/GoPro/empty.txt", b"");
        std::fs::create_dir_all(root.join("Nextcloud/Archive/Desktop")).unwrap();
        let no_quarantine = |_: &[PathBuf]| Err::<u64, String>("not used".into());
        // Same drive: a rename.
        let moved = move_folder(
            &root.join("Videos/GoPro"),
            &root.join("Nextcloud/Archive/Desktop/GoPro"),
            &no_quarantine,
        )
        .unwrap();
        assert!(moved.renamed);
        assert_eq!(moved.files, 3);
        assert_eq!(moved.bytes, 5300);
        assert!(
            root.join("Nextcloud/Archive/Desktop/GoPro/2024/b.mp4")
                .is_file()
        );
        assert!(!root.join("Videos/GoPro").exists());
        assert!(moved.detail().contains("same drive"));
        // A copy that checks out, then the original aside.
        write("Pictures/x.jpg", &[3u8; 1234]);
        let mut out = MoveOutcome::default();
        copy_tree(
            &root.join("Pictures"),
            &root.join("Nextcloud/Pictures"),
            &mut out,
        )
        .unwrap();
        assert_eq!((out.files, out.bytes), (1, 1234));
        check_tree(&root.join("Pictures"), &root.join("Nextcloud/Pictures")).unwrap();
        std::fs::write(root.join("Nextcloud/Pictures/x.jpg"), [3u8; 1233]).unwrap();
        assert!(
            check_tree(&root.join("Pictures"), &root.join("Nextcloud/Pictures"))
                .unwrap_err()
                .contains("differs")
        );
        // The checks around the sync folder.
        assert!(below(root, "a/../b").is_err());
        assert_eq!(below(root, "a//b/").unwrap(), root.join("a").join("b"));
        assert!(
            move_in(
                &root.join("Pictures"),
                &root.join("Nextcloud"),
                "Pictures",
                &no_quarantine
            )
            .unwrap_err()
            .contains("isn't a folder the Nextcloud client syncs")
        );
        if !cfg!(windows) {
            assert!(
                online_only(&root.join("Nextcloud"), &[], false)
                    .unwrap_err()
                    .contains("syncs")
            );
            assert!(!online_only_pointer().is_empty());
        }
    }

    #[test]
    fn android_folders_name_the_account() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir
            .path()
            .join("Android/media/com.nextcloud.client/nextcloud");
        std::fs::create_dir_all(base.join("bob@cloud.example.com")).unwrap();
        std::fs::create_dir_all(base.join("alice@host.example.org%2Fnextcloud")).unwrap();
        let accounts = android_accounts(dir.path());
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].user, "alice");
        assert_eq!(accounts[0].server, "https://host.example.org/nextcloud");
        assert_eq!(accounts[1].user, "bob");
        assert_eq!(accounts[1].server, "https://cloud.example.com");
        assert_eq!(accounts[1].client, "android");
        assert_eq!(accounts[1].folders[0].remote, "/");
        assert_eq!(
            accounts[1].folders[0].local,
            base.join("bob@cloud.example.com")
        );
    }
}
