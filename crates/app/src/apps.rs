//! Space used by installed apps and by the system.
//!
//! Android keeps every app's private data (and its folders under
//! `Android/data` and `Android/obb`) out of reach of other apps' file access,
//! so a file scan can't see most of what fills a phone. Android does report
//! the sizes through `StorageStatsManager` once the user grants "Usage
//! access"; that is what this module surfaces. Other platforms have no
//! equivalent, so there it reports itself as unsupported.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use disktree_core::{DiskSpace, disk_space};

use crate::cleanup::{self, Junk, MediaUse};
use crate::history::{self, Entry};
use crate::locations;

/// Times are seconds since 1970.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppUsage {
    pub package: String,
    pub label: String,
    /// APK, compiled code and native libraries.
    pub code_bytes: u64,
    /// Private data, including caches and the app's folders in shared storage.
    pub data_bytes: u64,
    /// The part of `data_bytes` that is cache, which the app can rebuild.
    pub cache_bytes: u64,
    /// Preinstalled and never updated, so its code lives on the system
    /// partition rather than in user storage.
    pub system_image: bool,
    /// Preinstalled, updated or not. Can't be uninstalled.
    pub system: bool,
    /// Package name of the app that installed it.
    pub installer: Option<String>,
    /// Display name of the installer, e.g. "Chrome".
    pub installer_label: Option<String>,
    pub installed_at: Option<u64>,
    pub version: String,
    /// When the user last had it on screen; `None` if Android has no record.
    pub last_opened: Option<u64>,
    /// Time on screen over about the last month.
    pub screen_time: Option<u64>,
    /// Has an icon in the app drawer.
    pub has_launcher: bool,
    /// Why the app keeps working without being opened ("your keyboard"),
    /// which keeps it out of the suggestions.
    pub protected: Option<String>,
    /// Archived (Android 15+): its code was removed, its data kept.
    pub archived: bool,
    /// Developer-declared category (`ApplicationInfo.category`), -1 if none.
    pub category: i32,
    /// DiskTree itself.
    pub own: bool,
}

