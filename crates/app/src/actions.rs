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
