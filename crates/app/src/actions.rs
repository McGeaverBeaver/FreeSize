//! Platform actions: picking a folder, showing a file in the file manager,
//! and deleting.

use std::path::{Path, PathBuf};

/// Whether a native folder picker is available.
pub const CAN_BROWSE: bool = cfg!(not(target_os = "android"));

/// Whether "Show in folder" makes sense on this platform.
pub const CAN_REVEAL: bool = cfg!(not(target_os = "android"));

/// Label of the confirm button for deleting.
pub const DELETE_LABEL: &str = if cfg!(target_os = "android") {
    "Delete permanently"
} else if cfg!(windows) {
    "Move to Recycle Bin"
} else {
    "Move to Trash"
};

#[cfg(not(target_os = "android"))]
pub fn browse(start: &Path) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title("Choose a folder to scan");
    if start.is_dir() {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_folder()
}

#[cfg(target_os = "android")]
pub fn browse(_start: &Path) -> Option<PathBuf> {
    None
}

/// Saves an exported report or scan file: through a save dialog on
/// desktops, into `Download/FreeSize` on phones. `None` when cancelled.
pub fn save_export(name: &str, bytes: &[u8]) -> Result<Option<PathBuf>, String> {
    #[cfg(not(target_os = "android"))]
    let path = {
        let extension = Path::new(name)
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
        match rfd::FileDialog::new()
            .set_title("Export")
            .set_file_name(name)
            .add_filter(extension.to_uppercase(), &[extension.as_str()])
            .save_file()
        {
            Some(path) => path,
            None => return Ok(None),
        }
    };
    #[cfg(target_os = "android")]
    let path = {
        let dir = crate::locations::default_path()
            .join("Download")
            .join("FreeSize");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        dir.join(name)
    };
    std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(path))
}

/// Asks where to move things to.
#[cfg(not(target_os = "android"))]
pub fn pick_destination() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Move the basket to")
        .pick_folder()
}

#[cfg(target_os = "android")]
pub fn pick_destination() -> Option<PathBuf> {
    None
}

/// Opens the system file manager with `path` selected.
pub fn reveal(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // explorer parses its own command line; the path must be quoted as one piece.
        std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    {
        let folder = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        std::process::Command::new("xdg-open").arg(folder).spawn()?;
    }
    #[cfg(target_os = "android")]
    let _ = path;
    Ok(())
}

/// How opening a file went.
pub enum Open {
    Done,
    /// Not possible yet (Android is still indexing the file); try again in
    /// a moment.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    TryAgain,
}

/// Opens `path` with the app that normally opens that kind of file, or a
/// folder in the file manager.
pub fn open(path: &Path) -> Result<Open, String> {
    #[cfg(target_os = "android")]
    {
        crate::android::open_file(path).map(|opened| match opened {
            crate::android::Opened::Shown => Open::Done,
            crate::android::Opened::Indexing => Open::TryAgain,
        })
    }
    #[cfg(not(target_os = "android"))]
    {
        #[cfg(windows)]
        let spawned = {
            use std::os::windows::process::CommandExt;
            std::process::Command::new("explorer")
                .raw_arg(format!("\"{}\"", path.display()))
                .spawn()
        };
        #[cfg(target_os = "macos")]
        let spawned = std::process::Command::new("open").arg(path).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let spawned = std::process::Command::new("xdg-open").arg(path).spawn();
        spawned.map(|_| Open::Done).map_err(|e| e.to_string())
    }
}

/// Moves `path` to the trash, or on Android deletes it permanently.
pub fn delete(path: &Path) -> Result<(), String> {
    #[cfg(not(target_os = "android"))]
    {
        trash::delete(path).map_err(|e| e.to_string())
    }
    #[cfg(target_os = "android")]
    {
        let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        let result = if meta.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        result.map_err(|e| e.to_string())
    }
}
