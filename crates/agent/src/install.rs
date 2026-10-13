//! Installing a verified desktop release over the running program: unpack
//! the archive, put the new executable beside the old one, swap them, and
//! start the new one. The old file stays as `<name>.old` until the next
//! start, since Windows can't delete a running program.

use std::io::Read;
use std::path::{Path, PathBuf};

/// The files in a release archive (`.tar.gz` or `.zip`) whose names are in
/// `wanted`, as `(name, bytes)`.
pub fn unpack(
    archive: &[u8],
    name: &str,
    wanted: &[&str],
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut found = Vec::new();
    if name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
            .map_err(|e| format!("{name}: {e}"))?;
        for i in 0..zip.len() {
            let mut file = zip.by_index(i).map_err(|e| format!("{name}: {e}"))?;
            let member = file.name().map_err(|e| format!("{name}: {e}"))?;
            let Some(base) = base_name(&member) else {
                continue;
            };
            if file.is_dir() || !wanted.contains(&base.as_str()) {
                continue;
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|e| format!("{name}: {base}: {e}"))?;
            found.push((base, bytes));
        }
    } else {
        let gz = flate2::read::GzDecoder::new(archive);
        let mut tar = tar::Archive::new(gz);
        for entry in tar.entries().map_err(|e| format!("{name}: {e}"))? {
            let mut entry = entry.map_err(|e| format!("{name}: {e}"))?;
            let path = entry.path().map_err(|e| format!("{name}: {e}"))?;
            let Some(base) = base_name(&path.to_string_lossy()) else {
                continue;
            };
            if !entry.header().entry_type().is_file() || !wanted.contains(&base.as_str()) {
                continue;
            }
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|e| format!("{name}: {base}: {e}"))?;
            found.push((base, bytes));
        }
    }
    if found.is_empty() {
        return Err(format!("{name} holds none of {}", wanted.join(", ")));
    }
    Ok(found)
}

/// The last part of an archive member's path (`./freesize` → `freesize`).
fn base_name(member: &str) -> Option<String> {
    member
        .rsplit(['/', '\\'])
        .next()
        .filter(|n| !n.is_empty() && *n != ".")
        .map(str::to_string)
}

/// The running program's file.
pub fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("where this program is: {e}"))
}

/// The `.old` file a swap leaves beside `exe`.
fn old_name(exe: &Path) -> PathBuf {
    let mut name = exe.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

/// Puts `bytes` in place of the program at `exe`: written beside it first,
/// then renamed over it (the old file is kept as `.old`), so a failure
/// halfway leaves the old program whole.
pub fn replace(exe: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut fresh = exe.as_os_str().to_owned();
    fresh.push(".new");
    let fresh = PathBuf::from(fresh);
    std::fs::write(&fresh, bytes).map_err(|e| format!("{}: {e}", fresh.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("{}: {e}", fresh.display()))?;
    }
    let old = old_name(exe);
    let _ = std::fs::remove_file(&old);
    // Windows can't overwrite a running program but can rename it.
    std::fs::rename(exe, &old).map_err(|e| format!("{}: {e}", exe.display()))?;
    if let Err(err) = std::fs::rename(&fresh, exe) {
        let _ = std::fs::rename(&old, exe);
        let _ = std::fs::remove_file(&fresh);
        return Err(format!("{}: {err}", exe.display()));
    }
    Ok(())
}

/// Replaces every unpacked file that's beside the running program: the
/// program itself and, on desktops, the command line next to it. Returns
/// what was replaced.
pub fn install_beside(exe: &Path, files: &[(String, Vec<u8>)]) -> Result<Vec<String>, String> {
    let dir = exe.parent().ok_or("the program has no folder")?;
    let own = exe.file_name().map(|n| n.to_string_lossy().into_owned());
    let mut replaced = Vec::new();
    // The running program last, so a failure before it changes nothing
    // that's running.
    let mut ordered: Vec<&(String, Vec<u8>)> = files.iter().collect();
    ordered.sort_by_key(|(name, _)| own.as_deref() == Some(name.as_str()));
    for (name, bytes) in ordered {
        let target = dir.join(name);
        if own.as_deref() != Some(name.as_str()) && !target.exists() {
            continue;
        }
        replace(&target, bytes)?;
        replaced.push(name.clone());
    }
    if replaced.is_empty() {
        return Err("nothing in the release matches this program".into());
    }
    Ok(replaced)
}

/// Removes the `.old` file a previous update left, if it's there.
pub fn tidy(exe: &Path) {
    let _ = std::fs::remove_file(old_name(exe));
}

/// Starts the new program with `args` and returns; the caller exits.
pub fn relaunch(exe: &Path, args: &[String]) -> Result<(), String> {
    std::process::Command::new(exe)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("starting {}: {e}", exe.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gz);
        for (name, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, format!("./{name}"), *bytes)
                .unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    fn zipped(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn archives_give_up_the_wanted_files() {
        let files: &[(&str, &[u8])] = &[
            ("freesize-gui", b"gui"),
            ("freesize", b"cli"),
            ("freesize-agent.service", b"unit"),
        ];
        let from_tar = unpack(
            &tar_gz(files),
            "freesize-linux-x86_64.tar.gz",
            &["freesize"],
        )
        .unwrap();
        assert_eq!(from_tar, vec![("freesize".to_string(), b"cli".to_vec())]);
        let from_zip = unpack(
            &zipped(files),
            "freesize-windows-x86_64.zip",
            &["freesize-gui", "freesize"],
        )
        .unwrap();
        assert_eq!(from_zip.len(), 2);
        assert!(unpack(&zipped(files), "x.zip", &["other"]).is_err());
        assert!(unpack(b"junk", "x.tar.gz", &["freesize"]).is_err());
    }

    #[test]
    fn a_swap_keeps_the_old_program_and_tidies_it_later() {
        let dir = std::env::temp_dir().join(format!("freesize-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("freesize");
        std::fs::write(&exe, b"old").unwrap();
        std::fs::write(dir.join("freesize-gui"), b"old gui").unwrap();
        let files = vec![
            ("freesize-gui".to_string(), b"new gui".to_vec()),
            ("freesize".to_string(), b"new".to_vec()),
            ("freesize-hub".to_string(), b"hub".to_vec()),
        ];
        let replaced = install_beside(&exe, &files).unwrap();
        assert_eq!(replaced, vec!["freesize-gui", "freesize"]);
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(dir.join("freesize-gui")).unwrap(), b"new gui");
        assert_eq!(std::fs::read(dir.join("freesize.old")).unwrap(), b"old");
        // Not there before, so not installed.
        assert!(!dir.join("freesize-hub").exists());
        tidy(&exe);
        assert!(!dir.join("freesize.old").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
