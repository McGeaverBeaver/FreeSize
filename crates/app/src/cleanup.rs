//! Space that can be freed without touching any app: trashed photos and
//! videos, old app installers, the thumbnail cache and folders left behind
//! by removed apps. Also works out which app saved which media, so an app's
//! photos and videos can be browsed from its details.
//!
//! Finding and deleting works on plain paths under the shared storage root,
//! so all of it is tested on the desktop. The trash and the media owners
//! come from Android's media index (see `android.rs`).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum JunkKind {
    /// Photos and videos deleted in the last 30 days (Android 11+).
    #[default]
    Trash,
    /// `.apk` files in Download.
    Installers,
    /// `DCIM/.thumbnails`, which gallery apps rebuild.
    Thumbnails,
    /// `Android/media/<package>` folders of apps that are gone.
    Leftovers,
}

impl JunkKind {
    pub const ALL: [JunkKind; 4] = [
        JunkKind::Trash,
        JunkKind::Installers,
        JunkKind::Thumbnails,
        JunkKind::Leftovers,
    ];

    pub fn id(self) -> &'static str {
        match self {
            JunkKind::Trash => "trash",
            JunkKind::Installers => "installers",
            JunkKind::Thumbnails => "thumbnails",
            JunkKind::Leftovers => "leftovers",
        }
    }

    pub fn from_id(id: &str) -> Option<JunkKind> {
        JunkKind::ALL.into_iter().find(|kind| kind.id() == id)
    }
}

/// One kind of deletable files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Junk {
    pub kind: JunkKind,
    pub bytes: u64,
    /// Files and folders to delete.
    pub paths: Vec<PathBuf>,
    /// What they are, for the card: file names, or the apps leftovers
    /// belonged to.
    pub names: Vec<String>,
}

/// Where DiskTree saves APKs for reinstalling apps; never offered as junk.
pub const SAVED_APKS: &str = "Download/DiskTree";

/// Finds old installers, the thumbnail cache and leftover folders under
/// `root`, the shared storage root. `installed` lists every installed
/// package; `labels` names packages seen before (from the app history).
pub fn find(
    root: &Path,
    installed: &HashSet<String>,
    labels: &HashMap<String, String>,
) -> Vec<Junk> {
    [
        installers(root),
        thumbnails(root),
        leftovers(root, installed, labels),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn installers(root: &Path) -> Option<Junk> {
    let keep = root.join(SAVED_APKS);
    let mut junk = Junk {
        kind: JunkKind::Installers,
        ..Junk::default()
    };
    walk(&root.join("Download"), &mut |path, meta| {
        if path == keep {
            return false;
        }
        if meta.is_file() && is_installer(path) {
            junk.bytes += meta.len();
            junk.paths.push(path.to_path_buf());
            junk.names.push(file_name(path));
        }
        true
    });
    (!junk.paths.is_empty()).then_some(junk)
}

fn is_installer(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ["apk", "apks", "apkm", "xapk"]
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

fn thumbnails(root: &Path) -> Option<Junk> {
    let dir = root.join("DCIM").join(".thumbnails");
    let bytes = dir_size(&dir);
    (bytes > 0).then(|| Junk {
        kind: JunkKind::Thumbnails,
        bytes,
        paths: vec![dir],
        names: Vec::new(),
    })
}

fn leftovers(
    root: &Path,
    installed: &HashSet<String>,
    labels: &HashMap<String, String>,
) -> Option<Junk> {
    // Without a believable list of installed apps every folder would look
    // like a leftover.
    if installed.len() < 10 {
        return None;
    }
    let media = root.join("Android").join("media");
    let mut junk = Junk {
        kind: JunkKind::Leftovers,
        ..Junk::default()
    };
    let mut entries: Vec<_> = fs::read_dir(&media).ok()?.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let Some(package) = entry.file_name().to_str().map(String::from) else {
            continue;
        };
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if !is_dir || !looks_like_package(&package) || installed.contains(&package) {
            continue;
        }
        let path = entry.path();
        let bytes = dir_size(&path);
        if bytes == 0 {
            continue;
        }
        junk.bytes += bytes;
        junk.paths.push(path);
        junk.names
            .push(labels.get(&package).cloned().unwrap_or(package));
    }
    (!junk.paths.is_empty()).then_some(junk)
}

fn looks_like_package(name: &str) -> bool {
    name.contains('.')
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
}

/// Visits everything below `dir`, without following symlinks. `visit`
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

/// Total size of the files in a folder, or of a file.
pub fn dir_size(path: &Path) -> u64 {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => meta.len(),
        Ok(meta) if meta.is_dir() => {
            let mut total = 0;
            walk(path, &mut |_, meta| {
                if meta.is_file() {
                    total += meta.len();
                }
                true
            });
            total
        }
        _ => 0,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Deletes files and folders. Returns the bytes freed and a message for
/// each item that couldn't be deleted.
pub fn delete(paths: &[PathBuf]) -> (u64, Vec<String>) {
    let mut freed = 0;
    let mut errors = Vec::new();
    for path in paths {
        let Ok(meta) = fs::symlink_metadata(path) else {
            // Already gone.
            continue;
        };
        let bytes = dir_size(path);
        let result = if meta.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        };
        match result {
            Ok(()) => freed += bytes,
            Err(err) => errors.push(format!("{}: {err}", path.display())),
        }
    }
    (freed, errors)
}

/// The name Android gave a trashed file back: `.trashed-1712345678-IMG.jpg`
/// becomes `IMG.jpg`.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn untrashed_name(name: &str) -> &str {
    name.strip_prefix(".trashed-")
        .and_then(|rest| rest.split_once('-'))
        .map_or(name, |(_, original)| original)
}

/// Media in shared storage saved by one app.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaUse {
    pub package: String,
    pub bytes: u64,
    pub files: u64,
    /// The folder holding most of it, to browse.
    pub folder: PathBuf,
    /// The folder is the app's own `Android/media/<package>`, which Android
    /// counts as part of the app's data.
    pub in_app_data: bool,
}

