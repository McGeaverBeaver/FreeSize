//! Moving files and folders elsewhere instead of deleting them, keeping the
//! folders they were in, with a record so a move can be put back.
//!
//! The record of the last move is kept in FreeSize's data folder
//! (`last-move.json`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::growth::data_dir;
use crate::history::write_atomically;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// When the move happened, seconds since 1970.
    pub when: u64,
    /// Where things went, under the destination.
    pub folder: PathBuf,
    /// `(from, to)` for each moved item.
    pub moved: Vec<(PathBuf, PathBuf)>,
}

/// Where `item`, found under the scanned folder `root`, goes when moved into
/// `destination`: the same path below a folder named after `root`.
pub fn target(root: &Path, item: &Path, destination: &Path) -> PathBuf {
    let base = root
        .file_name()
        .map_or_else(|| "Drive".into(), |n| n.to_string_lossy().into_owned());
    let rest = item.strip_prefix(root).unwrap_or(item);
    destination.join(base).join(rest)
}

/// Moves `from` to `to`, creating the folders above `to`. Within a drive
/// that's a rename; across drives the copy is checked before the original is
/// removed. Never overwrites anything.
pub fn move_item(from: &Path, to: &Path) -> io::Result<()> {
    if fs::symlink_metadata(to).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", to.display()),
        ));
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy_all(from, to)?;
            if size_of(from)? != size_of(to)? {
                let _ = remove_all(to);
                return Err(io::Error::other("the copy came out a different size"));
            }
            remove_all(from)
        }
    }
}

fn copy_all(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_all(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(from, to).map(|_| ())
    }
}

fn size_of(path: &Path) -> io::Result<u64> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        let mut total = 0;
        for entry in fs::read_dir(path)? {
            total += size_of(&entry?.path())?;
        }
        Ok(total)
    } else {
        Ok(meta.len())
    }
}

fn remove_all(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn manifest_path() -> Option<PathBuf> {
    Some(data_dir()?.join("last-move.json"))
}

pub fn save(manifest: &Manifest) {
    if let (Some(path), Ok(json)) = (manifest_path(), serde_json::to_vec_pretty(manifest)) {
        let _ = write_atomically(&path, &json);
    }
}

/// The last move, if it hasn't been put back.
pub fn last() -> Option<Manifest> {
    let bytes = fs::read(manifest_path()?).ok()?;
    serde_json::from_slice(&bytes)
        .ok()
        .filter(|m: &Manifest| !m.moved.is_empty())
}

/// Moves everything of `manifest` back. Returns how many came back and what
/// couldn't; forgets the move once everything is back.
pub fn put_back(manifest: &Manifest) -> (usize, Vec<String>) {
    let mut back = 0;
    let mut left = Vec::new();
    let mut errors = Vec::new();
    for (from, to) in &manifest.moved {
        match move_item(to, from) {
            Ok(()) => back += 1,
            Err(err) => {
                errors.push(format!("{}: {err}", from.display()));
                left.push((from.clone(), to.clone()));
            }
        }
    }
    let rest = Manifest {
        moved: left,
        ..manifest.clone()
    };
    if rest.moved.is_empty() {
        if let Some(path) = manifest_path() {
            let _ = fs::remove_file(path);
        }
    } else {
        save(&rest);
    }
    (back, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn moves_keep_their_folders_and_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("freesize-moves-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let root = dir.join("phone");
        write(&root.join("DCIM/Camera/a.mp4"), b"video");
        write(&root.join("Download/old/b.zip"), b"zip");
        let destination = dir.join("archive");

        let to = target(&root, &root.join("DCIM/Camera"), &destination);
        assert_eq!(to, destination.join("phone/DCIM/Camera"));
        move_item(&root.join("DCIM/Camera"), &to).unwrap();
        assert!(!root.join("DCIM/Camera").exists());
        assert_eq!(fs::read(to.join("a.mp4")).unwrap(), b"video");

        // An existing target stops the move.
        write(&destination.join("phone/Download/old/b.zip"), b"other");
        let to = target(&root, &root.join("Download/old/b.zip"), &destination);
        assert!(move_item(&root.join("Download/old/b.zip"), &to).is_err());
        assert!(root.join("Download/old/b.zip").exists());

        // Copying (as across drives) checks and then removes the original.
        let copy_to = dir.join("copy/b.zip");
        fs::create_dir_all(dir.join("copy")).unwrap();
        copy_all(&root.join("Download/old/b.zip"), &copy_to).unwrap();
        assert_eq!(size_of(&copy_to).unwrap(), 3);
        let _ = fs::remove_dir_all(&dir);
    }
}