impl AppUsage {
    /// Space the app takes in user storage. The code of preinstalled apps
    /// sits on the system partition, so only their data counts, which is how
    /// Android's own Settings attributes it.
    pub fn size(&self) -> u64 {
        if self.system_image {
            self.data_bytes
        } else {
            self.code_bytes + self.data_bytes
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppReport {
    /// Largest first.
    pub apps: Vec<AppUsage>,
    /// Every installed package, including any whose size couldn't be read.
    pub installed: Vec<String>,
    /// Shared storage that belongs to no app: the user's own files.
    pub shared_files: Option<u64>,
    /// The capacity printed on the box ("128 GB"), larger than user storage
    /// because the OS has partitions of its own.
    pub advertised_total: Option<u64>,
    /// Whether Android reported usage history. Without it no app can be
    /// called unused.
    pub usage_known: bool,
    /// Apps seen before that are no longer installed, newest first.
    pub removed: Vec<Entry>,
    /// Media in shared storage by the app that saved it, biggest first.
    pub media: Vec<MediaUse>,
    /// Trash, old installers and other files that can go.
    pub junk: Vec<Junk>,
    /// When the report was made.
    pub now: u64,
}

/// How user storage divides up. `apps + files + system + free == total`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Breakdown {
    pub total: u64,
    pub free: u64,
    pub apps: u64,
    /// Part of `apps` that is cache.
    pub cache: u64,
    pub files: u64,
    /// Everything else: OS data, other users, temporary files.
    pub system: u64,
    /// Space the OS uses outside user storage (advertised minus `total`).
    pub outside: u64,
}

impl AppReport {
    pub fn breakdown(&self, space: DiskSpace) -> Breakdown {
        let used = space.used();
        let apps = self.apps.iter().map(AppUsage::size).sum::<u64>().min(used);
        let cache = self
            .apps
            .iter()
            .map(|a| a.cache_bytes)
            .sum::<u64>()
            .min(apps);
        let files = self.shared_files.unwrap_or(0).min(used - apps);
        Breakdown {
            total: space.total,
            free: space.free,
            apps,
            cache,
            files,
            system: used - apps - files,
            outside: self
                .advertised_total
                .map_or(0, |advertised| advertised.saturating_sub(space.total)),
        }
    }
}

/// Whether app storage can be measured on this platform.
pub fn supported() -> bool {
    cfg!(target_os = "android") || demo::enabled()
}

/// Whether the user has allowed reading app storage.
pub fn has_access() -> bool {
    if demo::enabled() {
        return demo::access_granted();
    }
    platform::has_access()
}

/// Asks the user to allow reading app storage.
pub fn request_access() {
    platform::request_access();
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Measures every installed app, largest first, and updates the app
/// history. Slow, so call it off the UI thread.
pub fn collect() -> Result<AppReport, String> {
    let mut report = if demo::enabled() {
        demo::report()
    } else {
        platform::collect()?
    };
    report.now = now();
    report
        .apps
        .sort_by(|a, b| b.size().cmp(&a.size()).then_with(|| a.label.cmp(&b.label)));

    let paths = history_paths();
    let mut history = history::load(&paths);
    if demo::enabled() && history.apps.is_empty() {
        history = demo::history(report.now);
    }
    history.record(&report);
    if let Some(space) = disk_space(&locations::default_path()) {
        history.record_storage(report.now, report.breakdown(space));
    }
    if let Err(err) = history::save(&history, &paths) {
        log(&format!("could not save the app history: {err}"));
    }
    report.removed = history.removed();

    // Media and junk are in shared storage, which needs All files access.
    if demo::enabled() {
        report.media = demo::media();
        report.junk = demo::junk();
    } else if platform::storage_access() {
        let installed: HashSet<String> = report.installed.iter().cloned().collect();
        let labels: HashMap<String, String> = history
            .apps
            .iter()
            .map(|entry| (entry.package.clone(), entry.label.clone()))
            .collect();
        report.junk = platform::trash()
            .into_iter()
            .chain(cleanup::find(
                &locations::default_path(),
                &installed,
                &labels,
            ))
            .collect();
        report.media = cleanup::media_by_owner(platform::media_rows());
    }
    report
        .media
        .sort_by_key(|media| std::cmp::Reverse(media.bytes));
    Ok(report)
}

/// Deletes one kind of junk. Returns the bytes freed and the items that
/// couldn't be deleted. Slow for big folders, so call it off the UI thread.
pub fn delete_junk(junk: &Junk) -> (u64, Vec<String>) {
    if demo::enabled() {
        demo::clean(junk.kind);
        return (junk.bytes, Vec::new());
    }
    cleanup::delete(&junk.paths)
}

/// Removes an app from the history of removed apps.
pub fn forget(package: &str) -> Result<(), String> {
    let paths = history_paths();
    let mut history = history::load(&paths);
    history.forget(package);
    history::save(&history, &paths)
}

/// Where the app history is kept, most durable first.
fn history_paths() -> Vec<PathBuf> {
    const NAME: &str = "app-history.json";
    if demo::enabled() {
        return vec![std::env::temp_dir().join("disktree-demo").join(NAME)];
    }
    let mut paths = Vec::new();
    if cfg!(target_os = "android") {
        paths.push(
            locations::default_path()
                .join("Documents")
                .join("DiskTree")
                .join(NAME),
        );
    }
    paths.extend(platform::private_dir().map(|dir| dir.join(NAME)));
    paths
}

/// What this platform can do with an app.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Abilities {
    pub uninstall: bool,
    /// Android 15+ archives apps, from their info page.
    pub archive: bool,
    /// Android 11+ clears every app's cache in one system dialog.
    pub clear_cache: bool,
    pub save_apk: bool,
}

pub fn abilities() -> Abilities {
    if demo::enabled() {
        return Abilities {
            uninstall: true,
            archive: true,
            clear_cache: true,
            save_apk: true,
        };
    }
    platform::abilities()
}

/// Opens the app's system settings page (clear cache or data, uninstall).
pub fn open(package: &str) {
    platform::open(package);
}

/// Opens this app's own system settings page.
pub fn open_own_settings() {
    platform::open_own_settings();
}

/// Asks Android to uninstall the app; Android shows its own confirmation.
pub fn uninstall(package: &str) {
    platform::uninstall(package);
}

/// Opens the app's page in the store that installed it (the Play Store if
/// unknown).
pub fn open_in_store(package: &str, installer: Option<&str>) {
    platform::open_in_store(package, installer);
}

/// Searches the Play Store, for finding a removed app by name.
pub fn search_store(query: &str) {
    platform::search_store(query);
}

/// Shows Android's dialog for clearing every app's cache. Returns false if
/// the device has none, in which case storage settings open instead.
pub fn clear_all_cache() -> bool {
    platform::clear_all_cache()
}

/// Whether DiskTree's window is in front, used to notice the user coming
/// back from a system screen.
pub fn window_has_focus() -> bool {
    platform::window_has_focus()
}

/// Copies the app's install files to `Download/DiskTree`, so an app that
/// isn't in any store can be installed again. Slow for big apps, so call it
/// off the UI thread. Returns a message for the user.
pub fn save_apk(package: &str, label: &str, version: &str) -> Result<String, String> {
    let files = platform::apk_files(package)?;
    let dir = locations::default_path().join("Download").join("DiskTree");
    let name = file_name(&format!("{label} {version}"));
    let saved =
        copy_apks(&files, &dir, &name).map_err(|err| format!("Couldn't save the APK: {err}"))?;
    Ok(if files.len() == 1 {
        format!("Saved {saved} to Download/DiskTree.")
    } else {
        format!(
            "Saved {} files to Download/DiskTree/{saved}. They're split APKs: install them \
             together with an app like SAI, or with adb install-multiple.",
            files.len()
        )
    })
}

/// Copies `files` into `dir`: one file as `<name>.apk`, several into a
/// `<name>` folder under their own names. Returns what was created.
fn copy_apks(files: &[PathBuf], dir: &Path, name: &str) -> std::io::Result<String> {
    if let [file] = files {
        std::fs::create_dir_all(dir)?;
        let saved = format!("{name}.apk");
        std::fs::copy(file, dir.join(&saved))?;
        return Ok(saved);
    }
    let folder = dir.join(name);
    std::fs::create_dir_all(&folder)?;
    for file in files {
        let file_name = file.file_name().ok_or(std::io::ErrorKind::InvalidInput)?;
        std::fs::copy(file, folder.join(file_name))?;
    }
    Ok(name.to_string())
}

/// Percent-encodes `text` for a URL query.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn url_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Package names in an Android component list such as
/// `com.a/.Service:com.b/com.b.Other`.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn component_packages(list: &str) -> Vec<String> {
    list.split(':')
        .filter_map(|component| component.split('/').next())
        .map(str::trim)
        .filter(|package| !package.is_empty())
        .map(String::from)
        .collect()
}

/// `text` without characters that file systems reject.
fn file_name(text: &str) -> String {
    let name: String = text
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    let name = name.trim().trim_matches('.').to_string();
    if name.is_empty() { "app".into() } else { name }
}

fn log(message: &str) {
    #[cfg(target_os = "android")]
    crate::android::log(message);
    #[cfg(not(target_os = "android"))]
    eprintln!("{message}");
}

#[cfg(target_os = "android")]
mod platform {
    pub use crate::android::{
        abilities, apk_files, clear_all_cache, collect_app_report as collect,
        has_storage_access as storage_access, has_usage_access as has_access, media_rows,
        open_app_settings as open, open_in_store, open_own_app_settings as open_own_settings,
        private_dir, request_usage_access as request_access, search_store, trash, uninstall,
        window_has_focus,
    };
}

#[cfg(not(target_os = "android"))]
mod platform {
    use std::path::PathBuf;

