//! DiskTree became FreeSize in 0.8.6. What was kept under the old name
//! (data folders, settings in environment variables) is taken over on
//! first start, so an upgrade keeps pairing, settings and
//! history.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// `dir` (named for FreeSize), after moving the folder of the same name
/// from before the rename into its place when only that one exists. When
/// the move fails, the old folder is used as it is.
///
/// The old name is the last part of `dir` with `FreeSize` and `freesize`
/// swapped back: `…/FreeSize` → `…/DiskTree`, `…/freesize-hub` →
/// `…/disktree-hub`.
pub fn adopt_dir(dir: PathBuf) -> PathBuf {
    let Some(old) = old_dir(&dir) else {
        return dir;
    };
    if dir.exists() || !old.is_dir() {
        return dir;
    }
    match std::fs::rename(&old, &dir) {
        Ok(()) => dir,
        Err(_) => old,
    }
}

fn old_dir(dir: &Path) -> Option<PathBuf> {
    let name = dir.file_name()?.to_str()?;
    let old = name
        .replace("FreeSize", "DiskTree")
        .replace("freesize", "disktree");
    (old != name).then(|| dir.with_file_name(old))
}

/// Settings still named for DiskTree (`DISKTREE_HUB_URL`), as the
/// FreeSize names to set them under, where those aren't set already.
pub fn old_env() -> Vec<(String, OsString)> {
    old_env_from(std::env::vars_os())
}

fn old_env_from(vars: impl Iterator<Item = (OsString, OsString)>) -> Vec<(String, OsString)> {
    let vars: Vec<(String, OsString)> = vars
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v)))
        .collect();
    vars.iter()
        .filter_map(|(key, value)| {
            let new = format!("FREESIZE_{}", key.strip_prefix("DISKTREE_")?);
            (!vars.iter().any(|(k, _)| *k == new)).then(|| (new, value.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_folder_moves_into_place() {
        let base = std::env::temp_dir().join(format!("freesize-renamed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("DiskTree/scans")).unwrap();
        std::fs::write(base.join("DiskTree/agent.json"), "{}").unwrap();

        let dir = adopt_dir(base.join("FreeSize"));
        assert_eq!(dir, base.join("FreeSize"));
        assert!(dir.join("agent.json").is_file());
        assert!(dir.join("scans").is_dir());
        assert!(!base.join("DiskTree").exists());

        // Both there: the new one is kept and the old one left alone.
        std::fs::create_dir_all(base.join("disktree-hub")).unwrap();
        std::fs::create_dir_all(base.join("freesize-hub")).unwrap();
        assert_eq!(
            adopt_dir(base.join("freesize-hub")),
            base.join("freesize-hub")
        );
        assert!(base.join("disktree-hub").is_dir());

        // Neither: nothing is made.
        assert_eq!(adopt_dir(base.join("freesize")), base.join("freesize"));
        assert!(!base.join("freesize").exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn old_settings_fill_in_new_names() {
        let vars = [
            ("DISKTREE_HUB_URL", "https://hub.lan"),
            ("DISKTREE_HUB_NAME", "Old"),
            ("FREESIZE_HUB_NAME", "New"),
            ("PATH", "/bin"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        assert_eq!(
            old_env_from(vars.into_iter()),
            [(
                "FREESIZE_HUB_URL".to_string(),
                OsString::from("https://hub.lan")
            )]
        );
    }
}
