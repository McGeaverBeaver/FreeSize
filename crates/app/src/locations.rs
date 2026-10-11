//! Drives and folders offered on the start screen.

use std::path::{Path, PathBuf};

pub struct Location {
    pub label: String,
    pub path: PathBuf,
}

/// The default folder to put in the path box.
pub fn default_path() -> PathBuf {
    if cfg!(target_os = "android") {
        PathBuf::from("/storage/emulated/0")
    } else {
        std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    }
}

#[cfg(target_os = "android")]
pub fn list() -> Vec<Location> {
    let mut out = vec![Location {
        label: "Internal storage".into(),
        path: PathBuf::from("/storage/emulated/0"),
    }];
    // SD cards and USB drives show up as /storage/XXXX-XXXX.
    if let Ok(entries) = std::fs::read_dir("/storage") {
        let mut extra: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| !matches!(file_name(p).as_str(), "emulated" | "self"))
            .collect();
        extra.sort();
        out.extend(extra.into_iter().map(|path| Location {
            label: format!("SD card / USB ({})", file_name(&path)),
            path,
        }));
    }
    out
}

#[cfg(windows)]
pub fn list() -> Vec<Location> {
    let mut out: Vec<Location> = ('C'..='Z')
        .map(|letter| PathBuf::from(format!("{letter}:\\")))
        .filter(|path| std::fs::metadata(path).is_ok())
        .map(|path| Location {
            label: format!("Drive {}", path.display()),
            path,
        })
        .collect();
    if let Some(home) = std::env::home_dir() {
        out.push(Location {
            label: "Home folder".into(),
            path: home,
        });
    }
    out
}

#[cfg(target_os = "macos")]
pub fn list() -> Vec<Location> {
    let mut out = vec![Location {
        label: "Macintosh HD".into(),
        path: PathBuf::from("/"),
    }];
    if let Some(home) = std::env::home_dir() {
        out.push(Location {
            label: "Home folder".into(),
            path: home,
        });
    }
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        // The startup disk appears here as a symlink to "/".
        out.extend(
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| Location {
                    label: file_name(&e.path()),
                    path: e.path(),
                }),
        );
    }
    out
}

#[cfg(all(unix, not(target_os = "android"), not(target_os = "macos")))]
pub fn list() -> Vec<Location> {
    let mut out = Vec::new();
    if let Some(home) = std::env::home_dir() {
        out.push(Location {
            label: "Home folder".into(),
            path: home,
        });
    }
    out.push(Location {
        label: "Root filesystem".into(),
        path: PathBuf::from("/"),
    });
    // Other real disks: /proc/mounts entries backed by a block device.
    if let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") {
        for line in mounts.lines() {
            let mut fields = line.split_whitespace();
            let (Some(device), Some(mount), Some(fs_type)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let mount = mount.replace("\\040", " ");
            let skip = !device.starts_with("/dev/")
                || fs_type == "squashfs"
                || mount == "/"
                || ["/boot", "/snap", "/efi", "/var/lib", "/run/user"]
                    .iter()
                    .any(|p| mount.starts_with(p));
            if skip || out.iter().any(|l| l.path == Path::new(&mount)) {
                continue;
            }
            out.push(Location {
                label: file_name(Path::new(&mount)),
                path: PathBuf::from(mount),
            });
        }
    }
    out
}

#[allow(dead_code)]
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