    use super::{Abilities, AppReport};
    use crate::cleanup::Junk;

    pub fn has_access() -> bool {
        false
    }

    pub fn storage_access() -> bool {
        false
    }

    pub fn trash() -> Option<Junk> {
        None
    }

    pub fn media_rows() -> Vec<(String, u64, PathBuf)> {
        Vec::new()
    }

    pub fn request_access() {}

    pub fn collect() -> Result<AppReport, String> {
        Err("App storage is only available on Android".into())
    }

    pub fn abilities() -> Abilities {
        Abilities::default()
    }

    pub fn private_dir() -> Option<PathBuf> {
        None
    }

    pub fn open(_package: &str) {}

    pub fn open_own_settings() {}

    pub fn uninstall(_package: &str) {}

    pub fn open_in_store(_package: &str, _installer: Option<&str>) {}

    pub fn search_store(_query: &str) {}

    pub fn clear_all_cache() -> bool {
        false
    }

    pub fn window_has_focus() -> bool {
        true
    }

    pub fn apk_files(_package: &str) -> Result<Vec<PathBuf>, String> {
        Err("Saving APKs only works on Android.".into())
    }
}

/// Sample data for working on the UI without a phone: debug builds started
/// with `DISKTREE_FAKE_APPS=1` show it on the home screen, and
/// `DISKTREE_FAKE_APPS=no-access` shows the Usage access prompt.
mod demo {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::{AppReport, AppUsage};
    use crate::cleanup::{Junk, JunkKind, MediaUse};
    use crate::history::{Entry, History};
    use crate::locations;

