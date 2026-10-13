//! What this device keeps of its Nextcloud folders (0.12): the desktop
//! client's accounts and sync folders from its `nextcloud.cfg`, the
//! Android app's folder under `Android/media`, and for each folder how much
//! is stored here against how much is only a placeholder. Nothing is read
//! from the client's sync journal (an SQLite file the client keeps open):
//! paths are the key, which are the same on every client of a server, and
//! the Hub matches them to the server's file ids itself.

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
            // folder number -> (local, remote, mode)
            let mut folders: BTreeMap<(bool, u32), Folder> = BTreeMap::new();
            for (key, value) in &keys {
                let mut parts = key.split('\\');
                let group = parts.next().unwrap_or_default();
                let old = match group {
                    "Folders" => false,
                    "FoldersWithPlaceholders" => true,
                    _ => continue,
                };
                let Some(number) = parts.next().and_then(|n| n.parse::<u32>().ok()) else {
                    continue;
                };
                let Some(field) = parts.next() else {
                    continue;
                };
                let folder = folders.entry((old, number)).or_default();
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
