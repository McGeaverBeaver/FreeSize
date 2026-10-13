//! Which app or game a folder belongs to, and its icon, so the map views can
//! show Steam's icon on the Steam folder and each game's own icon on the
//! game.
//!
//! - Android: `Android/data`, `Android/media` and `Android/obb` hold one
//!   folder per app, named by its package; the icons come from the app
//!   report.
//! - Steam, everywhere: `steamapps/common/<folder>` is a game, named by a
//!   `steamapps/appmanifest_<id>.acf`; Steam's library cache has its icon.
//! - Linux: folders where apps keep their files (`~/.config`, `~/.cache`,
//!   `~/.local/share`, `~/.var/app`, `/opt`, …) matched to `.desktop`
//!   entries, with icons from the icon theme.
//! - Windows: folders in Program Files and AppData, by the program inside.
//! - macOS: `.app` bundles, and folders in `~/Library` matched to apps.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

/// An icon as raw RGBA pixels, as Android draws it or Windows extracts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Pixels {
    pub fn image(&self) -> Image {
        Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            &self.rgba,
            self.width,
            self.height,
        ))
    }
}

/// Where an icon comes from.
#[derive(Debug, PartialEq)]
enum Source {
    File(PathBuf),
    Pixels(Arc<Pixels>),
}

#[derive(Default)]
pub struct Owners {
    /// Icons already looked up, by folder (`None`: belongs to no app).
    cache: RefCell<HashMap<PathBuf, Option<Image>>>,
    /// Android app icons by package name, and as images once made.
    android: HashMap<String, Arc<Pixels>>,
    app_images: RefCell<HashMap<String, Image>>,
    /// Games of each Steam library, by `steamapps` folder.
    steam: RefCell<HashMap<PathBuf, HashMap<String, String>>>,
}

impl Owners {
    pub fn set_android_icons(&mut self, icons: HashMap<String, Arc<Pixels>>) {
        if icons == self.android {
            return;
        }
        self.android = icons;
        self.cache.borrow_mut().clear();
        self.app_images.borrow_mut().clear();
    }

    /// An Android app's icon by package name.
    pub fn app_icon(&self, package: &str) -> Option<Image> {
        if let Some(image) = self.app_images.borrow().get(package) {
            return Some(image.clone());
        }
        let image = self.android.get(package)?.image();
        self.app_images
            .borrow_mut()
            .insert(package.to_string(), image.clone());
        Some(image)
    }

    /// Forgets what was looked up, for a new scan.
    pub fn clear(&self) {
        self.cache.borrow_mut().clear();
        self.steam.borrow_mut().clear();
    }

    /// The icon of the app or game the folder at `path` belongs to.
    pub fn icon(&self, path: &Path) -> Option<Image> {
        if let Some(hit) = self.cache.borrow().get(path) {
            return hit.clone();
        }
        let image = self.source(path).and_then(|source| match source {
            Source::File(file) => Image::load_from_path(&file).ok(),
            Source::Pixels(pixels) => Some(pixels.image()),
        });
        self.cache
            .borrow_mut()
            .insert(path.to_path_buf(), image.clone());
        image
    }

    fn source(&self, path: &Path) -> Option<Source> {
        let name = path.file_name()?.to_str()?;
        let parent = path.parent()?;
        let parent_name = file_name(parent);
        let grandparent = parent.parent();

        if matches!(parent_name, "data" | "media" | "obb")
            && grandparent.map(file_name) == Some("Android")
            && let Some(pixels) = self.android.get(name)
        {
            return Some(Source::Pixels(pixels.clone()));
        }

        if parent_name == "common"
            && let Some(steamapps) =
                grandparent.filter(|g| file_name(g).eq_ignore_ascii_case("steamapps"))
        {
            let mut libraries = self.steam.borrow_mut();
            let games = libraries
                .entry(steamapps.to_path_buf())
                .or_insert_with(|| steam::games(steamapps));
            if let Some(app) = games.get(&name.to_lowercase()) {
                return steam::icon(steamapps, app).map(Source::File);
            }
            return None;
        }

        platform::source(path, name, parent)
    }
}

