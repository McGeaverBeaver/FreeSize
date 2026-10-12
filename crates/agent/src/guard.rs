//! Guards that hold on any server: a file that's open right now isn't
//! deleted (a database, a download in progress, a VM's disk).

use std::path::{Path, PathBuf};

use crate::commands::Guard;

/// Refuses paths that a process has open, or folders holding one.
pub struct OpenFiles {
    /// `/proc`, or a stand-in for tests.
    pub proc: PathBuf,
}

impl Default for OpenFiles {
    fn default() -> OpenFiles {
        OpenFiles {
            proc: PathBuf::from("/proc"),
        }
    }
}

/// Every open file, with the process holding it: `(path, process name)`.
pub fn open_files(proc: &Path) -> Vec<(PathBuf, String, u32)> {
    let mut open = Vec::new();
    let Ok(entries) = std::fs::read_dir(proc) else {
        return open;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        let name = std::fs::read_to_string(entry.path().join("comm"))
            .map(|n| n.trim().to_string())
            .unwrap_or_else(|_| pid.to_string());
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path())
                && target.is_absolute()
            {
                open.push((target, name.clone(), pid));
            }
        }
    }
    open
}

impl Guard for OpenFiles {
    fn check(&self, path: &Path) -> Option<String> {
        open_files(&self.proc)
            .into_iter()
            .find(|(open, _, _)| open.starts_with(path))
            .map(|(open, name, _)| {
                if open == path {
                    format!("open in {name} right now")
                } else {
                    format!("{} is open in {name} right now", open.display())
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn open_files_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let proc = dir.path().join("proc");
        let data = dir.path().join("data");
        std::fs::create_dir_all(proc.join("123/fd")).unwrap();
        std::fs::create_dir_all(data.join("db")).unwrap();
        std::fs::write(proc.join("123/comm"), "postgres\n").unwrap();
        std::os::unix::fs::symlink(data.join("db/base.dat"), proc.join("123/fd/5")).unwrap();
        std::os::unix::fs::symlink("socket:[999]", proc.join("123/fd/6")).unwrap();
        let guard = OpenFiles { proc };
        assert_eq!(
            guard.check(&data.join("db/base.dat")).as_deref(),
            Some("open in postgres right now")
        );
        assert!(guard.check(&data).unwrap().contains("is open in postgres"));
        assert_eq!(guard.check(&dir.path().join("other")), None);
    }
}