    const DAY: u64 = 24 * 60 * 60;
    const MB: u64 = 1024 * 1024;

    /// Junk "deleted" in this run; nothing real is touched.
    static CLEANED: Mutex<Vec<JunkKind>> = Mutex::new(Vec::new());

    pub fn clean(kind: JunkKind) {
        CLEANED.lock().unwrap_or_else(|e| e.into_inner()).push(kind);
    }

    pub fn junk() -> Vec<Junk> {
        let cleaned = CLEANED.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let junk = |kind, mb: u64, names: &[&str]| Junk {
            kind,
            bytes: mb * MB,
            paths: if names.is_empty() {
                vec![PathBuf::from("/demo/DCIM/.thumbnails")]
            } else {
                names
                    .iter()
                    .map(|n| PathBuf::from("/demo").join(n))
                    .collect()
            },
            names: names.iter().map(|n| n.to_string()).collect(),
        };
        [
            junk(
                JunkKind::Trash,
                2_450,
                &["PXL_20260901.mp4", "PXL_20260902.jpg", "IMG_0042.jpg"],
            ),
            junk(
                JunkKind::Installers,
                1_130,
                &["Insta360_v1.2.apk", "Bambu Handy.apk"],
            ),
            junk(JunkKind::Thumbnails, 320, &[]),
            junk(
                JunkKind::Leftovers,
                2_040,
                &["Snapseed", "org.example.camerahelper"],
            ),
        ]
        .into_iter()
        .filter(|j| !cleaned.contains(&j.kind))
        .collect()
    }

    pub fn media() -> Vec<MediaUse> {
        let root = locations::default_path();
        let media = |package: &str, mb: u64, files, folder: &str, in_app_data| MediaUse {
            package: package.into(),
            bytes: mb * MB,
            files,
            folder: root.join(folder),
            in_app_data,
        };
        vec![
            media(
                "com.whatsapp",
                9_200,
                3_400,
                "Android/media/com.whatsapp",
                true,
            ),
            media(
                "com.arashivision.insta360akiko",
                12_300,
                210,
                "DCIM/Insta360",
                false,
            ),
            media(
                "com.google.android.apps.photos",
                800,
                95,
                "Pictures/Photos",
                false,
            ),
        ]
        .into_iter()
        .map(|mut m| {
            if !m.folder.is_absolute() {
                m.folder = PathBuf::from("/").join(&m.folder);
            }
            m
        })
        .collect()
    }