fn file_name(path: &Path) -> &str {
    path.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

mod steam {
    use super::*;

    /// Install folder (lower case) → app id, from the library's manifests.
    pub fn games(steamapps: &Path) -> HashMap<String, String> {
        let mut games = HashMap::new();
        let Ok(entries) = fs::read_dir(steamapps) else {
            return games;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
                continue;
            }
            let Ok(text) = fs::read_to_string(entry.path()) else {
                continue;
            };
            if let (Some(app), Some(folder)) = (value(&text, "appid"), value(&text, "installdir")) {
                games.insert(folder.to_lowercase(), app);
            }
        }
        games
    }

    /// A `"key"  "value"` line's value in Valve's KeyValues text format.
    pub fn value(text: &str, key: &str) -> Option<String> {
        text.lines().find_map(|line| {
            let mut parts = line.trim().split('"').filter(|p| !p.trim().is_empty());
            (parts.next()?.eq_ignore_ascii_case(key))
                .then(|| parts.next().map(|v| v.replace("\\\\", "\\")))
                .flatten()
        })
    }

    /// Steam's own folders: the library's parent first, then the usual
    /// install places.
    fn roots(steamapps: &Path) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = steamapps
            .parent()
            .map(Path::to_path_buf)
            .into_iter()
            .collect();
        if let Some(home) = std::env::home_dir() {
            roots.push(home.join(".local/share/Steam"));
            roots.push(home.join(".steam/steam"));
            roots.push(home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
            roots.push(home.join("Library/Application Support/Steam"));
        }
        if let Some(dir) = std::env::var_os("ProgramFiles(x86)") {
            roots.push(PathBuf::from(dir).join("Steam"));
        }
        roots
    }

    /// The game's icon from Steam's library cache.
    pub fn icon(steamapps: &Path, app: &str) -> Option<PathBuf> {
        for root in roots(steamapps) {
            let cache = root.join("appcache").join("librarycache");
            // Older Steam: flat files.
            for name in [
                format!("{app}_icon.jpg"),
                format!("{app}_library_600x900.jpg"),
            ] {
                let file = cache.join(name);
                if file.is_file() {
                    return Some(file);
                }
            }
            // Steam since 2024: a folder per game, the icon named by its hash.
            let dir = cache.join(app);
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            let mut fallback = None;
            for entry in entries.flatten() {
                let path = entry.path();
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let jpg = path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("jpg"));
                if jpg && stem.len() == 40 && stem.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Some(path);
                }
                if stem == "library_600x900" {
                    fallback = Some(path);
                }
            }
            if fallback.is_some() {
                return fallback;
            }
        }
        None
    }
}

#[cfg(target_os = "android")]
mod platform {
    use super::*;

    pub fn source(_path: &Path, _name: &str, _parent: &Path) -> Option<Source> {
        None
    }
}

#[cfg(all(unix, not(target_os = "android"), not(target_os = "macos")))]
mod platform {
    use std::sync::OnceLock;

    use super::*;

    pub fn source(_path: &Path, name: &str, parent: &Path) -> Option<Source> {
        let home = std::env::home_dir()?;
        let dot = name.starts_with('.');
        let container = [
            ".config",
            ".cache",
            ".local/share",
            ".var/app",
            "snap",
            ".local/share/flatpak/app",
        ]
        .iter()
        .any(|dir| parent == home.join(dir))
            || (parent == home && dot)
            || [
                "/opt",
                "/usr/share",
                "/usr/lib",
                "/usr/lib64",
                "/usr/local/share",
                "/var/lib",
                "/var/cache",
                "/var/lib/flatpak/app",
            ]
            .iter()
            .any(|dir| parent == Path::new(dir));
        if !container {
            return None;
        }
        let index = INDEX.get_or_init(|| Desktop::build(&data_dirs()));
        let icon = index.icon_for(name)?;
        find_icon(&data_dirs(), icon).map(Source::File)
    }

    static INDEX: OnceLock<Desktop> = OnceLock::new();