/// Adds up media files by the app that saved them. Each row is the owning
/// package, the size and the path. Biggest first.
pub fn media_by_owner(rows: impl IntoIterator<Item = (String, u64, PathBuf)>) -> Vec<MediaUse> {
    struct Owner {
        bytes: u64,
        files: u64,
        folders: HashMap<PathBuf, u64>,
    }
    let mut owners: HashMap<String, Owner> = HashMap::new();
    for (package, bytes, path) in rows {
        // Files in the app's Android/media folder count together, so
        // browsing starts at that folder rather than one subfolder.
        let marker = format!("/Android/media/{package}/");
        let path_text = path.to_string_lossy();
        let folder = match path_text.find(&marker) {
            Some(at) => PathBuf::from(&path_text[..at + marker.len() - 1]),
            None => path.parent().map(Path::to_path_buf).unwrap_or_default(),
        };
        let owner = owners.entry(package).or_insert_with(|| Owner {
            bytes: 0,
            files: 0,
            folders: HashMap::new(),
        });
        owner.bytes += bytes;
        owner.files += 1;
        *owner.folders.entry(folder).or_default() += bytes;
    }
    let mut media: Vec<MediaUse> = owners
        .into_iter()
        .map(|(package, owner)| {
            let folder = owner
                .folders
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
                .map(|(folder, _)| folder)
                .unwrap_or_default();
            let in_app_data = folder.ends_with(Path::new("Android/media").join(&package));
            MediaUse {
                package,
                bytes: owner.bytes,
                files: owner.files,
                folder,
                in_app_data,
            }
        })
        .collect();
    media.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then_with(|| a.package.cmp(&b.package))
    });
    media
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(name: &str) -> TempRoot {
            let dir = std::env::temp_dir().join(format!("disktree-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempRoot(dir)
        }

        fn file(&self, path: &str, bytes: usize) -> PathBuf {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, vec![0u8; bytes]).unwrap();
            path
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn installed(packages: &[&str]) -> HashSet<String> {
        let mut set: HashSet<String> = (0..10).map(|i| format!("com.app{i}")).collect();
        set.extend(packages.iter().map(|p| p.to_string()));
        set
    }

    #[test]
    fn finds_installers_but_keeps_saved_apks() {
        let root = TempRoot::new("installers");
        root.file("Download/old.apk", 100);
        root.file("Download/sub/Other.APK", 50);
        root.file("Download/notes.txt", 10);
        root.file("Download/DiskTree/Tool 1.0.apk", 999);
        let junk = find(&root.0, &installed(&[]), &HashMap::new());
        let installers = junk
            .iter()
            .find(|j| j.kind == JunkKind::Installers)
            .unwrap();
        assert_eq!(installers.bytes, 150);
        let mut names = installers.names.clone();
        names.sort();
        assert_eq!(names, ["Other.APK", "old.apk"]);
    }

    #[test]
    fn finds_thumbnails() {
        let root = TempRoot::new("thumbs");
        root.file("DCIM/.thumbnails/1.jpg", 30);
        root.file("DCIM/.thumbnails/2.jpg", 20);
        root.file("DCIM/Camera/photo.jpg", 500);
        let junk = find(&root.0, &installed(&[]), &HashMap::new());
        let thumbs = junk
            .iter()
            .find(|j| j.kind == JunkKind::Thumbnails)
            .unwrap();
        assert_eq!(thumbs.bytes, 50);
        assert_eq!(thumbs.paths, [root.0.join("DCIM/.thumbnails")]);
    }

    #[test]
    fn finds_leftovers_of_removed_apps_only() {
        let root = TempRoot::new("leftovers");
        root.file("Android/media/com.whatsapp/WhatsApp/Media/a.jpg", 70);
        root.file("Android/media/com.gone/x.jpg", 40);
        root.file("Android/media/org.other.gone/y.mp4", 60);
        root.file("Android/media/com.empty.gone/.nomedia", 0);
        root.file("Android/media/notapackage/z.jpg", 10);
        let labels = HashMap::from([("com.gone".to_string(), "Gone App".to_string())]);
        let junk = find(&root.0, &installed(&["com.whatsapp"]), &labels);
        let leftovers = junk.iter().find(|j| j.kind == JunkKind::Leftovers).unwrap();
        assert_eq!(leftovers.bytes, 100);
        assert_eq!(leftovers.names, ["Gone App", "org.other.gone"]);
    }

    #[test]
    fn no_leftovers_without_a_believable_app_list() {
        let root = TempRoot::new("noapps");
        root.file("Android/media/com.gone/x.jpg", 40);
        let junk = find(&root.0, &HashSet::new(), &HashMap::new());
        assert!(junk.iter().all(|j| j.kind != JunkKind::Leftovers));
    }

    #[test]
    fn deletes_files_and_folders() {
        let root = TempRoot::new("delete");
        let file = root.file("Download/old.apk", 100);
        root.file("Android/media/com.gone/a/b.jpg", 40);
        let folder = root.0.join("Android/media/com.gone");
        let missing = root.0.join("missing.apk");
        let (freed, errors) = delete(&[file.clone(), folder.clone(), missing]);
        assert_eq!(freed, 140);
        assert!(errors.is_empty());
        assert!(!file.exists() && !folder.exists());
    }

    #[test]
    fn trashed_names() {
        assert_eq!(untrashed_name(".trashed-1712345678-IMG_1.jpg"), "IMG_1.jpg");
        assert_eq!(untrashed_name("IMG_1.jpg"), "IMG_1.jpg");
    }

    #[test]
    fn media_is_grouped_by_owner_and_folder() {
        let root = PathBuf::from("/storage/emulated/0");
        let rows = vec![
            (
                "com.instagram".to_string(),
                300,
                root.join("Pictures/Instagram/a.jpg"),
            ),
            (
                "com.instagram".to_string(),
                100,
                root.join("Movies/Instagram/b.mp4"),
            ),
            (
                "com.whatsapp".to_string(),
                500,
                root.join("Android/media/com.whatsapp/WhatsApp/Media/WhatsApp Images/c.jpg"),
            ),
            (
                "com.whatsapp".to_string(),
                400,
                root.join("Android/media/com.whatsapp/WhatsApp/Media/WhatsApp Video/d.mp4"),
            ),
        ];
        let media = media_by_owner(rows);
        assert_eq!(media[0].package, "com.whatsapp");
        assert_eq!(media[0].bytes, 900);
        assert_eq!(media[0].files, 2);
        assert_eq!(media[0].folder, root.join("Android/media/com.whatsapp"));
        assert!(media[0].in_app_data);
        assert_eq!(media[1].package, "com.instagram");
        assert_eq!(media[1].folder, root.join("Pictures/Instagram"));
        assert!(!media[1].in_app_data);
    }

    #[test]
    fn kind_ids_round_trip() {
        for kind in JunkKind::ALL {
            assert_eq!(JunkKind::from_id(kind.id()), Some(kind));
        }
    }
}