    pub fn enabled() -> bool {
        cfg!(debug_assertions) && std::env::var_os("DISKTREE_FAKE_APPS").is_some()
    }

    pub fn access_granted() -> bool {
        std::env::var("DISKTREE_FAKE_APPS").as_deref() != Ok("no-access")
    }

    pub fn report() -> AppReport {
        let now = super::now();
        let app =
            |label: &str, package: &str, code: u64, data: u64, cache: u64, opened: u64| AppUsage {
                package: package.into(),
                label: label.into(),
                code_bytes: code * MB,
                data_bytes: data * MB,
                cache_bytes: cache * MB,
                installer: Some("com.android.vending".into()),
                installed_at: Some(now - 500 * DAY),
                version: "1.0".into(),
                last_opened: Some(now - opened * DAY),
                screen_time: Some(if opened < 30 { 5 * 3600 } else { 0 }),
                has_launcher: true,
                category: -1,
                ..AppUsage::default()
            };
        let mut apps = vec![
            app("WhatsApp", "com.whatsapp", 120, 18_400, 650, 0),
            app("Spotify", "com.spotify.music", 160, 9_800, 7_900, 2),
            app("Chrome", "com.android.chrome", 410, 3_100, 2_300, 0),
            app(
                "Call of Duty",
                "com.activision.callofduty.shooter",
                2_900,
                6_200,
                140,
                200,
            ),
            app("Netflix", "com.netflix.mediaclient", 95, 4_700, 80, 120),
            app(
                "Google Photos",
                "com.google.android.apps.photos",
                240,
                1_900,
                1_100,
                1,
            ),
            app(
                "Insta360",
                "com.arashivision.insta360akiko",
                2_090,
                3_710,
                0,
                400,
            ),
            app("Duolingo", "com.duolingo", 180, 420, 60, 0),
            app(
                "Gboard",
                "com.google.android.inputmethod.latin",
                90,
                310,
                20,
                0,
            ),
            app("Old Game", "com.example.oldgame", 0, 1_200, 0, 0),
            app("Sideloaded Tool", "org.example.tool", 45, 120, 5, 3),
            app("DiskTree", "dev.disktree.app", 16, 2, 0, 0),
        ];
        apps[3].category = 0;
        apps[7].last_opened = None;
        apps[8].protected = Some("your keyboard".into());
        apps[8].has_launcher = false;
        apps[9].archived = true;
        apps[9].last_opened = None;
        apps[10].installer = Some("com.google.android.packageinstaller".into());
        apps[10].installer_label = Some("Chrome".into());
        apps[11].installer = Some("com.google.android.packageinstaller".into());
        apps[11].own = true;
        apps[11].protected = Some("DiskTree itself".into());
        let mut aicore = app("AICore", "com.google.android.aicore", 127, 6_030, 0, 0);
        aicore.system = true;
        aicore.has_launcher = false;
        aicore.last_opened = None;
        apps.push(aicore);
        AppReport {
            installed: apps.iter().map(|a| a.package.clone()).collect(),
            apps,
            shared_files: Some(4_400 * MB),
            advertised_total: Some(128 * 1024 * MB),
            usage_known: true,
            ..AppReport::default()
        }
    }