    /// `$XDG_DATA_HOME` and `$XDG_DATA_DIRS`, plus where Flatpak and Snap
    /// export their apps.
    pub fn data_dirs() -> Vec<PathBuf> {
        let home = std::env::home_dir().unwrap_or_default();
        let mut dirs = vec![
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|d| d.is_absolute())
                .unwrap_or_else(|| home.join(".local/share")),
        ];
        let system = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        dirs.extend(system.split(':').map(PathBuf::from));
        dirs.push(home.join(".local/share/flatpak/exports/share"));
        dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));
        dirs.push(PathBuf::from("/var/lib/snapd/desktop"));
        dirs
    }

    /// Icon names of installed apps by the names their folders go by.
    #[derive(Default)]
    pub struct Desktop {
        icons: HashMap<String, String>,
    }

    impl Desktop {
        pub fn build(data_dirs: &[PathBuf]) -> Desktop {
            let mut index = Desktop::default();
            for dir in data_dirs {
                index.add_dir(&dir.join("applications"), 0);
            }
            index
        }

        fn add_dir(&mut self, dir: &Path, depth: u32) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && depth < 2 {
                    self.add_dir(&path, depth + 1);
                } else if path.extension().is_some_and(|e| e == "desktop") {
                    self.add_file(&path);
                }
            }
        }

        fn add_file(&mut self, file: &Path) {
            let Ok(text) = fs::read_to_string(file) else {
                return;
            };
            let mut in_entry = false;
            let (mut icon, mut class, mut exec) = (None, None, None);
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_entry = line == "[Desktop Entry]";
                    continue;
                }
                if !in_entry {
                    continue;
                }
                if let Some(v) = line.strip_prefix("Icon=") {
                    icon = Some(v.trim().to_string());
                } else if let Some(v) = line.strip_prefix("StartupWMClass=") {
                    class = Some(v.trim().to_lowercase());
                } else if let Some(v) = line.strip_prefix("Exec=") {
                    exec = v
                        .split_whitespace()
                        .find(|w| !w.contains('='))
                        .and_then(|w| Path::new(w.trim_matches('"')).file_name())
                        .map(|n| n.to_string_lossy().to_lowercase());
                }
            }
            let Some(icon) = icon.filter(|i| !i.is_empty()) else {
                return;
            };
            let id = file
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            // org.mozilla.firefox is also known as firefox.
            let short = id.rsplit('.').next().map(str::to_string);
            let icon_key = Path::new(&icon)
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase());
            for key in [Some(id), short, class, exec, icon_key]
                .into_iter()
                .flatten()
                .filter(|k| k.len() >= 3)
            {
                self.icons.entry(key).or_insert_with(|| icon.clone());
            }
        }

        /// The icon of the app keeping its files in a folder called `name`.
        pub fn icon_for(&self, name: &str) -> Option<&str> {
            let key = name.trim_start_matches('.').to_lowercase();
            let key = match key.as_str() {
                // Folders named after the vendor rather than the app.
                "mozilla" => "firefox",
                "google-chrome-beta" | "google-chrome-unstable" => "google-chrome",
                "code - oss" => "code-oss",
                other => other,
            };
            self.icons
                .get(key)
                .or_else(|| self.icons.get(&key.replace(' ', "-")))
                .map(String::as_str)
        }
    }

    /// Finds an icon by name in the hicolor theme or pixmaps, biggest first.
    pub fn find_icon(data_dirs: &[PathBuf], icon: &str) -> Option<PathBuf> {
        let path = Path::new(icon);
        if path.is_absolute() {
            return path.is_file().then(|| path.to_path_buf());
        }
        for dir in data_dirs {
            for size in ["256x256", "128x128", "96x96", "64x64", "48x48", "scalable"] {
                for ext in ["png", "svg"] {
                    let file = dir
                        .join("icons/hicolor")
                        .join(size)
                        .join("apps")
                        .join(format!("{icon}.{ext}"));
                    if file.is_file() {
                        return Some(file);
                    }
                }
            }
        }
        data_dirs.iter().find_map(|dir| {
            ["png", "svg"]
                .iter()
                .map(|ext| dir.join("pixmaps").join(format!("{icon}.{ext}")))
                .find(|f| f.is_file())
        })
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    pub fn source(path: &Path, name: &str, parent: &Path) -> Option<Source> {
        let containers: Vec<PathBuf> = [
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "LOCALAPPDATA",
            "APPDATA",
            "ProgramData",
        ]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .flat_map(|dir| [dir.join("Programs"), dir])
        .collect();
        let same = |a: &Path, b: &Path| a.as_os_str().eq_ignore_ascii_case(b.as_os_str());
        if !containers.iter().any(|c| same(c, parent)) {
            return None;
        }
        let exe = find_program(path, name)?;
        crate::winicon::extract(&exe).map(|(width, height, rgba)| {
            Source::Pixels(Arc::new(Pixels {
                width,
                height,
                rgba,
            }))
        })
    }

    /// The program in an app's folder: the `.exe` named most like the
    /// folder, else the biggest, looking two folders deep.
    fn find_program(dir: &Path, name: &str) -> Option<PathBuf> {
        const SKIP: [&str; 9] = [
            "unins", "setup", "update", "crash", "helper", "install", "elevat", "notif", "report",
        ];
        let wanted = name.to_lowercase();
        let mut best: Option<(u32, u64, PathBuf)> = None;
        let mut queue = vec![(dir.to_path_buf(), 0)];
        while let Some((folder, depth)) = queue.pop() {
            let Ok(entries) = fs::read_dir(&folder) else {
                continue;
            };
            for entry in entries.flatten().take(200) {
                let path = entry.path();
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                if meta.is_dir() {
                    if depth < 2 {
                        queue.push((path, depth + 1));
                    }
                    continue;
                }
                if !path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
                {
                    continue;
                }
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if SKIP.iter().any(|s| stem.contains(s)) {
                    continue;
                }
                let score = if stem == wanted {
                    3
                } else if wanted.contains(&stem) || stem.contains(&wanted) {
                    2
                } else {
                    1
                };
                let candidate = (score, meta.len(), path);
                if best
                    .as_ref()
                    .is_none_or(|b| (candidate.0, candidate.1) > (b.0, b.1))
                {
                    best = Some(candidate);
                }
            }
        }
        best.map(|(_, _, path)| path)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::sync::OnceLock;

    use super::*;

    pub fn source(path: &Path, name: &str, parent: &Path) -> Option<Source> {
        if name.ends_with(".app") {
            return bundle_icon(path).map(|p| Source::Pixels(Arc::new(p)));
        }
        let home = std::env::home_dir()?;
        let library = home.join("Library");
        let container = [
            "Application Support",
            "Caches",
            "Containers",
            "Group Containers",
            "Logs",
            "Saved Application State",
        ]
        .iter()
        .any(|dir| parent == library.join(dir))
            || parent == Path::new("/Library/Application Support");
        if !container {
            return None;
        }
        let key = name
            .trim_start_matches("group.")
            .trim_end_matches(".savedState")
            .to_lowercase();
        let apps = APPS.get_or_init(index);
        let bundle = apps.get(&key)?;
        bundle_icon(bundle).map(|p| Source::Pixels(Arc::new(p)))
    }

    static APPS: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();

    /// Installed apps by bundle id and by name, both lower case.
    fn index() -> HashMap<String, PathBuf> {
        let mut apps = HashMap::new();
        let mut dirs = vec![
            PathBuf::from("/Applications"),
            PathBuf::from("/Applications/Utilities"),
            PathBuf::from("/System/Applications"),
            PathBuf::from("/System/Applications/Utilities"),
        ];
        if let Some(home) = std::env::home_dir() {
            dirs.push(home.join("Applications"));
        }
        for dir in dirs {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.extension().is_some_and(|e| e == "app") {
                    continue;
                }
                let info = plist::Value::from_file(path.join("Contents/Info.plist")).ok();
                let dict = info.as_ref().and_then(|v| v.as_dictionary());
                let text = |key: &str| {
                    dict.and_then(|d| d.get(key))
                        .and_then(|v| v.as_string())
                        .map(str::to_lowercase)
                };
                let stem = path.file_stem().map(|s| s.to_string_lossy().to_lowercase());
                for key in [text("CFBundleIdentifier"), text("CFBundleName"), stem]
                    .into_iter()
                    .flatten()
                {
                    apps.entry(key).or_insert_with(|| path.clone());
                }
            }
        }
        apps
    }

    /// The icon in an app bundle's `.icns` file.
    fn bundle_icon(bundle: &Path) -> Option<Pixels> {
        let info = plist::Value::from_file(bundle.join("Contents/Info.plist")).ok()?;
        let name = info
            .as_dictionary()?
            .get("CFBundleIconFile")?
            .as_string()?
            .to_string();
        let mut file = bundle.join("Contents/Resources").join(&name);
        if file.extension().is_none() {
            file.set_extension("icns");
        }
        decode_icns(&fs::read(file).ok()?)
    }

    /// The biggest icon up to 256 px in an `.icns` file.
    pub fn decode_icns(bytes: &[u8]) -> Option<Pixels> {
        let family = icns::IconFamily::read(std::io::Cursor::new(bytes)).ok()?;
        let mut kinds: Vec<_> = family
            .available_icons()
            .into_iter()
            .filter(|t| t.pixel_width() <= 256)
            .collect();
        kinds.sort_by_key(|t| std::cmp::Reverse(t.pixel_width()));
        // Some sizes may be in a format this can't decode; take the next.
        let image = kinds
            .into_iter()
            .find_map(|kind| family.get_icon_with_type(kind).ok())?
            .convert_to(icns::PixelFormat::RGBA);
        Some(Pixels {
            width: image.width(),
            height: image.height(),
            rgba: image.data().to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_app_folders_use_the_app_icon() {
        let mut owners = Owners::default();
        let pixels = Arc::new(Pixels {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 255],
        });
        owners.set_android_icons(HashMap::from([(
            "com.whatsapp".to_string(),
            pixels.clone(),
        )]));
        let root = Path::new("/storage/emulated/0/Android");
        assert_eq!(
            owners.source(&root.join("media/com.whatsapp")),
            Some(Source::Pixels(pixels.clone()))
        );
        assert_eq!(owners.source(&root.join("media/com.unknown")), None);
        assert_eq!(owners.source(Path::new("/x/media/com.whatsapp")), None);
        assert!(owners.icon(&root.join("obb/com.whatsapp")).is_some());
    }

    #[test]
    fn steam_games_by_manifest() {
        let dir = std::env::temp_dir().join(format!("freesize-steam-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let steamapps = dir.join("steamapps");
        fs::create_dir_all(steamapps.join("common/Hades")).unwrap();
        fs::write(
            steamapps.join("appmanifest_1145360.acf"),
            "\"AppState\"\n{\n\t\"appid\"\t\t\"1145360\"\n\t\"name\"\t\t\"Hades\"\n\t\"installdir\"\t\t\"Hades\"\n}\n",
        )
        .unwrap();
        let cache = dir.join("appcache/librarycache/1145360");
        fs::create_dir_all(&cache).unwrap();
        let icon = cache.join("0123456789abcdef0123456789abcdef01234567.jpg");
        fs::write(&icon, b"jpg").unwrap();
        fs::write(cache.join("header.jpg"), b"jpg").unwrap();

        let owners = Owners::default();
        assert_eq!(
            owners.source(&steamapps.join("common/hades")),
            Some(Source::File(icon))
        );
        assert_eq!(owners.source(&steamapps.join("common/Other")), None);
        assert_eq!(
            steam::value("\t\"installdir\"\t\t\"Hollow Knight\"", "installdir").as_deref(),
            Some("Hollow Knight")
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(all(unix, not(target_os = "android"), not(target_os = "macos")))]
    #[test]
    fn linux_folders_match_desktop_entries() {
        use platform::{Desktop, find_icon};
        let dir = std::env::temp_dir().join(format!("freesize-xdg-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let apps = dir.join("applications");
        fs::create_dir_all(&apps).unwrap();
        fs::write(
            apps.join("org.mozilla.firefox.desktop"),
            "[Desktop Entry]\nName=Firefox\nExec=/usr/lib/firefox/firefox %u\nIcon=org.mozilla.firefox\n\n[Desktop Action new]\nIcon=other\n",
        )
        .unwrap();
        fs::write(
            apps.join("code.desktop"),
            "[Desktop Entry]\nName=Visual Studio Code\nExec=/usr/share/code/code\nIcon=vscode\nStartupWMClass=Code\n",
        )
        .unwrap();
        let icons = dir.join("icons/hicolor/128x128/apps");
        fs::create_dir_all(&icons).unwrap();
        fs::write(icons.join("vscode.png"), b"png").unwrap();

        let index = Desktop::build(std::slice::from_ref(&dir));
        assert_eq!(index.icon_for(".mozilla"), Some("org.mozilla.firefox"));
        assert_eq!(index.icon_for("firefox"), Some("org.mozilla.firefox"));
        assert_eq!(index.icon_for("Code"), Some("vscode"));
        assert_eq!(index.icon_for("music"), None);
        assert_eq!(
            find_icon(std::slice::from_ref(&dir), "vscode"),
            Some(icons.join("vscode.png"))
        );
        assert_eq!(find_icon(std::slice::from_ref(&dir), "missing"), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