    /// A couple of removed apps for the Removed view.
    pub fn history(now: u64) -> History {
        let entry = |label: &str, package: &str, installer: &str, days: u64| Entry {
            package: package.into(),
            label: label.into(),
            installer: Some(installer.into()),
            version: "2.4".into(),
            installed_at: Some(now - 700 * DAY),
            first_seen: now - 300 * DAY,
            last_seen: now - days * DAY,
            removed_at: Some(now - days * DAY),
            size: 850 * MB,
            sizes: Vec::new(),
        };
        History {
            apps: vec![
                entry(
                    "Snapseed",
                    "com.niksoftware.snapseed",
                    "com.android.vending",
                    40,
                ),
                entry(
                    "Camera Helper",
                    "org.example.camerahelper",
                    "com.google.android.packageinstaller",
                    95,
                ),
            ],
            ..History::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    fn app(code: u64, data: u64, cache: u64, system_image: bool) -> AppUsage {
        AppUsage {
            package: "p".into(),
            label: "l".into(),
            code_bytes: code,
            data_bytes: data,
            cache_bytes: cache,
            system_image,
            ..AppUsage::default()
        }
    }

    #[test]
    fn preinstalled_apps_only_count_their_data() {
        assert_eq!(app(5, 10, 1, false).size(), 15);
        assert_eq!(app(5, 10, 1, true).size(), 10);
    }

    #[test]
    fn breakdown_adds_up_to_total() {
        let report = AppReport {
            apps: vec![
                app(2 * GB, 50 * GB, 5 * GB, false),
                app(GB, 8 * GB, GB, true),
            ],
            shared_files: Some(4 * GB),
            advertised_total: Some(128 * GB),
            ..AppReport::default()
        };
        let space = DiskSpace {
            total: 109 * GB,
            free: 21 * GB,
        };
        let b = report.breakdown(space);
        assert_eq!(b.apps, 60 * GB);
        assert_eq!(b.cache, 6 * GB);
        assert_eq!(b.files, 4 * GB);
        assert_eq!(b.system, 24 * GB);
        assert_eq!(b.outside, 19 * GB);
        assert_eq!(b.apps + b.files + b.system + b.free, b.total);
    }

    #[test]
    fn breakdown_never_exceeds_used_space() {
        let report = AppReport {
            apps: vec![app(0, 200 * GB, 0, false)],
            shared_files: Some(50 * GB),
            advertised_total: None,
            ..AppReport::default()
        };
        let b = report.breakdown(DiskSpace {
            total: 100 * GB,
            free: 10 * GB,
        });
        assert_eq!(b.apps, 90 * GB);
        assert_eq!(b.files, 0);
        assert_eq!(b.system, 0);
        assert_eq!(b.outside, 0);
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_name("Tool: v1/2"), "Tool- v1-2");
        assert_eq!(file_name(" .. "), "app");
    }

    #[test]
    fn copies_one_apk_or_a_folder_of_splits() {
        let dir = std::env::temp_dir().join(format!("disktree-apk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("base.apk");
        let split = dir.join("split_config.arm64_v8a.apk");
        std::fs::write(&base, b"base").unwrap();
        std::fs::write(&split, b"split").unwrap();
        let out = dir.join("out");

        assert_eq!(
            copy_apks(std::slice::from_ref(&base), &out, "Tool 1.0").unwrap(),
            "Tool 1.0.apk"
        );
        assert_eq!(std::fs::read(out.join("Tool 1.0.apk")).unwrap(), b"base");

        assert_eq!(copy_apks(&[base, split], &out, "Big 2").unwrap(), "Big 2");
        assert!(out.join("Big 2/base.apk").exists());
        assert!(out.join("Big 2/split_config.arm64_v8a.apk").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn url_encoding() {
        assert_eq!(
            url_encode("Camera Helper & co"),
            "Camera%20Helper%20%26%20co"
        );
        assert_eq!(url_encode("Ünï"), "%C3%9Cn%C3%AF");
    }

    #[test]
    fn component_lists() {
        assert_eq!(
            component_packages("com.a/.Service:com.b/com.b.Other: :"),
            ["com.a", "com.b"]
        );
        assert!(component_packages("").is_empty());
    }
}
